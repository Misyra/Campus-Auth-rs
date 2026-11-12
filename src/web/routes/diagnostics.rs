//! 一键网络诊断路由：POST /api/diagnostics/run
//!
//! 诊断 runner 按需构造（见 `monitor::diagnostics` 模块说明）：handler 从
//! AppState 细粒度取出 ConfigApi 与 HistoryStore 传入，不进入容器拓扑、
//! 不新增服务字段。走标准鉴权中间件，无任何豁免。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::Value;

use crate::config::ConfigApi;
use crate::login::HistoryStore;
use crate::monitor::diagnostics::{self, DiagnosticInputs};
use crate::network::egress::PreparedEgress;
use crate::web::error::{ApiError, data};

/// POST /api/diagnostics/run — 一键网络诊断
///
/// 五项检查并发执行（本机网卡 / 默认网关 / DNS / 认证门户 / 近期登录失败
/// 归因），整体受 15s 总限时约束；结果已脱敏（不含凭据 / token / 带查询串的
/// URL），失败项以结构化明细返回而非 HTTP 错误——诊断失败是「数据」不是
/// 「请求出错」。
pub async fn run_diagnostics(
    State(config): State<Arc<dyn ConfigApi>>,
    State(history): State<Arc<dyn HistoryStore>>,
) -> Result<Json<Value>, ApiError> {
    let rt = config.runtime_snapshot();
    // 门户地址取「登录导航有效地址」：auth_url 为空（重定向/兜底）时落到
    // 触发地址，与浏览器渠道实际导航目标保持一致
    let portal_source = if rt.profile.auth_url.trim().is_empty() {
        "fallback_trigger"
    } else {
        "auth_url"
    };
    let portal_url = rt.profile.effective_browser_login_url().to_string();
    // 出口租约与客户端：与登录/监测同一出口语义（直连/系统代理/静态代理）。
    // prepare 或 client 构建失败不拦截整个诊断——原因进门户检查项落 fail
    let (http_client, egress_lease) = match PreparedEgress::prepare(&rt.network).await {
        Ok(egress) => {
            let client = egress
                .client_builder()
                .and_then(|b| b.redirect(reqwest::redirect::Policy::none()).build())
                .map_err(|e| e.to_string());
            (client, Some(egress))
        }
        Err(e) => (Err(format!("出口配置无法生效：{e}")), None),
    };
    let inputs = DiagnosticInputs {
        portal_url,
        portal_source,
        http_client,
        egress: egress_lease,
    };
    let report = diagnostics::run(inputs, history.as_ref()).await;
    tracing::info!(
        verdict = ?report.verdict,
        total_ms = report.total_ms,
        "一键网络诊断完成"
    );
    Ok(data(serde_json::to_value(report)?))
}
