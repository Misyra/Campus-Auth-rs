//! Engine 主循环：select! + 定时器驱动

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Local, Timelike};
use tokio::sync::mpsc;
use tokio::time::{Duration, Interval};

use crate::engine::{
    Engine, EngineCommand, EngineDeps, EngineError, ProfileSwitchSource, TestNetworkResult,
    ProbeDetails, MAX_IDLE_SLEEP_SECS, PROFILE_CHECK_INTERVAL_MIN, PROFILE_CHECK_INTERVAL_MAX,
};
use crate::status::{EngineState, LoginSource, NetworkStatus, PartialSnapshot};
use crate::status::Notifier;
use crate::login::LoginResult;

/// 连续失败多少次后进入冷却期
const COOLING_DOWN_THRESHOLD: u32 = 3;
/// 冷却期持续时间（秒）
const COOLING_DOWN_DURATION_SECS: u64 = 300;

/// Engine 内部栈上状态（单 task 独占，不跨 Arc）
struct EngineInner {
    /// 监测循环是否启用
    monitoring: bool,
    /// 上次网络状态
    last_network_status: NetworkStatus,
    /// 上次网络检测时间
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
    /// 登录结果回传 sender（后台 spawn 的登录任务完成后通知主循环）
    login_result_tx: mpsc::Sender<LoginResult>,
    /// 登录失败通知去重器（同 Profile 仅提醒一次，切换/成功后重置）
    notifier: Notifier,
}

impl EngineInner {
    fn new(login_result_tx: mpsc::Sender<LoginResult>) -> Self {
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
    // 登录结果回传 channel（后台 spawn 的登录任务完成后通知主循环，携带完整结果以区分来源）
    let (login_result_tx, mut login_result_rx) = mpsc::channel::<LoginResult>(16);
    let mut inner = EngineInner::new(login_result_tx);
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
            _ = inner.check_timer.tick() => {
                // 定时器常驻，仅在监测中且未暂停时执行探测
                if inner.monitoring && !is_any_pause_active(&inner, &deps) {
                    handle_network_check(&mut inner, &deps).await;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(MAX_IDLE_SLEEP_SECS)) => {}
        }

        // 步骤 3：低频 Profile 切换检测
        if inner.monitoring
            && !is_any_pause_active(&inner, &deps)
            && deps.config_service.load_settings().auto_switch
            && inner.last_profile_check.elapsed() >= profile_check_interval_duration(&deps)
        {
            check_profile_switch(&mut inner, &deps).await;
            inner.last_profile_check = Instant::now();
        }
    }
}

/// 分发命令到对应处理函数。返回 `true` 表示应退出主循环。
async fn handle_command(cmd: EngineCommand, inner: &mut EngineInner, deps: &EngineDeps) -> bool {
    match cmd {
        EngineCommand::Start => { handle_start(inner, deps).await; false }
        EngineCommand::Stop => { handle_stop(inner, deps).await; false }
        EngineCommand::Reload => { handle_reload(inner, deps).await; false }
        EngineCommand::ApplyProfile { profile_id, source } => {
            handle_apply_profile(&profile_id, source, inner, deps).await;
            false
        }
        EngineCommand::TestNetwork { reply } => {
            let result = handle_test_network(inner, deps).await;
            let _ = reply.send(result);
            false
        }
        EngineCommand::Pause => { handle_pause(inner, deps).await; false }
        EngineCommand::Resume => { handle_resume(inner, deps).await; false }
        EngineCommand::Shutdown => {
            handle_shutdown(inner, deps).await;
            true
        }
    }
}

async fn handle_start(inner: &mut EngineInner, deps: &EngineDeps) {
    if inner.monitoring {
        tracing::debug!("监测已在运行中，忽略 Start 命令");
        return;
    }
    inner.monitoring = true;
    merge_engine_state(inner, deps, EngineState::Running);
    // 立即执行一次检测
    handle_network_check(inner, deps).await;
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
    deps.status_manager
        .merge(PartialSnapshot::ActiveProfile {
            id: profile_id.to_string(),
        });
    tracing::info!("Profile 已切换: {} (来源: {:?})", profile_id, _source);
    // 新 Profile 可能有不同的 auth_url / 凭证，重新判断网络状态
    if inner.monitoring {
        handle_network_check(inner, deps).await;
        reset_check_timer(inner, deps).await;
    }
}

async fn handle_test_network(inner: &EngineInner, deps: &EngineDeps) -> Result<TestNetworkResult, EngineError> {
    tracing::info!("开始网络连通性测试");
    // Engine 统一负责暂停检查：暂停期内直接返回 Paused，不执行探测
    if is_any_pause_active(inner, deps) {
        tracing::info!("网络测试跳过：监测已暂停");
        return Ok(TestNetworkResult {
            status: NetworkStatus::Paused,
            details: ProbeDetails {
                tcp: vec!["Disabled".to_string()],
                http: vec!["Disabled".to_string()],
                url: vec!["Disabled".to_string()],
            },
            duration_ms: 0,
        });
    }
    let start = std::time::Instant::now();
    let report = deps
        .monitor_service
        .check_once()
        .await
        .map_err(|e| EngineError::ProbeError(e.to_string()))?;
    let duration_ms = start.elapsed().as_millis() as u64;
    let details = ProbeDetails {
        tcp: vec![format!("{:?}", report.tcp_outcome)],
        http: vec![format!("{:?}", report.http_outcome)],
        url: vec![format!("{:?}", report.url_outcome)],
    };
    tracing::info!("网络测试完成: status={:?}, duration={}ms", report.status, duration_ms);
    Ok(TestNetworkResult {
        status: report.status,
        details,
        duration_ms,
    })
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
    // 立即执行一次检测（不等待定时器到期）
    if inner.monitoring {
        handle_network_check(inner, deps).await;
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
fn handle_login_result(result: LoginResult, inner: &mut EngineInner, deps: &EngineDeps) {
    if result.source == LoginSource::Auto {
        inner.auto_login_in_flight = false;
    }
    // 当前活跃 Profile 作为通知去重的键
    let profile_id = deps.config_service.load_settings().active_profile_id;
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
        tracing::warn!(
            "登录失败，连续失败次数: {}",
            inner.consecutive_failures
        );
        if inner.consecutive_failures >= COOLING_DOWN_THRESHOLD && inner.cooling_down_until.is_none() {
            inner.cooling_down_until =
                Some(Instant::now() + Duration::from_secs(COOLING_DOWN_DURATION_SECS));
            tracing::warn!(
                "连续失败达到 {} 次，进入冷却期（{}s）",
                inner.consecutive_failures,
                COOLING_DOWN_DURATION_SECS
            );
        }
    }
    // 监测已停止时不得把状态合并回 Running（后台登录任务可能在 Stop 后才完成）
    let state = if inner.monitoring {
        EngineState::Running
    } else {
        EngineState::Stopped
    };
    merge_engine_state(inner, deps, state);
}

/// 单次网络检查：探测 → 更新状态 → 按结论决定是否触发登录
async fn handle_network_check(inner: &mut EngineInner, deps: &EngineDeps) {
    // 周期性检测入口日志：稳定网络下状态不变也会触发，保证默认 info 级别下可见
    tracing::info!("周期性网络检测触发");
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
    // 冷却期内检查：若仍在冷却则跳过登录
    let cooling_down = inner
        .cooling_down_until
        .is_some();
    let cooling_remaining = if cooling_down {
        inner
            .cooling_down_until
            .map(|until| until.saturating_duration_since(Instant::now()).as_secs() as u32)
    } else {
        None
    };

    let report = match deps.monitor_service.check_once().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("网络探测执行失败: {}", e);
            return;
        }
    };
    let now = Local::now();
    inner.last_check_time = Some(now);
    // 状态变化日志：仅在状态发生转换时记录 info，未变化保持静默（debug）
    let old_status = inner.last_network_status;
    if report.status != old_status {
        tracing::info!(
            "网络状态变化: {:?} → {:?}",
            old_status,
            report.status
        );
    } else {
        tracing::info!("网络状态未变化: {:?}", report.status);
    }
    inner.last_network_status = report.status;
    let paused = is_any_pause_active(inner, deps);
    deps.status_manager.merge(PartialSnapshot::Engine {
        state: EngineState::Running,
        network: report.status,
        last_check: now,
        pause: paused,
        cooling_down,
        cooling_down_remaining: cooling_remaining,
        consecutive_failures: inner.consecutive_failures,
    });

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
    if let Some(matched_id) = deps.profile_service.detect_matching_profile(&gateway_str, ssid_str) {
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
fn merge_engine_state(inner: &EngineInner, deps: &EngineDeps, state: EngineState) {
    let now = Local::now();
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
        last_check: now,
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
}
