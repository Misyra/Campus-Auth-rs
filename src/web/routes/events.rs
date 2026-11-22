//! 事件流查询路由：GET /api/events（近 N 天 JSONL 事件流读回）
//!
//! 数据源为 `src/events/sink.rs` 落盘的 `<base>/events/YYYY-MM-DD.jsonl`
//! （UTC 日期命名，含今日共 N 天）。事件载荷在事件类型层已按**白名单式摘要
//! 设计**脱敏（见 `crate::events` 模块 doc 的脱敏红线：结构上不存在凭据 /
//! token / URL 等敏感字段），本端点**不做二次过滤**——信任链为「事件结构
//! 白名单 → 落盘原文即脱敏产物 → 本端点原样读回」。
//!
//! M1 细粒度 state：handler 声明 `State<Arc<dyn ConfigApi>>` 仅取 `base_path`
//! （事件流目录由 `utils::paths::events_dir` 拼出），测试可注入
//! `test_support::MockConfigApi` 指向临时目录。

use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};

use crate::config::ConfigApi;
use crate::events::EventEnvelope;
use crate::web::error::{ApiError, data};
use crate::web::openapi::schemas::{DataEnvelope, ErrorResponse};

/// 缺省回看天数（Dashboard 活动概览的默认窗口）
const DEFAULT_QUERY_DAYS: u32 = 7;

/// 单个事件流文件的读取行数上限：到达即停止读取该文件并置 `truncated` 标记
///
/// 事件流按日分文件、固定保留 30 天，正常使用下单文件远小于该值；上限只为
/// 兜底异常场景（事件风暴等）把响应体与前端图表拖垮。截断丢弃的是该文件中
/// 更早的物理行。
const MAX_LINES_PER_FILE: usize = 50_000;

// ---- 契约 DTO（events 域）----
//
// 命名与引用规范见 `crate::web::openapi::schemas`。事件载荷为 JSON 透传
// （`type` 判别字段与摘要字段平铺，形状见 `crate::events::AppEvent` 九变体），
// 与 repo 域的 `Value` 透传同口径；字段级契约以前端 `frontend/src/api/types.ts`
// 的 `EventEnvelope` 为准。

/// GET /api/events 200 响应 data 形状（schema 载体）
// 待集成批在 openapi.rs 注册 paths/components 后移除本 allow（当前仅被
// #[utoipa::path] 注解引用，运行时未构造）
#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct EventsData {
    /// 事件封装数组（时间升序；`id` / `timestamp`（UTC RFC3339）/ `type` 与
    /// 载荷摘要字段平铺，载荷为白名单式脱敏结构）
    pub events: Vec<Value>,
    /// 实际生效的回看天数（缺省 7，钳制 1..=30）
    pub days: u32,
    /// 返回的事件条数
    pub total: usize,
    /// 是否发生单文件行数截断（true 时某些日期文件超过行上限，更早的行未读入）
    pub truncated: bool,
}

/// GET /api/events 查询参数
#[derive(Deserialize, Default, IntoParams)]
pub struct EventsQuery {
    /// 回看天数（缺省 7；钳制 1..=30，超出按边界取值）
    pub days: Option<u32>,
}

/// GET /api/events — 查询近 N 天事件流
///
/// 响应可能较大（30 天全量事件），体积由 `days` 参数（≤30）与单文件行数上限
/// 兜底；gzip 由 axum 层 CompressionLayer 统一处理。
#[utoipa::path(
    get,
    path = "/api/events",
    operation_id = "get_api_events",
    tag = "events",
    params(EventsQuery),
    responses(
        (status = 200, description = "近 N 天事件流（时间升序；缺失文件与损坏行跳过，截断见 data.truncated）", body = DataEnvelope<EventsData>),
        (status = 400, description = "days 参数非法（非正整数，由 Query 提取器拒绝）", body = ErrorResponse),
    )
)]
pub async fn get_events(
    State(config): State<Arc<dyn ConfigApi>>,
    Query(params): Query<EventsQuery>,
) -> Result<Json<Value>, ApiError> {
    let days = params
        .days
        .unwrap_or(DEFAULT_QUERY_DAYS)
        .clamp(1, crate::events::EVENT_RETENTION_DAYS);
    let events_dir = crate::utils::paths::events_dir(&config.base_path());
    // JSONL 逐行读取是阻塞 I/O，30 天窗口可能数 MB：放 spawn_blocking 免卡运行时
    let page = tokio::task::spawn_blocking(move || {
        read_recent_events(&events_dir, days, MAX_LINES_PER_FILE)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("事件流读取任务失败: {e}")))?;
    Ok(data(serde_json::json!({
        "events": page.events,
        "days": days,
        "total": page.events.len(),
        "truncated": page.truncated,
    })))
}

/// 近 N 天读取结果（读取层中间结构，行上限分支可直接单测）
struct RecentEventsPage {
    /// 事件封装（按时间升序）
    events: Vec<EventEnvelope>,
    /// 是否有文件触达行上限（该文件更早的物理行未读入）
    truncated: bool,
}

/// 读取 `[今日-(days-1), 今日]`（UTC 日期闭区间）的事件流 JSONL
///
/// - 缺失文件跳过（当天无事件属正常路径，落盘侧惰性建文件）；
/// - 打开失败（权限等）单文件跳过，仅 debug 留痕（与登录历史同口径）；
/// - 损坏行静默跳过、不占用行数配额（与登录历史同口径）；
/// - 单文件读取物理行数达到 `max_lines_per_file` 即停止该文件并置 truncated
///   （丢弃的是文件中更早的行）；
/// - 结果显式按时间戳升序排序，不假设物理文件顺序（与登录历史同口径）。
fn read_recent_events(dir: &Path, days: u32, max_lines_per_file: usize) -> RecentEventsPage {
    let today = Utc::now().date_naive();
    let mut events = Vec::new();
    let mut truncated = false;
    // 按日期升序逐文件读（旧日在前），最终仍显式排序兜底物理顺序
    for offset in (0..days).rev() {
        let date = today - Duration::days(i64::from(offset));
        let path = dir.join(format!("{date}.jsonl"));
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            // 文件缺失属正常路径（当天无事件）
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "打开事件流文件失败，跳过该日期");
                continue;
            }
        };
        let reader = std::io::BufReader::new(file);
        // 行上限按物理行（含空行/损坏行）计：界定单文件读取工作量
        for (file_lines, line) in reader.lines().enumerate() {
            if file_lines >= max_lines_per_file {
                truncated = true;
                break;
            }
            let Ok(line) = line else {
                // 读行 IO 失败：该文件剩余内容不可信，停止读取（已读部分保留）
                tracing::debug!(path = %path.display(), "事件流文件读取中断，已读部分保留");
                break;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<EventEnvelope>(line) {
                Ok(envelope) => events.push(envelope),
                Err(_) => {
                    // 损坏行静默丢弃会让用户误以为事件丢失，留 debug 痕迹
                    tracing::debug!(path = %path.display(), "事件流行解析失败，已跳过");
                }
            }
        }
    }
    events.sort_by_key(|e| e.timestamp);
    RecentEventsPage { events, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use chrono::{Datelike, TimeZone};
    use tower::ServiceExt; // oneshot

    use crate::events::{AppEvent, LoginSummary, NetworkSummary, TaskSummary};
    use crate::web::routes::test_support::{MockConfigApi, body_json};

    /// 写一行事件封装（手动指定 id 与 UTC 时间戳，便于跨日构造）
    fn line(id: &str, ts: chrono::DateTime<Utc>, event: &AppEvent) -> String {
        let envelope = EventEnvelope {
            id: id.to_string(),
            timestamp: ts,
            event: event.clone(),
        };
        serde_json::to_string(&envelope).unwrap()
    }

    /// 组装测试用 Router：MockConfigApi 的 base_path 指向事件流临时目录
    fn mock_app(base_path: std::path::PathBuf) -> Router {
        let (config, inner) = MockConfigApi::mocked();
        inner.lock().unwrap().base_path = base_path;
        Router::new()
            .route("/api/events", get(get_events))
            .with_state(config)
    }

    fn today() -> chrono::NaiveDate {
        Utc::now().date_naive()
    }

    /// 事件流目录不存在的空场景：200 + 空数组 + 未截断
    #[tokio::test]
    async fn test_get_events_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let app = mock_app(dir.path().to_path_buf());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let data = v.get("data").unwrap();
        assert_eq!(data["events"].as_array().unwrap().len(), 0);
        assert_eq!(data["days"], 7, "缺省 days 为 7");
        assert_eq!(data["total"], 0);
        assert_eq!(data["truncated"], false);
    }

    /// 目录存在但无文件（启动后从未落盘）：同样空结果
    #[tokio::test]
    async fn test_get_events_empty_when_dir_exists_without_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("events")).unwrap();
        let app = mock_app(dir.path().to_path_buf());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["data"]["events"].as_array().unwrap().len(), 0);
        assert_eq!(v["data"]["truncated"], false);
    }

    /// 正常多日读取：只落了今日与昨日文件，跨日事件按时间升序合并；
    /// 缺失日期文件跳过不影响其他日期
    #[tokio::test]
    async fn test_get_events_multi_day_sorted_ascending() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let today = today();
        let yesterday = today - Duration::days(1);
        let t = |date: chrono::NaiveDate, h: u32| {
            Utc.with_ymd_and_hms(date.year(), date.month(), date.day(), h, 0, 0)
                .unwrap()
        };
        // 今日文件：两条（含一条损坏行与空行，应被跳过）
        std::fs::write(
            events_dir.join(format!("{today}.jsonl")),
            format!(
                "{}\n\n损坏行不是 JSON\n{}\n",
                line(
                    "evt-2",
                    t(today, 9),
                    &AppEvent::LoginFailed(LoginSummary {
                        profile_id: "p-1".into(),
                        channel: "http".into(),
                        reason: Some("invalid_credential".into()),
                        attempt: Some(2),
                    })
                ),
                line(
                    "evt-3",
                    t(today, 15),
                    &AppEvent::NetworkDown(NetworkSummary {
                        profile_id: None,
                        reason: Some("tcp timeout".into()),
                    })
                ),
            ),
        )
        .unwrap();
        // 昨日文件：一条
        std::fs::write(
            events_dir.join(format!("{yesterday}.jsonl")),
            line(
                "evt-1",
                t(yesterday, 20),
                &AppEvent::LoginSucceeded(LoginSummary {
                    profile_id: "p-1".into(),
                    channel: "browser".into(),
                    reason: None,
                    attempt: Some(1),
                }),
            ),
        )
        .unwrap();
        // 更早日期文件：窗口外（days=2 不覆盖），不应被读入
        let older = today - Duration::days(5);
        std::fs::write(
            events_dir.join(format!("{older}.jsonl")),
            line(
                "evt-old",
                t(older, 8),
                &AppEvent::TaskFailed(TaskSummary {
                    task_id: "t-9".into(),
                    task_kind: Some("script".into()),
                    reason: Some("exit_code_1".into()),
                }),
            ),
        )
        .unwrap();

        let app = mock_app(dir.path().to_path_buf());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/events?days=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let data = v.get("data").unwrap();
        assert_eq!(data["days"], 2);
        let items = data["events"].as_array().unwrap();
        assert_eq!(items.len(), 3, "损坏行与空行跳过、窗口外文件不读");
        // 升序：昨日 → 今日 9 点 → 今日 15 点
        assert_eq!(items[0]["id"], "evt-1");
        assert_eq!(items[1]["id"], "evt-2");
        assert_eq!(items[2]["id"], "evt-3");
        // 载荷字段平铺（type 判别 + 摘要字段）
        assert_eq!(items[0]["type"], "login_succeeded");
        assert_eq!(items[0]["channel"], "browser");
        assert_eq!(items[1]["reason"], "invalid_credential");
        assert_eq!(items[2]["type"], "network_down");
        assert_eq!(data["truncated"], false);
    }

    /// 单文件行数上限：超出部分不读入并置 truncated 标记
    #[tokio::test]
    async fn test_get_events_truncation_marks_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let today = today();
        // 5 行（上限 3）：读入前 3 行，后 2 行丢弃，truncated=true
        let mut content = String::new();
        for i in 0..5 {
            content.push_str(&line(
                &format!("evt-{i}"),
                Utc.with_ymd_and_hms(today.year(), today.month(), today.day(), 8 + i as u32, 0, 0)
                    .unwrap(),
                &AppEvent::TaskFinished(TaskSummary {
                    task_id: format!("t-{i}"),
                    task_kind: None,
                    reason: None,
                }),
            ));
            content.push('\n');
        }
        std::fs::write(events_dir.join(format!("{today}.jsonl")), content).unwrap();

        // 通过直连 read_recent_events 用小上限验证截断分支（生产上限 5 万行，
        // oneshot 造 5 万行文件成本过高）
        let page = read_recent_events(&events_dir, 1, 3);
        assert!(page.truncated, "超限应置 truncated");
        assert_eq!(page.events.len(), 3, "仅读入上限内的前 3 行");
        assert_eq!(page.events[0].id, "evt-0");
        assert_eq!(page.events[2].id, "evt-2");

        // 未超限：truncated=false
        let page = read_recent_events(&events_dir, 1, 100);
        assert!(!page.truncated);
        assert_eq!(page.events.len(), 5);
    }

    /// days 参数边界：缺省 7、0 与超上限钳到 1..=30
    #[tokio::test]
    async fn test_get_events_days_param_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let app = mock_app(dir.path().to_path_buf());
        for (query, expect) in [("", 7), ("days=0", 1), ("days=100", 30), ("days=14", 14)] {
            let uri = format!("/api/events?{query}");
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "query={query}");
            let v = body_json(resp).await;
            assert_eq!(v["data"]["days"], expect, "query={query}");
        }
    }

    /// days 非正整数（负数 / 非数字）：Query 提取器拒绝，400
    #[tokio::test]
    async fn test_get_events_rejects_invalid_days() {
        let dir = tempfile::tempdir().unwrap();
        let app = mock_app(dir.path().to_path_buf());
        for query in ["days=-1", "days=abc"] {
            let uri = format!("/api/events?{query}");
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "query={query}");
        }
    }
}
