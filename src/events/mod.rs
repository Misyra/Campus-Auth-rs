//! 内部事件总线：类型化应用事件 + 广播分发 + JSONL 事件流落盘
//!
//! 由容器构造并持有（[`EventBus`]），供通知、诊断、Dashboard 等消费方订阅；
//! 源头（登录 / 监测 / 任务 / 更新 / 环境等）按需调用 `publish` 发布事件。
//!
//! # 脱敏红线（结构层面保证）
//!
//! 事件载荷是**白名单式摘要设计**：只携带 profile_id、渠道标识、原因码/短语、
//! 任务 id、版本号等非敏感字段。**凭据 / 密码 / token / URL 查询串在结构上就
//! 不存在对应字段**——没有 `password` / `credential` / `token` / `url` 字段，
//! 新增变体时必须遵守同一条红线；需要表达"哪里失败"时用原因码或短语，
//! 发布方负责保证短语本身不含敏感片段（写入层不做二次脱敏，信任链即此）。

pub mod bus;
pub mod sink;

pub use bus::EventBus;
pub use sink::{EVENT_RETENTION_DAYS, spawn_event_sink};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 应用事件封装：唯一 id + UTC 时间戳 + 类型化载荷
///
/// 总线与落盘均以封装为单位传输；JSONL 每行即一个封装（`id` / `timestamp` /
/// `type` 与载荷字段平铺在同一对象内，与 `logs/login_history` 的行格式风格一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// 事件唯一 id（uuid v4，字符串形式；落盘查询与去重的主键）
    pub id: String,
    /// 事件发生时间（UTC，serde 序列化为 RFC3339 字符串）
    pub timestamp: DateTime<Utc>,
    /// 类型化载荷（`type` 为判别字段，与载荷字段平铺）
    #[serde(flatten)]
    pub event: AppEvent,
}

impl EventEnvelope {
    /// 为事件补全 id 与 UTC 时间戳（由 [`EventBus::publish`] 在发布时调用）
    pub fn new(event: AppEvent) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            event,
        }
    }
}

/// 应用事件（类型化枚举，判别字段 `type`，snake_case）
///
/// 载荷统一为摘要结构体（见模块 doc 的脱敏红线）；渠道/类型等标识用
/// `String` 而非上游枚举，避免 events 反向依赖 login / tasks 等业务模块
/// （业务模块发布事件时自行转字符串）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    /// 登录成功
    LoginSucceeded(LoginSummary),
    /// 登录失败（`reason` 为失败原因码或短语）
    LoginFailed(LoginSummary),
    /// 网络探测由通转断（`reason` 为断网原因短语）
    NetworkDown(NetworkSummary),
    /// 网络探测由断转通
    NetworkUp(NetworkSummary),
    /// 自定义任务执行成功
    TaskFinished(TaskSummary),
    /// 自定义任务执行失败（`reason` 为失败原因短语）
    TaskFailed(TaskSummary),
    /// 发现新版本
    UpdateAvailable(UpdateSummary),
    /// Python / Worker 环境就绪（引导或重装完成）
    EnvironmentReady(EnvironmentSummary),
    /// 档案切换完成
    ProfileSwitched(ProfileSwitchSummary),
}

impl AppEvent {
    /// 事件类型标识（与 serde 判别字段同值，供日志与测试断言使用）
    pub fn kind(&self) -> &'static str {
        match self {
            Self::LoginSucceeded(_) => "login_succeeded",
            Self::LoginFailed(_) => "login_failed",
            Self::NetworkDown(_) => "network_down",
            Self::NetworkUp(_) => "network_up",
            Self::TaskFinished(_) => "task_finished",
            Self::TaskFailed(_) => "task_failed",
            Self::UpdateAvailable(_) => "update_available",
            Self::EnvironmentReady(_) => "environment_ready",
            Self::ProfileSwitched(_) => "profile_switched",
        }
    }
}

/// 登录事件摘要
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginSummary {
    /// 触发登录的档案 id
    pub profile_id: String,
    /// 登录渠道标识（`browser` / `http` / `script`）
    pub channel: String,
    /// 失败原因码或短语（成功时为 `None`；发布方保证已脱敏）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 第几次尝试（从 1 起；未知时为 `None`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

/// 网络状态事件摘要
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSummary {
    /// 关联档案 id（探测未绑定具体档案时为 `None`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// 状态变化原因短语（未知时为 `None`；发布方保证已脱敏）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 任务执行事件摘要
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    /// 任务 id
    pub task_id: String,
    /// 任务类型标识（`browser` / `script` / `http`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    /// 失败原因短语（成功时为 `None`；发布方保证已脱敏）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 版本更新事件摘要
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSummary {
    /// 新版本号（semver）
    pub version: String,
    /// 当前运行版本号
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
}

/// 环境就绪事件摘要
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSummary {
    /// Python 环境是否可用
    pub python_ready: bool,
    /// uv 是否可用
    pub uv_ready: bool,
}

/// 档案切换事件摘要
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSwitchSummary {
    /// 切换前档案 id（无前值时为 `None`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_profile_id: Option<String>,
    /// 切换后档案 id
    pub to_profile_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 封装序列化：id / timestamp（RFC3339）/ type 与载荷平铺在同一对象
    #[test]
    fn test_envelope_json_shape() {
        let envelope = EventEnvelope::new(AppEvent::LoginFailed(LoginSummary {
            profile_id: "p-1".into(),
            channel: "http".into(),
            reason: Some("invalid_credential".into()),
            attempt: Some(2),
        }));
        let value: serde_json::Value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["type"], "login_failed");
        assert_eq!(value["profile_id"], "p-1");
        assert_eq!(value["channel"], "http");
        assert_eq!(value["reason"], "invalid_credential");
        assert_eq!(value["attempt"], 2);
        // id 是合法 uuid、timestamp 是 RFC3339
        let id = value["id"].as_str().unwrap();
        assert_eq!(id.len(), uuid::Uuid::nil().to_string().len());
        uuid::Uuid::parse_str(id).expect("id 应为合法 uuid");
        let ts = value["timestamp"].as_str().unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(ts).expect("timestamp 应为 RFC3339");
        assert_eq!(parsed.with_timezone(&Utc), envelope.timestamp);
    }

    /// `None` 字段不落盘（skip_serializing_if）：行体积与隐私面都收敛
    #[test]
    fn test_none_fields_omitted() {
        let envelope = EventEnvelope::new(AppEvent::NetworkUp(NetworkSummary {
            profile_id: None,
            reason: None,
        }));
        let value: serde_json::Value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["type"], "network_up");
        assert!(value.get("profile_id").is_none());
        assert!(value.get("reason").is_none());
    }

    /// 反序列化往返：JSONL 读回后还原同一事件（诊断包消费侧的兼容保证）
    #[test]
    fn test_event_serde_roundtrip() {
        let envelope = EventEnvelope::new(AppEvent::ProfileSwitched(ProfileSwitchSummary {
            from_profile_id: Some("p-old".into()),
            to_profile_id: "p-new".into(),
        }));
        let line = serde_json::to_string(&envelope).unwrap();
        let back: EventEnvelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.event, envelope.event);
        assert_eq!(back.timestamp, envelope.timestamp);
        assert_eq!(back.id, envelope.id);
    }

    /// 事件 id 唯一性：两次构造产生不同 uuid
    #[test]
    fn test_envelope_ids_unique() {
        let a = EventEnvelope::new(AppEvent::UpdateAvailable(UpdateSummary {
            version: "5.2.0".into(),
            current_version: Some("5.1.3".into()),
        }));
        let b = EventEnvelope::new(a.event.clone());
        assert_ne!(a.id, b.id);
        assert_eq!(a.event.kind(), "update_available");
    }
}
