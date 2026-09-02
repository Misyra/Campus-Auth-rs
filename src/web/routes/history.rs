//! 历史路由：登录历史查询与清除
//!
//! M1 细粒度 state 试点：handler 直接声明 `State<Arc<dyn HistoryStore>>` 依赖
//! （经 AppState 的 FromRef 委派提取），不再触达 `state.container`，
//! 测试可注入内存实现做 handler 级单测（见模块测试）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use chrono::{Duration, Local};
use serde::Deserialize;
use serde_json::Value;

use crate::login::{HistoryResult, HistoryStore};
use crate::web::error::{ApiError, data};

/// 查询参数：可选 limit 截断 / page+page_size 分页
#[derive(Deserialize, Default)]
pub struct HistoryQuery {
    /// 返回最近 N 条记录（None 表示不限制）
    pub limit: Option<usize>,
    /// 页码（从 1 开始），与 page_size 同时存在时启用分页响应格式
    pub page: Option<usize>,
    /// 每页条数（默认 50），仅 page 存在时生效
    pub page_size: Option<usize>,
}

/// GET /api/history — 查询最近 30 天登录历史
///
/// 响应中每条记录额外包含 `success: boolean` 字段（`result == "success"` 时为 true），
/// 前端 `LoginHistoryItem` 依赖此字段显示成功/失败状态。
///
/// 响应格式：
/// - 不传 `page` 时：返回裸数组 `LoginHistoryItem[]`（向后兼容）
/// - 传 `page` 时：返回 `{ total, page, page_size, items }` 分页结构
pub async fn get_history(
    State(history): State<Arc<dyn HistoryStore>>,
    Query(params): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let to = Local::now();
    let from = to - Duration::days(30);
    let mut history = history.query(from, to).await?;

    // 按 limit 截断，保留最近的 N 条（列表已按时间升序排列）
    if let Some(limit) = params.limit {
        let len = history.len();
        if len > limit {
            history.drain(..len - limit);
        }
    }

    // total 在 limit 截断**之后**取值，使分页的 total 与可翻页条目数一致
    // （否则 total 反映全量条数，而翻页只作用于 limit 截断后的子集，出现 total 与 items 不一致）
    let total = history.len();

    // 分页（page + page_size 同时存在时启用）
    let use_pagination = params.page.is_some();
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(50).max(1);

    if use_pagination {
        // 从末尾往前分页（最新的是最后一条）
        let start = history.len().saturating_sub(page * page_size);
        let end = history.len().saturating_sub((page - 1) * page_size);
        if start < end {
            history.drain(..start);
            history.truncate(end - start);
        } else {
            history.clear();
        }
    }

    // 为每条记录添加 success 计算字段
    let items: Vec<Value> = history
        .into_iter()
        .map(|e| {
            let success = e.result == HistoryResult::Success;
            let mut v = serde_json::to_value(&e).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("success".to_string(), Value::Bool(success));
            }
            v
        })
        .collect();

    if use_pagination {
        Ok(data(serde_json::json!({
            "total": total,
            "page": page,
            "page_size": page_size,
            "items": items,
        })))
    } else {
        Ok(data(Value::Array(items)))
    }
}

/// DELETE /api/history — 清除登录历史
pub async fn clear_history(
    State(history): State<Arc<dyn HistoryStore>>,
) -> Result<Json<Value>, ApiError> {
    // 破坏性操作（不可恢复地清空全部登录历史），warn 留痕
    tracing::warn!("收到清空登录历史请求，执行清空");
    history.clear().await?;
    Ok(data(Value::String("ok".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use chrono::TimeZone;
    use tower::ServiceExt; // oneshot

    use crate::login::{LoginHistoryEntry, LoginSource};

    /// 内存 HistoryStore：handler 级单测无需磁盘与完整 ServiceContainer（M1）
    #[derive(Default, Clone)]
    struct MockInner {
        entries: Vec<LoginHistoryEntry>,
        clear_calls: usize,
    }

    struct MockHistory(std::sync::Arc<std::sync::Mutex<MockInner>>);

    #[async_trait::async_trait]
    impl HistoryStore for MockHistory {
        async fn query(
            &self,
            _from: chrono::DateTime<Local>,
            _to: chrono::DateTime<Local>,
        ) -> Result<Vec<LoginHistoryEntry>, std::io::Error> {
            Ok(self.0.lock().unwrap().entries.clone())
        }

        async fn clear(&self) -> Result<(), std::io::Error> {
            self.0.lock().unwrap().clear_calls += 1;
            Ok(())
        }
    }

    fn entry(ts_min: u32, result: HistoryResult) -> LoginHistoryEntry {
        LoginHistoryEntry {
            timestamp: Local.with_ymd_and_hms(2026, 8, 17, 12, ts_min, 0).unwrap(),
            source: LoginSource::Manual,
            profile_id: "default".into(),
            result,
            message: "test".into(),
            duration_secs: 1.0,
        }
    }

    fn mock_app() -> (Router, std::sync::Arc<std::sync::Mutex<MockInner>>) {
        let inner = std::sync::Arc::new(std::sync::Mutex::new(MockInner {
            entries: vec![
                entry(0, HistoryResult::Failed),
                entry(1, HistoryResult::Cancelled),
                entry(2, HistoryResult::Success),
            ],
            clear_calls: 0,
        }));
        let store: Arc<dyn HistoryStore> = Arc::new(MockHistory(inner.clone()));
        let app = Router::new()
            .route("/api/history", get(get_history).delete(clear_history))
            .with_state(store);
        (app, inner)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// limit 截断保留最近 N 条，且每条带 success 计算字段
    #[tokio::test]
    async fn test_get_history_limit_keeps_newest() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/history?limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let data = v.get("data").and_then(|d| d.as_array()).unwrap();
        assert_eq!(data.len(), 2);
        // 时间升序排列，limit 保留最后两条（Cancelled / Success）
        assert_eq!(data[0]["result"], "cancelled");
        assert_eq!(data[1]["result"], "success");
        // success 计算字段
        assert_eq!(data[0]["success"], false);
        assert_eq!(data[1]["success"], true);
    }

    /// 分页：total 反映 limit 截断后的条目数，首页返回最新条目
    #[tokio::test]
    async fn test_get_history_pagination() {
        let (app, _inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/history?page=1&page_size=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let data = v.get("data").unwrap();
        assert_eq!(data["total"], 3);
        assert_eq!(data["page"], 1);
        assert_eq!(data["page_size"], 2);
        assert_eq!(data["items"].as_array().unwrap().len(), 2);
        // 首页（最新页）应包含最后两条：Cancelled 与 Success
        assert_eq!(data["items"][0]["result"], "cancelled");
        assert_eq!(data["items"][1]["result"], "success");
    }

    /// 清除接口调用存储 clear 恰好一次
    #[tokio::test]
    async fn test_clear_history_calls_store_once() {
        let (app, inner) = mock_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/history")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(inner.lock().unwrap().clear_calls, 1);
    }
}
