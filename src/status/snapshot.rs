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
    /// 定时浏览器任务触发（历史遗留：现定时任务统一走通用执行，不再经登录编排器；
    /// 保留以兼容历史记录中的 "browser" 来源反序列化）
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
    /// 快照单调版本号（每次 merge +1）
    ///
    /// 前端据此判断状态新旧：`uptime_seconds` 仅秒级精度，同一秒内的多次
    /// 状态变化无法区分先后；版本号随每次发布严格递增。
    pub snapshot_version: u64,
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
    /// 本次监控连续运行时长（秒）；未监控时为 0
    pub monitoring_seconds: u64,
    /// 监控连续运行的起点（本次进入 Running 的时刻）；内部计时用，不序列化
    #[serde(skip)]
    pub monitoring_started_at: Option<DateTime<Local>>,
    /// 调度器是否运行中
    pub scheduler_running: bool,
    /// 调度器下次触发时间（ISO 8601 字符串）
    pub scheduler_next_fire_at: Option<String>,
    /// 调度器管理的任务数量
    pub scheduler_task_count: usize,
    /// 累计网络检测次数（源自 Metrics::probe_total，由 Engine/监测循环递增；
    /// 接线完成前恒为 0）
    pub probe_total: u64,
    /// 累计登录尝试次数（源自 Metrics::login_total，由 LoginOrchestrator 递增；
    /// 接线完成前恒为 0）
    pub login_total: u64,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            snapshot_version: 0,
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
            monitoring_seconds: 0,
            monitoring_started_at: None,
            scheduler_running: false,
            scheduler_next_fire_at: None,
            scheduler_task_count: 0,
            probe_total: 0,
            login_total: 0,
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
    /// 累计指标更新（probe_total / login_total，取自 `Metrics` 原子计数器）
    ///
    /// 由 Engine（每次网络检测后）与 LoginOrchestrator（每次登录尝试后）
    /// 推送当前计数器值；合并语义为直接覆盖（计数器只增，覆盖即最新值）。
    Totals {
        /// 累计网络检测次数（Metrics::probe_total 当前值）
        probe_total: u64,
        /// 累计登录尝试次数（Metrics::login_total 当前值）
        login_total: u64,
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
            // 监控时长起点：进入 Running 时记录，离开时清零（每次启动重新计时）
            match state {
                EngineState::Running => {
                    if snapshot.engine_state != EngineState::Running {
                        snapshot.monitoring_started_at = Some(Local::now());
                    }
                }
                _ => snapshot.monitoring_started_at = None,
            }
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
            // 监控时长随每秒心跳一并刷新；未监控时归零
            snapshot.monitoring_seconds = snapshot
                .monitoring_started_at
                .map(|t| (Local::now() - t).num_seconds().max(0) as u64)
                .unwrap_or(0);
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
        PartialSnapshot::Totals {
            probe_total,
            login_total,
        } => {
            snapshot.probe_total = *probe_total;
            snapshot.login_total = *login_total;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Engine 变体：整体覆盖引擎相关字段，且不触碰其他字段
    #[test]
    fn test_apply_engine_partial() {
        let mut s = StatusSnapshot::default();
        apply_partial(
            &mut s,
            &PartialSnapshot::Engine {
                state: EngineState::Running,
                network: NetworkStatus::Online,
                last_check: Local::now(),
                pause: true,
                cooling_down: true,
                cooling_down_remaining: Some(120),
                consecutive_failures: 3,
            },
        );
        assert_eq!(s.engine_state, EngineState::Running);
        assert_eq!(s.network_status, NetworkStatus::Online);
        assert!(s.pause_active);
        assert!(s.cooling_down);
        assert_eq!(s.cooling_down_remaining, Some(120));
        assert_eq!(s.consecutive_failures, 3);
        assert!(s.last_check_time.is_some());
        // 无关字段保持默认，不被覆盖
        assert_eq!(s.login_status, LoginStatus::Idle);
        assert_eq!(s.active_profile, "default");
    }

    /// Login 变体：覆盖登录状态字段
    #[test]
    fn test_apply_login_partial() {
        let mut s = StatusSnapshot::default();
        apply_partial(
            &mut s,
            &PartialSnapshot::Login {
                status: LoginStatus::Running,
                source: Some(LoginSource::Manual),
                message: Some("正在认证".into()),
                retry_count: 2,
            },
        );
        assert_eq!(s.login_status, LoginStatus::Running);
        assert_eq!(s.login_source, Some(LoginSource::Manual));
        assert_eq!(s.login_message.as_deref(), Some("正在认证"));
        assert_eq!(s.retry_count, 2);
    }

    /// Worker / Environment / Update 变体
    #[test]
    fn test_apply_worker_environment_update_partials() {
        let mut s = StatusSnapshot::default();
        apply_partial(
            &mut s,
            &PartialSnapshot::Worker {
                state: WorkerStatus::Busy,
            },
        );
        assert_eq!(s.worker_state, WorkerStatus::Busy);

        apply_partial(
            &mut s,
            &PartialSnapshot::Environment {
                progress: Some(InstallProgress {
                    phase: "uv".into(),
                    percent: 50,
                    message: "下载中".into(),
                }),
            },
        );
        assert_eq!(
            s.environment_progress.as_ref().map(|p| p.phase.as_str()),
            Some("uv")
        );
        assert_eq!(s.environment_progress.as_ref().map(|p| p.percent), Some(50));

        apply_partial(&mut s, &PartialSnapshot::Environment { progress: None });
        assert!(s.environment_progress.is_none(), "None 应清空安装进度");

        apply_partial(&mut s, &PartialSnapshot::Update { available: true });
        assert!(s.update_available);
    }

    /// ActiveProfile / MonitorEnabled / Uptime / Scheduler 变体
    #[test]
    fn test_apply_profile_monitor_uptime_scheduler_partials() {
        let mut s = StatusSnapshot::default();
        apply_partial(
            &mut s,
            &PartialSnapshot::ActiveProfile { id: "dorm".into() },
        );
        assert_eq!(s.active_profile, "dorm");

        apply_partial(&mut s, &PartialSnapshot::MonitorEnabled { enabled: false });
        assert!(!s.monitor_enabled);

        apply_partial(&mut s, &PartialSnapshot::Uptime(3600));
        assert_eq!(s.uptime_seconds, 3600);

        apply_partial(
            &mut s,
            &PartialSnapshot::Scheduler {
                running: true,
                next_fire_at: Some("2026-08-14T00:00:00Z".into()),
                task_count: 5,
            },
        );
        assert!(s.scheduler_running);
        assert_eq!(
            s.scheduler_next_fire_at.as_deref(),
            Some("2026-08-14T00:00:00Z")
        );
        assert_eq!(s.scheduler_task_count, 5);
    }

    /// 串行合并：多次部分更新按顺序叠加，后写覆盖先写
    #[test]
    fn test_serial_merge_accumulates() {
        let mut s = StatusSnapshot::default();
        apply_partial(&mut s, &PartialSnapshot::Uptime(10));
        apply_partial(&mut s, &PartialSnapshot::Uptime(20));
        assert_eq!(s.uptime_seconds, 20, "后写应覆盖先写");

        apply_partial(&mut s, &PartialSnapshot::ActiveProfile { id: "a".into() });
        apply_partial(&mut s, &PartialSnapshot::ActiveProfile { id: "b".into() });
        assert_eq!(s.active_profile, "b");
    }

    /// Totals 变体：覆盖累计指标字段，不触碰其他字段
    #[test]
    fn test_apply_totals_partial() {
        let mut s = StatusSnapshot::default();
        assert_eq!(s.probe_total, 0);
        assert_eq!(s.login_total, 0);
        apply_partial(
            &mut s,
            &PartialSnapshot::Totals {
                probe_total: 42,
                login_total: 7,
            },
        );
        assert_eq!(s.probe_total, 42);
        assert_eq!(s.login_total, 7);
        // 计数器语义为「覆盖为最新值」：再次推送更大值直接覆盖
        apply_partial(
            &mut s,
            &PartialSnapshot::Totals {
                probe_total: 43,
                login_total: 8,
            },
        );
        assert_eq!(s.probe_total, 43);
        assert_eq!(s.login_total, 8);
        // 无关字段保持默认，不被覆盖
        assert_eq!(s.uptime_seconds, 0);
        assert_eq!(s.login_status, LoginStatus::Idle);
    }

    /// 序列化契约：probe_total / login_total 字段名精确且默认输出（非 Option、不省略）
    #[test]
    fn test_snapshot_serializes_total_fields() {
        let s = StatusSnapshot::default();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["probe_total"], serde_json::json!(0));
        assert_eq!(json["login_total"], serde_json::json!(0));

        let s = StatusSnapshot {
            probe_total: 123,
            login_total: 45,
            ..StatusSnapshot::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["probe_total"], serde_json::json!(123));
        assert_eq!(json["login_total"], serde_json::json!(45));
    }
}
