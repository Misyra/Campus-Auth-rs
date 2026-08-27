//! Engine 主循环：select! + 定时器驱动
//!
//! 网络探测不在主循环内联 await（F5）：`MonitorService::check_once` 可能耗时
//! 数秒到数十秒（多目标超时叠加），内联执行期间命令通道（Shutdown/Stop 等）
//! 只能排队。探测统一移入独立 tokio 任务，结果经 mpsc channel 回传主循环
//! 处理（模式与登录结果 `LoginResult` channel 一致），命令保持即时响应。

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Local, Timelike};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::{Duration, Interval};

use crate::engine::{
    Engine, EngineCommand, EngineDeps, EngineError, MAX_IDLE_SLEEP_SECS,
    PROFILE_CHECK_INTERVAL_MAX, PROFILE_CHECK_INTERVAL_MIN, ProbeDetails, ProfileSwitchSource,
    TestNetworkResult,
};
use crate::login::LoginResult;
use crate::monitor::ProbeReport;
use crate::status::Notifier;
use crate::status::{EngineState, LoginSource, NetworkStatus, PartialSnapshot};

/// 连续失败多少次后进入冷却期
const COOLING_DOWN_THRESHOLD: u32 = 3;
/// 冷却期持续时间（秒）
const COOLING_DOWN_DURATION_SECS: u64 = 300;

/// 后台探测任务的回传消息
///
/// 成败均须回传：主循环靠它重置 `probe_in_flight` 在途标记，
/// 只回传成功会导致标记永不复位、自动监测永久停摆。
enum ProbeMessage {
    /// 探测成功完成，携带报告
    Report(ProbeReport),
    /// 探测执行失败（错误描述）
    Failed(String),
}

/// Engine 内部栈上状态（单 task 独占，不跨 Arc）
struct EngineInner {
    /// 监测循环是否启用
    monitoring: bool,
    /// 上次网络状态
    last_network_status: NetworkStatus,
    /// 上次网络检测时间
    ///
    /// 仅在真实探测结果回传时更新（G3）：登录结果等非检测路径合并引擎状态时
    /// 读取此值，不得用「当前时刻」刷新，否则 last_check 语义被登录事件污染。
    last_check_time: Option<DateTime<Local>>,
    /// 手动暂停标记
    manual_paused: bool,
    /// 上次 Profile 切换检测时间
    last_profile_check: Instant,
    /// 上次检测到的网关 IP
    last_gateway: Option<Ipv4Addr>,
    /// 上次检测到的 WiFi SSID
    last_ssid: Option<String>,
    /// 网络检查定时器（常驻，由 monitoring + 暂停状态门控）
    check_timer: Interval,
    /// 连续登录失败次数
    consecutive_failures: u32,
    /// 冷却期截止时刻（None 表示不在冷却中）
    cooling_down_until: Option<Instant>,
    /// 是否有 source=Auto 的登录会话在途
    ///
    /// 每轮 CaptivePortal 检测都会提交 Auto 登录，若上一轮会话仍在运行，
    /// Orchestrator 去重会返回同一句柄——多个后台任务等待同一会话并各自
    /// 回传结果，同一次登录被重复计数、冷却阈值被提前触发。此标记保证
    /// 同一时刻只有一个 Auto 结果会被回传计数。
    auto_login_in_flight: bool,
    /// 是否有网络探测任务在途（F5）
    ///
    /// 探测移入后台任务后的防重入标记：周期定时器在途时直接忽略
    /// （高频事件无需排队）；用户主动的 Resume/Start/ApplyProfile 在途
    /// 时排队一次，避免“恢复监测后立即检测被吞”导致延迟一个间隔。
    probe_in_flight: bool,
    /// 在途期间排队的即时检测请求（仅用户主动触发时置位，结果回传后补发一次）
    probe_pending: bool,
    /// 探测结果回传 sender（后台探测任务完成后通知主循环）
    probe_result_tx: mpsc::Sender<ProbeMessage>,
    /// 登录结果回传 sender（后台 spawn 的登录任务完成后通知主循环）
    login_result_tx: mpsc::Sender<LoginResult>,
    /// 登录失败通知去重器（同 Profile 仅提醒一次，切换/成功后重置）
    notifier: Notifier,
}

impl EngineInner {
    fn new(
        probe_result_tx: mpsc::Sender<ProbeMessage>,
        login_result_tx: mpsc::Sender<LoginResult>,
    ) -> Self {
        Self {
            monitoring: false,
            last_network_status: NetworkStatus::Offline,
            last_check_time: None,
            manual_paused: false,
            last_profile_check: Instant::now(),
            last_gateway: None,
            last_ssid: None,
            check_timer: {
                let mut t = tokio::time::interval(Duration::from_secs(
                    crate::engine::DEFAULT_CHECK_INTERVAL_SECS,
                ));
                t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                t
            },
            consecutive_failures: 0,
            cooling_down_until: None,
            auto_login_in_flight: false,
            probe_in_flight: false,
            probe_pending: false,
            probe_result_tx,
            login_result_tx,
            notifier: Notifier::new(),
        }
    }
}

/// 主循环
pub(crate) async fn run_loop(
    _engine: Arc<Engine>,
    deps: EngineDeps,
    mut cmd_rx: mpsc::Receiver<EngineCommand>,
) {
    // 探测结果回传 channel（后台探测任务 → 主循环，F5）
    let (probe_result_tx, mut probe_result_rx) = mpsc::channel::<ProbeMessage>(8);
    // 登录结果回传 channel（后台 spawn 的登录任务完成后通知主循环，携带完整结果以区分来源）
    let (login_result_tx, mut login_result_rx) = mpsc::channel::<LoginResult>(16);
    let mut inner = EngineInner::new(probe_result_tx, login_result_tx);
    loop {
        // 步骤 1：命令优先（try_recv 预检）
        match cmd_rx.try_recv() {
            Ok(cmd) => {
                if handle_command(cmd, &mut inner, &deps).await {
                    break;
                }
                continue;
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }

        // 步骤 2：tokio::select! 等待事件（biased 保证命令优先）
        tokio::select! {
            biased;
            Some(cmd) = cmd_rx.recv() => {
                if handle_command(cmd, &mut inner, &deps).await {
                    break;
                }
            }
            Some(result) = login_result_rx.recv() => {
                // 登录结果回传：更新连续失败计数与冷却状态
                handle_login_result(result, &mut inner, &deps);
            }
            Some(msg) = probe_result_rx.recv() => {
                // 探测结果回传：更新网络状态并决策是否触发登录（F5）
                handle_probe_message(msg, &mut inner, &deps);
            }
            _ = inner.check_timer.tick() => {
                // 定时器常驻，仅在监测中且未暂停时执行探测
                if inner.monitoring && !is_any_pause_active(&inner, &deps) {
                    handle_network_check(&mut inner, &deps);
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(MAX_IDLE_SLEEP_SECS)) => {}
        }

        // 步骤 3：低频 Profile 切换检测
        if inner.monitoring
            && !is_any_pause_active(&inner, &deps)
            && deps.config_service.runtime().load().auto_switch
            && inner.last_profile_check.elapsed() >= profile_check_interval_duration(&deps)
        {
            check_profile_switch(&mut inner, &deps).await;
            inner.last_profile_check = Instant::now();
        }
    }
    // 主循环退出（Shutdown / 命令端全关）后在途探测任务自然终结：
    // 其持有的 probe_result_tx 克隆指向已 drop 的接收端，send 失败即返回，
    // 结果被丢弃——关闭后的引擎不再处理任何探测结果。
}

/// 分发命令到对应处理函数。返回 `true` 表示应退出主循环。
async fn handle_command(cmd: EngineCommand, inner: &mut EngineInner, deps: &EngineDeps) -> bool {
    match cmd {
        EngineCommand::Start => {
            handle_start(inner, deps).await;
            false
        }
        EngineCommand::Stop => {
            handle_stop(inner, deps).await;
            false
        }
        EngineCommand::Reload => {
            handle_reload(inner, deps).await;
            false
        }
        EngineCommand::ApplyProfile { profile_id, source } => {
            handle_apply_profile(&profile_id, source, inner, deps).await;
            false
        }
        EngineCommand::TestNetwork { reply } => {
            handle_test_network(inner, deps, reply);
            false
        }
        EngineCommand::Pause => {
            handle_pause(inner, deps).await;
            false
        }
        EngineCommand::Resume => {
            handle_resume(inner, deps).await;
            false
        }
        EngineCommand::Shutdown => {
            handle_shutdown(inner, deps).await;
            true
        }
    }
}

/// 手动操作（Start/Resume/ApplyProfile）触发的立即检测是否被暂停窗口拦截（F4）
///
/// 定时器分支已有 `is_any_pause_active` 门控，但历史实现的立即检测路径没有：
/// 定时暂停窗口内手动 Start/Resume/切 Profile 仍会触发探测乃至自动登录，
/// 暂停语义被绕过。三处复用本函数统一拦截：Start 仍会置 monitoring=true
/// （暂停窗口结束后由定时器恢复正常检测），仅跳过立即检测本身。
fn immediate_check_blocked_by_pause(inner: &EngineInner, deps: &EngineDeps) -> bool {
    if is_any_pause_active(inner, deps) {
        tracing::info!("监测处于暂停时段，跳过手动操作触发的立即检测");
        true
    } else {
        false
    }
}

async fn handle_start(inner: &mut EngineInner, deps: &EngineDeps) {
    if inner.monitoring {
        tracing::debug!("监测已在运行中，忽略 Start 命令");
        return;
    }
    inner.monitoring = true;
    merge_engine_state(inner, deps, EngineState::Running);
    // 立即执行一次检测（暂停窗口内跳过，F4；探测在后台任务执行，F5）
    // 用户主动触发，使用优先级标记以在在途时排队一次
    if !immediate_check_blocked_by_pause(inner, deps) {
        handle_network_check_with_priority(inner, deps, true);
    }
    // 用配置间隔重建定时器（内部会消费首个立即 tick，避免紧随本次手动检测再探测一轮）
    reset_check_timer(inner, deps).await;
    tracing::info!("监测已启动");
}

async fn handle_stop(inner: &mut EngineInner, deps: &EngineDeps) {
    if !inner.monitoring {
        tracing::debug!("监测已停止，忽略 Stop 命令");
        return;
    }
    inner.monitoring = false;
    merge_engine_state(inner, deps, EngineState::Stopped);
    tracing::info!("监测已停止");
}

async fn handle_reload(inner: &mut EngineInner, deps: &EngineDeps) {
    match deps.config_service.reload().await {
        Ok(()) => {
            tracing::info!("配置已重载");
        }
        Err(e) => {
            tracing::warn!("配置重载失败: {}", e);
            // 继续使用旧配置运行
        }
    }
    // 用新配置重置检查间隔（Reload 语义只改间隔，不应意外触发一次即时探测）
    reset_check_timer(inner, deps).await;
}

async fn handle_apply_profile(
    profile_id: &str,
    _source: ProfileSwitchSource,
    inner: &mut EngineInner,
    deps: &EngineDeps,
) {
    // 切换活跃 Profile（内部重建 RuntimeConfig 并原子替换）
    if let Err(e) = deps.profile_service.switch_profile(profile_id).await {
        tracing::warn!("切换 Profile 失败: {} ({})", profile_id, e);
        return;
    }
    deps.status_manager.merge(PartialSnapshot::ActiveProfile {
        id: profile_id.to_string(),
    });
    tracing::info!("Profile 已切换: {} (来源: {:?})", profile_id, _source);
    // 新 Profile 可能有不同的 auth_url / 凭证，重新判断网络状态。
    // 与 Start/Resume 相同的暂停门控（F4）：暂停窗口内只切换不探测
    if inner.monitoring && !immediate_check_blocked_by_pause(inner, deps) {
        handle_network_check_with_priority(inner, deps, true);
        reset_check_timer(inner, deps).await;
    }
}

/// 处理 TestNetwork 命令：探测在后台任务执行并直接回传 oneshot（F5）
///
/// 手动诊断探测不参与 `probe_in_flight` 在途合并（与周期检测语义独立，
/// 并发执行无害），也不修改引擎状态——结果仅供命令发起方消费。
fn handle_test_network(
    inner: &EngineInner,
    deps: &EngineDeps,
    reply: oneshot::Sender<Result<TestNetworkResult, EngineError>>,
) {
    tracing::info!("开始网络连通性测试");
    // Engine 统一负责暂停检查：暂停期内直接返回 Paused，不执行探测
    if is_any_pause_active(inner, deps) {
        tracing::info!("网络测试跳过：监测已暂停");
        let _ = reply.send(Ok(TestNetworkResult {
            status: NetworkStatus::Paused,
            details: ProbeDetails {
                tcp: vec!["Disabled".to_string()],
                http: vec!["Disabled".to_string()],
                url: vec!["Disabled".to_string()],
            },
            duration_ms: 0,
        }));
        return;
    }
    // 探测移入后台任务：check_once 可能耗时数十秒，内联 await 会阻塞
    // 命令通道（Shutdown/Stop 排队，F5）
    let monitor = deps.monitor_service.clone();
    tokio::spawn(async move {
        let start = Instant::now();
        let result = match monitor.check_once().await {
            Ok(report) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                tracing::info!(
                    "网络测试完成: status={:?}, duration={}ms",
                    report.status,
                    duration_ms
                );
                Ok(TestNetworkResult {
                    status: report.status,
                    details: ProbeDetails {
                        tcp: vec![format!("{:?}", report.tcp_outcome)],
                        http: vec![format!("{:?}", report.http_outcome)],
                        url: vec![format!("{:?}", report.url_outcome)],
                    },
                    duration_ms,
                })
            }
            Err(e) => Err(EngineError::ProbeError(e.to_string())),
        };
        // 主循环退出后 reply 接收端可能已 drop：发送失败即丢弃
        let _ = reply.send(result);
    });
}

async fn handle_pause(inner: &mut EngineInner, deps: &EngineDeps) {
    inner.manual_paused = true;
    // 监测未启动时不得把状态合并成 Running
    merge_engine_state(inner, deps, engine_state_for(inner));
    tracing::info!("监测已暂停");
}

async fn handle_resume(inner: &mut EngineInner, deps: &EngineDeps) {
    inner.manual_paused = false;
    merge_engine_state(inner, deps, engine_state_for(inner));
    // 立即执行一次检测（不等待定时器到期）。手动 Resume 只解除手动暂停，
    // 定时暂停窗口可能仍然生效——沿用同一门控跳过检测（F4）
    if inner.monitoring && !immediate_check_blocked_by_pause(inner, deps) {
        handle_network_check_with_priority(inner, deps, true);
        reset_check_timer(inner, deps).await;
    }
    tracing::info!("监测已恢复");
}

async fn handle_shutdown(inner: &mut EngineInner, deps: &EngineDeps) {
    merge_engine_state(inner, deps, EngineState::Stopped);
    tracing::info!("引擎正在关闭");
}

/// 处理登录结果回传：更新连续失败计数与冷却状态
///
/// 仅 `source=Auto` 的失败计入冷却统计——Engine 的 Auto 提交可能去重复用
/// Manual/Browser 会话（低优先级 Reuse 高优先级），用户手动登录的结果
/// 与自动重试预算无关，混入会提前触发冷却。
///
/// 注意：该 channel 的发送方只有 Engine 自动登录 spawn 的任务（见
/// `handle_probe_message`），因此收到任意来源的结果都意味着本轮 Auto
/// 提交已结束，必须无条件重置 `auto_login_in_flight`——若按 source 判断，
/// 去重复用到非 Auto 会话时回传的是原始来源，标记将永不重置，
/// 自动登录功能从此永久失效。
fn handle_login_result(result: LoginResult, inner: &mut EngineInner, deps: &EngineDeps) {
    inner.auto_login_in_flight = false;
    // 当前活跃 Profile 作为通知去重的键
    let profile_id = deps.config_service.runtime().load().profile.id.clone();
    if result.success {
        inner.consecutive_failures = 0;
        inner.cooling_down_until = None;
        inner.notifier.on_login_success(&profile_id);
        tracing::info!(source = ?result.source, "登录成功，重置连续失败计数");
    } else if result.source == LoginSource::Auto {
        inner.consecutive_failures += 1;
        // 登录失败通知去重：同一 Profile 首次失败才提醒，避免每次探测失败都刷屏
        if inner.notifier.should_notify_login_failure(&profile_id) {
            tracing::warn!(
                target: "notification",
                "登录失败（{} 已通知，后续同 Profile 失败静默）: profile={profile_id}",
                profile_id
            );
        }
        tracing::warn!("登录失败，连续失败次数: {}", inner.consecutive_failures);
        if inner.consecutive_failures >= COOLING_DOWN_THRESHOLD
            && inner.cooling_down_until.is_none()
        {
            inner.cooling_down_until =
                Some(Instant::now() + Duration::from_secs(COOLING_DOWN_DURATION_SECS));
            tracing::warn!(
                "连续失败达到 {} 次，进入冷却期（{}s）",
                inner.consecutive_failures,
                COOLING_DOWN_DURATION_SECS
            );
        }
    }
    // 监测已停止时不得把状态合并回 Running（后台登录任务可能在 Stop 后才完成）。
    // last_check 使用 inner 中的真实检测时间（G3）：登录结果不是网络检测，
    // 不得刷新 last_check。
    let state = if inner.monitoring {
        EngineState::Running
    } else {
        EngineState::Stopped
    };
    merge_engine_state(inner, deps, state);
}

/// 触发一次网络探测（后台任务执行，F5）
///
/// 原 `handle_network_check` 在 select 分支内联 await `check_once`，探测期间
/// 命令通道完全阻塞。现在只负责「在途检查 + spawn」，探测本体在独立任务：
/// - 防重入：`probe_in_flight` 在途时忽略本次触发（合并为等待当前探测完成，
///   探测本身有限时、无需排队积压；定时器 MissedTickBehavior::Skip 也保证
///   周期触发不积压）；
/// - 结果经 `probe_result_tx` 回传主循环统一处理（last_check / 状态合并 /
///   登录决策都留在主循环，保证与命令处理的串行一致性）。
fn handle_network_check(inner: &mut EngineInner, deps: &EngineDeps) {
    handle_network_check_with_priority(inner, deps, false);
}

/// 带优先级标记的网络探测触发
///
/// `priority=true` 表示用户主动触发（Resume/Start/ApplyProfile），在途时排队一次；
/// `priority=false` 表示周期定时器触发，在途时直接忽略（高频事件不积压）。
fn handle_network_check_with_priority(inner: &mut EngineInner, deps: &EngineDeps, priority: bool) {
    // 周期性检测属于高频内部事件，保持在 debug 级别，避免稳定网络下刷屏。
    tracing::debug!("触发网络检测（后台执行）");
    if inner.probe_in_flight {
        if priority {
            if !inner.probe_pending {
                tracing::debug!("在途期间收到优先级探测请求，已排队一次（结果回传后补发）");
            }
            inner.probe_pending = true;
        } else {
            tracing::debug!("网络探测任务仍在途，忽略本次触发（完成后由周期定时器继续）");
        }
        return;
    }
    inner.probe_in_flight = true;
    let monitor = deps.monitor_service.clone();
    let tx = inner.probe_result_tx.clone();
    tokio::spawn(async move {
        let msg = match monitor.check_once().await {
            Ok(report) => ProbeMessage::Report(report),
            Err(e) => ProbeMessage::Failed(e.to_string()),
        };
        // 主循环退出后接收端已 drop：send 失败即丢弃（shutdown 弃在途探测）
        let _ = tx.send(msg).await;
    });
}

/// 处理探测结果回传：更新状态 → 决策登录（F5）
///
/// 原 `handle_network_check` 内联探测后的后置逻辑整体迁移至此；
/// 冷却清理/判定改在结果到达时执行（比探测发起时更接近决策时刻）。
fn handle_probe_message(msg: ProbeMessage, inner: &mut EngineInner, deps: &EngineDeps) {
    // 无论成败都先复位在途标记，否则后续检测被永久忽略
    inner.probe_in_flight = false;
    // 若在途期间有优先级检测排队，立即补发一次（仅一次，避免无限自激）
    let pending = std::mem::replace(&mut inner.probe_pending, false);
    // 清除过期的冷却标记；冷却期满后重置失败计数，
    // 恢复完整的"连续失败 3 次"预算（否则第二轮起退化为失败 1 次即再冷却）
    if inner
        .cooling_down_until
        .map(|until| Instant::now() >= until)
        .unwrap_or(false)
    {
        inner.cooling_down_until = None;
        inner.consecutive_failures = 0;
    }

    let report = match msg {
        ProbeMessage::Report(r) => r,
        ProbeMessage::Failed(e) => {
            tracing::warn!("网络探测执行失败: {}", e);
            return;
        }
    };

    let now = Local::now();
    inner.last_check_time = Some(now);
    // 状态变化日志：仅在状态发生转换时记录 info，未变化保持静默（debug）
    let old_status = inner.last_network_status;
    if report.status != old_status {
        tracing::info!("网络状态变化: {:?} → {:?}", old_status, report.status);
    } else {
        tracing::debug!("网络状态未变化: {:?}", report.status);
    }
    inner.last_network_status = report.status;
    let paused = is_any_pause_active(inner, deps);
    // 冷却期内检查：若仍在冷却则跳过登录
    let cooling_down = inner.cooling_down_until.is_some();
    let cooling_remaining = if cooling_down {
        inner
            .cooling_down_until
            .map(|until| until.saturating_duration_since(Instant::now()).as_secs() as u32)
    } else {
        None
    };
    // 监测已停止时不得把状态合并回 Running（探测可能在 Stop 后才完成，
    // 与登录结果回传路径同一约束）
    let state = if inner.monitoring {
        EngineState::Running
    } else {
        EngineState::Stopped
    };
    deps.status_manager.merge(PartialSnapshot::Engine {
        state,
        network: report.status,
        last_check: now,
        pause: paused,
        cooling_down,
        cooling_down_remaining: cooling_remaining,
        consecutive_failures: inner.consecutive_failures,
    });
    // G23：探测完成后在同一位置推送累计指标（probe_total 已由监测侧
    // check_once 完成路径单点递增；login_total 读取登录侧维护的计数器）
    if let Some((probe_total, login_total)) = deps.monitor_service.metrics_totals() {
        deps.status_manager.merge(PartialSnapshot::Totals {
            probe_total,
            login_total,
        });
    }

    // 按网络结论决策
    match report.status {
        NetworkStatus::CaptivePortal => {
            // 冷却期内跳过登录
            if cooling_down {
                tracing::info!(
                    "冷却期中（连续失败 {} 次），跳过本轮登录",
                    inner.consecutive_failures
                );
                return;
            }
            // 上一轮 Auto 会话仍在途：跳过本轮触发。即使再提交也会被
            // Orchestrator 去重复用同一会话，只会造成结果重复计数
            if inner.auto_login_in_flight {
                tracing::debug!("自动登录会话仍在途，跳过本轮触发");
                return;
            }
            // auth_url 不可达时不触发登录，避免无效尝试
            if report.auth_url_reachable != Some(false) {
                tracing::info!("检测到门户劫持，触发自动登录");
                inner.auto_login_in_flight = true;
                let orchestrator = deps.orchestrator.clone();
                let tx = inner.login_result_tx.clone();
                tokio::spawn(async move {
                    let handle = orchestrator.submit(LoginSource::Auto, None, None).await;
                    let result = handle.await_result().await;
                    let _ = tx.send(result).await;
                });
            } else {
                tracing::info!("认证地址不可达，跳过本轮登录");
            }
        }
        NetworkStatus::Online | NetworkStatus::Offline | NetworkStatus::Paused => {
            // 无需操作
        }
    }

    // 补发排队的优先级探测（在当前批次的决策与合并完成后触发，避免递归）
    if pending {
        tracing::debug!("补发排队的优先级探测");
        handle_network_check(inner, deps);
    }
}

/// 低频 Profile 切换检测：网关/SSID 变化 → 自动匹配并切换
async fn check_profile_switch(inner: &mut EngineInner, deps: &EngineDeps) {
    let gateways = match deps.network_detect.default_gateways().await {
        Ok(g) => g,
        Err(e) => {
            tracing::debug!("网关探测失败: {}", e);
            return;
        }
    };
    let ssid = match deps.network_detect.current_ssid().await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("SSID 探测失败: {}", e);
            return;
        }
    };
    inner.last_gateway = gateways.first().copied();
    inner.last_ssid = ssid.clone();

    let gateway_str = gateways.first().map(|g| g.to_string()).unwrap_or_default();
    let ssid_str = ssid.as_deref().unwrap_or("");
    if let Some(matched_id) = deps
        .profile_service
        .detect_matching_profile(&gateway_str, ssid_str)
    {
        let current = deps.config_service.runtime().load().profile.id.clone();
        if matched_id != current {
            tracing::info!("检测到网络变化，自动切换到 Profile: {}", matched_id);
            // Profile 已切换：重置登录失败通知去重记录，允许新 Profile 失败时再次提醒
            inner.notifier.on_profile_switch();
            handle_apply_profile(&matched_id, ProfileSwitchSource::AutoSwitch, inner, deps).await;
        }
    }
}

/// 按监测开关推导对外合并的 Engine 状态
fn engine_state_for(inner: &EngineInner) -> EngineState {
    if inner.monitoring {
        EngineState::Running
    } else {
        EngineState::Stopped
    }
}

/// 重建网络检查定时器并消费首个立即 tick
///
/// `tokio::time::interval` 的第一次 `tick()` 立即完成；调用方通常刚做过一次
/// 手动检测（Start/Resume/ApplyProfile），不消费首 tick 会导致紧接着重复探测
/// 一轮；Reload 语义只改间隔，更不应意外触发即时探测。
async fn reset_check_timer(inner: &mut EngineInner, deps: &EngineDeps) {
    let mut t = tokio::time::interval(check_interval_duration(deps));
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let _ = t.tick().await;
    inner.check_timer = t;
}

/// 合并 Engine 状态到 StatusManager 快照
///
/// `last_check` 使用 inner 中记录的真实检测时间（G3）：本函数也被登录结果
/// 等「非网络检测」路径调用，用当前时刻刷新会把登录事件伪装成检测时间。
/// 尚未发生过检测时退化为当前时刻（保持首帧快照可读）。
fn merge_engine_state(inner: &EngineInner, deps: &EngineDeps, state: EngineState) {
    let last_check = inner.last_check_time.unwrap_or_else(Local::now);
    let cooling_down = inner
        .cooling_down_until
        .map(|until| Instant::now() < until)
        .unwrap_or(false);
    let cooling_down_remaining = if cooling_down {
        inner
            .cooling_down_until
            .map(|until| until.saturating_duration_since(Instant::now()).as_secs() as u32)
    } else {
        None
    };
    deps.status_manager.merge(PartialSnapshot::Engine {
        state,
        network: inner.last_network_status,
        last_check,
        pause: inner.manual_paused,
        cooling_down,
        cooling_down_remaining,
        consecutive_failures: inner.consecutive_failures,
    });
}

/// 判断是否存在任意暂停（手动或定时时段）
fn is_any_pause_active(inner: &EngineInner, deps: &EngineDeps) -> bool {
    if inner.manual_paused {
        return true;
    }
    let cfg = deps.config_service.runtime().load();
    if !cfg.pause.enabled {
        return false;
    }
    is_in_pause_window(
        Local::now(),
        cfg.pause.start_hour as u32,
        cfg.pause.start_minute as u32,
        cfg.pause.end_hour as u32,
        cfg.pause.end_minute as u32,
    )
}

/// 判断当前时间是否处于暂停时段（支持跨天）
fn is_in_pause_window(
    now: DateTime<Local>,
    start_hour: u32,
    start_minute: u32,
    end_hour: u32,
    end_minute: u32,
) -> bool {
    let now_min = now.hour() * 60 + now.minute();
    let start = start_hour * 60 + start_minute;
    let end = end_hour * 60 + end_minute;
    if start == end {
        // 全天暂停
        true
    } else if start < end {
        // 同天窗口（半开区间 [start, end)）
        now_min >= start && now_min < end
    } else {
        // 跨天窗口（如 23:00 ~ 06:00，半开区间）
        now_min >= start || now_min < end
    }
}

/// 网络检查间隔（从 RuntimeConfig 读取）
fn check_interval_duration(deps: &EngineDeps) -> Duration {
    let secs = deps.config_service.runtime().load().monitor.check_interval as u64;
    Duration::from_secs(secs.max(1))
}

/// Profile 切换检测间隔（从 RuntimeConfig 读取，clamp 到合法范围）
fn profile_check_interval_duration(deps: &EngineDeps) -> Duration {
    let secs = deps
        .config_service
        .runtime()
        .load()
        .monitor
        .profile_check_interval as u64;
    Duration::from_secs(secs.clamp(PROFILE_CHECK_INTERVAL_MIN, PROFILE_CHECK_INTERVAL_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use chrono::TimeZone;
    use std::net::Ipv4Addr;
    use std::sync::atomic::Ordering;

    use crate::bridge::BridgeSupervisor;
    use crate::config::{ConfigService, ProfileService};
    use crate::environment::EnvironmentManager;
    use crate::login::{LoginHistoryService, LoginOrchestrator};
    use crate::monitor::MonitorService;
    use crate::network::detect::{InterfaceInfo, NetworkDetect, NetworkError};
    use crate::status::StatusManager;
    use crate::tasks::TaskManager;
    use crate::utils::metrics::Metrics;

    /// 构造本地时区的固定时刻（年月日固定，避开 DST 边界）
    fn t(hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2025, 1, 15, hour, minute, 0)
            .unwrap()
    }

    // ============ is_in_pause_window 同天窗口测试 ============

    #[test]
    fn test_is_in_pause_window_same_day_inside() {
        // [09:00, 17:00)：10:00 在窗口内
        assert!(is_in_pause_window(t(10, 0), 9, 0, 17, 0));
    }

    #[test]
    fn test_is_in_pause_window_same_day_before_window() {
        // 08:00 早于 09:00 → 不在窗口
        assert!(!is_in_pause_window(t(8, 0), 9, 0, 17, 0));
    }

    #[test]
    fn test_is_in_pause_window_same_day_after_window() {
        // 18:00 晚于等于 17:00 → 不在窗口（半开区间）
        assert!(!is_in_pause_window(t(18, 0), 9, 0, 17, 0));
    }

    #[test]
    fn test_is_in_pause_window_start_boundary_inclusive() {
        // 起点包含：09:00 == start → true
        assert!(is_in_pause_window(t(9, 0), 9, 0, 17, 0));
    }

    #[test]
    fn test_is_in_pause_window_end_boundary_exclusive() {
        // 终点排除：17:00 == end → false（半开区间）
        assert!(!is_in_pause_window(t(17, 0), 9, 0, 17, 0));
    }

    // ============ is_in_pause_window 跨天窗口测试 ============

    #[test]
    fn test_is_in_pause_window_cross_day_late_night_inside() {
        // [22:00, 06:00) 跨天：23:00 → true
        assert!(is_in_pause_window(t(23, 0), 22, 0, 6, 0));
    }

    #[test]
    fn test_is_in_pause_window_cross_day_early_morning_inside() {
        // 03:00 → true（落入次日 [00:00, 06:00) 段）
        assert!(is_in_pause_window(t(3, 0), 22, 0, 6, 0));
    }

    #[test]
    fn test_is_in_pause_window_cross_day_midday_outside() {
        // 12:00 → false
        assert!(!is_in_pause_window(t(12, 0), 22, 0, 6, 0));
    }

    #[test]
    fn test_is_in_pause_window_cross_day_start_inclusive() {
        // 22:00 == start → true
        assert!(is_in_pause_window(t(22, 0), 22, 0, 6, 0));
    }

    #[test]
    fn test_is_in_pause_window_cross_day_end_exclusive() {
        // 06:00 == end → false（半开区间）
        assert!(!is_in_pause_window(t(6, 0), 22, 0, 6, 0));
    }

    // ============ is_in_pause_window 全天暂停测试 ============

    #[test]
    fn test_is_in_pause_window_all_day_when_start_equals_end() {
        // start == end → 全天暂停
        assert!(is_in_pause_window(t(0, 0), 0, 0, 0, 0));
        assert!(is_in_pause_window(t(12, 30), 0, 0, 0, 0));
        assert!(is_in_pause_window(t(23, 59), 12, 0, 12, 0));
    }

    // ============ is_in_pause_window 分钟精度测试 ============

    #[test]
    fn test_is_in_pause_window_minute_precision_inside() {
        // [09:30, 10:30)：09:30 → true，10:29 → true
        assert!(is_in_pause_window(t(9, 30), 9, 30, 10, 30));
        assert!(is_in_pause_window(t(10, 29), 9, 30, 10, 30));
    }

    #[test]
    fn test_is_in_pause_window_minute_precision_end_exclusive() {
        // 10:30 == end → false
        assert!(!is_in_pause_window(t(10, 30), 9, 30, 10, 30));
    }

    // ================================================================
    // F4/F5/F1 集成测试：后台探测 + 暂停门控（tokio::time 虚拟时钟）
    // ================================================================

    /// 挂起的网络检测器：让 `check_once` 停在物理网卡检查一步（受
    /// `INTERFACE_CHECK_TIMEOUT`（3s）约束），用于构造「探测在途」状态
    struct HangingDetect;

    #[async_trait::async_trait]
    impl NetworkDetect for HangingDetect {
        async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
            std::future::pending().await
        }
        async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
            Ok(vec![])
        }
        async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
            Ok(None)
        }
    }

    /// 构造完整 EngineDeps 并启动 run_loop（真实服务 + 挂起检测器）
    ///
    /// 监测配置：仅启用 TCP 探测但目标为空（通过「全部禁用」检查、不产生
    /// 真实网络请求），物理网卡检查开启 → `check_once` 挂在
    /// `list_interfaces` 上直至 3s 超时返回 Offline。`pause_all_day` 为 true
    /// 时配置全天定时暂停窗口（start == end）。
    #[allow(clippy::type_complexity)]
    async fn make_engine_with_hanging_probe(
        pause_all_day: bool,
    ) -> (
        tempfile::TempDir,
        mpsc::Sender<EngineCommand>,
        Arc<StatusManager>,
        Arc<Metrics>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let (reload_tx, _reload_rx) = mpsc::channel(8);
        let config = ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let mut settings = config.load_settings();
        settings.global.monitor.tcp_enabled = true;
        settings.global.monitor.tcp_targets = vec![];
        settings.global.monitor.http_enabled = false;
        settings.global.monitor.url_enabled = false;
        settings.global.monitor.local_check_enabled = true;
        // 周期定时器调大：测试期间不产生周期 tick 干扰断言
        settings.global.monitor.check_interval = 3600;
        settings.global.monitor.profile_check_interval = 600;
        if pause_all_day {
            settings.global.pause.enabled = true;
            // start == end → 全天暂停（is_in_pause_window 语义）
            settings.global.pause.start_hour = 0;
            settings.global.pause.start_minute = 0;
            settings.global.pause.end_hour = 0;
            settings.global.pause.end_minute = 0;
        }
        config.save_settings(&settings).await.unwrap();
        config.reload().await.unwrap();

        let status = Arc::new(StatusManager::new());
        let metrics = Metrics::new();
        let detector: Arc<dyn NetworkDetect> = Arc::new(HangingDetect);
        let monitor = Arc::new(
            MonitorService::new(
                config.clone(),
                detector.clone(),
                None,
                Some(metrics.clone()),
            )
            .unwrap(),
        );
        let history = Arc::new(LoginHistoryService::new(tmp.path()));
        let profiles = Arc::new(ProfileService::new(config.clone()));
        let bridge = BridgeSupervisor::new(
            tmp.path().to_path_buf(),
            config.clone(),
            status.clone(),
            Some(metrics.clone()),
        );
        let environment = EnvironmentManager::new(tmp.path().to_path_buf(), status.clone(), false);
        let tasks = TaskManager::new(tmp.path(), config.clone());
        let orchestrator = Arc::new(LoginOrchestrator::new(
            config.clone(),
            history,
            status.clone(),
            bridge,
            environment,
            tasks,
            monitor.clone(),
            tokio_util::sync::CancellationToken::new(),
            Some(metrics.clone()),
        ));
        let deps = EngineDeps {
            config_service: config,
            profile_service: profiles,
            orchestrator,
            status_manager: status.clone(),
            monitor_service: monitor,
            network_detect: detector,
        };
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(8);
        let engine = Arc::new(Engine::from_sender(cmd_tx.clone()));
        tokio::spawn(run_loop(engine, deps, cmd_rx));
        (tmp, cmd_tx, status, metrics)
    }

    /// 轮询等待条件成立（虚拟时钟下每次 sleep 1ms；预算 1.5s 虚拟时间，
    /// 刻意低于 3s 的网卡检查超时，保证等待期间探测不会自行完成）
    async fn wait_for(cond: impl Fn() -> bool) {
        for _ in 0..1_500 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("等待条件超时（虚拟时钟 1.5s 内未满足）");
    }

    /// F5：探测在后台任务执行——命令通道保持即时响应、在途触发合并忽略、
    /// 结果回传主循环统一处理；Stop 后完成的探测不得回写 Running。
    #[tokio::test(start_paused = true)]
    async fn test_background_probe_keeps_commands_responsive_and_merges_reentry() {
        let (_tmp, cmd_tx, status, _metrics) = make_engine_with_hanging_probe(false).await;
        let snap = || status.borrow();

        // Start：监测开启，立即检测转入后台（挂在网卡检查上，3s 后才完成）
        cmd_tx.send(EngineCommand::Start).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Running).await;

        // Pause → Resume：Resume 再次触发立即检测，但首个探测仍在途 → 必须被忽略
        cmd_tx.send(EngineCommand::Pause).await.unwrap();
        wait_for(|| snap().pause_active).await;
        cmd_tx.send(EngineCommand::Resume).await.unwrap();
        wait_for(|| !snap().pause_active).await;

        // 探测仍在途时下发 Stop：若探测内联阻塞命令通道（回归），
        // Stop 只能在探测完成后（虚拟 3s 后）才被处理；后台化后立即生效。
        // 等待预算 1.5s（虚拟）内完成即为「未被阻塞」
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Stopped).await;

        // 探测尚未完成（虚拟时钟未越过 3s 超时）：无 Totals 推送。
        //（last_check_time 不可作判据：Start 的状态合并会在首次检测前
        // 以当前时刻兜底填充，见 merge_engine_state 的 G3 注释）
        assert_eq!(snap().probe_total, 0, "探测在途时不应推送任何探测结果指标");

        // 推进虚拟时钟越过网卡检查超时（3s），探测完成并回传主循环
        tokio::time::advance(Duration::from_secs(4)).await;
        wait_for(|| snap().probe_total == 1).await;

        let s = snap();
        // 网卡检查超时 → Offline 报告，且产生了真实 last_check
        assert_eq!(s.network_status, NetworkStatus::Offline);
        assert!(s.last_check_time.is_some());
        // Stop 之后完成的探测不得把引擎状态拉回 Running
        assert_eq!(s.engine_state, EngineState::Stopped);
        // F5 重入合并：Start 与 Resume 共触发两次立即检测，实际只执行一次探测
        //（若未合并在途探测，第二次探测会在 Resume 后 ~3s 完成并把计数推到 2）
        assert_eq!(s.probe_total, 1, "在途探测应被合并忽略而非重复执行");
    }

    /// F4：定时暂停窗口内 Start/Resume 只切状态、不触发立即检测
    #[tokio::test(start_paused = true)]
    async fn test_scheduled_pause_blocks_immediate_probes() {
        let (_tmp, cmd_tx, status, metrics) = make_engine_with_hanging_probe(true).await;
        let snap = || status.borrow();

        // Start：仍应置 monitoring=true（Running），但跳过立即检测
        cmd_tx.send(EngineCommand::Start).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Running).await;
        assert_eq!(snap().probe_total, 0, "暂停窗口内 Start 不应触发探测");

        // Pause → Resume：定时窗口仍生效，Resume 不得触发立即检测
        cmd_tx.send(EngineCommand::Pause).await.unwrap();
        wait_for(|| snap().pause_active).await;
        cmd_tx.send(EngineCommand::Resume).await.unwrap();
        wait_for(|| !snap().pause_active).await;
        assert_eq!(snap().probe_total, 0, "定时暂停窗口内 Resume 不应触发探测");

        // 足量虚拟时间流逝后仍无任何探测执行（定时器分支同样被门控）
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(snap().probe_total, 0);
        assert_eq!(
            metrics.probe_total.load(Ordering::Relaxed),
            0,
            "暂停窗口内不应执行任何探测"
        );
    }

    /// TestNetwork：暂停期直接返回 Paused；正常期探测后台执行、
    /// 回复不阻塞命令通道
    #[tokio::test(start_paused = true)]
    async fn test_test_network_reply_from_background() {
        // 暂停场景：直接返回 Paused，不执行探测
        let (_tmp, cmd_tx, _status, metrics) = make_engine_with_hanging_probe(true).await;
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(EngineCommand::TestNetwork { reply: reply_tx })
            .await
            .unwrap();
        let result = reply_rx.await.unwrap().unwrap();
        assert_eq!(result.status, NetworkStatus::Paused);
        assert_eq!(metrics.probe_total.load(Ordering::Relaxed), 0);

        // 正常场景：先 Start（探测 1 在途），再下发 TestNetwork（探测 2 独立执行），
        // 紧接着 Stop——Stop 在两个探测完成前即被处理（命令不被探测阻塞）
        let (_tmp2, cmd_tx2, status2, metrics2) = make_engine_with_hanging_probe(false).await;
        let snap2 = || status2.borrow();
        cmd_tx2.send(EngineCommand::Start).await.unwrap();
        wait_for(|| snap2().engine_state == EngineState::Running).await;
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx2
            .send(EngineCommand::TestNetwork { reply: reply_tx })
            .await
            .unwrap();
        cmd_tx2.send(EngineCommand::Stop).await.unwrap();
        wait_for(|| snap2().engine_state == EngineState::Stopped).await;
        assert_eq!(snap2().probe_total, 0, "探测在途时不应推送任何探测结果指标");

        // 越过 3s 超时：TestNetwork 的 oneshot 回复从后台任务到达
        tokio::time::advance(Duration::from_secs(4)).await;
        let result = reply_rx.await.unwrap().unwrap();
        assert_eq!(result.status, NetworkStatus::Offline);
        // Start 的探测 + TestNetwork 的探测各计一次（G23 单点递增）
        assert_eq!(metrics2.probe_total.load(Ordering::Relaxed), 2);
    }
}
