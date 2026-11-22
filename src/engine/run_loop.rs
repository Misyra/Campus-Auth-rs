//! Engine 主循环：select! + 定时器驱动
//!
//! 网络探测不在主循环内联 await（F5）：`MonitorService::check_auto_monitor` 可能耗时
//! 数秒到数十秒（多目标超时叠加），内联执行期间命令通道（Shutdown/Stop 等）
//! 只能排队。探测统一移入独立 tokio 任务，结果经 mpsc channel 回传主循环
//! 处理（模式与登录结果 `LoginResult` channel 一致），命令保持即时响应。
//! Profile 切换检测的网关/SSID 探测同此决策（两者各为子进程探测、各 10s 超时，
//! 见 `network::detect::SUBPROCESS_TIMEOUT_SECS`）：探测段同样后台化，匹配与
//! 切换留在主循环执行（见 `check_profile_switch` / `handle_profile_probe_message`）。

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use chrono::{DateTime, Local, Timelike};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
// 时间源分界线（P2-2 墙钟化）：
// - 业务周期（冷却 / 谨慎重试间隔 / 探测周期 / Profile 切换检测周期）按墙钟
//   `SystemTime` 判定——类 Unix 平台系统睡眠期间单调钟停摆，合盖数小时唤醒后
//   睡眠中到期的周期若按单调钟计需再等满一个周期，「唤醒即重认证」被显著推迟。
//   经可注入的 [`WallClock`] 判定，测试注入伪造时钟显式推进（tokio 虚拟时钟
//   `advance` 不影响 `SystemTime`，无法驱动墙钟判定）。
// - 纯性能测量（TestNetwork 耗时统计）与超时护栏（单次探测 timeout）保持
//   tokio 单调钟不动：护栏要的就是单调，防墙钟跳变（NTP 校时）误伤。
use tokio::time::Duration;

use crate::engine::{
    Engine, EngineCommand, EngineDeps, EngineError, MAX_IDLE_SLEEP_SECS,
    PROFILE_CHECK_INTERVAL_MAX, PROFILE_CHECK_INTERVAL_MIN, ProbeDetails, ProfileSwitchSource,
    TestNetworkResult,
};
use crate::events::{AppEvent, NetworkSummary, ProfileSwitchSummary};
use crate::login::{LoginResult, LoginTerminal};
use crate::monitor::{
    AssessmentReason, ConnectivityAssessment, ProbeEvidence, ProbeReport, RecoveryAdvice,
};
use crate::status::Notifier;
use crate::status::{EngineState, LoginSource, NetworkStatus, PartialSnapshot};

/// 连续失败多少次后进入冷却期
const COOLING_DOWN_THRESHOLD: u32 = 3;
/// 冷却期持续时间（秒）
const COOLING_DOWN_DURATION_SECS: u64 = 300;
/// 自适应探测的异常态周期（秒）：CaptivePortal/Offline 状态下的探测间隔。
/// 断网感知延迟从最差一个 check_interval（默认 300s）降到此值
const PROBE_ONLINE_BACKOFF_BASE_SECS: u64 = 30;

/// 周期探测到期睡眠的单片上限：长睡眠切成 ≤60s 的短片，每片醒来按墙钟重估
/// 剩余时长（口径同 `scheduler::cron_loop::MAX_SLEEP_SLICE`），抵御类 Unix
/// 休眠唤醒（CLOCK_MONOTONIC 不计入挂起时间）与墙钟前跳导致的过睡
const MAX_SLEEP_SLICE: Duration = Duration::from_secs(60);

/// 业务周期判定的墙钟时钟源（可注入）
///
/// Engine 的冷却计时 / 谨慎重试间隔 / 探测周期 / Profile 切换检测周期均按
/// 墙钟（[`SystemTime`]）判定：类 Unix 平台系统睡眠期间单调钟（tokio
/// `Instant`）停摆，合盖数小时唤醒后睡眠中到期的周期会再等满一个周期才触发，
/// 「唤醒即重认证」恰是校园网场景刚需，故与 `scheduler::cron_loop` 对齐改用
/// 墙钟口径。
///
/// [`WallClock::system`] 为生产实现（= `SystemTime::now`）；测试经
/// [`WallClock::fake`] 注入伪造时钟并显式推进（原先业务周期用 tokio
/// `Instant` 与虚拟时钟 `advance` 同源驱动，墙钟化后改由注入时钟承担）。
#[derive(Clone)]
pub(crate) struct WallClock {
    now_fn: Arc<dyn Fn() -> SystemTime + Send + Sync>,
}

impl WallClock {
    /// 系统墙钟（生产实现）
    pub(crate) fn system() -> Self {
        Self {
            now_fn: Arc::new(SystemTime::now),
        }
    }

    /// 当前墙钟时刻
    pub(crate) fn now(&self) -> SystemTime {
        (self.now_fn)()
    }

    /// 测试用伪造时钟：返回时钟与推进句柄，`handle` 前进/回拨即时生效
    #[cfg(test)]
    fn fake() -> (Self, FakeClockHandle) {
        let shared = Arc::new(std::sync::Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ));
        let handle = FakeClockHandle(Arc::clone(&shared));
        (
            Self {
                now_fn: Arc::new(move || *shared.lock().expect("伪造时钟互斥锁不应中毒")),
            },
            handle,
        )
    }
}

/// 测试用伪造墙钟的推进句柄（clone / 传引用共享同一时刻）
#[cfg(test)]
struct FakeClockHandle(Arc<std::sync::Mutex<SystemTime>>);

#[cfg(test)]
impl FakeClockHandle {
    /// 把伪造墙钟向前推进（模拟系统睡眠 / 墙钟前跳）
    fn advance(&self, d: Duration) {
        let mut t = self.0.lock().expect("伪造时钟互斥锁不应中毒");
        *t += d;
    }
}

/// 后台探测任务的回传消息
///
/// 成败均须回传：主循环靠它重置 `probe_in_flight` 在途标记，
/// 只回传成功会导致标记永不复位、自动监测永久停摆。
///
/// 携带探测发起时的配置版本：结果回传时版本失配说明期间发生过配置变更
///（如切换 Profile），该结果的认证入口补充证据与恢复建议基于旧配置，
/// 不得用于自动登录决策。
enum ProbeMessage {
    /// 探测成功完成，携带报告与发起时的配置版本
    Report(ProbeReport, u64),
    /// 探测执行失败（错误描述）与发起时的配置版本
    Failed(String, u64),
}

impl ProbeMessage {
    /// 探测发起时的配置版本
    fn config_version(&self) -> u64 {
        match self {
            ProbeMessage::Report(_, v) | ProbeMessage::Failed(_, v) => *v,
        }
    }
}

/// Profile 切换探测任务的回传消息（模式同 [`ProbeMessage`]，F5 同构）
///
/// 成败均须回传：主循环靠它复位 `profile_probe_in_flight` 在途标记，
/// 只回传成功会导致标记永不复位、切换检测从此被防重入永久吞掉。
///
/// 不携带配置版本：与登录决策不同，匹配读取的是**回传时刻**的 Profile 列表
///（`detect_matching_profile` 即时求值），不存在基于旧配置决策的问题；
/// 停止/暂停/开关关闭的迟到门控在 [`handle_profile_probe_message`] 重查。
enum ProfileProbeMessage {
    /// 探测完成：默认网关列表与当前 SSID
    Completed(Vec<Ipv4Addr>, Option<String>),
    /// 探测执行失败（网关或 SSID 子进程探测的错误描述）
    Failed(String),
}

/// Engine 内部栈上状态（单 task 独占，不跨 Arc）
struct EngineInner {
    /// 监测循环是否启用
    monitoring: bool,
    /// 最近一次连通性解释
    last_assessment: ConnectivityAssessment,
    /// 最近一次原始探测证据
    last_evidence: Option<ProbeEvidence>,
    /// 当前门户事件与配置版本是否已经执行过一次谨慎登录
    ///
    /// 明确门户证据存在、但认证入口 TCP 预检失败时，监测层会给出
    /// `AttemptLoginOnce`。同一轮持续门户状态、同一配置版本仅放行一次，防止错误
    /// 地址导致每轮探测都拉起浏览器；网络重新确认非门户或配置变化后恢复预算。
    cautious_attempted_config_version: Option<u64>,
    /// 网络故障后的谨慎登录冷却截止；凭据及人工验证失败没有此预算。
    /// 墙钟计时（可注入）：系统睡眠期间到期的冷却在唤醒后立即视为期满。
    cautious_retry_after: Option<SystemTime>,
    /// 上次网络检测时间
    ///
    /// 仅在真实探测结果回传时更新（G3）：登录结果等非检测路径合并引擎状态时
    /// 读取此值，不得用「当前时刻」刷新，否则 last_check 语义被登录事件污染。
    last_check_time: Option<DateTime<Local>>,
    /// 手动暂停标记
    manual_paused: bool,
    /// 上次 Profile 切换检测时间（墙钟，可注入）
    last_profile_check: SystemTime,
    /// 业务周期判定的墙钟时钟源（冷却 / 谨慎重试 / 探测周期 / 切换检测共用）
    clock: WallClock,
    /// 下一次周期探测的墙钟到期点
    ///
    /// 取代原先的 tokio `Interval`（单调钟）：系统睡眠期间单调钟停摆，唤醒后
    /// 首探最多推迟一个检测周期（钳制上限 20 分钟）；墙钟到期点在睡眠期间越过
    /// 后唤醒即到期。到期睡眠按 [`MAX_SLEEP_SLICE`] 分片，每片醒来由主循环按
    /// 墙钟重估剩余（由 monitoring + 暂停状态门控是否真正探测）。
    next_check_at: SystemTime,
    /// 当前探测周期（秒）：到期点越过后的 Skip 前移与自适应重算共用
    ///
    /// 取值来源：EngineInner::new / reset_check_deadline 用配置钳制值；
    /// 探测结果回传后按自适应间隔更新。到期触发时只前移不改值（Skip 语义）。
    check_period_secs: u64,
    /// 连续登录失败次数
    consecutive_failures: u32,
    /// 连续 Online 探测次数（自适应间隔的退避进度）
    ///
    /// 非 Online 结果归零；Online 连续次数越多，下一次探测间隔按指数退避
    /// 越接近配置的 check_interval（见 [`adaptive_check_interval_secs`]）。
    online_streak: u32,
    /// 冷却期截止时刻（None 表示不在冷却中；墙钟计时，可注入）
    cooling_down_until: Option<SystemTime>,
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
    /// 是否有 Profile 切换探测任务在途（网关/SSID 子进程探测，F5 同构）
    ///
    /// 切换检测为低频周期触发（无用户主动即时路径），在途时到期触发直接
    /// 忽略——无须网络探测的 `probe_pending` 排队语义，被忽略的到期由下一轮
    /// 空转唤醒（≤MAX_IDLE_SLEEP_SECS）重估补检。
    profile_probe_in_flight: bool,
    /// Profile 切换探测结果回传 sender（后台探测任务完成后通知主循环）
    profile_probe_result_tx: mpsc::Sender<ProfileProbeMessage>,
    /// 登录结果回传 sender（后台 spawn 的登录任务完成后通知主循环）
    login_result_tx: mpsc::Sender<LoginResult>,
    /// TestNetwork 并发名额（A23）：诊断探测耗时数十秒，无上限时 Web 端高频
    /// 点击会无界堆积后台任务。`Arc<AtomicUsize>` 使名额守卫能移入 spawn 的
    /// 后台任务，Drop 时归还（panic 路径同样回收）。
    test_network_in_flight: Arc<AtomicUsize>,
    /// 登录失败通知去重器（同 Profile 仅提醒一次，切换/成功后重置）
    notifier: Notifier,
}

impl EngineInner {
    /// 仅网络故障恢复谨慎预算，凭据、人工验证和手动操作保持原限制。
    fn observe_cautious_result(&mut self, result: &LoginResult) {
        if result.source == LoginSource::Auto && self.cautious_attempted_config_version.is_some() {
            self.cautious_retry_after =
                if result.terminal == LoginTerminal::Failed && result.network_retryable {
                    tracing::info!("谨慎登录遇到临时网络故障，将在冷却后重新尝试");
                    Some(self.clock.now() + Duration::from_secs(COOLING_DOWN_DURATION_SECS))
                } else {
                    None
                };
        }
    }

    fn cautious_blocked(&self, config_version: u64) -> bool {
        self.cautious_attempted_config_version == Some(config_version)
            && self
                .cautious_retry_after
                .is_none_or(|until| self.clock.now() < until)
    }

    fn new(
        probe_result_tx: mpsc::Sender<ProbeMessage>,
        profile_probe_result_tx: mpsc::Sender<ProfileProbeMessage>,
        login_result_tx: mpsc::Sender<LoginResult>,
        clock: WallClock,
    ) -> Self {
        // 初始到期点：一个默认探测周期后（受 monitoring=false 门控，到期只
        // 前移不探测；Start/结果回传后按配置/自适应周期重算）
        let now = clock.now();
        Self {
            monitoring: false,
            last_assessment: ConnectivityAssessment::default(),
            last_evidence: None,
            cautious_attempted_config_version: None,
            cautious_retry_after: None,
            last_check_time: None,
            manual_paused: false,
            last_profile_check: now,
            clock,
            next_check_at: now + Duration::from_secs(crate::engine::DEFAULT_CHECK_INTERVAL_SECS),
            check_period_secs: crate::engine::DEFAULT_CHECK_INTERVAL_SECS,
            consecutive_failures: 0,
            online_streak: 0,
            cooling_down_until: None,
            auto_login_in_flight: false,
            probe_in_flight: false,
            probe_pending: false,
            probe_result_tx,
            profile_probe_in_flight: false,
            profile_probe_result_tx,
            login_result_tx,
            test_network_in_flight: Arc::new(AtomicUsize::new(0)),
            notifier: Notifier::new(),
        }
    }

    /// 冷却期满则清除冷却标记并重置失败计数。
    ///
    /// 恢复完整的「连续失败 N 次」预算；若只清冷却不重置计数，第二轮起会退化为
    /// 「失败 1 次即再次冷却」。
    ///
    /// 冷却按墙钟判定（可注入时钟）：系统睡眠期间到期的冷却在唤醒后立即期满
    /// （原 tokio 单调钟实现下，合盖睡眠不计入挂起时间，唤醒后冷却要重新计满
    /// 一个周期，登录恢复被无谓推迟）。单独成函数以便直接测试。
    fn clear_expired_cooling_down(&mut self) {
        if self
            .cooling_down_until
            .is_some_and(|until| self.clock.now() >= until)
        {
            self.cooling_down_until = None;
            self.consecutive_failures = 0;
        }
    }

    /// 周期探测是否已到期（墙钟判定；到期点被墙钟回拨推回未来时自然推迟）
    fn check_due(&self) -> bool {
        self.clock.now() >= self.next_check_at
    }

    /// 到期触发后的 Skip 前移：从当前墙钟起算一个当前周期
    ///
    /// 对齐原 tokio `Interval` + `MissedTickBehavior::Skip` 语义：一次到期只
    /// 触发一轮（在途探测期间反复到期被忽略），探测结果回传后再按自适应间隔
    /// 重算。前移必须先于门控判定执行——否则未监测/暂停期间到期点滞留过去，
    /// 主循环会在 select 分支上热自旋。
    fn advance_check_deadline(&mut self) {
        self.next_check_at = self.clock.now() + Duration::from_secs(self.check_period_secs);
    }
}

/// 距下次周期探测的睡眠时长：剩余墙钟时间钳制到 ≤[`MAX_SLEEP_SLICE`]，
/// 每片醒来由主循环按墙钟重估剩余（到期点已过返回 0，立即唤醒）
fn periodic_check_slice(inner: &EngineInner) -> Duration {
    let remaining = inner
        .next_check_at
        .duration_since(inner.clock.now())
        .unwrap_or(Duration::ZERO);
    remaining.min(MAX_SLEEP_SLICE)
}

/// 主循环
pub(crate) async fn run_loop(
    _engine: Arc<Engine>,
    deps: EngineDeps,
    mut cmd_rx: mpsc::Receiver<EngineCommand>,
    clock: WallClock,
) {
    // 探测结果回传 channel（后台探测任务 → 主循环，F5）
    let (probe_result_tx, mut probe_result_rx) = mpsc::channel::<ProbeMessage>(8);
    // Profile 切换探测结果回传 channel（后台网关/SSID 探测任务 → 主循环，F5 同构）
    let (profile_probe_tx, mut profile_probe_rx) = mpsc::channel::<ProfileProbeMessage>(8);
    // 登录结果回传 channel（后台 spawn 的登录任务完成后通知主循环，携带完整结果以区分来源）
    let (login_result_tx, mut login_result_rx) = mpsc::channel::<LoginResult>(16);
    let mut inner = EngineInner::new(probe_result_tx, profile_probe_tx, login_result_tx, clock);
    // 配置版本订阅：任何入口的配置变更（Web 保存 / 切换 Profile / CLI）都会经
    // ConfigService reload 广播版本号，Engine 据此重建派生状态。此前只有
    // Scheduler 收到 reload 信号，Engine 的检测定时器等派生状态不随保存刷新
    //（修改检测间隔后旧定时器继续按旧间隔运行直到下次 Start/Reload 命令）。
    let mut config_version_rx = deps.config_service.subscribe_version();
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
            Some(msg) = profile_probe_rx.recv() => {
                // Profile 切换探测结果回传：匹配与切换留在主循环执行（F5 同构），
                // 与命令处理保持同一串行顺序语义（handle_apply_profile 不并发重入）
                handle_profile_probe_message(msg, &mut inner, &deps).await;
            }
            Ok(()) = config_version_rx.changed() => {
                // 配置版本变化：RuntimeConfig 已由 ConfigService 原子替换，
                // Engine 只需重建派生状态（检测周期等），不再二次 reload。
                // Ok 模式：通道关闭（Err）时本分支在本轮 select 中被禁用，
                // 等待其它分支事件，不会忙轮询
                let _ = config_version_rx.borrow_and_update();
                tracing::debug!("配置版本变化，重建引擎派生状态（检测周期）");
                reset_check_deadline(&mut inner, &deps);
            }
            _ = tokio::time::sleep(periodic_check_slice(&inner)) => {
                // 周期探测到期点（墙钟）的分片睡眠（口径同 scheduler::cron_loop）：
                // 醒来后按墙钟重估——仅当到期点已越过才前移到期点（Skip 语义，
                // 防在途探测 / 未监测 / 暂停期间反复到期空转）并执行探测；分片
                // 唤醒（剩余仍为正）只重估剩余时长，不改到期点
                if inner.check_due() {
                    inner.advance_check_deadline();
                    if inner.monitoring && !is_any_pause_active(&inner, &deps) {
                        handle_network_check(&mut inner, &deps);
                    }
                }
            }
            // 无事件空转唤醒：冷却清理、切换检测等低频判定与墙钟到期点重估的
            // 兜底节奏（分片上限之外的第二重保险）
            _ = tokio::time::sleep(Duration::from_secs(MAX_IDLE_SLEEP_SECS)) => {}
        }

        // 步骤 3：低频 Profile 切换检测（探测段在后台任务执行，F5 同构）
        if inner.monitoring
            && !is_any_pause_active(&inner, &deps)
            && deps.config_service.runtime().load().auto_switch
            && profile_check_due(&inner, &deps)
        {
            // 只发起探测不 await：网关/SSID 均为子进程探测（各 10s 超时），
            // 内联 await 会在探测子进程挂起时阻塞命令通道最长 ~20s
            check_profile_switch(&mut inner, &deps);
            // 时间戳在「发起探测」时刷新（而非结果回传时）：切换检测节奏以发起
            // 时刻锚定，探测耗时（子进程各 10s 预算）不推迟下一轮到期点；
            // 在途期间的重复到期由 profile_probe_in_flight 防重入忽略
            inner.last_profile_check = inner.clock.now();
        }
    }
    // 主循环退出（Shutdown / 命令端全关）后在途探测任务自然终结：
    // 其持有的 probe_result_tx / profile_probe_result_tx 克隆指向已 drop 的
    // 接收端，send 失败即返回，结果被丢弃——关闭后的引擎不再处理任何探测结果。
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
        tracing::info!("检测处于暂停时段，跳过手动操作触发的立即检测");
        true
    } else {
        false
    }
}

async fn handle_start(inner: &mut EngineInner, deps: &EngineDeps) {
    if inner.monitoring {
        tracing::debug!("检测已在运行中，忽略 Start 命令");
        return;
    }
    inner.monitoring = true;
    merge_engine_state(inner, deps, EngineState::Running);
    // 立即执行一次检测（暂停窗口内跳过，F4；探测在后台任务执行，F5）
    // 用户主动触发，使用优先级标记以在在途时排队一次
    if !immediate_check_blocked_by_pause(inner, deps) {
        handle_network_check_with_priority(inner, deps, true);
    }
    // 用配置间隔重置到期点（从现在起算一个周期，避免紧随本次手动检测再探测一轮）
    reset_check_deadline(inner, deps);
    tracing::info!("检测已启动");
}

async fn handle_stop(inner: &mut EngineInner, deps: &EngineDeps) {
    if !inner.monitoring {
        tracing::debug!("检测已停止，忽略 Stop 命令");
        return;
    }
    inner.monitoring = false;
    merge_engine_state(inner, deps, EngineState::Stopped);
    tracing::info!("检测已停止");
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
    // 用新配置重置检查周期（Reload 语义只改周期，不应意外触发一次即时探测）
    reset_check_deadline(inner, deps);
}

async fn handle_apply_profile(
    profile_id: &str,
    _source: ProfileSwitchSource,
    inner: &mut EngineInner,
    deps: &EngineDeps,
) {
    // 幂等切换：Web 路由已同步完成切换时（保留「响应即生效」的前端契约），
    // 此处跳过重复落盘/reload，只执行派生状态同步；自动切换等其它入口
    // 仍由此处完成切换本体
    let current = deps.config_service.runtime().load().profile.id.clone();
    let switched = current != profile_id;
    if switched {
        // 切换活跃 Profile（内部重建 RuntimeConfig 并原子替换）。存在性校验会
        // 全量同步读取 Profile 文件（load_all_profiles 磁盘 IO），整段经
        // spawn_blocking 移出异步工作线程，慢盘不拖住引擎主循环的命令处理
        //（cf9ffe8 阻塞点收敛批次漏网之一）；阻塞线程内以 Handle::block_on
        // 驱动内含的异步段落（多线程 runtime 下的合法用法，阻塞线程非异步
        // 上下文）。错误语义不变：切换失败仅告警并中止本次切换
        let profile_service = deps.profile_service.clone();
        let target = profile_id.to_string();
        let switch = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(profile_service.switch_profile(&target))
        })
        .await;
        match switch {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("切换 Profile 失败: {} ({})", profile_id, e);
                return;
            }
            // 阻塞任务 panic：与切换失败同语义（告警 + 中止），不中断引擎主循环
            Err(join_err) => {
                tracing::warn!("切换 Profile 任务异常退出: {} ({})", profile_id, join_err);
                return;
            }
        }
    }
    deps.status_manager.merge(PartialSnapshot::ActiveProfile {
        id: profile_id.to_string(),
    });
    // 实际发生切换才记 info；幂等重复 ApplyProfile（Web 路由已切换）降为 debug
    if switched {
        tracing::info!("Profile 已切换: {} (来源: {:?})", profile_id, _source);
        // 事件发布（渐进吸收，紧邻现有切换日志）：档案切换实际生效的收敛点——
        // 自动切换（check_profile_switch）与手动 ApplyProfile 命令共用本路径，
        // 仅在实际发生切换时发布一次；幂等重复应用同 Profile 不发布。
        // 载荷为 from/to id 白名单摘要（不含名称/凭据）
        deps.events
            .publish(AppEvent::ProfileSwitched(ProfileSwitchSummary {
                from_profile_id: Some(current),
                to_profile_id: profile_id.to_string(),
            }));
    } else {
        tracing::debug!("Profile 无变化（已确认）: {}", profile_id);
    }
    // 新 Profile 可能有不同的 auth_url / 凭证，重新判断网络状态。
    // 与 Start/Resume 相同的暂停门控（F4）：暂停窗口内只切换不探测
    if inner.monitoring && !immediate_check_blocked_by_pause(inner, deps) {
        handle_network_check_with_priority(inner, deps, true);
        reset_check_deadline(inner, deps);
    }
}

/// TestNetwork 最大并发数（A23）：诊断探测单次可达数十秒，超过即拒绝新请求
const TEST_NETWORK_MAX_IN_FLIGHT: usize = 3;

/// 尝试占用一个 TestNetwork 并发名额；已达上限返回 `false`（A23）
///
/// 仅由主循环（`&mut EngineInner` 串行处理命令）调用，检查与自增之间无并发。
fn try_acquire_test_network_slot(inner: &EngineInner) -> bool {
    let in_flight = inner.test_network_in_flight.load(Ordering::SeqCst);
    if in_flight >= TEST_NETWORK_MAX_IN_FLIGHT {
        return false;
    }
    inner.test_network_in_flight.fetch_add(1, Ordering::SeqCst);
    true
}

/// TestNetwork 名额守卫：随后台任务结束（含 panic / abort）归还名额（A23）
struct TestNetworkSlotGuard(Arc<AtomicUsize>);

impl Drop for TestNetworkSlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 处理 TestNetwork 命令：探测在后台任务执行并直接回传 oneshot（F5）
///
/// 手动诊断探测不参与 `probe_in_flight` 在途合并（与周期检测语义独立，
/// 并发执行无害），也不修改引擎状态——结果仅供命令发起方消费。
/// 并发上限 [`TEST_NETWORK_MAX_IN_FLIGHT`]（A23）：超限时直接拒绝并回传错误，
/// 不再 spawn 任务。
fn handle_test_network(
    inner: &EngineInner,
    deps: &EngineDeps,
    reply: oneshot::Sender<Result<TestNetworkResult, EngineError>>,
) {
    if !try_acquire_test_network_slot(inner) {
        tracing::warn!("网络测试并发已达上限（{TEST_NETWORK_MAX_IN_FLIGHT}），拒绝本次请求");
        let _ = reply.send(Err(EngineError::ProbeError(format!(
            "网络测试并发已达上限（{TEST_NETWORK_MAX_IN_FLIGHT}），请稍后重试"
        ))));
        return;
    }
    tracing::info!("开始网络连通性测试");
    // 探测移入后台任务：check_once 可能耗时数十秒，内联 await 会阻塞
    // 命令通道（Shutdown/Stop 排队，F5）
    let monitor = deps.monitor_service.clone();
    // Drop 守卫：diagnose_once panic 或任务被 abort 时名额同样回收（A23）
    let slot = TestNetworkSlotGuard(inner.test_network_in_flight.clone());
    tokio::spawn(async move {
        let _slot = slot;
        // 纯耗时测量，保持单调钟（分界线见文件头 import 注释）
        let start = tokio::time::Instant::now();
        let result = match monitor.diagnose_once().await {
            Ok(report) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                tracing::info!(
                    "网络测试完成: status={:?}, duration={}ms",
                    report.assessment.status,
                    duration_ms
                );
                Ok(TestNetworkResult {
                    status: report.assessment.status,
                    confidence: report.assessment.confidence,
                    reason: report.assessment.reason,
                    local_link: report.evidence.local_link,
                    auth_endpoint: report.assessment.auth_endpoint,
                    details: ProbeDetails {
                        tcp: vec![format!("{:?}", report.evidence.tcp)],
                        http: vec![format!("{:?}", report.evidence.http)],
                        url: vec![format!("{:?}", report.evidence.url)],
                    },
                    duration_ms,
                })
            }
            Err(e) => {
                tracing::warn!("网络测试执行失败: {e}");
                Err(EngineError::ProbeError(e.to_string()))
            }
        };
        // 主循环退出后 reply 接收端可能已 drop：发送失败即丢弃
        let _ = reply.send(result);
    });
}

async fn handle_pause(inner: &mut EngineInner, deps: &EngineDeps) {
    inner.manual_paused = true;
    // 监测未启动时不得把状态合并成 Running
    merge_engine_state(inner, deps, engine_state_for(inner));
    tracing::info!("检测已暂停");
}

async fn handle_resume(inner: &mut EngineInner, deps: &EngineDeps) {
    inner.manual_paused = false;
    merge_engine_state(inner, deps, engine_state_for(inner));
    // 立即执行一次检测（不等待定时器到期）。手动 Resume 只解除手动暂停，
    // 定时暂停窗口可能仍然生效——沿用同一门控跳过检测（F4）
    if inner.monitoring && !immediate_check_blocked_by_pause(inner, deps) {
        handle_network_check_with_priority(inner, deps, true);
        reset_check_deadline(inner, deps);
    }
    tracing::info!("检测已恢复");
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
    inner.observe_cautious_result(&result);
    // 当前活跃 Profile 作为通知去重的键
    let profile_id = deps.config_service.runtime().load().profile.id.clone();
    match login_failure_budget_action(result.terminal, result.source) {
        LoginFailureBudgetAction::Reset => {
            inner.consecutive_failures = 0;
            inner.cooling_down_until = None;
            inner.notifier.on_login_success(&profile_id);
            tracing::debug!(source = ?result.source, "登录成功，重置连续失败计数");
        }
        LoginFailureBudgetAction::Increment => {
            inner.consecutive_failures += 1;
            // 登录失败日志合并为一条结构化 warn（target=notification 保持前端通知源）：
            // 首败通知与否经 first_failure 字段表达（notifier 仍负责同 Profile 去重）
            let first_failure = inner.notifier.should_notify_login_failure(&profile_id);
            tracing::warn!(
                target: "notification",
                profile = %profile_id,
                consecutive_failures = inner.consecutive_failures,
                reason = %result.message,
                first_failure,
                "登录失败"
            );
            if inner.consecutive_failures >= COOLING_DOWN_THRESHOLD
                && inner.cooling_down_until.is_none()
            {
                inner.cooling_down_until =
                    Some(inner.clock.now() + Duration::from_secs(COOLING_DOWN_DURATION_SECS));
                tracing::warn!(
                    "连续失败达到 {} 次，进入冷却期（{}s）",
                    inner.consecutive_failures,
                    COOLING_DOWN_DURATION_SECS
                );
            }
        }
        LoginFailureBudgetAction::Preserve => {
            tracing::debug!(
                source = ?result.source,
                terminal = ?result.terminal,
                "登录终态不影响自动登录失败预算"
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

/// 登录终态对自动登录失败预算的影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginFailureBudgetAction {
    /// 登录成功，重置连续失败与冷却。
    Reset,
    /// Auto 的真实失败，累加预算。
    Increment,
    /// 取消或非 Auto 失败，不改变预算。
    Preserve,
}

fn login_failure_budget_action(
    terminal: LoginTerminal,
    source: LoginSource,
) -> LoginFailureBudgetAction {
    match (terminal, source) {
        (LoginTerminal::Success, _) => LoginFailureBudgetAction::Reset,
        (LoginTerminal::Failed, LoginSource::Auto) => LoginFailureBudgetAction::Increment,
        (LoginTerminal::Failed | LoginTerminal::Cancelled, _) => LoginFailureBudgetAction::Preserve,
    }
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
    // 探测发起时的配置版本：结果回传后与当前版本比对，失配即拒绝用于登录决策
    let config_version = deps.config_service.config_version();
    tokio::spawn(async move {
        // panic 兜底：`probe_in_flight` 的唯一复位点是结果回传（handle_probe_message），
        // 任务若 panic，`tx` 随任务 drop、回传永不发生 → 此后每轮检测都被防重入吞掉，
        // 自动监测静默停摆（症状是"卡死"而非崩溃，极难排障）。Drop 在 unwind 中
        // 仍会执行，故由此守卫补发一条失败消息复位标记。
        let mut guard = ProbeReturnGuard::new(tx.clone(), config_version);
        let msg = match monitor.check_auto_monitor().await {
            Ok(report) => ProbeMessage::Report(report, config_version),
            Err(e) => ProbeMessage::Failed(e.to_string(), config_version),
        };
        // 主循环退出后接收端已 drop：send 失败即丢弃（shutdown 弃在途探测）
        let _ = guard.tx.send(msg).await;
        guard.sent = true;
    });
}

/// 探测任务的回传守卫：无论正常退出还是 panic 展开，都保证向主循环回传一条消息。
///
/// 主循环以「收到回传」作为 `probe_in_flight` 的复位条件，任何未回传的退出路径
/// 都会让自动监测永久停摆。正常路径显式置 `sent = true`，守卫随即不再补发。
struct ProbeReturnGuard {
    tx: mpsc::Sender<ProbeMessage>,
    config_version: u64,
    /// 正常路径是否已自行回传（避免重复回传）
    sent: bool,
}

impl ProbeReturnGuard {
    fn new(tx: mpsc::Sender<ProbeMessage>, config_version: u64) -> Self {
        Self {
            tx,
            config_version,
            sent: false,
        }
    }
}

impl Drop for ProbeReturnGuard {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        // Drop 不能 await：用 try_send。容量 8（见 probe_result_tx），而 probe_in_flight
        // 保证同一时刻至多一个探测在途，即至多一条未消费消息，故不会因满载而丢。
        // 若主循环已退出则发送失败，属预期（shutdown 弃在途探测）。
        let _ = self.tx.try_send(ProbeMessage::Failed(
            "探测任务异常退出（panic），已复位在途标记".to_string(),
            self.config_version,
        ));
    }
}

/// 自动登录任务的回传守卫：panic 时补发失败结果，保证 `auto_login_in_flight` 复位。
///
/// 与 [`ProbeReturnGuard`] 同源动机：该标记的唯一复位点是 `handle_login_result`，
/// 登录任务 panic 会让标记永不复位，自动登录从此永久失效。
struct AutoLoginReturnGuard {
    tx: mpsc::Sender<LoginResult>,
    sent: bool,
}

impl Drop for AutoLoginReturnGuard {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        let _ = self.tx.try_send(LoginResult {
            network_retryable: false,
            terminal: LoginTerminal::Failed,
            message: "自动登录任务异常退出（panic）".to_string(),
            source: LoginSource::Auto,
            duration: Duration::ZERO,
            attempts: 0,
        });
    }
}

/// 处理探测结果回传：更新状态 → 决策登录（F5）
///
/// 原 `handle_network_check` 内联探测后的后置逻辑整体迁移至此；
/// 冷却清理/判定改在结果到达时执行（比探测发起时更接近决策时刻）。
///
/// 登录决策前的三重门（在途探测可能跨越 Stop/Pause/配置变更）：
/// - 监测已停止：探测发起后用户点了停止，结果不得触发登录；
/// - 暂停生效中：暂停语义必须覆盖在途探测的迟到结果；
/// - 配置版本失配：探测发起后发生过配置变更（如切换 Profile），
///   结果中基于旧配置的判断（auth_url 可达性）不得触发自动登录。
///
/// 状态合并不受前三者影响——迟到的网络事实仍值得呈现给前端。
///
/// 结构上按消息类型与处理阶段拆分（T7-5）：失败回传走
/// [`handle_probe_failure`]，成功回传依次经 [`record_assessment_and_events`] →
/// [`reschedule_adaptive_probe`] → [`merge_probe_snapshot`] →
/// [`decide_auto_login`] → [`retrigger_pending_probe_if_due`]。
fn handle_probe_message(msg: ProbeMessage, inner: &mut EngineInner, deps: &EngineDeps) {
    // 无论成败都先复位在途标记，否则后续检测被永久忽略
    inner.probe_in_flight = false;
    // 若在途期间有优先级检测排队，立即补发一次（仅一次，避免无限自激）
    let pending = std::mem::replace(&mut inner.probe_pending, false);
    // 清除过期的冷却标记；冷却期满后重置失败计数，
    // 恢复完整的"连续失败 3 次"预算（否则第二轮起退化为失败 1 次即再冷却）
    inner.clear_expired_cooling_down();
    let probe_config_version = msg.config_version();
    let current_config_version = deps.config_service.config_version();
    // 配置版本失配：期间发生过 reload（切换 Profile / 保存配置）。
    // 网络状态仍合并（迟到的事实仍真实），但登录决策必须丢弃。
    let config_stale = probe_config_version != current_config_version;
    if config_stale {
        tracing::info!(
            "探测结果基于过期配置（发起时版本 {probe_config_version}，当前 {current_config_version}），跳过自动登录决策"
        );
    }
    let report = match msg {
        ProbeMessage::Report(r, _) => r,
        ProbeMessage::Failed(e, _) => {
            handle_probe_failure(e, pending, inner, deps);
            return;
        }
    };

    // 成功报告的处理链：记录与事件 → 自适应周期重算 → 快照合并 → 登录决策 → 补发排队探测
    let now = Local::now();
    inner.last_check_time = Some(now);
    // 上一轮恢复建议供 FixConfiguration 日志去重（更新前取值，见记录段覆写）
    let old_advice = inner.last_assessment.recovery_advice;
    record_assessment_and_events(&report, inner, deps);
    reschedule_adaptive_probe(&report, inner, deps);
    let paused = is_any_pause_active(inner, deps);
    // 冷却期内检查：若仍在冷却则跳过登录
    let cooling_down = inner.cooling_down_until.is_some();
    let gates = LoginDecisionGates {
        probe_config_version,
        config_stale,
        paused,
        cooling_down,
        old_advice,
    };
    merge_probe_snapshot(&report, now, paused, cooling_down, inner, deps);
    decide_auto_login(&report, &gates, inner, deps);
    retrigger_pending_probe_if_due(pending, inner, deps);
}

/// 探测执行失败分支：补发排队探测 + 合并最小快照（ENG-2）
///
/// 失败同样刷新「最近检测」——monitor 系统性故障期间前端时间冻结在最后一次
/// 成功值，会让用户误判检测停摆。合并最小快照：网络结论与证据沿用上次
/// （失败不产生新证据，evidence 置 None），仅刷新 last_check；探测到期点
/// 维持原值不重算（有意设计，见 [`reschedule_adaptive_probe`] 文档）。
fn handle_probe_failure(e: String, pending: bool, inner: &mut EngineInner, deps: &EngineDeps) {
    tracing::warn!("网络探测执行失败: {}", e);
    // 失败也要消费并立即执行排队的优先级检测；仅归还标记会因本分支提前
    // 返回而无人消费，最终仍要等下一个常规周期。不要重新置 pending，
    // 否则下一轮结果会额外触发一次无意义探测。
    retrigger_pending_probe_if_due(pending, inner, deps);
    let now = Local::now();
    inner.last_check_time = Some(now);
    let paused = is_any_pause_active(inner, deps);
    let cooling_down = inner.cooling_down_until.is_some();
    let cooling_down_remaining = cooling_down.then(|| cooling_remaining_secs(inner));
    let state = if inner.monitoring {
        EngineState::Running
    } else {
        EngineState::Stopped
    };
    deps.status_manager.merge(PartialSnapshot::Engine {
        state,
        network: inner.last_assessment.status,
        assessment: inner.last_assessment.clone(),
        evidence: None,
        last_check: now,
        pause: paused,
        cooling_down,
        cooling_down_remaining,
        consecutive_failures: inner.consecutive_failures,
    });
}

/// 记录探测结论：状态转换日志 + 事件发布 + 会话内最新结论更新
///
/// 事件发布（渐进吸收，紧邻现有状态翻转日志）：仅在「通 ⇄ 断」真实翻转
/// 边界发布，宁缺勿滥——
/// - 初始未知态（Unknown）只是「尚未探测过」，不构成由断转通/由通转断，
///   首探结论不发布；
/// - 断态之间迁移（Offline ⇄ CaptivePortal）同属"未联网"，同态不重复发。
///
/// 探测执行失败分支不发布（无新证据、状态未翻转）。
fn record_assessment_and_events(report: &ProbeReport, inner: &mut EngineInner, deps: &EngineDeps) {
    // 状态变化日志：仅在状态发生转换时记录 info，未变化保持静默（debug）。
    // 附各通道探测结论作为判定依据——排查"为什么判为离线/劫持"不必再翻 debug 日志
    let old_status = inner.last_assessment.status;
    let status = report.assessment.status;
    if status != NetworkStatus::CaptivePortal {
        // 离开门户态即结束本次认证事件；以后再次进入门户态可重新谨慎尝试一次。
        inner.cautious_attempted_config_version = None;
        inner.cautious_retry_after = None;
    }
    if status != old_status {
        tracing::info!(
            "网络状态变化: {:?} → {:?}（原因={:?}, 置信度={:?}, TCP={:?}, 204门户={:?}, URL标题={:?}, 耗时{}ms）",
            old_status,
            status,
            report.assessment.reason,
            report.assessment.confidence,
            report.evidence.tcp,
            report.evidence.http,
            report.evidence.url,
            report.latency_ms
        );
        if old_status == NetworkStatus::Online && status != NetworkStatus::Online {
            deps.events.publish(AppEvent::NetworkDown(NetworkSummary {
                profile_id: None,
                reason: Some(assessment_reason_code(&report.assessment.reason)),
            }));
        } else if old_status != NetworkStatus::Unknown
            && old_status != NetworkStatus::Online
            && status == NetworkStatus::Online
        {
            deps.events.publish(AppEvent::NetworkUp(NetworkSummary {
                profile_id: None,
                reason: Some(assessment_reason_code(&report.assessment.reason)),
            }));
        }
    } else {
        tracing::debug!(
            status = ?status,
            reason = ?report.assessment.reason,
            recovery = ?report.assessment.recovery_advice,
            "网络状态未变化"
        );
    }
    inner.last_assessment = report.assessment.clone();
    inner.last_evidence = Some(report.evidence.clone());
}

/// 自适应探测间隔重算（C4）：异常态（CaptivePortal/Offline）短周期加密探测，
/// 稳定在线按指数退避放大到配置的 check_interval。按墙钟从结果时刻重算
/// 到期点——与 reset_check_deadline 同语义：刚做完一轮探测，到期点从现在
/// 起算，不会立即重复探测形成风暴。探测 Failed 分支不重算（monitor 系统性
/// 故障时维持原到期点，不放大故障）；暂停期无结果回传，周期自然保持
fn reschedule_adaptive_probe(report: &ProbeReport, inner: &mut EngineInner, deps: &EngineDeps) {
    let status = report.assessment.status;
    if status == NetworkStatus::Online {
        inner.online_streak = inner.online_streak.saturating_add(1);
    } else {
        inner.online_streak = 0;
    }
    let configured_interval = check_interval_secs(deps);
    let next_secs = if report.assessment.recovery_advice == RecoveryAdvice::NoProbeEvidence {
        // 没有任何有效探测时没有需要快速追踪的异常；沿用用户配置周期，
        // 避免空配置退化成每 30 秒一次的无意义循环。
        configured_interval
    } else {
        adaptive_check_interval_secs(status, inner.online_streak, configured_interval)
    };
    inner.check_period_secs = next_secs;
    inner.next_check_at = inner.clock.now() + Duration::from_secs(next_secs);
}

/// 合并本轮探测快照到 StatusManager（含 G23 累计指标推送）
fn merge_probe_snapshot(
    report: &ProbeReport,
    now: DateTime<Local>,
    paused: bool,
    cooling_down: bool,
    inner: &EngineInner,
    deps: &EngineDeps,
) {
    // 监测已停止时不得把状态合并回 Running（探测可能在 Stop 后才完成，
    // 与登录结果回传路径同一约束）
    let state = if inner.monitoring {
        EngineState::Running
    } else {
        EngineState::Stopped
    };
    deps.status_manager.merge(PartialSnapshot::Engine {
        state,
        network: report.assessment.status,
        assessment: report.assessment.clone(),
        evidence: Some(report.evidence.clone()),
        last_check: now,
        pause: paused,
        cooling_down,
        cooling_down_remaining: cooling_down.then(|| cooling_remaining_secs(inner)),
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
}

/// 登录决策的门控输入（探测回传时刻的一次性快照）
struct LoginDecisionGates {
    /// 探测发起时的配置版本（谨慎重试预算的判定键）
    probe_config_version: u64,
    /// 探测结果基于过期配置（期间发生过 reload，登录决策丢弃）
    config_stale: bool,
    /// 暂停生效中（手动或计划暂停）
    paused: bool,
    /// 冷却期内
    cooling_down: bool,
    /// 上一轮恢复建议（FixConfiguration 日志去重）
    old_advice: RecoveryAdvice,
}

/// 登录决策段：监测层给出恢复建议后由 Engine 的运行态门控统一决定是否执行
///
/// 建议为自动登录且通过完整门控链（停止/暂停/配置失配/冷却/在途登录/
/// 谨慎重试预算）时 `tokio::spawn` 自动登录会话，结果经 `login_result_tx`
/// 回传主循环；其余建议仅记录决策日志。
fn decide_auto_login(
    report: &ProbeReport,
    gates: &LoginDecisionGates,
    inner: &mut EngineInner,
    deps: &EngineDeps,
) {
    // 监测层只给出恢复建议；是否执行由 Engine 的运行态门控统一决定。
    let advice = report.assessment.recovery_advice;
    if matches!(
        advice,
        RecoveryAdvice::AttemptLogin | RecoveryAdvice::AttemptLoginOnce
    ) {
        let cautious_already_attempted = advice == RecoveryAdvice::AttemptLoginOnce
            && inner.cautious_blocked(gates.probe_config_version);

        if !inner.monitoring {
            tracing::debug!("检测已停止，跳过本轮自动登录（迟到探测结果）");
        } else if gates.paused {
            tracing::debug!("检测处于暂停状态，跳过本轮自动登录（迟到探测结果）");
        } else if gates.config_stale {
            tracing::info!("探测结果基于过期配置，跳过本轮自动登录");
        } else if gates.cooling_down {
            tracing::debug!(
                consecutive_failures = inner.consecutive_failures,
                "冷却期中，跳过本轮登录"
            );
        } else if inner.auto_login_in_flight {
            tracing::debug!("自动登录会话仍在途，跳过本轮触发");
        } else if cautious_already_attempted {
            tracing::debug!(
                config_version = gates.probe_config_version,
                "本次门户事件已执行过谨慎登录，等待网络状态或配置变化"
            );
        } else {
            if advice == RecoveryAdvice::AttemptLoginOnce {
                inner.cautious_attempted_config_version = Some(gates.probe_config_version);
                inner.cautious_retry_after = None;
                tracing::info!("检测到门户劫持但认证入口预检失败，谨慎尝试一次自动登录");
            } else {
                tracing::info!("检测结论建议恢复认证，触发自动登录");
            }
            inner.auto_login_in_flight = true;
            let orchestrator = deps.orchestrator.clone();
            let tx = inner.login_result_tx.clone();
            tokio::spawn(async move {
                // panic 兜底：`auto_login_in_flight` 的唯一复位点是结果回传
                // （handle_login_result），任务 panic 会让标记永不复位、
                // 自动登录永久失效。Drop 在 unwind 中仍执行，故由守卫补发。
                let mut guard = AutoLoginReturnGuard {
                    tx: tx.clone(),
                    sent: false,
                };
                let handle = orchestrator.submit(LoginSource::Auto, None, None).await;
                let result = handle.await_result().await;
                let _ = guard.tx.send(result).await;
                guard.sent = true;
            });
        }
    } else {
        match advice {
            RecoveryAdvice::FixConfiguration => {
                if gates.old_advice != RecoveryAdvice::FixConfiguration {
                    tracing::warn!("认证配置无效，自动恢复暂停，等待用户修正 Profile");
                } else {
                    tracing::debug!("认证配置仍无效，继续等待用户修正 Profile");
                }
            }
            RecoveryAdvice::WaitForNetwork => {
                tracing::debug!("更像物理断网，等待网络恢复");
            }
            RecoveryAdvice::WaitForMoreEvidence => {
                tracing::debug!("当前证据不足或冲突，等待下一轮探测");
            }
            RecoveryAdvice::NoProbeEvidence => {
                tracing::debug!("没有启用有效公网探测，不执行自动恢复");
            }
            RecoveryAdvice::NoAction
            | RecoveryAdvice::NotEvaluated
            | RecoveryAdvice::AttemptLogin
            | RecoveryAdvice::AttemptLoginOnce => {}
        }
    }
}

/// 补发排队的优先级探测（在途期间有优先级检测排队时，仅一次，避免无限自激）
///
/// 门控与定时器分支一致：停止/暂停期间不补发，避免停止后又自动开始探测。
/// 成功报告路径在决策与合并完成后触发（避免递归）；探测失败分支在最小
/// 快照合并前立即补发。
fn retrigger_pending_probe_if_due(pending: bool, inner: &mut EngineInner, deps: &EngineDeps) {
    if pending && inner.monitoring && !is_any_pause_active(inner, deps) {
        tracing::debug!("补发排队的优先级探测");
        handle_network_check(inner, deps);
    }
}

/// 探测结论的原因码（事件载荷 `NetworkSummary.reason`）
///
/// 取 [`AssessmentReason`] 的 serde 序列化结果（snake_case），与监测层对外
/// API 的原因码口径一致；枚举词表固定、不含任何用户数据，满足事件载荷
/// 脱敏红线（发布方保证短语本身不含敏感片段）。
fn assessment_reason_code(reason: &AssessmentReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{reason:?}"))
}

/// 低频 Profile 切换检测：只负责「在途检查 + 发起后台探测」（F5 同构）
///
/// 原实现内联 await `default_gateways` / `current_ssid`（各为子进程探测、
/// 各 10s 超时，见 `network::detect::SUBPROCESS_TIMEOUT_SECS`），auto_switch
/// 开启且探测子进程挂起时 Stop/Pause/Shutdown 命令最长阻塞 ~20s，违背文件头
/// 「命令保持即时响应」的 F5 决策。探测段移入独立 tokio 任务、结果经 mpsc
/// 回传主循环；匹配与切换（`handle_apply_profile`）留在主循环的
/// [`handle_profile_probe_message`] 执行，保持与命令处理的串行顺序语义。
///
/// 后台任务只克隆探测所需数据（`network_detect` Arc 与回传 sender），不持有
/// `EngineInner` 借用——回传 `(gateways, ssid)` 即可，配置与 Profile 列表在
/// 主循环收到结果时读取。
fn check_profile_switch(inner: &mut EngineInner, deps: &EngineDeps) {
    // 防重入（同 probe_in_flight 模式）：探测在途时到期触发直接忽略。
    // 切换检测本身低频（周期下限 60s）且无用户主动即时路径，无须排队语义
    if inner.profile_probe_in_flight {
        tracing::debug!("Profile 切换探测仍在途，忽略本次到期触发");
        return;
    }
    inner.profile_probe_in_flight = true;
    let detector = deps.network_detect.clone();
    let tx = inner.profile_probe_result_tx.clone();
    tokio::spawn(async move {
        // panic 兜底：`profile_probe_in_flight` 的唯一复位点是结果回传
        // （handle_profile_probe_message），任务若 panic 未回传 → 此后每轮
        // 切换检测都被防重入吞掉、auto_switch 静默失效（症状是"永不切换"
        // 而非崩溃，极难排障）。Drop 在 unwind 中仍执行，故由守卫补发失败消息
        let mut guard = ProfileProbeReturnGuard::new(tx);
        // 与原内联实现同语义：网关探测失败即放弃本轮，不再探测 SSID
        let msg = match detector.default_gateways().await {
            Ok(gateways) => match detector.current_ssid().await {
                Ok(ssid) => ProfileProbeMessage::Completed(gateways, ssid),
                Err(e) => ProfileProbeMessage::Failed(e.to_string()),
            },
            Err(e) => ProfileProbeMessage::Failed(e.to_string()),
        };
        // 主循环退出后接收端已 drop：send 失败即丢弃（shutdown 弃在途探测）
        let _ = guard.tx.send(msg).await;
        guard.sent = true;
    });
}

/// Profile 切换探测任务的回传守卫：无论正常退出还是 panic 展开，
/// 都保证向主循环回传一条消息（模式同 [`ProbeReturnGuard`]）。
///
/// 主循环以「收到回传」作为 `profile_probe_in_flight` 的复位条件，任何未
/// 回传的退出路径都会让切换检测从此被防重入吞掉、auto_switch 永久失效。
struct ProfileProbeReturnGuard {
    tx: mpsc::Sender<ProfileProbeMessage>,
    /// 正常路径是否已自行回传（避免重复补发）
    sent: bool,
}

impl ProfileProbeReturnGuard {
    fn new(tx: mpsc::Sender<ProfileProbeMessage>) -> Self {
        Self { tx, sent: false }
    }
}

impl Drop for ProfileProbeReturnGuard {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        // Drop 不能 await：用 try_send。容量 8（见 profile_probe_tx），而
        // profile_probe_in_flight 保证同一时刻至多一个探测在途，即至多一条
        // 未消费消息，故不会因满载而丢。若主循环已退出则发送失败属预期
        let _ = self.tx.try_send(ProfileProbeMessage::Failed(
            "Profile 切换探测任务异常退出（panic），已复位在途标记".to_string(),
        ));
    }
}

/// 处理 Profile 切换探测结果回传：网关/SSID 变化 → 自动匹配并切换
///
/// 原 `check_profile_switch` 探测后的匹配与切换段整体迁移至此，仍在主循环
/// 执行：`handle_apply_profile` 与命令处理共用同一串行顺序语义，后台任务只
/// 负责探测、不碰任何引擎状态。
///
/// 迟到结果门控（探测在途可跨越 Stop/Pause/开关变更，口径同
/// [`handle_probe_message`] 的登录决策三重门）：监测停止 / 暂停生效 /
/// auto_switch 已关闭时丢弃结果、不触发切换——用户显式停止的自动行为不得
/// 被探测发起前已定的结果反向驱动。匹配读取回传时刻的 Profile 列表，无
/// 基于旧配置决策的问题，故不比对配置版本。
async fn handle_profile_probe_message(
    msg: ProfileProbeMessage,
    inner: &mut EngineInner,
    deps: &EngineDeps,
) {
    // 无论成败/门控如何都先复位在途标记，否则后续切换检测被永久忽略
    inner.profile_probe_in_flight = false;
    if !inner.monitoring
        || is_any_pause_active(inner, deps)
        || !deps.config_service.runtime().load().auto_switch
    {
        tracing::debug!("Profile 切换探测结果迟到（已停止/暂停/开关关闭），丢弃");
        return;
    }
    let (gateways, ssid) = match msg {
        ProfileProbeMessage::Completed(g, s) => (g, s),
        ProfileProbeMessage::Failed(e) => {
            // 无 WiFi 网卡/受限环境下每轮都会失败，保持 debug 防止刷屏
            tracing::debug!(error = %e, "网关/SSID 探测失败");
            return;
        }
    };
    let gateway_str = gateways.first().map(|g| g.to_string()).unwrap_or_default();
    let ssid_str = ssid.as_deref().unwrap_or("");
    // 匹配需全量同步读取 Profile 文件（load_all_profiles 磁盘 IO），经
    // spawn_blocking 移出异步工作线程（与 handle_apply_profile 的切换调用点
    // 同批次收敛，cf9ffe8 漏网之二）；阻塞任务 panic 按「无匹配」兜底并告警，
    // 语义与原本就有的「无匹配即不切换」一致
    let profile_service = deps.profile_service.clone();
    let gateway = gateway_str.clone();
    let ssid = ssid_str.to_string();
    let matched = tokio::task::spawn_blocking(move || {
        profile_service.detect_matching_profile(&gateway, &ssid)
    })
    .await
    .unwrap_or_else(|join_err| {
        tracing::warn!("Profile 匹配任务异常退出: {join_err}");
        None
    });
    if let Some(matched_id) = matched {
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

/// 重置周期探测到期点（墙钟）：从当前时刻起算一个配置周期
///
/// 原 `reset_check_timer` 语义（重建 `Interval` 并消费首个立即 tick）：调用方
/// 通常刚做过一次检测（探测处理/Start/Resume/ApplyProfile），到期点从「现在 +
/// 周期」起算，不会紧随其后重复探测一轮；Reload 语义只改周期，更不应意外触发
/// 即时探测。墙钟计业务周期，睡眠期间越过即唤醒后立即到期（P2-2）。
fn reset_check_deadline(inner: &mut EngineInner, deps: &EngineDeps) {
    inner.check_period_secs = check_interval_secs(deps);
    inner.advance_check_deadline();
}

/// 冷却剩余秒数（墙钟）：到期点已过（墙钟回拨越过等）返回 0
fn cooling_remaining_secs(inner: &EngineInner) -> u32 {
    inner
        .cooling_down_until
        .and_then(|until| until.duration_since(inner.clock.now()).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
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
        .is_some_and(|until| inner.clock.now() < until);
    let cooling_down_remaining = if cooling_down {
        Some(cooling_remaining_secs(inner))
    } else {
        None
    };
    deps.status_manager.merge(PartialSnapshot::Engine {
        state,
        network: inner.last_assessment.status,
        assessment: inner.last_assessment.clone(),
        evidence: inner.last_evidence.clone(),
        last_check,
        // 暂停状态并入定时暂停窗口：窗口期内引擎实际不探测，快照需如实反映，
        // 否则前端在定时暂停时段仍显示"运行中"
        pause: inner.manual_paused || is_scheduled_pause_active(deps),
        cooling_down,
        cooling_down_remaining,
        consecutive_failures: inner.consecutive_failures,
    });
}

/// 定时暂停窗口是否当前生效（不含手动暂停）
fn is_scheduled_pause_active(deps: &EngineDeps) -> bool {
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

/// 判断是否存在任意暂停（手动或定时时段）
fn is_any_pause_active(inner: &EngineInner, deps: &EngineDeps) -> bool {
    if inner.manual_paused {
        return true;
    }
    is_scheduled_pause_active(deps)
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

/// 网络检查周期（从 RuntimeConfig 读取，钳制到 20~1200s，见 [`crate::engine::CHECK_INTERVAL_MIN`]）
fn check_interval_secs(deps: &EngineDeps) -> u64 {
    let secs = deps.config_service.runtime().load().monitor.check_interval as u64;
    secs.clamp(
        crate::engine::CHECK_INTERVAL_MIN,
        crate::engine::CHECK_INTERVAL_MAX,
    )
}

/// Profile 切换检测是否到期（墙钟判定）
///
/// 墙钟回拨把 `last_profile_check` 推到未来时 `duration_since` 为 Err，视为
/// 未到期、等墙钟追平再查——与 `scheduler::cron_loop` 的回拨容忍口径一致。
fn profile_check_due(inner: &EngineInner, deps: &EngineDeps) -> bool {
    let interval = profile_check_interval_secs(deps);
    inner
        .clock
        .now()
        .duration_since(inner.last_profile_check)
        .map(|elapsed| elapsed >= Duration::from_secs(interval))
        .unwrap_or(false)
}

/// 按探测结果计算下一次探测间隔（自适应，纯函数）
///
/// - 非 Online（CaptivePortal/Offline）：固定 [`PROBE_ONLINE_BACKOFF_BASE_SECS`]
///   短周期探测，被踢下线后 30s 内即可感知并触发自动登录（原为最长一个
///   check_interval）；
/// - Online：按连续在线次数指数退避（30 → 60 → 120 → …），封顶配置的
///   `base_secs`——稳态探测频率与固定间隔方案完全一致，只有异常态变密；
/// - 冷却期由调用方的登录门控拦截，加速探测不会引发登录风暴。
fn adaptive_check_interval_secs(status: NetworkStatus, online_streak: u32, base_secs: u64) -> u64 {
    if status == NetworkStatus::Online {
        let shift = online_streak.saturating_sub(1).min(16);
        (PROBE_ONLINE_BACKOFF_BASE_SECS << shift).min(base_secs.max(1))
    } else {
        PROBE_ONLINE_BACKOFF_BASE_SECS
    }
}

/// Profile 切换检测周期（从 RuntimeConfig 读取，clamp 到合法范围）
fn profile_check_interval_secs(deps: &EngineDeps) -> u64 {
    let secs = deps
        .config_service
        .runtime()
        .load()
        .monitor
        .profile_check_interval as u64;
    secs.clamp(PROFILE_CHECK_INTERVAL_MIN, PROFILE_CHECK_INTERVAL_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use chrono::TimeZone;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::bridge::BridgeSupervisor;
    use crate::config::{ConfigService, ProfileService};
    use crate::environment::EnvironmentManager;
    use crate::events::{EventBus, EventEnvelope};
    use crate::login::{LoginHistoryService, LoginOrchestrator};
    use crate::monitor::MonitorService;
    use crate::network::detect::{InterfaceInfo, NetworkDetect, NetworkError};
    use crate::status::StatusManager;
    use crate::tasks::TaskManager;
    use crate::utils::metrics::Metrics;

    #[test]
    fn test_login_failure_budget_only_counts_auto_failures() {
        assert_eq!(
            login_failure_budget_action(LoginTerminal::Failed, LoginSource::Auto),
            LoginFailureBudgetAction::Increment
        );
        assert_eq!(
            login_failure_budget_action(LoginTerminal::Failed, LoginSource::Manual),
            LoginFailureBudgetAction::Preserve
        );
    }

    /// 冷却恢复路径：期满清冷却标记 + 失败计数归零（P2-13 的回归锚点）
    ///
    /// 冷却按墙钟判定后由注入的伪造时钟驱动：tokio 虚拟时钟 `advance` 不影响
    /// `SystemTime`，注入时钟是推进墙钟判定的唯一途径。大步推进同时模拟
    /// 「合盖睡眠 8h 唤醒」场景——睡眠期间到期的冷却应在唤醒后立即期满，
    /// 而非按单调钟重新计满一个周期（P2-2）。
    #[test]
    fn test_cooling_down_expiry_resets_failure_count() {
        let (tx, _rx) = mpsc::channel(1);
        let (ptx, _prx) = mpsc::channel(1);
        let (ltx, _lrx) = mpsc::channel(1);
        let (clock, fake) = WallClock::fake();
        let mut inner = EngineInner::new(tx, ptx, ltx, clock);

        // 进入冷却期：3 次失败后置位（模拟 handle_login_result 的冷却分支）
        inner.consecutive_failures = COOLING_DOWN_THRESHOLD;
        inner.cooling_down_until =
            Some(inner.clock.now() + Duration::from_secs(COOLING_DOWN_DURATION_SECS));

        // 未到期：不得清除
        inner.clear_expired_cooling_down();
        assert!(inner.cooling_down_until.is_some(), "冷却未到期不应清除");
        assert_eq!(
            inner.consecutive_failures, COOLING_DOWN_THRESHOLD,
            "冷却未到期不应重置失败计数"
        );

        // 模拟系统睡眠：伪造墙钟大步前进越过冷却期（合盖 8h 唤醒）
        fake.advance(Duration::from_secs(8 * 3600));
        inner.clear_expired_cooling_down();

        assert!(
            inner.cooling_down_until.is_none(),
            "睡眠期间到期的冷却应在唤醒后立即期满（若仍用单调钟，此处会失败）"
        );
        assert_eq!(
            inner.consecutive_failures, 0,
            "冷却期满应重置失败计数，恢复完整的连续失败预算"
        );
    }

    /// 网络故障冷却后重新放行，人工验证或凭据失败仍保留谨慎门禁。
    #[test]
    fn cautious_network_failure_recovers_without_configuration_change() {
        let (probe_tx, _) = mpsc::channel(1);
        let (pprobe_tx, _) = mpsc::channel(1);
        let (login_tx, _) = mpsc::channel(1);
        let (clock, fake) = WallClock::fake();
        let mut inner = EngineInner::new(probe_tx, pprobe_tx, login_tx, clock);
        inner.cautious_attempted_config_version = Some(7);
        let mut result = LoginResult {
            network_retryable: true,
            terminal: LoginTerminal::Failed,
            message: "导航失败".into(),
            source: LoginSource::Auto,
            duration: Duration::ZERO,
            attempts: 1,
        };
        inner.observe_cautious_result(&result);
        assert!(inner.cautious_blocked(7));
        assert!(!inner.cautious_blocked(8));
        fake.advance(Duration::from_secs(COOLING_DOWN_DURATION_SECS + 1));
        assert!(!inner.cautious_blocked(7));
        result.network_retryable = false;
        inner.observe_cautious_result(&result);
        fake.advance(Duration::from_secs(COOLING_DOWN_DURATION_SECS + 1));
        assert!(inner.cautious_blocked(7));
        result.network_retryable = true;
        result.source = LoginSource::Manual;
        inner.observe_cautious_result(&result);
        assert!(inner.cautious_blocked(7));
    }

    /// 睡眠模拟（P2-2 墙钟化）：伪造墙钟大步前进（系统睡眠 8h）后，周期探测
    /// 到期点立即视为到期、睡眠分片归零（唤醒即首探）；到期触发一次后按 Skip
    /// 语义前移一个周期，不会连续到期形成热循环；剩余远超单片上限时按
    /// [`MAX_SLEEP_SLICE`] 分片。
    #[test]
    fn test_probe_deadline_due_immediately_after_clock_jump() {
        let (tx, _rx) = mpsc::channel(1);
        let (ptx, _prx) = mpsc::channel(1);
        let (ltx, _lrx) = mpsc::channel(1);
        let (clock, fake) = WallClock::fake();
        let mut inner = EngineInner::new(tx, ptx, ltx, clock);
        // 模拟刚完成一轮探测（Offline）：自适应短周期后再探
        inner.check_period_secs = PROBE_ONLINE_BACKOFF_BASE_SECS;
        inner.next_check_at =
            inner.clock.now() + Duration::from_secs(PROBE_ONLINE_BACKOFF_BASE_SECS);

        assert!(!inner.check_due(), "未到期不应探测");
        assert_eq!(
            periodic_check_slice(&inner),
            Duration::from_secs(PROBE_ONLINE_BACKOFF_BASE_SECS),
            "剩余时长应完整作为睡眠时长（短于单片上限）"
        );

        // 模拟合盖睡眠 8h：墙钟大步前进。原单调钟实现下该跳变不影响 tokio
        // Instant，唤醒后首探要再等满一个周期——正是被修复的缺陷。
        fake.advance(Duration::from_secs(8 * 3600));
        assert!(
            inner.check_due(),
            "睡眠期间越过到期点，唤醒后应立即到期（若仍用单调钟，此处会失败）"
        );
        assert_eq!(
            periodic_check_slice(&inner),
            Duration::ZERO,
            "到期后睡眠分片应为 0（立即唤醒执行探测）"
        );

        // 到期处理（Skip 语义）：前移一个周期后不再立即到期
        inner.advance_check_deadline();
        assert!(!inner.check_due(), "前移一个周期后不应立即到期");

        // 剩余远超单片上限：只睡 ≤MAX_SLEEP_SLICE，每片醒来按墙钟重估
        inner.next_check_at = inner.clock.now() + Duration::from_secs(8 * 3600);
        assert_eq!(periodic_check_slice(&inner), MAX_SLEEP_SLICE);
    }

    /// A23：TestNetwork 并发名额达上限即拒绝，守卫 Drop（panic / abort 路径）归还名额
    #[tokio::test]
    async fn test_network_slot_limit_and_guard_release() {
        let (tx, _rx) = mpsc::channel(1);
        let (ptx, _prx) = mpsc::channel(1);
        let (ltx, _lrx) = mpsc::channel(1);
        let inner = EngineInner::new(tx, ptx, ltx, WallClock::system());

        for _ in 0..TEST_NETWORK_MAX_IN_FLIGHT {
            assert!(try_acquire_test_network_slot(&inner), "上限内应放行");
        }
        assert!(
            !try_acquire_test_network_slot(&inner),
            "达到上限后应拒绝新名额"
        );

        // 守卫 Drop 归还名额：diagnose_once panic 或任务被 abort 时同样生效
        drop(TestNetworkSlotGuard(inner.test_network_in_flight.clone()));
        assert!(try_acquire_test_network_slot(&inner), "守卫释放后应放行");
    }

    /// P2-5：探测任务 panic 时守卫补发失败消息（否则 `probe_in_flight` 永久卡住）
    ///
    /// 主循环只在收到回传时复位 `probe_in_flight`；任务 panic 后 `tx` 随之 drop，
    /// 回传永不发生 → 此后每轮检测被防重入吞掉，自动监测静默停摆。
    #[tokio::test]
    async fn test_probe_return_guard_sends_on_panic() {
        let (tx, mut rx) = mpsc::channel::<ProbeMessage>(8);
        // 模拟探测任务 panic：守卫在 unwind 中 drop
        let task = tokio::spawn(async move {
            let _guard = ProbeReturnGuard::new(tx, 42);
            panic!("模拟探测任务 panic");
        });
        assert!(task.await.is_err(), "任务应 panic");

        // 守卫补发的失败消息必须到达，且携带发起时的配置版本
        let msg = rx.recv().await.expect("panic 后守卫应补发一条消息");
        assert_eq!(msg.config_version(), 42);
        assert!(matches!(msg, ProbeMessage::Failed(_, _)));
    }

    /// P2-5：正常路径只回传一次（守卫不得重复补发）
    #[tokio::test]
    async fn test_probe_return_guard_does_not_duplicate_on_success() {
        let (tx, mut rx) = mpsc::channel::<ProbeMessage>(8);
        {
            let mut guard = ProbeReturnGuard::new(tx, 7);
            guard
                .tx
                .send(ProbeMessage::Report(
                    ProbeReport {
                        evidence: ProbeEvidence::default(),
                        assessment: ConnectivityAssessment::default(),
                        latency_ms: 0,
                        check_number: 1,
                    },
                    7,
                ))
                .await
                .unwrap();
            guard.sent = true;
        } // 守卫在此 drop：不应再补发
        assert!(matches!(rx.recv().await, Some(ProbeMessage::Report(_, 7))));
        assert!(rx.try_recv().is_err(), "正常回传后守卫不得重复补发");
    }

    /// P2-5：自动登录任务 panic 时守卫补发失败结果（否则 `auto_login_in_flight` 永不复位）
    #[tokio::test]
    async fn test_auto_login_return_guard_sends_on_panic() {
        let (tx, mut rx) = mpsc::channel::<LoginResult>(16);
        let task = tokio::spawn(async move {
            let _guard = AutoLoginReturnGuard { tx, sent: false };
            panic!("模拟自动登录任务 panic");
        });
        assert!(task.await.is_err(), "任务应 panic");

        let result = rx.recv().await.expect("panic 后守卫应补发登录结果");
        assert_eq!(result.source, LoginSource::Auto);
        assert_eq!(result.terminal, LoginTerminal::Failed);
    }

    /// F5 同构（P2-5 口径）：Profile 切换探测任务 panic 时守卫补发失败消息
    /// （否则 `profile_probe_in_flight` 永久卡住、切换检测从此被防重入吞掉）
    #[tokio::test]
    async fn test_profile_probe_return_guard_sends_on_panic() {
        let (tx, mut rx) = mpsc::channel::<ProfileProbeMessage>(8);
        // 模拟切换探测任务 panic：守卫在 unwind 中 drop
        let task = tokio::spawn(async move {
            let _guard = ProfileProbeReturnGuard::new(tx);
            panic!("模拟 Profile 切换探测任务 panic");
        });
        assert!(task.await.is_err(), "任务应 panic");

        // 守卫补发的失败消息必须到达（主循环据此复位在途标记）
        assert!(
            matches!(rx.recv().await, Some(ProfileProbeMessage::Failed(_))),
            "panic 后守卫应补发一条失败消息"
        );
    }

    #[test]
    fn test_login_cancel_preserves_failure_budget() {
        for source in [
            LoginSource::Auto,
            LoginSource::Manual,
            LoginSource::Browser,
            LoginSource::LoginOnce,
        ] {
            assert_eq!(
                login_failure_budget_action(LoginTerminal::Cancelled, source),
                LoginFailureBudgetAction::Preserve
            );
        }
    }

    #[test]
    fn test_login_success_resets_failure_budget_for_every_source() {
        for source in [
            LoginSource::Auto,
            LoginSource::Manual,
            LoginSource::Browser,
            LoginSource::LoginOnce,
        ] {
            assert_eq!(
                login_failure_budget_action(LoginTerminal::Success, source),
                LoginFailureBudgetAction::Reset
            );
        }
    }

    #[test]
    fn test_adaptive_interval_offline_uses_short_period() {
        // 异常态：固定短周期探测，断网感知从最差 300s 降到 30s
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Offline, 0, 300),
            PROBE_ONLINE_BACKOFF_BASE_SECS
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::CaptivePortal, 0, 300),
            PROBE_ONLINE_BACKOFF_BASE_SECS
        );
    }

    #[test]
    fn test_adaptive_interval_online_exponential_backoff() {
        // 连续在线：30 → 60 → 120 → 240 → 300（封顶 check_interval）
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 1, 300),
            30
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 2, 300),
            60
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 3, 300),
            120
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 5, 300),
            300
        );
        // streak 极大时不溢出、恒收敛到 base
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, u32::MAX, 300),
            300
        );
    }

    #[test]
    fn test_adaptive_interval_caps_at_configured_base() {
        // 用户配置了更短的 check_interval（如 60s）：Online 退避到 60 封顶，
        // 异常态仍用固定短周期
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 1, 60),
            30
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 2, 60),
            60
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Online, 9, 60),
            60
        );
        assert_eq!(
            adaptive_check_interval_secs(NetworkStatus::Offline, 4, 60),
            PROBE_ONLINE_BACKOFF_BASE_SECS
        );
    }

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

    /// 挂起的网关检测器：`default_gateways` 永不回返——模拟 Profile 切换探测
    /// 子进程挂起（生产各 10s 超时），构造「切换探测在途」状态；
    /// `gateways_called` 标记探测已被发起，供用例断言命令下发时探测确在途
    /// （否则「Stop 即时处理」可能是探测尚未开始而 vacuously 通过）
    struct HangingGatewayDetect {
        gateways_called: AtomicBool,
    }

    #[async_trait::async_trait]
    impl NetworkDetect for HangingGatewayDetect {
        async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
            // 返回空而非挂起：网络探测路径不依赖本检测器挂起（由 HTTP 超时
            // 驱动完成），本检测器只需网关探测挂起
            Ok(vec![])
        }
        async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
            self.gateways_called.store(true, Ordering::SeqCst);
            std::future::pending().await
        }
        async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
            Ok(None)
        }
    }

    /// 构造完整 EngineDeps 并启动 run_loop（真实服务 + 可注入检测器）
    ///
    /// 监测配置：HTTP 目标指向只接受连接但不响应的本地服务，使自动探测挂在
    /// HTTP 超时；手动诊断仅等待公网探测，不额外执行本地链路诊断。
    /// `pause_all_day` 为 true 时配置全天定时暂停窗口（start == end）。
    /// `clock` 注入业务周期判定的墙钟时钟源（常规用例传 [`WallClock::system`]，
    /// 睡眠模拟用例传伪造时钟）。同时返回装配内的事件总线供事件断言订阅，
    /// 以及 `ConfigService` 供用例在启动后补改设置（如开启 auto_switch）。
    #[allow(clippy::type_complexity)]
    async fn make_engine_with_detector(
        detector: Arc<dyn NetworkDetect>,
        pause_all_day: bool,
        clock: WallClock,
    ) -> (
        tempfile::TempDir,
        mpsc::Sender<EngineCommand>,
        Arc<StatusManager>,
        Arc<Metrics>,
        Arc<EventBus>,
        Arc<ConfigService>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let (reload_tx, _reload_rx) = mpsc::channel(8);
        let config = ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stall_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _stream = stream;
                    std::future::pending::<()>().await;
                });
            }
        });
        // 活跃 Profile 显式写入「非重定向」形态的登录网址：`uses_redirect_login()`
        // 在「Browser 渠道且 auth_url 为空」时返回 true，会额外触发
        // `redirect_fallback` 分支（见 `monitor::check_once`）。本组用例的
        // `HangingDetect::list_interfaces` 是永久 pending，一旦进入
        // `probe_local_link()` 探测便永不回传 —— 实测推进 40s 虚拟时钟亦无法完成，
        // 与用例本意（验证 F5 命令通道不被探测阻塞）无关，须从源头避开。
        //
        // 同时必须填**非空** auth_url：留空会走 `AuthEndpointState::Missing` 分支，
        // 而填了可达地址会引入真实 TCP 等待。指向本机高位端口外的 9 号端口
        // （无监听）使 `inspect_auth_endpoint` 立即得到「连接被拒」并返回
        // `Unreachable`，不占用 `auth_url_timeout`。
        let profiles_for_setup = ProfileService::new(config.clone());
        let mut profile = profiles_for_setup.get_profile("default").unwrap();
        profile.auth_url = "http://127.0.0.1:9/login".to_string();
        profile.trigger_url = String::new();
        profiles_for_setup
            .update_profile("default", profile, false)
            .await
            .unwrap();

        let mut settings = config.load_settings();
        settings.global.monitor.tcp_enabled = false;
        settings.global.monitor.tcp_targets = vec![];
        settings.global.monitor.http_enabled = true;
        settings.global.monitor.http_targets = vec![format!("http://{stall_addr}/generate_204")];
        settings.global.monitor.http_timeout = 3;
        // 认证入口探测超时显式收紧到 1s（默认 5s）：指向无监听的 127.0.0.1:9 时
        // 连接被立即拒绝、通常不消耗该预算，但显式收紧可保证任何平台差异下
        // 都不会越过本用例 4s 的虚拟时钟推进量（§monitor_with_http_target 同范式）
        settings.global.monitor.auth_url_timeout = 1;
        settings.global.monitor.url_enabled = false;
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
        } else {
            // 显式关闭：PauseSettings 默认自 2026-09-16 起为 enabled=true（23-6 点），
            // 隐式吃默认值会让这些用例在真实夜间时段落入暂停窗口、立即检测被
            // F4 门控拦下而超时——测试不得依赖墙钟时间
            settings.global.pause.enabled = false;
        }
        config.save_settings(&settings).await.unwrap();
        config.reload().await.unwrap();

        // 前提守卫：后续断言依赖「自动监测不进入重定向兜底分支」。该前提一旦被
        // 配置默认值或判定逻辑变更打破，探测会挂在 HangingDetect 上永不回传，
        // 症状只是笼统的「等待条件超时」，故此处在源头直接断言，让失败原因自明。
        assert!(
            !config.runtime().load().profile.uses_redirect_login(),
            "测试 Profile 不得落在重定向登录分支：否则 check_once 会调用 \
             probe_local_link()，而 HangingDetect 永久 pending，探测永不回传"
        );

        let metrics = Metrics::new();
        let (status, bus, deps) =
            build_test_deps_with_events(tmp.path(), config.clone(), detector, metrics.clone());
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(8);
        let engine = Arc::new(Engine::from_sender(cmd_tx.clone()));
        tokio::spawn(run_loop(engine, deps, cmd_rx, clock));
        (tmp, cmd_tx, status, metrics, bus, config)
    }

    /// 默认装配（挂起网卡检测器）：见 [`make_engine_with_detector`]
    #[allow(clippy::type_complexity)]
    async fn make_engine_with_hanging_probe(
        pause_all_day: bool,
        clock: WallClock,
    ) -> (
        tempfile::TempDir,
        mpsc::Sender<EngineCommand>,
        Arc<StatusManager>,
        Arc<Metrics>,
        Arc<EventBus>,
    ) {
        let (tmp, cmd_tx, status, metrics, bus, _config) =
            make_engine_with_detector(Arc::new(HangingDetect), pause_all_day, clock).await;
        (tmp, cmd_tx, status, metrics, bus)
    }

    /// 测试装配：真实服务 + 注入检测器 + 独立事件总线（不 spawn run_loop）
    ///
    /// 供两类用例复用：`make_engine_with_hanging_probe` 经此装配驱动完整主循环；
    /// 事件发布断言用例直接构造 `EngineInner` 调用内部处理函数。事件总线随
    /// 返回值交出，供测试订阅断言。
    fn build_test_deps_with_events(
        base: &std::path::Path,
        config: Arc<ConfigService>,
        detector: Arc<dyn NetworkDetect>,
        metrics: Arc<Metrics>,
    ) -> (Arc<StatusManager>, Arc<EventBus>, EngineDeps) {
        let status = Arc::new(StatusManager::new());
        let monitor = Arc::new(
            MonitorService::new(config.clone(), detector.clone(), Some(metrics.clone())).unwrap(),
        );
        let history = Arc::new(LoginHistoryService::new(base));
        let profiles = Arc::new(ProfileService::new(config.clone()));
        let bridge = BridgeSupervisor::new(
            base.to_path_buf(),
            config.clone(),
            status.clone(),
            Some(metrics.clone()),
        );
        let environment = EnvironmentManager::new(base.to_path_buf(), status.clone());
        let tasks = TaskManager::new(base);
        // 脚本登录渠道的执行能力：与生产同一条路径（TaskExecutor 构造本身只是装配
        // 几个 Arc，不需要真起进程）
        let script_runner = crate::tasks::TaskExecutor::new(
            base,
            status.clone(),
            bridge.clone(),
            environment.clone(),
            config.clone(),
        );
        let orchestrator = Arc::new(LoginOrchestrator::new(
            config.clone(),
            history,
            status.clone(),
            bridge,
            environment,
            tasks,
            script_runner,
            monitor.clone(),
            tokio_util::sync::CancellationToken::new(),
            Some(metrics),
        ));
        let deps = EngineDeps {
            config_service: config,
            profile_service: profiles,
            orchestrator,
            status_manager: status.clone(),
            monitor_service: monitor,
            network_detect: detector,
            events: Arc::new(EventBus::new()),
        };
        let bus = deps.events.clone();
        (status, bus, deps)
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
        let (_tmp, cmd_tx, status, _metrics, _bus) =
            make_engine_with_hanging_probe(false, WallClock::system()).await;
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

        // 推进虚拟时钟越过 HTTP 超时（3s），探测完成并回传主循环
        tokio::time::advance(Duration::from_secs(4)).await;
        wait_for(|| snap().probe_total == 1).await;

        let s = snap();
        // HTTP 超时 → Offline 报告，且产生了真实 last_check
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
        let (_tmp, cmd_tx, status, metrics, _bus) =
            make_engine_with_hanging_probe(true, WallClock::system()).await;
        let snap = || status.borrow();

        // Start：仍应置 monitoring=true（Running），但跳过立即检测
        cmd_tx.send(EngineCommand::Start).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Running).await;
        assert_eq!(snap().probe_total, 0, "暂停窗口内 Start 不应触发探测");

        // Pause → Resume：定时窗口仍生效，Resume 不得触发立即检测。
        // F3 语义变更：窗口期内快照如实保持暂停态（原实现回显 false、
        // 前端在定时暂停时段错误显示"运行中"），故此处不再等待 pause_active 翻回 false
        cmd_tx.send(EngineCommand::Pause).await.unwrap();
        wait_for(|| snap().pause_active).await;
        cmd_tx.send(EngineCommand::Resume).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            snap().pause_active,
            "定时窗口期内 Resume 后快照仍应如实显示暂停"
        );
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

    /// TestNetwork：暂停期仍可独立执行手动诊断；正常期探测后台执行，
    /// 回复不阻塞命令通道。
    #[tokio::test(start_paused = true)]
    async fn test_test_network_reply_from_background() {
        // 暂停场景：手动诊断不受自动监测暂停影响，仍在后台执行
        let (_tmp, cmd_tx, _status, metrics, _bus) =
            make_engine_with_hanging_probe(true, WallClock::system()).await;
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(EngineCommand::TestNetwork { reply: reply_tx })
            .await
            .unwrap();
        assert_eq!(metrics.probe_total.load(Ordering::Relaxed), 0);
        tokio::time::advance(Duration::from_secs(4)).await;
        let result = reply_rx.await.unwrap().unwrap();
        assert_eq!(result.status, NetworkStatus::Offline);
        assert_eq!(
            result.local_link,
            crate::monitor::LocalLinkState::NotChecked
        );
        assert_eq!(
            result.auth_endpoint,
            crate::monitor::AuthEndpointState::NotChecked
        );
        assert_eq!(metrics.probe_total.load(Ordering::Relaxed), 1);

        // 正常场景：先 Start（探测 1 在途），再下发 TestNetwork（探测 2 独立执行），
        // 紧接着 Stop——Stop 在两个探测完成前即被处理（命令不被探测阻塞）
        let (_tmp2, cmd_tx2, status2, metrics2, _bus2) =
            make_engine_with_hanging_probe(false, WallClock::system()).await;
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
        // 手动诊断不再等待网卡枚举，可能先于自动探测完成；分别等待实际完成信号。
        wait_for(|| metrics2.probe_total.load(Ordering::Relaxed) == 2).await;
        // Start 的探测 + TestNetwork 的探测各计一次（G23 单点递增）
        assert_eq!(metrics2.probe_total.load(Ordering::Relaxed), 2);
    }

    /// 睡眠模拟（端到端，P2-2 墙钟化）：伪造墙钟大步前进（模拟合盖睡眠 8h）后，
    /// 周期探测的墙钟到期点已在睡眠期间越过，主循环应立即触发下一次探测
    /// ——原单调钟实现（tokio Interval）下该跳变不影响到期点，唤醒后首探要
    /// 再等满一个周期（本用例等待预算 ≪ 30s 自适应周期，单调钟下必然超时）。
    #[tokio::test(start_paused = true)]
    async fn test_wall_clock_probe_fires_after_sleep_jump() {
        let (clock, fake) = WallClock::fake();
        let (_tmp, cmd_tx, status, _metrics, _bus) =
            make_engine_with_hanging_probe(false, clock).await;
        let snap = || status.borrow();

        // Start：立即探测 #1（HTTP 目标挂 3s 超时后以 Offline 完成并回传）。
        // 分步推进虚拟时钟（单步 100ms）：给真实 I/O（连接本地 stall 服务）与
        // 探测超时定时器的注册留出被轮询的机会——一次性大步 advance 可能在
        // 探测任务尚未被首次轮询时就越过其超时时刻
        cmd_tx.send(EngineCommand::Start).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Running).await;
        let mut probe1_done = false;
        for _ in 0..40 {
            tokio::time::advance(Duration::from_millis(100)).await;
            // 真实小睡一拍：虚拟时钟推进不产生真实时间，本地 stall 服务的真实
            // TCP 连接完成与超时注册需要真实调度窗口——CI 慢 runner 上不插真实
            // 睡，4s 虚拟预算在探测任务获得轮询前就耗尽（PR #33 windows 矩阵
            // 实锤）；每次阻塞 5ms 不影响虚拟时钟语义
            std::thread::sleep(Duration::from_millis(5));
            if snap().probe_total == 1 {
                probe1_done = true;
                break;
            }
        }
        assert!(probe1_done, "4s 虚拟时间内探测 #1 未完成（3s 超时 + 余量）");

        // 探测结果（Offline）→ 自适应短周期 30s：到期点 = 结果时刻 + 30s。
        // 模拟合盖睡眠 8h：伪造墙钟大步前进（tokio 虚拟时钟不动）
        fake.advance(Duration::from_secs(8 * 3600));

        // 唤醒：到期点已在睡眠期间越过 → 应立即触发探测 #2。空转唤醒 ≤5s +
        // 探测 3s 完成，10s 虚拟时钟预算足够；若按单调钟计到期点，探测 #2 在
        // 结果时刻 + 30s 前不会发生，远超预算
        let mut probe2_done = false;
        for _ in 0..100 {
            tokio::time::advance(Duration::from_millis(100)).await;
            if snap().probe_total == 2 {
                probe2_done = true;
                break;
            }
        }
        assert!(
            probe2_done,
            "睡眠唤醒后 10s 虚拟时间内首探未立即到期（若按单调钟计到期点，30s 周期内不会发生）"
        );
        assert!(
            snap().last_check_time.is_some(),
            "唤醒后的首探应产生真实检测时间"
        );
    }

    /// F5 同构（Profile 切换检测）：auto_switch 开启且网关探测子进程挂起时，
    /// Stop 命令仍须即时处理。原实现在主循环内联 await 网关/SSID 探测
    /// （各 10s 超时，见 `network::detect::SUBPROCESS_TIMEOUT_SECS`），挂起
    /// 期间命令通道被阻塞——本用例的探测**永不完成**，内联实现（回归）下
    /// Stop 永不被处理、`wait_for` 在 1.5s 虚拟预算内超时 panic。
    #[tokio::test(start_paused = true)]
    async fn test_profile_switch_hanging_probe_keeps_commands_responsive() {
        let (clock, fake) = WallClock::fake();
        let detector = Arc::new(HangingGatewayDetect {
            gateways_called: AtomicBool::new(false),
        });
        let (_tmp, cmd_tx, status, _metrics, _bus, config) =
            make_engine_with_detector(detector.clone(), false, clock).await;
        let snap = || status.borrow();

        // auto_switch 默认关闭，显式开启；切换检测周期收到下限（60s），
        // 伪造墙钟推进一档即可到期。另显式关闭暂停窗口（PauseSettings 默认
        // 自 2026-09-16 起为 enabled=true，夜间跑测试会落入暂停窗口、
        // 切换检测被门控拦下——测试不得依赖墙钟时间）
        let mut settings = config.load_settings();
        settings.auto_switch = true;
        settings.global.monitor.profile_check_interval = PROFILE_CHECK_INTERVAL_MIN as u32;
        settings.global.pause.enabled = false;
        config.save_settings(&settings).await.unwrap();
        config.reload().await.unwrap();

        cmd_tx.send(EngineCommand::Start).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Running).await;

        // 伪造墙钟前进越过切换检测到期点（last_profile_check 锚定在引擎构造
        // 时刻），分步推进虚拟时钟唤醒主循环（周期分片睡眠 ≤60s、空转唤醒
        // ≤5s，且 Start 的首轮探测 ~3s 完成会提前唤醒）：到期 → 发起后台
        // 探测 → default_gateways 挂起（gateways_called 置位后永不返回）
        fake.advance(Duration::from_secs(PROFILE_CHECK_INTERVAL_MIN + 5));
        let mut probe_started = false;
        for _ in 0..600 {
            tokio::time::advance(Duration::from_millis(100)).await;
            if detector.gateways_called.load(Ordering::SeqCst) {
                probe_started = true;
                break;
            }
        }
        assert!(
            probe_started,
            "60s 虚拟时钟内 Profile 切换探测应已发起并挂起（否则 Stop 断言没有意义）"
        );

        // 探测确已挂起时下发 Stop：后台化后命令通道不受探测影响、
        // 1.5s 虚拟预算内即时生效（与网络探测同构的判据口径）
        cmd_tx.send(EngineCommand::Stop).await.unwrap();
        wait_for(|| snap().engine_state == EngineState::Stopped).await;
    }

    // ================================================================
    // 事件总线接入：网络状态翻转 / Profile 切换的发布断言
    // ================================================================

    /// 取下一条事件（超时即 panic，防悬挂）
    async fn next_event(rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>) -> AppEvent {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(envelope)) => envelope.event,
            other => panic!("等待事件超时或失败: {other:?}"),
        }
    }

    /// 断言窗口期内无事件到达（「宁可少发不错发」的回归锚点）
    async fn assert_no_event(rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>) {
        let got = tokio::time::timeout(Duration::from_millis(80), rx.recv()).await;
        assert!(got.is_err(), "不应有事件到达，实际: {got:?}");
    }

    /// 构造探测结果回传消息：advice 恒为非登录建议（NoAction），避免触发
    /// 自动登录路径——本组用例只关注事件发布，不驱动登录编排
    fn probe_msg(status: NetworkStatus, reason: AssessmentReason, version: u64) -> ProbeMessage {
        ProbeMessage::Report(
            ProbeReport {
                evidence: ProbeEvidence::default(),
                assessment: ConnectivityAssessment {
                    status,
                    confidence: crate::monitor::AssessmentConfidence::High,
                    reason,
                    auth_endpoint: crate::monitor::AuthEndpointState::NotChecked,
                    recovery_advice: RecoveryAdvice::NoAction,
                },
                latency_ms: 0,
                check_number: 1,
            },
            version,
        )
    }

    /// 事件接入（NetworkDown / NetworkUp）：探测结果仅在「通 ⇄ 断」翻转边界发布。
    /// 初始未知态（Unknown）首探不发布；断态之间（Offline ⇄ CaptivePortal）
    /// 同态不发布；探测执行失败不发布；翻转各恰好一次。
    #[tokio::test]
    async fn test_probe_flip_publishes_network_up_down_events() {
        let tmp = tempfile::tempdir().unwrap();
        let (reload_tx, _reload_rx) = mpsc::channel(8);
        let config = ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let detector: Arc<dyn NetworkDetect> = Arc::new(HangingDetect);
        let (_status, bus, deps) =
            build_test_deps_with_events(tmp.path(), config, detector, Metrics::new());
        let mut rx = bus.subscribe();
        let (probe_tx, _probe_rx) = mpsc::channel(1);
        let (pprobe_tx, _pprobe_rx) = mpsc::channel(1);
        let (login_tx, _login_rx) = mpsc::channel(1);
        let (clock, _fake) = WallClock::fake();
        let mut inner = EngineInner::new(probe_tx, pprobe_tx, login_tx, clock);
        let version = deps.config_service.config_version();

        // 首探 Online：上一态 Unknown（既非通也非断），不发布
        handle_probe_message(
            probe_msg(
                NetworkStatus::Online,
                AssessmentReason::InternetVerified,
                version,
            ),
            &mut inner,
            &deps,
        );
        assert_no_event(&mut rx).await;
        assert_eq!(inner.last_assessment.status, NetworkStatus::Online);

        // Online → Offline：发布 NetworkDown（原因为探测结论原因码）
        handle_probe_message(
            probe_msg(
                NetworkStatus::Offline,
                AssessmentReason::AllProbesFailed,
                version,
            ),
            &mut inner,
            &deps,
        );
        match next_event(&mut rx).await {
            AppEvent::NetworkDown(summary) => {
                assert_eq!(summary.reason.as_deref(), Some("all_probes_failed"));
                assert!(summary.profile_id.is_none(), "周期探测不绑定具体档案");
            }
            other => panic!("应为 NetworkDown，实际: {other:?}"),
        }

        // Offline → CaptivePortal：同为断态，同态不发布
        handle_probe_message(
            probe_msg(
                NetworkStatus::CaptivePortal,
                AssessmentReason::CaptiveDetected,
                version,
            ),
            &mut inner,
            &deps,
        );
        assert_no_event(&mut rx).await;

        // CaptivePortal → Online：发布 NetworkUp
        handle_probe_message(
            probe_msg(
                NetworkStatus::Online,
                AssessmentReason::InternetVerified,
                version,
            ),
            &mut inner,
            &deps,
        );
        match next_event(&mut rx).await {
            AppEvent::NetworkUp(summary) => {
                assert_eq!(summary.reason.as_deref(), Some("internet_verified"));
            }
            other => panic!("应为 NetworkUp，实际: {other:?}"),
        }

        // Online → Online：同态不发布
        handle_probe_message(
            probe_msg(
                NetworkStatus::Online,
                AssessmentReason::InternetVerified,
                version,
            ),
            &mut inner,
            &deps,
        );
        assert_no_event(&mut rx).await;

        // 探测执行失败：无新证据、状态未翻转，不发布
        handle_probe_message(
            ProbeMessage::Failed("模拟探测执行失败".into(), version),
            &mut inner,
            &deps,
        );
        assert_no_event(&mut rx).await;
        assert_eq!(inner.last_assessment.status, NetworkStatus::Online);
    }

    /// 事件接入（ProfileSwitched）：实际生效的 Profile 切换发布一次 from/to
    /// 白名单摘要（自动切换与手动 ApplyProfile 共用 handle_apply_profile 收敛点）；
    /// 幂等重复 ApplyProfile（目标即当前 Profile）不发布。
    #[tokio::test]
    async fn test_apply_profile_publishes_switch_event() {
        let tmp = tempfile::tempdir().unwrap();
        let (reload_tx, _reload_rx) = mpsc::channel(8);
        let config = ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let detector: Arc<dyn NetworkDetect> = Arc::new(HangingDetect);
        let (_status, bus, deps) =
            build_test_deps_with_events(tmp.path(), config, detector, Metrics::new());

        // 预置第二个 Profile 作为切换目标
        let mut data = deps.profile_service.get_profile("default").unwrap();
        data.name = "Second".into();
        deps.profile_service
            .create_profile("second", data)
            .await
            .unwrap();

        let mut rx = bus.subscribe();
        let (probe_tx, _probe_rx) = mpsc::channel(1);
        let (pprobe_tx, _pprobe_rx) = mpsc::channel(1);
        let (login_tx, _login_rx) = mpsc::channel(1);
        let (clock, _fake) = WallClock::fake();
        let mut inner = EngineInner::new(probe_tx, pprobe_tx, login_tx, clock);
        // 关闭监测：切换分支不触发后续探测（本用例只关注事件发布）
        inner.monitoring = false;

        // 幂等路径：ApplyProfile 目标即当前 Profile → 不发布
        handle_apply_profile("default", ProfileSwitchSource::Manual, &mut inner, &deps).await;
        assert_no_event(&mut rx).await;

        // 实际切换 default → second：发布 ProfileSwitched
        handle_apply_profile("second", ProfileSwitchSource::AutoSwitch, &mut inner, &deps).await;
        match next_event(&mut rx).await {
            AppEvent::ProfileSwitched(summary) => {
                assert_eq!(summary.from_profile_id.as_deref(), Some("default"));
                assert_eq!(summary.to_profile_id, "second");
            }
            other => panic!("应为 ProfileSwitched，实际: {other:?}"),
        }
    }

    /// Profile 切换探测结果回传（功能性）：网关匹配 → 自动切换 + 发布事件
    /// + 复位在途标记（原 `check_profile_switch` 的匹配段迁移至
    /// `handle_profile_probe_message`）；幂等（匹配结果即当前 Profile）不切换；
    /// 监测停止 / auto_switch 关闭的迟到结果不触发切换；失败回传不切换但同样
    /// 复位在途标记
    #[tokio::test]
    async fn test_profile_probe_message_matching_and_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let (reload_tx, _reload_rx) = mpsc::channel(8);
        let config = ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let detector: Arc<dyn NetworkDetect> = Arc::new(HangingDetect);
        let (_status, bus, deps) =
            build_test_deps_with_events(tmp.path(), config, detector, Metrics::new());

        // 预置两个带网关匹配规则的 Profile（当前活跃为 default）
        for (id, gateway) in [("dorm", "10.0.0.1"), ("lab", "192.168.5.1")] {
            let mut data = deps.profile_service.get_profile("default").unwrap();
            data.name = id.to_string();
            data.gateway_ip = gateway.to_string();
            deps.profile_service.create_profile(id, data).await.unwrap();
        }

        // 执行条件：auto_switch 开启（默认关闭）+ 暂停窗口显式关闭
        // （PauseSettings 默认 23-6 点，夜间跑测试会落入暂停窗口、结果被
        // 门控丢弃——测试不得依赖墙钟时间，口径同 make_engine_with_detector）
        let mut settings = deps.config_service.load_settings();
        settings.auto_switch = true;
        settings.global.pause.enabled = false;
        deps.config_service.save_settings(&settings).await.unwrap();
        deps.config_service.reload().await.unwrap();

        let mut rx = bus.subscribe();
        let (probe_tx, _probe_rx) = mpsc::channel(1);
        let (pprobe_tx, _pprobe_rx) = mpsc::channel(1);
        let (login_tx, _login_rx) = mpsc::channel(1);
        let (clock, _fake) = WallClock::fake();
        let mut inner = EngineInner::new(probe_tx, pprobe_tx, login_tx, clock);
        // 三重门之一：监测开启（切换会顺带触发一次后台立即探测，
        // HangingDetect 下挂在 list_interfaces 上永不回传，任务随测试运行时
        // 结束取消，无断言干扰——本用例只关注切换路径本身）
        inner.monitoring = true;

        // 网关 10.0.0.1 → 命中 dorm：自动切换 + 发布事件 + 复位在途标记
        inner.profile_probe_in_flight = true; // 模拟探测在途（回传后必须复位）
        handle_profile_probe_message(
            ProfileProbeMessage::Completed(vec![Ipv4Addr::new(10, 0, 0, 1)], Some("Campus".into())),
            &mut inner,
            &deps,
        )
        .await;
        assert!(!inner.profile_probe_in_flight, "结果回传后应复位在途标记");
        assert_eq!(
            deps.config_service.runtime().load().profile.id,
            "dorm",
            "网关匹配应自动切换到 dorm"
        );
        match next_event(&mut rx).await {
            AppEvent::ProfileSwitched(summary) => {
                assert_eq!(summary.from_profile_id.as_deref(), Some("default"));
                assert_eq!(summary.to_profile_id, "dorm");
            }
            other => panic!("应为 ProfileSwitched，实际: {other:?}"),
        }

        // 幂等：再次回传同一网关（匹配结果即当前 Profile）不重复切换、不发布
        handle_profile_probe_message(
            ProfileProbeMessage::Completed(vec![Ipv4Addr::new(10, 0, 0, 1)], None),
            &mut inner,
            &deps,
        )
        .await;
        assert_no_event(&mut rx).await;

        // 门控：监测停止后的迟到结果不得切换（Stop 可能晚于探测发起）
        inner.monitoring = false;
        handle_profile_probe_message(
            ProfileProbeMessage::Completed(vec![Ipv4Addr::new(192, 168, 5, 1)], None),
            &mut inner,
            &deps,
        )
        .await;
        assert_eq!(
            deps.config_service.runtime().load().profile.id,
            "dorm",
            "监测停止后到达的结果不得触发切换"
        );
        assert_no_event(&mut rx).await;

        // 门控：auto_switch 关闭后的迟到结果不得切换
        inner.monitoring = true;
        deps.profile_service.set_auto_switch(false).await.unwrap();
        handle_profile_probe_message(
            ProfileProbeMessage::Completed(vec![Ipv4Addr::new(192, 168, 5, 1)], None),
            &mut inner,
            &deps,
        )
        .await;
        assert_eq!(
            deps.config_service.runtime().load().profile.id,
            "dorm",
            "auto_switch 关闭后到达的结果不得触发切换"
        );

        // 失败回传：不切换，但同样复位在途标记
        inner.profile_probe_in_flight = true;
        handle_profile_probe_message(
            ProfileProbeMessage::Failed("模拟网关/SSID 探测失败".into()),
            &mut inner,
            &deps,
        )
        .await;
        assert!(!inner.profile_probe_in_flight, "失败回传也应复位在途标记");
        assert_no_event(&mut rx).await;
    }
}
