//! 历史路由：登录历史查询与清除

use axum::extract::{Query, State};
use axum::Json;
use chrono::{Duration, Local};
use serde::Deserialize;
use serde_json::Value;

use crate::login::HistoryResult;
use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

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
    State(state): State<AppState>,
    Query(params): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let to = Local::now();
    let from = to - Duration::days(30);
    let mut history = state.container.history.query(from, to).await?;

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
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    state.container.history.clear().await?;
    Ok(data(Value::String("ok".into())))
}
