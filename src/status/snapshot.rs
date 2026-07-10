//! 状态快照数据类型：StatusSnapshot / PartialSnapshot / 状态枚举
//!
//! 本文件定义全局状态快照及其部分更新枚举。各服务（engine、login、bridge 等）通过构造
//! [`PartialSnapshot`] 变体并调用 `StatusManager::merge` 推送更新。

use chrono::{DateTime, Local};
use serde::Serialize;

/// 引擎运行状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    /// 监测循环运行中
    Running,
    /// 用户主动停止（Start 命令可恢复）
    Stopped,
    /// 崩溃后多次重启均失败，需用户手动重启
    Dead,
}

/// 网络探测最终结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkStatus {
    /// 互联网正常
    Online,
    /// 需要认证（captive portal）
    CaptivePortal,
    /// 物理断网或所有探测失败
    Offline,
    /// 处于暂停时段，本轮跳过探测
    Paused,
}

/// 登录状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    /// 空闲
    Idle,
    /// 登录执行中
    Running,
    /// 登录成功
    Success,
    /// 登录失败
    Failed,
    /// 登录被取消
    Cancelled,
}

/// 登录来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginSource {
    /// Engine 自动触发
    Auto,
    /// 用户手动触发
    Manual,
    /// CLI 一次性登录
    LoginOnce,
    /// 定时任务浏览器任务触发
    Browser,
}

/// Python Worker 外部状态（前端可见）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    /// 环境未就绪
    NotInstalled,
    /// 环境就绪但 Worker 未启动
    Stopped,
    /// spawn 中
    Starting,
    /// 已通过健康检查
    Ready,
    /// 执行中
    Busy,
    /// 崩溃
    Error,
}

/// 环境安装进度
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallProgress {
    /// 阶段标识
    pub phase: String,
    /// 百分比 0~100
    pub percent: u8,
    /// 人类可读消息
    pub message: String,
}

/// 全局状态快照
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusSnapshot {
    /// 引擎运行状态
    pub engine_state: EngineState,
    /// 是否处于冷却期（连续失败后的等待）
    pub cooling_down: bool,
    /// 冷却剩余秒数
    pub cooling_down_remaining: Option<u32>,
    /// 连续失败次数
    pub consecutive_failures: u32,
    /// 当前网络状态
    pub network_status: NetworkStatus,
    /// 上次网络检测时间
    pub last_check_time: Option<DateTime<Local>>,
    /// 当前登录状态
    pub login_status: LoginStatus,
    /// 当前登录来源
    pub login_source: Option<LoginSource>,
    /// 登录结果消息
    pub login_message: Option<String>,
    /// 当前登录重试次数
    pub retry_count: u32,
    /// 当前活跃 Profile ID
    pub active_profile: String,
    /// 监测是否启用
    pub monitor_enabled: bool,
    /// 是否处于暂停时段
    pub pause_active: bool,
    /// 是否有新版本
    pub update_available: bool,
    /// Python Worker 外部状态
    pub worker_state: WorkerStatus,
    /// 环境安装进度
    pub environment_progress: Option<InstallProgress>,
    /// 应用运行时长（秒）
    pub uptime_seconds: u64,
    /// 调度器是否运行中
    pub scheduler_running: bool,
    /// 调度器下次触发时间（ISO 8601 字符串）
    pub scheduler_next_fire_at: Option<String>,
    /// 调度器管理的任务数量
    pub scheduler_task_count: usize,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            engine_state: EngineState::Stopped,
            cooling_down: false,
            cooling_down_remaining: None,
            consecutive_failures: 0,
            network_status: NetworkStatus::Offline,
            last_check_time: None,
            login_status: LoginStatus::Idle,
            login_source: None,
            login_message: None,
            retry_count: 0,
            active_profile: "default".to_string(),
            monitor_enabled: true,
            pause_active: false,
            update_available: false,
            worker_state: WorkerStatus::NotInstalled,
            environment_progress: None,
            uptime_seconds: 0,
            scheduler_running: false,
            scheduler_next_fire_at: None,
            scheduler_task_count: 0,
        }
    }
}

/// 部分状态更新枚举（8 变体）
#[derive(Debug, Clone)]
pub enum PartialSnapshot {
    /// 监测循环后的整体更新
    Engine {
        /// 引擎状态
        state: EngineState,
        /// 网络状态
        network: NetworkStatus,
        /// 上次检测时间
        last_check: DateTime<Local>,
        /// 是否处于暂停时段
        pause: bool,
        /// 是否冷却中
        cooling_down: bool,
        /// 冷却剩余秒数
        cooling_down_remaining: Option<u32>,
        /// 连续失败次数
        consecutive_failures: u32,
    },
    /// 登录状态机转换更新
    Login {
        /// 登录状态
        status: LoginStatus,
        /// 登录来源
        source: Option<LoginSource>,
        /// 结果消息
        message: Option<String>,
        /// 重试次数
        retry_count: u32,
    },
    /// Worker spawn/exit/崩溃更新
    Worker {
        /// Worker 状态
        state: WorkerStatus,
    },
    /// 环境安装进度更新
    Environment {
        /// 安装进度（None 表示清空）
        progress: Option<InstallProgress>,
    },
    /// 发现新版本
    Update {
        /// 是否有更新
        available: bool,
    },
    /// Profile 切换
    ActiveProfile {
        /// 切换后的 Profile ID
        id: String,
    },
    /// 监测开关变更
    MonitorEnabled {
        /// 是否启用
        enabled: bool,
    },
    /// uptime 周期更新（tuple 变体）
    Uptime(u64),
    /// 调度器运行状态更新
    Scheduler {
        /// 是否运行中
        running: bool,
        /// 下次触发时间（ISO 8601）
        next_fire_at: Option<String>,
        /// 任务数量
        task_count: usize,
    },
}

/// 将单个 [`PartialSnapshot`] 应用到快照上（就地修改）
pub fn apply_partial(snapshot: &mut StatusSnapshot, partial: &PartialSnapshot) {
    match partial {
        PartialSnapshot::Engine {
            state,
            network,
            last_check,
            pause,
            cooling_down,
            cooling_down_remaining,
            consecutive_failures,
        } => {
            snapshot.engine_state = *state;
            snapshot.network_status = *network;
            snapshot.last_check_time = Some(*last_check);
            snapshot.pause_active = *pause;
            snapshot.cooling_down = *cooling_down;
            snapshot.cooling_down_remaining = *cooling_down_remaining;
            snapshot.consecutive_failures = *consecutive_failures;
        }
        PartialSnapshot::Login {
            status,
            source,
            message,
            retry_count,
        } => {
            snapshot.login_status = *status;
            snapshot.login_source = *source;
            snapshot.login_message = message.clone();
            snapshot.retry_count = *retry_count;
        }
        PartialSnapshot::Worker { state } => {
            snapshot.worker_state = *state;
        }
        PartialSnapshot::Environment { progress } => {
            snapshot.environment_progress = progress.clone();
        }
        PartialSnapshot::Update { available } => {
            snapshot.update_available = *available;
        }
        PartialSnapshot::ActiveProfile { id } => {
            snapshot.active_profile = id.clone();
        }
        PartialSnapshot::MonitorEnabled { enabled } => {
            snapshot.monitor_enabled = *enabled;
        }
        PartialSnapshot::Uptime(secs) => {
            snapshot.uptime_seconds = *secs;
        }
        PartialSnapshot::Scheduler {
            running,
            next_fire_at,
            task_count,
        } => {
            snapshot.scheduler_running = *running;
            snapshot.scheduler_next_fire_at = next_fire_at.clone();
            snapshot.scheduler_task_count = *task_count;
        }
    }
}
