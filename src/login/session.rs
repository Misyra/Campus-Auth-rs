//! 登录会话状态机：单次登录的尝试循环、结果分类、重试与取消传播
//!
//! [`LoginSession`] 由 [`crate::login::LoginOrchestrator::submit`] 创建并 `tokio::spawn`，
//! 独立运行直至终态。每一轮尝试通过 `BridgeSupervisor::execute` 驱动 Python Worker，
//! 根据返回的 [`Outcome`] 分类为终态或可重试；可重试时检查预算并 `sleep` 等待（期间监听取消）。

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use std::future::Future;
use std::pin::Pin;

use crate::bridge::{BridgeError, IpcResponse, Outcome, StructuredResult};
use crate::config::{ConfigService, LoginChannel};
use crate::events::{AppEvent, LoginSummary};
use crate::login::history::{HistoryResult, LoginHistoryEntry, LoginHistoryService};
use crate::login::http_login;
use crate::login::script_login;
use crate::login::{LoginHandleInner, recover_lock};
use crate::status::{LoginSource, LoginStatus, PartialSnapshot, StatusManager};
use crate::utils::metrics::Metrics;

/// 单轮尝试的执行 future 类型（T7-5）：三渠道统一为
/// `Result<StructuredResult, String>`，可被 select! 撤销回收
type AttemptWork = Pin<Box<dyn Future<Output = Result<StructuredResult, String>> + Send>>;

/// 登录终态（成功 / 取消 / 失败）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginTerminal {
    /// 登录成功
    Success,
    /// 登录被取消
    Cancelled,
    /// 登录失败
    Failed,
}

/// 结果分类后的决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultAction {
    /// 终态（成功 / 取消 / 凭证无效 / 验证码 / 未知错误）
    Terminal(LoginTerminal),
    /// 可重试（导航超时 / 选择器失败 / 网络错误）
    Retry,
    /// 重试预算耗尽
    Exhausted,
}

/// 登录终态结果（句柄 `await_result` 返回、历史与调度器读取）
#[derive(Debug, Clone)]
pub struct LoginResult {
    /// 仅导航或网络故障允许引擎在冷却后重启谨慎登录。
    pub network_retryable: bool,
    /// 唯一终态语义；调用方不得再用布尔值猜测取消与失败。
    pub terminal: LoginTerminal,
    /// 结果消息（成功提示 / 失败原因 / 取消原因）
    pub message: String,
    /// 登录来源
    pub source: LoginSource,
    /// 总耗时
    pub duration: Duration,
    /// 尝试次数（含首次）
    pub attempts: u32,
}

impl LoginResult {
    /// 是否为成功终态。
    pub fn is_success(&self) -> bool {
        self.terminal == LoginTerminal::Success
    }

    /// 是否为取消终态。
    pub fn is_cancelled(&self) -> bool {
        self.terminal == LoginTerminal::Cancelled
    }
}

/// 登录后网络验证结果；取消不能与可重试的网络失败共用布尔值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkVerification {
    /// 已确认公网在线。
    Online,
    /// 未确认在线，可按登录重试策略处理。
    RetryableFailure,
    /// 用户取消、抢占或应用关闭。
    Cancelled,
}

/// 单次尝试结果分类（纯函数）
///
/// 分类规则：
/// - `Success` → 终态（成功）
/// - `Cancelled` → 终态（取消）
/// - `InvalidCredential` / `ManualRequired` / `UnknownError` → 终态（失败，不重试）
/// - `CaptchaFailed` / `NavigationTimeout` / `SelectorFailed` / `NetworkError` → 可重试
///
/// 验证码失败（OCR 误识别）与网络/导航失败同属瞬时性失败，难以可靠区分具体成因，
/// 故统一重试整个登录流程（受 `max_retries` 预算约束），达到上限才以失败终态结束（历史遗留 #6）。
///
/// `Outcome` 为 `#[non_exhaustive]`，未知变体兜底为终态（失败）。
#[allow(unreachable_patterns)]
pub fn classify(outcome: Outcome) -> ResultAction {
    match outcome {
        Outcome::Success => ResultAction::Terminal(LoginTerminal::Success),
        Outcome::Cancelled => ResultAction::Terminal(LoginTerminal::Cancelled),
        Outcome::InvalidCredential => ResultAction::Terminal(LoginTerminal::Failed),
        Outcome::ManualRequired => ResultAction::Terminal(LoginTerminal::Failed),
        Outcome::UnknownError => ResultAction::Terminal(LoginTerminal::Failed),
        Outcome::CaptchaFailed => ResultAction::Retry,
        Outcome::NavigationTimeout => ResultAction::Retry,
        Outcome::SelectorFailed => ResultAction::Retry,
        // P11：断言失败（assert_text 超时/不匹配）可重试、不回收 Worker
        Outcome::AssertionFailed => ResultAction::Retry,
        Outcome::NetworkError => ResultAction::Retry,
        // 前向兼容：未知 outcome 兜底为终态（失败），避免无限重试
        _ => ResultAction::Terminal(LoginTerminal::Failed),
    }
}

/// 判断某次失败结果是否需要强制回收 Worker
///
/// 仅 `NetworkError` 回收：它可能伴随 Worker 网络栈异常（与 IPC 枚举文档一致：
/// 网络错误强制回收），回收后由 `ensure_worker` 重新 spawn，避免已损坏上下文
/// 被复用导致后续重试持续失败。
///
/// 注意 `UnknownError` **不在**回收之列——`classify(UnknownError)` 返回
/// `Terminal(Failed)`，在 `try_retry` 之前即以失败终态 return，本函数对其的
/// 判定分支实际不可达；终态路径不回收 Worker（emit 仅在 Worker 存活时发
/// `close_browser` 收尾，进程保留）。
/// 其余可重试结果（NavigationTimeout / SelectorFailed / AssertionFailed /
/// CaptchaFailed）上下文未损坏，复用即可。
pub fn should_force_recycle(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::NetworkError)
}

/// 从 StructuredResult.data 提取页面弹窗文案，拼接为可读后缀
///
/// Worker 在任务期间捕获页面 alert/confirm 文案（如「账号或密码错误」「登录成功！」），
/// 存于 `data.dialogs`。此处格式化为 `；页面提示: A / B` 形式，供登录日志展示；
/// 无弹窗时返回空串。
fn dialog_note(data: &Value) -> String {
    let msgs: Vec<&str> = data
        .get("dialogs")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if msgs.is_empty() {
        String::new()
    } else {
        format!("；页面提示: {}", msgs.join(" / "))
    }
}

/// 渠道标识（事件载荷 `LoginSummary.channel`）：与 `LoginChannel` 的 serde
/// snake_case 序列化同值（browser / http / script），词表固定、不含用户数据
pub(crate) fn login_channel_code(channel: LoginChannel) -> &'static str {
    match channel {
        LoginChannel::Browser => "browser",
        LoginChannel::Http => "http",
        LoginChannel::Script => "script",
    }
}

/// `close_browser` 的命令级超时。
///
/// 与 Python 侧 `handle_close_browser` 的内部超时一致
/// （`playwright_worker.py` 的 `_WAIT_TIMEOUT_SECS = 8`）：两端同值时以 Python
/// 自愈为主，Rust 侧不再多等 4s。该值是抢占收尾预算的组成部分——调大此处会
/// 使 `login::PREEMPT_WAIT_BUDGET` 不再覆盖最坏路径（由不变量测试锁定）。
pub(crate) const CLOSE_BROWSER_TIMEOUT: Duration = Duration::from_secs(8);

/// 会话跨实例共享的服务依赖集（由 LoginOrchestrator 构造一次并复用，A-2）
pub(crate) struct SessionDeps {
    /// Bridge 句柄（trait 化：可注入 mock 做状态机单测）
    pub bridge: std::sync::Arc<dyn crate::bridge::BridgeApi>,
    /// 网络监测服务（登录后网络验证使用）
    pub monitor: std::sync::Arc<crate::monitor::MonitorService>,
    /// 配置服务（读取登录后网络验证延迟 post_login_delay）
    pub config_service: std::sync::Arc<ConfigService>,
    /// 状态管理器（广播登录状态）
    pub status_manager: std::sync::Arc<StatusManager>,
    /// 历史服务（终态写入）
    pub history_service: std::sync::Arc<LoginHistoryService>,
    /// 脚本执行能力（脚本登录渠道用；trait 化以便状态机单测注入替身，见
    /// [`crate::tasks::ScriptRunnerApi`]）
    pub script_runner: std::sync::Arc<dyn crate::tasks::ScriptRunnerApi>,
    /// 事件总线（登录终态在 [`LoginSession::emit`] 发布 [`AppEvent::LoginSucceeded`]
    /// / [`AppEvent::LoginFailed`]；构造注入，测试兼容装配发往孤立总线）
    pub events: std::sync::Arc<crate::events::EventBus>,
    /// 运行指标（可选）
    pub metrics: Option<std::sync::Arc<Metrics>>,
}

/// 本次登录的执行计划；三种渠道互斥，避免多个 Option 组合出非法状态。
pub(crate) enum LoginAttemptPlan {
    /// 浏览器 Worker 配置。
    Browser(Value),
    /// 浏览器渠道持久会话自动登录：派发 Worker `profile_auto_login`。
    ///
    /// 载荷由编排层预装配完整（`user_data_dir` / `portal_url` /
    /// `recorded_actions` / `browser_settings`（步骤预算 ≥45s 口径）），会话层
    /// 仅注入 cancel_id 后经 [`crate::bridge::BridgeApi::execute_persistent_auto_login`]
    /// 下发。Worker 的 Success 只代表回放完成，登录成败仍走既有网络验证兜底
    /// （本计划无可声明的 `success_condition`）；持久上下文由 Worker 在命令结束
    /// 即关闭，终态不再发 `close_browser`。
    BrowserPersistent(Value),
    /// Rust 进程内直连请求。
    Http(Box<crate::login::http_login::HttpLoginRequest>),
    /// Rust 进程内脚本任务。
    Script(Box<crate::login::script_login::ScriptLoginPlan>),
}

impl LoginAttemptPlan {
    /// 由唯一执行计划得到本次实际登录渠道。
    fn channel(&self) -> LoginChannel {
        match self {
            Self::Browser(_) | Self::BrowserPersistent(_) => LoginChannel::Browser,
            Self::Http(_) => LoginChannel::Http,
            Self::Script(_) => LoginChannel::Script,
        }
    }

    /// 仅浏览器渠道会占用 Bridge 会话并需要取消 Worker attempt。
    fn uses_bridge(&self) -> bool {
        matches!(self, Self::Browser(_) | Self::BrowserPersistent(_))
    }

    /// 是否为持久会话自动登录计划（失效提示与终态收尾的门控依据）。
    fn is_persistent_auto_login(&self) -> bool {
        matches!(self, Self::BrowserPersistent(_))
    }

    /// 仅浏览器任务可声明跳过登录后网络验证的成功条件。
    fn has_explicit_success_condition(&self) -> bool {
        match self {
            Self::Browser(config) => config
                .get("task_config")
                .and_then(|t| t.get("success_condition"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty()),
            // 持久自动登录无任务步骤，成功判定恒走网络验证兜底
            Self::BrowserPersistent(_) | Self::Http(_) | Self::Script(_) => false,
        }
    }
}

/// 单次会话的值参数（从配置快照派生，A-2）
pub(crate) struct SessionParams {
    /// 登录来源
    pub source: LoginSource,
    /// 浏览器任务 ID（仅 `Browser` 来源有值），为空时回退到 Profile 的 `active_task`
    pub task_id: Option<String>,
    /// 最大重试次数（不含首次尝试）
    pub max_retries: u32,
    /// 重试间隔
    pub retry_interval: Duration,
    /// 单次登录会话总超时
    pub login_timeout: Duration,
    /// 关联 Profile ID（写入历史）
    pub profile_id: String,
    /// 本次方案的出口；验证不能因活动方案切换而改走其他网卡。
    pub network: crate::network::settings::NetworkSettings,
    /// 本次唯一执行计划。
    pub plan: LoginAttemptPlan,
}

/// 登录会话：持有会话参数、取消原语与服务依赖，驱动单次登录状态机
pub struct LoginSession {
    /// 最后一次已分类的执行结果，不通过错误文案猜测重试资格。
    last_outcome: Option<Outcome>,
    /// 会话参数（来源/重试预算/凭证等）
    params: SessionParams,
    /// 会话级取消令牌（整个会话生命周期）
    cancel_token: CancellationToken,
    /// 结果共享槽（与 [`crate::login::LoginHandle`] 共享）
    result_slot: Arc<LoginHandleInner>,
    /// 当前在途 attempt 的 cancel_id（供取消传播读取）
    attempt_cancel_id: Arc<ArcSwapOption<String>>,
    /// 应用级 shutdown 信号（触发时会话立即以取消终态退出）
    shutdown_token: CancellationToken,
    /// 取消原因（由 `cancel_current` / `cancel_auto_pending` 设置，供终态消息使用）
    cancel_reason: Arc<StdMutex<Option<String>>>,
    /// 服务依赖集
    deps: SessionDeps,
}

impl LoginSession {
    /// 构造登录会话
    pub(crate) fn new(
        params: SessionParams,
        cancel_token: CancellationToken,
        result_slot: Arc<LoginHandleInner>,
        attempt_cancel_id: Arc<ArcSwapOption<String>>,
        shutdown_token: CancellationToken,
        cancel_reason: Arc<StdMutex<Option<String>>>,
        deps: SessionDeps,
    ) -> Self {
        Self {
            last_outcome: None,
            params,
            cancel_token,
            result_slot,
            attempt_cancel_id,
            shutdown_token,
            cancel_reason,
            deps,
        }
    }

    /// 获取本次会话计划实际使用的登录渠道。
    pub(super) fn channel(&self) -> LoginChannel {
        self.params.plan.channel()
    }

    /// 会话主循环：执行 → 分类 → 重试/终态
    ///
    /// 每轮尝试按阶段协作（T7-5）：循环头守卫（取消/会话总超时）→
    /// [`Self::prepare_attempt`] 尝试准备（cancel_id / 进度状态 / 执行 future）→
    /// [`Self::await_attempt`] 执行等待（取消/关闭/超时/完成竞速）→
    /// [`Self::handle_attempt_result`] 结果处理（分类 → 终态收尾或重试）。
    pub async fn run(mut self) {
        let session_start = Instant::now();
        self.deps.status_manager.merge(PartialSnapshot::Login {
            status: LoginStatus::Running,
            source: Some(self.params.source),
            message: Some("登录中...".into()),
            retry_count: 0,
        });
        info!(
            source = ?self.params.source,
            max_retries = self.params.max_retries,
            "登录会话开始"
        );

        let total_attempts = self.params.max_retries + 1;
        let mut attempts_used: u32 = 0;
        // 只有浏览器渠道经 Bridge 驱动 Worker，脚本失败不能回收别人的浏览器会话。
        let uses_bridge = self.params.plan.uses_bridge();

        loop {
            // 取消检查（状态机任意阶段）
            if self.cancel_token.is_cancelled() {
                self.finish_with_cancelled(session_start, attempts_used, None)
                    .await;
                return;
            }

            // 会话总超时检查（login_timeout 至少 1s，见 SessionParams 构造 clamp）
            if session_start.elapsed() > self.params.login_timeout.max(Duration::from_secs(1)) {
                if uses_bridge && let Some(cid) = self.attempt_cancel_id.load_full() {
                    self.deps.bridge.cancel(cid.as_str());
                }
                self.finish_with_failure(session_start, attempts_used, "登录超时".into())
                    .await;
                return;
            }

            // 尝试准备段：cancel_id 生成/登记 + 进度状态刷新 + 执行 future 构造
            let (cancel_id, work) =
                self.prepare_attempt(attempts_used, total_attempts, uses_bridge);

            // 执行等待段：取消/关闭/总超时/完成竞速，命中前三者即已终态收尾
            let Some(exec) = self
                .await_attempt(work, &cancel_id, uses_bridge, session_start, attempts_used)
                .await
            else {
                return;
            };

            let structured = match exec {
                Ok(s) => s,
                Err(e) => {
                    error!(attempt = attempts_used + 1, "登录执行失败: {e}");
                    self.finish_with_failure(session_start, attempts_used, e)
                        .await;
                    return;
                }
            };

            // 结果处理段：分类 → 终态收尾或重试（false 表示已 emit 终态，退出循环）
            if !self
                .handle_attempt_result(
                    structured,
                    &mut attempts_used,
                    session_start,
                    uses_bridge,
                    total_attempts,
                )
                .await
            {
                return;
            }
        }
    }

    /// 尝试准备段：生成本轮 attempt 的 cancel_id（UUID v4）、刷新进度状态、
    /// 构造本轮执行的 future
    ///
    /// cancel_id 同时登记到 `attempt_cancel_id`（仅浏览器渠道），供取消传播
    /// 与归属感知回收读取。
    fn prepare_attempt(
        &self,
        attempts_used: u32,
        total_attempts: u32,
        uses_bridge: bool,
    ) -> (String, AttemptWork) {
        // 生成本轮 attempt 的 cancel_id（UUID v4）
        let cancel_id = uuid::Uuid::new_v4().to_string();
        if uses_bridge {
            self.attempt_cancel_id
                .store(Some(Arc::new(cancel_id.clone())));
        }

        let attempt_no = attempts_used + 1;
        self.deps.status_manager.merge(PartialSnapshot::Login {
            status: LoginStatus::Running,
            source: Some(self.params.source),
            message: Some(format!("尝试 {attempt_no}/{total_attempts}")),
            retry_count: attempts_used,
        });

        let work = self.build_attempt_work(&cancel_id);
        (cancel_id, work)
    }

    /// 构造本轮尝试的执行 future
    ///
    /// 直连渠道：Rust 进程内构造/发送请求；脚本渠道：Rust 进程内起子进程跑脚本；
    /// 浏览器渠道：Bridge 驱动 Worker（普通任务按来源选择命令，持久会话模式
    /// 派发 `profile_auto_login`）。三者统一为
    /// `Result<StructuredResult, String>` 的可取消 future，取消/shutdown/超时边界对
    /// 三种渠道一致（进程内路径无 Bridge 可取消，靠丢弃 future 回收子进程）。
    fn build_attempt_work(&self, cancel_id: &str) -> AttemptWork {
        match &self.params.plan {
            LoginAttemptPlan::Http(plan) => {
                let plan = plan.clone();
                Box::pin(async move { Ok(http_login::run_once(&plan).await.to_structured()) })
            }
            LoginAttemptPlan::Script(plan) => {
                let plan = plan.clone();
                // 取消时本 future 被 `select!` 直接丢弃，`run_command` 里带
                // `kill_on_drop` 的子进程随之回收（Windows 另有 Job Object 兜住
                // 整棵进程树），故脚本渠道不需要 Bridge 那种 cancel_id 收尾。
                let runner = self.deps.script_runner.clone();
                Box::pin(async move { Ok(script_login::run_once(&runner, &plan).await) })
            }
            LoginAttemptPlan::Browser(worker_config) => {
                // 普通浏览器任务路径（非持久模式零变化）：按来源选择 Bridge 命令
                let method = match self.params.source {
                    LoginSource::Browser => "execute_browser_task",
                    _ => "execute_login_attempt",
                };
                let mut cfg = worker_config.clone();
                if let Some(tid) = &self.params.task_id {
                    cfg["task_id"] = json!(tid.clone());
                }
                self.build_browser_work(method, &cfg, cancel_id)
            }
            LoginAttemptPlan::BrowserPersistent(params) => {
                self.build_browser_work("profile_auto_login", params, cancel_id)
            }
        }
    }

    /// 浏览器渠道的执行 future：按给定 Bridge 命令驱动 Worker
    ///
    /// Worker 崩溃 / 启动超时 / 忙归一为 `NetworkError` 结构化结果走既有
    /// classify/try_retry 路径（见各分支注释）；其余 `Err` 保持终态失败。
    fn build_browser_work(
        &self,
        method: &str,
        worker_config: &Value,
        cancel_id: &str,
    ) -> AttemptWork {
        let mut params = worker_config.clone();
        params["cancel_id"] = json!(cancel_id.to_string());
        let bridge = self.deps.bridge.clone();
        let method = method.to_string();
        Box::pin(async move {
            let result = if method == "profile_auto_login" {
                bridge.execute_persistent_auto_login(params).await
            } else {
                bridge.execute(&method, params).await
            };
            match result {
                Ok(resp) => Ok(Self::parse_ipc_response(resp)),
                // Worker 崩溃 / 启动超时到达调用方是 Err 而非 Outcome：
                // Worker 可能已被 Supervisor 重建或仅是瞬时故障，与
                // Outcome::NetworkError 同属可重试失败——转成 NetworkError
                // 结构化结果走既有 classify/try_retry 路径（此前直接终态
                // 失败，Worker 崩溃一次即放弃整场登录；重试前的
                // force_recycle_if_unowned 对已死 Worker 幂等安全）。
                Err(
                    e @ (BridgeError::WorkerCrashed { .. }
                    | BridgeError::WorkerStartupTimeout
                    | BridgeError::NetworkEgress(_)),
                ) => Ok(StructuredResult {
                    outcome: Outcome::NetworkError,
                    message: format!("Worker 异常: {e}"),
                    data: Value::Null,
                    screenshot_url: None,
                    duration_ms: 0,
                }),
                // WorkerBusy（调试会话 / 重定向检测占用会话槽位）是
                // 典型的**瞬时**冲突：占用方可短则数秒（调试单步）、
                // 长则贯穿调试会话。此前归为终态失败，调试期间自动
                // 登录被静默压制且登录历史被 WorkerBusy 刷屏。转成
                // NetworkError 走既有重试/退避路径（与上方崩溃分支
                // 同一处理）；随后的归属感知回收（force_recycle_
                // if_unowned）发现槽位不属于本会话时不会误杀占用方。
                Err(e @ BridgeError::WorkerBusy) => Ok(StructuredResult {
                    outcome: Outcome::NetworkError,
                    message: format!("Worker 忙（被其他任务占用，将重试）: {e}"),
                    data: Value::Null,
                    screenshot_url: None,
                    duration_ms: 0,
                }),
                // 其余 Err（WorkerNotInstalled /
                // WorkerSpawnBlocked / WorkerEnvironmentInvalid /
                // Cancelled / Timeout / SupervisorNotRunning 等）保持
                // 终态失败：重试不会变好，或取消/超时语义要求立即退出
                Err(e) => Err(format!("Bridge 执行失败: {e}")),
            }
        })
    }

    /// 执行等待段：等待尝试完成，期间监听会话级取消、应用 shutdown 与会话总超时
    ///
    /// bridge.execute() 内部最长阻塞 300s。此处用 tokio::select! (biased) 同时监听：
    /// - cancel_token 触发（用户/系统取消，最高优先级）
    /// - shutdown_token 触发（应用关闭，立即以取消终态退出）
    /// - 会话总超时到期（防止 Worker 卡死导致整会话永不退出）
    /// - 尝试完成（正常路径）
    ///
    /// biased 保证取消类信号先于执行/超时生效，避免取消被延迟。
    /// 取消通知仅对 Bridge 路径有意义（携带 cancel_id 停 Worker 任务），
    /// 直连路径的请求 future 随 select 撤销直接丢弃。
    ///
    /// 返回 `None` 表示等待期间会话已以终态收尾（emit 已完成），调用方应立即退出。
    async fn await_attempt(
        &self,
        mut work: AttemptWork,
        cancel_id: &str,
        uses_bridge: bool,
        session_start: Instant,
        attempts_used: u32,
    ) -> Option<Result<StructuredResult, String>> {
        let ct = self.cancel_token.clone();
        let remaining = self
            .params
            .login_timeout
            .max(Duration::from_secs(1))
            .saturating_sub(session_start.elapsed());
        tokio::select! {
            biased;
            _ = ct.cancelled() => {
                if uses_bridge {
                    self.deps.bridge.cancel(cancel_id);
                }
                self.finish_with_cancelled(session_start, attempts_used, None).await;
                None
            }
            _ = self.shutdown_token.cancelled() => {
                if uses_bridge {
                    self.deps.bridge.cancel(cancel_id);
                }
                self.finish_with_cancelled(
                    session_start,
                    attempts_used,
                    Some("应用关闭".to_string()),
                )
                .await;
                None
            }
            _ = sleep(remaining) => {
                if uses_bridge {
                    self.deps.bridge.cancel(cancel_id);
                }
                self.finish_with_failure(session_start, attempts_used, "登录超时".into())
                    .await;
                None
            }
            res = &mut work => Some(res),
        }
    }

    /// 结果处理段：分类本轮结果 → 终态收尾或转入下一轮
    ///
    /// 返回 `true` 表示继续下一轮循环，`false` 表示已 emit 终态结果
    /// （终态 / 重试耗尽 / 重试等待期被取消）。
    async fn handle_attempt_result(
        &mut self,
        structured: StructuredResult,
        attempts_used: &mut u32,
        session_start: Instant,
        uses_bridge: bool,
        total_attempts: u32,
    ) -> bool {
        self.last_outcome = Some(structured.outcome);

        // 本轮 attempt 结束，清除在途 cancel_id。清除前留一份：Bridge 强杀
        // Worker 前要用它证明会话槽位属于本会话（见 try_retry 的归属感知回收）。
        let finished_attempt_cancel_id = self.attempt_cancel_id.load_full();
        self.attempt_cancel_id.store(None);

        // 分类结果：可重试但重试预算已耗尽时归入 Exhausted（避免进入 try_retry）
        // 后再次判断，保持"预算耗尽"这一决策与 classify 一起表达
        let action = classify(structured.outcome);
        let action = if action == ResultAction::Retry && *attempts_used >= self.params.max_retries {
            ResultAction::Exhausted
        } else {
            action
        };
        match action {
            ResultAction::Terminal(kind) => match kind {
                LoginTerminal::Success => {
                    self.handle_success_outcome(
                        &structured,
                        attempts_used,
                        session_start,
                        finished_attempt_cancel_id.as_deref().map(String::as_str),
                    )
                    .await
                }
                LoginTerminal::Cancelled => {
                    // 取消原因（用户取消/抢占/应用关闭等）存于 cancel_reason，
                    // 经 make_cancelled_result 带出，不得硬编码覆盖
                    self.finish_with_cancelled(session_start, *attempts_used, None)
                        .await;
                    false
                }
                LoginTerminal::Failed => {
                    self.finish_with_failure(
                        session_start,
                        *attempts_used,
                        format!("{}{}", structured.message, dialog_note(&structured.data)),
                    )
                    .await;
                    false
                }
            },
            ResultAction::Retry => {
                self.try_retry(
                    &structured,
                    attempts_used,
                    session_start,
                    // 直连路径无 Worker 参与，网络类失败不需要回收 Worker
                    uses_bridge && should_force_recycle(structured.outcome),
                    finished_attempt_cancel_id.as_deref().map(String::as_str),
                )
                .await
            }
            ResultAction::Exhausted => {
                // 可重试结果但重试预算已耗尽：以"重试耗尽"终态收尾，
                // 不再进入 try_retry 重复尝试（由下方 classify 前预算预检产生）。
                // 附带最后一次失败的步骤错误与页面弹窗文案，便于定位真实原因。
                self.finish_with_failure(
                    session_start,
                    *attempts_used,
                    // 口径统一为总尝试次数（含首试），与 try_retry 耗尽分支
                    // 及「尝试 N/M」进度文案一致（历史实现两处各说各话）
                    format!(
                        "重试耗尽（共 {} 次尝试）: {}{}{}",
                        total_attempts,
                        structured.message,
                        dialog_note(&structured.data),
                        self.persistent_session_hint(structured.outcome)
                    ),
                )
                .await;
                false
            }
        }
    }

    /// 成功终态处理：汇总成功消息；显式 `success_condition` 直接受理，否则
    /// 登录后真实网络验证兜底，验证未通过转可重试路径
    ///
    /// 返回 `true` 表示继续下一轮循环（网络验证未过转入重试），`false`
    /// 表示已 emit 终态结果。
    async fn handle_success_outcome(
        &mut self,
        structured: &StructuredResult,
        attempts_used: &mut u32,
        session_start: Instant,
        finished_owner_cancel_id: Option<&str>,
    ) -> bool {
        // 汇总成功消息：Worker message（如「成功条件命中」「N 个非必须
        // 步骤失败」）与页面弹窗文案（如「登录成功！」）一并进入日志
        let note = dialog_note(&structured.data);
        let detail = {
            let m = structured.message.trim();
            if !m.is_empty() && m != "执行成功" {
                format!("（{m}）")
            } else {
                String::new()
            }
        };
        let msg = format!("登录成功{detail}{note}");
        // 任务声明 success_condition → 信任 Worker 的变量真值判定，跳过网络检测兜底
        // （对齐原项目 v4.2.3 login_attempt 的 has_explicit_condition 分支）
        if self.has_explicit_success_condition() {
            debug!("任务声明 success_condition，跳过登录后网络检测");
            self.emit(self.make_result(LoginTerminal::Success, msg, session_start, *attempts_used))
                .await;
            return false;
        }
        // 步骤全部成功后做真实网络验证：避免 Worker 假成功（步骤未抛异常
        // 但页面实际未登录成功）被误报。参考老实现 _check_success：
        // 等待 post_login_delay 让认证生效 → check_once → 仅 Online 才算真成功。
        // 延迟 + 探测消耗会话总超时预算：传入剩余预算，不得叠加在
        // login_timeout 之外拖延整场登录
        let remaining = self
            .params
            .login_timeout
            .saturating_sub(session_start.elapsed());
        match self.verify_network_after_login(remaining).await {
            NetworkVerification::Online => {
                self.emit(self.make_result(
                    LoginTerminal::Success,
                    msg,
                    session_start,
                    *attempts_used,
                ))
                .await;
                return false;
            }
            NetworkVerification::Cancelled => {
                self.finish_with_cancelled(session_start, *attempts_used, None)
                    .await;
                return false;
            }
            NetworkVerification::RetryableFailure => {
                self.last_outcome = Some(Outcome::NetworkError);
            }
        }
        // 网络验证未通过：不直接判终态失败，而是走可重试路径。
        // 理由：网络探测可能因瞬时波动误判，重试一次登录比直接判死更稳妥。
        // 策略与 Worker 返回 NetworkError 时一致（classify → Retry）。
        warn!(
            attempt = *attempts_used + 1,
            "Worker 返回 Success 但网络验证未通过，转入重试"
        );
        // 构造一个 NetworkError 的 structured 以复用下方 Retry 分支
        let retry_structured = StructuredResult {
            outcome: Outcome::NetworkError,
            message: "网络验证未通过".into(),
            data: structured.data.clone(),
            screenshot_url: structured.screenshot_url.clone(),
            duration_ms: structured.duration_ms,
        };
        self.try_retry(
            &retry_structured,
            attempts_used,
            session_start,
            false,
            finished_owner_cancel_id,
        )
        .await
    }

    /// 尝试重试：更新状态、按需回收 Worker、等待重试间隔。
    ///
    /// `force_recycle` 控制重试前是否强制回收 Worker：Worker 自身报错的路径
    /// 按 `should_force_recycle(outcome)` 传入保持既有语义；「步骤成功但网络
    /// 验证未过」路径传 `false`——步骤全绿证明 Worker/浏览器健康，验证失败
    /// 多为探测瞬时误判，回收整棵进程树代价远超必要，还可能在门户留下
    /// 重复登录痕迹。
    ///
    /// 第 `attempt` 次重试（从 1 计）的退避等待：基准间隔 × 2^(attempt-1)。
    /// 默认 5s 配置下依次等待 5/10/20s。位移上限 8（×256）+ 饱和乘法，
    /// 防御异常大的 max_retries/interval 组合导致溢出或过久阻塞。
    fn retry_backoff_delay(base: Duration, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.saturating_sub(1).min(8);
        base.max(Duration::from_secs(1)).saturating_mul(factor)
    }

    /// 返回 `true` 表示继续下一轮循环，`false` 表示已 emit 终态结果（重试耗尽或被取消）。
    ///
    /// `owner_cancel_id` 为本会话刚结束那轮 attempt 的 cancel_id，用于向 Bridge
    /// 证明会话槽位归属（`None` 表示本会话未持有槽位/为直连渠道）。
    async fn try_retry(
        &self,
        structured: &StructuredResult,
        attempts_used: &mut u32,
        session_start: Instant,
        force_recycle: bool,
        owner_cancel_id: Option<&str>,
    ) -> bool {
        if *attempts_used >= self.params.max_retries {
            self.finish_with_failure(
                session_start,
                *attempts_used,
                format!(
                    "重试耗尽（共 {} 次尝试）: {}{}{}",
                    self.params.max_retries.saturating_add(1),
                    structured.message,
                    dialog_note(&structured.data),
                    self.persistent_session_hint(structured.outcome)
                ),
            )
            .await;
            return false;
        }
        *attempts_used += 1;
        // 指数退避：第 n 次重试等待基准间隔 × 2^(n-1)，给门户/网络恢复留递增窗口，
        // 避免瞬时抖动下固定短间隔白耗重试预算
        let backoff = Self::retry_backoff_delay(self.params.retry_interval, *attempts_used);
        self.deps.status_manager.merge(PartialSnapshot::Login {
            status: LoginStatus::Running,
            source: Some(self.params.source),
            message: Some(format!(
                "重试中 {attempts_used}/{}（{}s 后）",
                self.params.max_retries,
                backoff.as_secs()
            )),
            retry_count: *attempts_used,
        });
        if force_recycle {
            // 强制回收 Worker：直接 kill 当前子进程并标记 Error，
            // 下一次 bridge.execute() 内部的 ensure_worker 会自动重新 spawn。
            // 同步 await（而非 spawn）确保 kill 在重试间隔之前完成，避免下一轮
            // ensure_worker 复用即将被 kill 的 Worker（force_recycle 与 retry 竞态）。
            //
            // 归属感知（P1-4）：本会话刚结束一轮 attempt（槽位若仍被占，占用者
            // 只可能是**另一条路径**——如定时浏览器任务）。无条件强杀会以
            // `Cancelled` 摧毁对方在途执行（`force_recycle` 先 `trigger_all` 取消
            // 会话区 token，用户可见「请求已取消」而非"Worker 崩溃"），症状是
            // "任务自己失败"，根因却在登录重试上。故先校验槽位是否空闲或仍属
            // 本会话，否则跳过回收：本轮登录已失败，不回收最多让下一轮重新 spawn 一次。
            warn!("登录结果 {:?} 触发 Worker 强制回收", structured.outcome);
            let recycled = self
                .deps
                .bridge
                .force_recycle_if_unowned(owner_cancel_id)
                .await;
            if !recycled {
                warn!(
                    "Worker 会话槽位被其他请求占用，跳过强制回收（避免摧毁在途任务）；\
                     下一轮登录将由 ensure_worker 重新建立上下文"
                );
            }
        }
        let ct = self.cancel_token.clone();
        tokio::select! {
            biased;
            _ = ct.cancelled() => {
                self.emit(self.make_cancelled_result(session_start, *attempts_used))
                    .await;
                false
            }
            _ = self.shutdown_token.cancelled() => {
                *recover_lock(self.cancel_reason.as_ref()) = Some("应用关闭".to_string());
                self.emit(self.make_cancelled_result(session_start, *attempts_used))
                    .await;
                false
            }
            _ = sleep(backoff) => true,
        }
    }

    /// 任务是否声明了 `success_condition`（以变量真值判定登录成功）
    ///
    /// 声明时 Worker 已用变量真值判定过成功，登录路径应跳过网络检测兜底
    /// （对齐原项目 v4.2.3 `login_attempt` 的 `has_explicit_condition` 分支）。
    fn has_explicit_success_condition(&self) -> bool {
        self.params.plan.has_explicit_success_condition()
    }

    /// 持久会话失效提示（T3-4 最小面）：持久模式回放失败（`assertion_failed`）
    /// 走到终态失败时，在失败短语后追加引导用户重录的提示。
    ///
    /// 取舍：不新增 schema 字段——失效态可由「持久模式计划 + assertion_failed
    /// 终态」推断，前端与历史按失败消息文本呈现即可；`assertion_failed` 在
    /// classify 中可重试，只有重试耗尽的终态才落到本提示，回放过程中的
    /// 中间失败不打扰。
    fn persistent_session_hint(&self, outcome: Outcome) -> &'static str {
        if matches!(self.params.plan, LoginAttemptPlan::BrowserPersistent(_))
            && outcome == Outcome::AssertionFailed
        {
            "；持久会话疑似失效，请重新打开浏览器配置"
        } else {
            ""
        }
    }

    /// 登录后真实网络验证：等待 post_login_delay 让认证生效，再调用 MonitorService 做一次完整探测。
    ///
    /// 仅当探测结果为 [`NetworkStatus::Online`] 时返回 [`NetworkVerification::Online`]；
    /// 取消独立返回 `Cancelled`，其余结果返回 `RetryableFailure`。
    ///
    /// `remaining` 为会话总超时（`login_timeout`）的剩余预算：延迟 + 探测整体经
    /// `tokio::time::timeout` 收敛在该预算内，不得叠加在 `login_timeout` 之外
    /// （否则慢探测会把整场登录拖过总超时）；预算已耗尽时跳过验证并按未通过
    /// 处理，走既有失败/重试语义。
    ///
    /// 与老实现 `BrowserTaskRunner._network_detection_check` 等价：防止 Worker 步骤
    /// 全部成功但页面实际未登录成功（如填入字面量 `{{USERNAME}}` 却没点登录按钮）。
    async fn verify_network_after_login(&self, remaining: Duration) -> NetworkVerification {
        // 预算耗尽：跳过验证（视为未通过），由调用方走失败/重试路径
        if remaining.is_zero() {
            warn!("登录总超时预算已耗尽，跳过登录后网络验证");
            return NetworkVerification::RetryableFailure;
        }
        let monitor = &self.deps.monitor;
        // 登录后等待 portal 生效的延迟（可配置，默认 5s）：钳制上限 60s 对齐
        // 前端输入，防手改 settings.json 填大值导致登录后无限干等
        let delay = self
            .deps
            .config_service
            .runtime()
            .load()
            .monitor
            .post_login_delay
            .min(60);
        // 延迟与探测合并为一个被预算超时包裹的 future；取消/shutdown 由外层
        // select 独立监听（立即返回取消终态，不等预算或探测耗时到期）
        let verify = async {
            tokio::time::sleep(Duration::from_secs(delay as u64)).await;
            monitor.verify_internet_via(&self.params.network).await
        };
        let report = tokio::select! {
            biased;
            _ = self.cancel_token.cancelled() => {
                info!("登录后网络验证已取消");
                return NetworkVerification::Cancelled;
            }
            _ = self.shutdown_token.cancelled() => {
                *recover_lock(self.cancel_reason.as_ref()) = Some("应用关闭".to_string());
                info!("登录后网络验证已取消（应用关闭）");
                return NetworkVerification::Cancelled;
            }
            r = tokio::time::timeout(remaining, verify) => match r {
                Ok(inner) => inner,
                // 剩余预算内未完成（延迟 + 探测超预算）：按未通过处理，
                // 走既有失败/重试语义，不得在 login_timeout 之外继续等待
                Err(_) => {
                    warn!(
                        remaining = ?remaining,
                        "登录后网络验证超出剩余会话预算，按未通过处理"
                    );
                    return NetworkVerification::RetryableFailure;
                }
            },
        };
        match report {
            Ok(report) => {
                use crate::status::NetworkStatus;
                let ok = matches!(report.assessment.status, NetworkStatus::Online);
                if ok {
                    info!("登录后网络验证通过：Online");
                } else {
                    warn!(
                        status = ?report.assessment.status,
                        reason = ?report.assessment.reason,
                        tcp = ?report.evidence.tcp,
                        http = ?report.evidence.http,
                        url = ?report.evidence.url,
                        "登录后网络验证未通过"
                    );
                }
                if ok {
                    NetworkVerification::Online
                } else {
                    NetworkVerification::RetryableFailure
                }
            }
            Err(e) => {
                warn!("登录后网络验证异常: {e}");
                NetworkVerification::RetryableFailure
            }
        }
    }

    /// 构建终态结果（携带耗时与尝试次数）
    fn make_result(
        &self,
        terminal: LoginTerminal,
        message: String,
        start: Instant,
        attempts_used: u32,
    ) -> LoginResult {
        LoginResult {
            network_retryable: terminal == LoginTerminal::Failed
                && matches!(
                    self.last_outcome,
                    Some(Outcome::NetworkError | Outcome::NavigationTimeout)
                ),
            terminal,
            message,
            source: self.params.source,
            duration: start.elapsed(),
            attempts: attempts_used + 1,
        }
    }

    /// 构建取消终态结果（消息取自 `cancel_reason`，缺省为“已取消”）
    fn make_cancelled_result(&self, start: Instant, attempts_used: u32) -> LoginResult {
        let reason = recover_lock(self.cancel_reason.as_ref())
            .clone()
            .unwrap_or_else(|| "已取消".to_string());
        LoginResult {
            network_retryable: false,
            terminal: LoginTerminal::Cancelled,
            message: reason,
            source: self.params.source,
            duration: start.elapsed(),
            attempts: attempts_used + 1,
        }
    }

    /// 终态收尾：写入共享结果槽 → 广播状态 → 记录历史 → 更新内部状态
    async fn emit(&self, result: LoginResult) {
        self.result_slot.set_result(result.clone());

        let history = match result.terminal {
            LoginTerminal::Success => HistoryResult::Success,
            LoginTerminal::Failed => HistoryResult::Failed,
            LoginTerminal::Cancelled => HistoryResult::Cancelled,
        };

        // 统计登录终态指标
        if let Some(m) = &self.deps.metrics {
            match history {
                HistoryResult::Success => m.inc_login_success(),
                HistoryResult::Failed => m.inc_login_failure(),
                HistoryResult::Cancelled => m.inc_login_cancel(),
            }
        }

        let (status, msg) = match history {
            HistoryResult::Success => (LoginStatus::Success, "登录成功".to_string()),
            HistoryResult::Cancelled => (LoginStatus::Cancelled, result.message.clone()),
            HistoryResult::Failed => (LoginStatus::Failed, result.message.clone()),
        };
        self.deps.status_manager.merge(PartialSnapshot::Login {
            status,
            source: None,
            message: Some(msg),
            retry_count: result.attempts.saturating_sub(1),
        });

        // 终态事件发布（事件源接入，在既有状态发射旁追加）：emit 是结果槽写入
        // （终态首次发射、供所有等待方读回）的单一位置，成功发
        // [`AppEvent::LoginSucceeded`]、最终失败发 [`AppEvent::LoginFailed`]；
        // 取消终态不发（既非成功也非最终失败）。重试预算内的中间失败在
        // try_retry 处转入下一轮，不经过终态发射，天然不发。载荷为白名单摘要
        // （profile_id / channel / reason / attempt），reason 取失败消息首行截断
        // （口径同 tasks 域 brief_reason），不含凭据/token/URL 查询串。
        // 发布为同步非阻塞（broadcast 无订阅者时 send 的 Err 被忽略）。
        // 去重复用（Reuse）读回的是此前已发射过的旧结果、不再经过 emit，
        // 天然不会双发。
        let summary = LoginSummary {
            profile_id: self.params.profile_id.clone(),
            channel: login_channel_code(self.params.plan.channel()).to_string(),
            reason: None,
            attempt: Some(result.attempts),
        };
        match result.terminal {
            LoginTerminal::Success => {
                self.deps.events.publish(AppEvent::LoginSucceeded(summary));
            }
            LoginTerminal::Failed => {
                self.deps
                    .events
                    .publish(AppEvent::LoginFailed(LoginSummary {
                        reason: Some(crate::tasks::executor::brief_reason(
                            &result.message,
                            crate::tasks::executor::TASK_EVENT_REASON_MAX_CHARS,
                        )),
                        ..summary
                    }));
            }
            // 取消终态：不发布（通知与事件流口径均只覆盖成功/最终失败）
            LoginTerminal::Cancelled => {}
        }

        let entry = LoginHistoryEntry {
            timestamp: chrono::Local::now(),
            source: result.source,
            channel: Some(self.params.plan.channel()),
            profile_id: self.params.profile_id.clone(),
            result: history,
            message: result.message.clone(),
            duration_secs: result.duration.as_secs_f64(),
        };
        if let Err(e) = self.deps.history_service.record(&entry).await {
            warn!("登录历史写入失败: {e}");
        }

        // 会话终态后回收浏览器资源（Worker 进程保留）。**仅普通浏览器计划执行**：
        // 直连/脚本会话不经 Bridge 驱动 Worker，没有自己的浏览器可关——若不按
        // 渠道门控，脚本会话终态会把共享 Worker 上其他在途浏览器任务（如定时
        // 任务）的页面关掉；持久自动登录的持久上下文由 Worker 在
        // profile_auto_login 命令结束即关闭（README 生命周期），终态再发
        // close_browser 只是无谓的 Worker 往返，且 keep_alive 语义不应挂到
        // 持久上下文上，故一并跳过。默认全量关闭浏览器（会话内重试复用同一
        // 浏览器，终态即关闭）；worker.keep_alive 启用时改会话级释放，登录成功
        // 更是整页保留登录状态（门户页 JS 心跳不中断）。进程已被回收
        // （force_recycle / 空闲超时）时跳过，避免仅为关浏览器而重新 spawn 一个 Worker。
        {
            let b = &self.deps.bridge;
            if self.params.plan.uses_bridge()
                && !self.params.plan.is_persistent_auto_login()
                && b.has_live_worker()
            {
                // preserve_state 仅在 keep_alive 且登录成功时为真，非成功终态
                // 走会话级释放、默认配置走全量关闭，三档语义由 Worker 侧实现
                let preserve = result.is_success()
                    && self.deps.config_service.runtime().load().worker.keep_alive;
                if let Err(e) = b
                    .execute_with_timeout(
                        "close_browser",
                        json!({ "preserve_state": preserve }),
                        CLOSE_BROWSER_TIMEOUT,
                    )
                    .await
                {
                    warn!("登录终态关闭浏览器失败（忽略）: {e}");
                }
            }
        }
        // 失败升 warn 并附可能原因提示（成功/取消保持 info）：失败与成功同级别时
        // 在日志里不显眼，排查困难；自动来源的失败另有 handle_login_result 的
        // 结构化 warn（含连续失败计数），此处覆盖 Manual/Browser 全部来源
        if history == HistoryResult::Failed {
            warn!(
                source = ?result.source,
                attempts = result.attempts,
                "登录失败: {}（{}）",
                result.message,
                login_failure_hint(&result.message)
            );
        } else {
            info!(
                source = ?result.source,
                success = result.is_success(),
                "登录会话结束: {}",
                result.message
            );
        }
    }

    /// 以「已取消」终态收尾：写入取消原因（`None` 保留既有原因）→ emit 取消结果 → 写历史。
    ///
    /// 收敛 `run` 内多处 `emit(make_cancelled_result)+return` 样板（C4）。
    /// 注：不做 `-> !`——真实发散需要 panic/挂起，会误伤正常终止路径。
    async fn finish_with_cancelled(
        &self,
        session_start: Instant,
        attempts_used: u32,
        reason: Option<String>,
    ) {
        if let Some(reason) = reason {
            *recover_lock(self.cancel_reason.as_ref()) = Some(reason);
        }
        self.emit(self.make_cancelled_result(session_start, attempts_used))
            .await;
    }

    /// 以「失败」终态收尾：emit 失败结果 → 写历史（收敛 `run`/`try_retry` 内样板，C4）。
    async fn finish_with_failure(
        &self,
        session_start: Instant,
        attempts_used: u32,
        message: String,
    ) {
        self.emit(self.make_result(LoginTerminal::Failed, message, session_start, attempts_used))
            .await;
    }

    /// 将 `IpcResponse` 解析为 [`StructuredResult`]
    ///
    /// 成功时从 `result.data` 反序列化；失败信封（`success=false`，Worker 端
    /// WorkerError 路径见 worker_main._structured_result）同样在 `result.data`
    /// 携带可分类 outcome——从中恢复，供 classify 做重试/终态判定；`data`
    /// 缺失或宣称 Success（矛盾信封）时回退 `UnknownError` 兜底。
    fn parse_ipc_response(resp: IpcResponse) -> StructuredResult {
        if resp.result.success {
            match serde_json::from_value::<StructuredResult>(resp.result.data.clone()) {
                Ok(s) => s,
                Err(e) => {
                    warn!("结构化结果解析失败: {e}");
                    StructuredResult {
                        outcome: Outcome::UnknownError,
                        message: format!("结果解析失败: {e}"),
                        data: Value::Null,
                        screenshot_url: None,
                        duration_ms: 0,
                    }
                }
            }
        } else {
            // 失败信封仍携带可分类 outcome（Worker 端 WorkerError 路径，见
            // worker_main._structured_result）：从中恢复 outcome 供 classify 做重试/终态
            // 判定。仅当 data 可解析且未宣称 Success 时采信（防御矛盾信封），
            // 其余回退 UnknownError，与旧行为一致。
            let recovered = serde_json::from_value::<StructuredResult>(resp.result.data.clone())
                .ok()
                .filter(|s| s.outcome != Outcome::Success);
            // 回退消息源：error 须非空串才采信——空串错误（畸形/部分写入的信封）
            // 会让空 message 直入历史与状态广播（属性测试 S1 钉住该不变量）
            let fallback_message = || {
                resp.result
                    .error
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Worker 返回失败".into())
            };
            match recovered {
                Some(mut s) => {
                    if s.message.is_empty() {
                        s.message = fallback_message();
                    }
                    s
                }
                None => StructuredResult {
                    outcome: Outcome::UnknownError,
                    message: fallback_message(),
                    data: Value::Null,
                    screenshot_url: None,
                    duration_ms: 0,
                },
            }
        }
    }
}

/// 登录失败可能原因提示：按失败消息关键词轻量归类，随 warn 日志透出。
///
/// 失败消息来自任务步骤（导航/选择器/断言超时）、门户拒绝、环境未就绪等多源，
/// 没有统一错误码，只能按关键词归类；命中不到就走通用兜底。
/// 分支顺序即优先级：环境/Profile 属于本地配置问题，先于门户行为判定。
fn login_failure_hint(message: &str) -> &'static str {
    if message.contains("未就绪")
        || message.contains("初始化")
        || message.contains("Worker 未安装")
        || message.contains("浏览器")
    {
        "可能原因：Python 环境/浏览器未就绪，可先在设置页初始化 Python 环境或检查浏览器配置"
    } else if message.contains("Profile") || message.contains("加载失败") {
        "可能原因：Profile 加载失败，请检查该 Profile 的账号密码与认证地址配置"
    } else if message.contains("验证码") || message.contains("OCR") || message.contains("captcha")
    {
        "可能原因：门户出现验证码或 OCR 异常，自动登录受限，可改用手动登录"
    } else if message.contains("密码")
        || message.contains("账号")
        || message.contains("用户名")
        || message.contains("认证失败")
    {
        "可能原因：账号或密码有误、门户要求验证码，请核对 Profile 配置"
    } else if message.contains("选择器") || message.contains("selector") || message.contains("元素")
    {
        "可能原因：认证页结构可能已变化导致步骤选择器失效，需更新登录任务步骤"
    } else if message.contains("超时") || message.contains("timeout") || message.contains("Timeout")
    {
        "可能原因：门户响应超时，弱网或认证页加载异常，可重试"
    } else {
        "可能原因：门户状态异常或网络受限，可查看完整日志或手动打开认证页确认"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::Outcome;
    use std::sync::atomic::Ordering;

    // ============ classify 纯函数测试 ============

    /// 失败原因归类：环境/Profile/验证码/凭据/选择器/超时/兜底各走对应提示。
    #[test]
    fn test_login_failure_hint_branches() {
        assert!(
            login_failure_hint("浏览器能力未就绪，自动初始化失败").contains("初始化 Python 环境")
        );
        assert!(login_failure_hint("指定 Profile p1 加载失败，已拒绝登录").contains("Profile"));
        assert!(login_failure_hint("OCR 识别超时（>60s）").contains("验证码"));
        assert!(login_failure_hint("门户返回：用户名或密码错误").contains("账号或密码"));
        assert!(login_failure_hint("操作超时: 30000ms").contains("门户响应超时"));
        assert!(login_failure_hint("输入元素操作超时: #password").contains("选择器"));
        let fallback = login_failure_hint("boom");
        assert!(fallback.contains("门户状态异常"));
    }

    #[test]
    fn test_classify_success_is_terminal_success() {
        assert_eq!(
            classify(Outcome::Success),
            ResultAction::Terminal(LoginTerminal::Success)
        );
    }

    #[test]
    fn test_classify_cancelled_is_terminal_cancelled() {
        assert_eq!(
            classify(Outcome::Cancelled),
            ResultAction::Terminal(LoginTerminal::Cancelled)
        );
    }

    #[test]
    fn test_classify_invalid_credential_is_terminal_failed() {
        // 凭证无效属于终态失败，重试无意义
        assert_eq!(
            classify(Outcome::InvalidCredential),
            ResultAction::Terminal(LoginTerminal::Failed)
        );
    }

    #[test]
    fn test_classify_manual_required_is_terminal_failed() {
        assert_eq!(
            classify(Outcome::ManualRequired),
            ResultAction::Terminal(LoginTerminal::Failed)
        );
    }

    #[test]
    fn test_classify_captcha_failed_is_retry() {
        // 验证码失败（OCR 误识别）为瞬时性失败，重试整个流程（受 max_retries 预算约束）
        assert_eq!(classify(Outcome::CaptchaFailed), ResultAction::Retry);
    }

    #[test]
    fn test_classify_unknown_error_is_terminal_failed() {
        assert_eq!(
            classify(Outcome::UnknownError),
            ResultAction::Terminal(LoginTerminal::Failed)
        );
    }

    #[test]
    fn test_classify_navigation_timeout_is_retry() {
        assert_eq!(classify(Outcome::NavigationTimeout), ResultAction::Retry);
    }

    #[test]
    fn test_classify_selector_failed_is_retry() {
        assert_eq!(classify(Outcome::SelectorFailed), ResultAction::Retry);
    }

    #[test]
    fn test_classify_assertion_failed_is_retry() {
        // P11：assert_text 超时/不匹配归为 AssertionFailed，可重试、不回收 Worker
        assert_eq!(classify(Outcome::AssertionFailed), ResultAction::Retry);
    }

    #[test]
    fn test_classify_network_error_is_retry() {
        // NetworkError 可重试（与 should_force_recycle 配合：重试前先回收 Worker）
        assert_eq!(classify(Outcome::NetworkError), ResultAction::Retry);
    }

    // ============ should_force_recycle 纯函数测试（BUG-1 重点） ============

    #[test]
    fn test_should_force_recycle_network_error() {
        // BUG-1 修复点：NetworkError 必须强制回收，否则已损坏上下文被复用导致持续失败
        assert!(should_force_recycle(Outcome::NetworkError));
    }

    #[test]
    fn test_should_force_recycle_unknown_error_not_recycled() {
        // UnknownError 走终态失败（classify 在 try_retry 之前 return），
        // 不进入重试/回收路径；此处断言其即使被误传也不触发回收
        assert!(!should_force_recycle(Outcome::UnknownError));
    }

    #[test]
    fn test_should_force_recycle_preserves_context_for_navigation_timeout() {
        // 导航超时上下文未损坏，复用即可，不回收
        assert!(!should_force_recycle(Outcome::NavigationTimeout));
    }

    #[test]
    fn test_should_force_recycle_preserves_context_for_selector_failed() {
        assert!(!should_force_recycle(Outcome::SelectorFailed));
    }

    #[test]
    fn test_should_force_recycle_does_not_recycle_success() {
        assert!(!should_force_recycle(Outcome::Success));
    }

    #[test]
    fn test_should_force_recycle_does_not_recycle_cancelled() {
        assert!(!should_force_recycle(Outcome::Cancelled));
    }

    #[test]
    fn test_should_force_recycle_does_not_recycle_invalid_credential() {
        // 凭证无效属业务终态，浏览器上下文正常，无需回收
        assert!(!should_force_recycle(Outcome::InvalidCredential));
    }

    #[test]
    fn test_should_force_recycle_does_not_recycle_captcha_failed() {
        assert!(!should_force_recycle(Outcome::CaptchaFailed));
    }

    // ============ dialog_note 纯函数测试 ============

    #[test]
    fn test_dialog_note_empty_when_no_dialogs() {
        // 无 dialogs 字段 / 空数组 → 空串
        assert_eq!(dialog_note(&Value::Null), "");
        assert_eq!(dialog_note(&serde_json::json!({})), "");
        assert_eq!(dialog_note(&serde_json::json!({ "dialogs": [] })), "");
    }

    #[test]
    fn test_dialog_note_joins_messages() {
        let data =
            serde_json::json!({ "dialogs": ["账号或密码错误！", "请先阅读并同意免责声明条款"] });
        assert_eq!(
            dialog_note(&data),
            "；页面提示: 账号或密码错误！ / 请先阅读并同意免责声明条款"
        );
    }

    #[test]
    fn test_dialog_note_ignores_non_string_entries() {
        let data = serde_json::json!({ "dialogs": [1, true, "登录成功！"] });
        assert_eq!(dialog_note(&data), "；页面提示: 登录成功！");
    }

    // ============ A-2：BridgeApi trait 化后的状态机单测 ============

    use crate::bridge::BridgeError;
    use crate::login::session::{SessionDeps, SessionParams};
    use std::collections::VecDeque;

    /// 脚本化 mock Bridge：按序回放预设响应，记录调用次数
    struct ScriptedBridge {
        /// 队列元素：(success, outcome_value, message)
        script: std::sync::Mutex<VecDeque<(bool, &'static str, &'static str)>>,
        /// 已调用的方法名（诊断）
        methods: std::sync::Mutex<Vec<String>>,
        calls: std::sync::atomic::AtomicU32,
        recycled: std::sync::atomic::AtomicU32,
        /// has_live_worker 的可控返回值：现有用例恒 false，终态不关浏览器；
        /// 终态关浏览器门控测试据此模拟「共享 Worker 上有存活 Worker」
        live: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl crate::bridge::BridgeApi for ScriptedBridge {
        async fn execute(
            &self,
            _method: &str,
            _params: Value,
        ) -> Result<crate::bridge::IpcResponse, BridgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.methods.lock().unwrap().push(_method.to_string());
            let (success, outcome, message) = match self.script.lock().unwrap().pop_front() {
                Some(v) => v,
                None => panic!("脚本耗尽，已调用: {:?}", self.methods.lock().unwrap()),
            };
            Ok(crate::bridge::IpcResponse {
                id: 1,
                result: crate::bridge::IpcResult {
                    success,
                    data: serde_json::json!({
                        "outcome": outcome,
                        "message": message,
                        "duration_ms": 1,
                    }),
                    error: None,
                },
            })
        }

        fn cancel(&self, _cancel_id: &str) {}

        async fn execute_with_timeout(
            &self,
            method: &str,
            params: Value,
            _timeout: Duration,
        ) -> Result<crate::bridge::IpcResponse, BridgeError> {
            self.execute(method, params).await
        }

        async fn force_recycle(&self) {
            self.recycled.fetch_add(1, Ordering::SeqCst);
        }

        fn has_live_worker(&self) -> bool {
            self.live.load(Ordering::SeqCst)
        }

        async fn recycle_if_running(&self) {}

        async fn shutdown(&self) {}
    }

    /// 恒定返回指定 Bridge 错误的 mock：验证 bridge.execute Err 分流的
    /// 终态/重试语义（BridgeError 含 io::Error 变体不可 Clone，存工厂按次构造）
    struct FailingBridge {
        make_error: Box<dyn Fn() -> BridgeError + Send + Sync>,
        calls: std::sync::atomic::AtomicU32,
        recycled: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl crate::bridge::BridgeApi for FailingBridge {
        async fn execute(
            &self,
            _method: &str,
            _params: Value,
        ) -> Result<crate::bridge::IpcResponse, BridgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err((self.make_error)())
        }

        fn cancel(&self, _cancel_id: &str) {}

        async fn execute_with_timeout(
            &self,
            method: &str,
            params: Value,
            _timeout: Duration,
        ) -> Result<crate::bridge::IpcResponse, BridgeError> {
            self.execute(method, params).await
        }

        async fn force_recycle(&self) {
            self.recycled.fetch_add(1, Ordering::SeqCst);
        }

        fn has_live_worker(&self) -> bool {
            false
        }

        async fn recycle_if_running(&self) {}

        async fn shutdown(&self) {}
    }

    /// 构造带 mock Bridge 的会话依赖集（真实 ConfigService/StatusManager/History；
    /// 事件发往孤立总线——不关心事件发布的既有用例沿用本入口）
    async fn make_deps(bridge: Arc<dyn crate::bridge::BridgeApi>) -> SessionDeps {
        make_deps_with_events(bridge, Arc::new(crate::events::EventBus::new())).await
    }

    /// 同 [`make_deps`]，但注入调用方提供的事件总线（终态事件发布断言用）
    async fn make_deps_with_events(
        bridge: Arc<dyn crate::bridge::BridgeApi>,
        events: Arc<crate::events::EventBus>,
    ) -> SessionDeps {
        let dir = tempfile::TempDir::new().unwrap();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
        let config = ConfigService::new(dir.path().to_path_buf(), reload_tx)
            .await
            .expect("ConfigService 构造失败");
        SessionDeps {
            bridge,
            monitor: Arc::new(
                crate::monitor::MonitorService::new(
                    config.clone(),
                    crate::network::detect::create_detector(),
                    Some(crate::utils::metrics::Metrics::new()),
                )
                .expect("MonitorService 构造失败"),
            ),
            config_service: config,
            status_manager: Arc::new(StatusManager::new()),
            history_service: Arc::new(LoginHistoryService::new(dir.path())),
            script_runner: Arc::new(StubScriptRunner),
            events,
            metrics: None,
        }
    }

    /// 脚本执行替身：会话状态机的单测不需要真起子进程（这条路径由
    /// `script_login` 的单测与真机登录覆盖），只需"能拿到一个结果"。
    struct StubScriptRunner;

    #[async_trait::async_trait]
    impl crate::tasks::ScriptRunnerApi for StubScriptRunner {
        async fn run_script_with_env(
            &self,
            _cfg: &crate::tasks::ScriptTaskConfig,
            _extra_env: Vec<(String, String)>,
        ) -> Result<crate::tasks::TaskResult, crate::tasks::TaskError> {
            Err(crate::tasks::TaskError::ScriptNotFound(
                "单测替身不执行脚本".to_string(),
            ))
        }
    }

    #[test]
    fn test_retry_backoff_delay_doubles_per_attempt() {
        let base = Duration::from_secs(5);
        assert_eq!(
            LoginSession::retry_backoff_delay(base, 1),
            Duration::from_secs(5)
        );
        assert_eq!(
            LoginSession::retry_backoff_delay(base, 2),
            Duration::from_secs(10)
        );
        assert_eq!(
            LoginSession::retry_backoff_delay(base, 3),
            Duration::from_secs(20)
        );
        // 0 秒配置抬升到下限 1s
        assert_eq!(
            LoginSession::retry_backoff_delay(Duration::from_secs(0), 1),
            Duration::from_secs(1)
        );
        // 位移上限 8（×256）+ 饱和乘法：异常大 attempt 不 panic、不超界
        assert_eq!(
            LoginSession::retry_backoff_delay(Duration::from_secs(300), 40),
            Duration::from_secs(300 * 256)
        );
    }

    fn make_params() -> SessionParams {
        SessionParams {
            source: LoginSource::Auto,
            task_id: None,
            max_retries: 2,
            retry_interval: Duration::from_secs(0),
            login_timeout: Duration::from_secs(30),
            profile_id: "default".to_string(),
            network: Default::default(),
            plan: LoginAttemptPlan::Browser(serde_json::json!({})),
        }
    }

    #[test]
    fn test_attempt_plan_success_condition_only_applies_to_browser() {
        let config = serde_json::json!({
            "task_config": { "success_condition": "logged_in" }
        });
        assert!(LoginAttemptPlan::Browser(config).has_explicit_success_condition());
        assert!(
            !LoginAttemptPlan::Script(Box::new(script_plan())).has_explicit_success_condition()
        );
        assert!(!LoginAttemptPlan::Script(Box::new(script_plan())).uses_bridge());
    }

    /// 可重试失败持续到预算耗尽：验证重试次数与终态 Failed
    ///
    /// 注：Success 终态依赖登录后真实网络验证（MonitorService 探测 Online），
    /// 测试环境无法稳定通过，故以「可重试失败耗尽预算」路径覆盖 Retry 分支。
    #[tokio::test(start_paused = true)]
    async fn test_retry_exhaustion_via_mock_bridge() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (true, "selector_failed", "按钮未找到"),
                (true, "selector_failed", "按钮未找到"),
                (true, "selector_failed", "按钮未找到"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        let mut result_rx = result_rx;
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            3,
            "max_retries=2 应共尝试 3 次"
        );
        assert_eq!(
            bridge.recycled.load(Ordering::SeqCst),
            0,
            "SelectorFailed 不应触发 Worker 回收"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 3);
        assert!(result.message.contains("重试耗尽"));
    }

    /// UnknownError 终态：不重试、不回收（G1 语义改齐的回归验证）
    #[tokio::test(start_paused = true)]
    async fn test_unknown_error_is_terminal_without_recycle() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (true, "unknown_error", "意外错误"),
                (true, "success", "不应到达"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        let mut result_rx = result_rx;
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            1,
            "UnknownError 必须立即终态，不得重试"
        );
        assert_eq!(bridge.recycled.load(Ordering::SeqCst), 0);
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
    }

    /// P1-3：Worker 成功后的延迟验证若被取消，即使没有剩余重试预算，也必须
    /// 直接返回 Cancelled，不能降格为“网络验证失败 → 重试耗尽”。
    #[tokio::test(start_paused = true)]
    async fn test_post_login_verification_cancel_is_terminal_cancelled() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![(true, "success", "步骤执行成功")])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let cancel = CancellationToken::new();
        let mut params = make_params();
        params.max_retries = 0;
        let session = LoginSession::new(
            params,
            cancel.clone(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );

        let task = tokio::spawn(session.run());
        while bridge.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        cancel.cancel();
        task.await.unwrap();

        let result = result_rx.borrow_and_update().clone().expect("应有取消终态");
        assert_eq!(result.terminal, LoginTerminal::Cancelled);
        assert!(!result.message.contains("重试耗尽"));
        assert_eq!(result.attempts, 1);
    }

    // ============ 终态关浏览器的渠道门控（P1） ============

    /// 构造脚本登录计划（状态机单测不真跑脚本：StubScriptRunner 恒 Err →
    /// UnknownError 终态失败，正好触发 emit 收尾路径）
    fn script_plan() -> crate::login::script_login::ScriptLoginPlan {
        use crate::tasks::models::{CommonFields, ScriptTaskConfig};
        crate::login::script_login::ScriptLoginPlan {
            task: ScriptTaskConfig {
                common: CommonFields {
                    task_id: "login-script".to_string(),
                    name: "登录脚本".to_string(),
                    description: String::new(),
                },
                script_path: None,
                content: Some("print(1)".to_string()),
                args: Vec::new(),
                work_dir: None,
                timeout: 30,
                binary_path: None,
            },
            extra_env: Vec::new(),
        }
    }

    /// P1：脚本渠道会话终态不得向共享 Worker 发 close_browser——mock
    /// has_live_worker()=true（模拟共享 Worker 上有定时浏览器任务在跑）时，
    /// 脚本会话终态收尾绝不能把别人的浏览器关掉。
    #[tokio::test(start_paused = true)]
    async fn test_script_session_terminal_does_not_close_shared_worker_browser() {
        let bridge = Arc::new(ScriptedBridge {
            // 脚本渠道不经 bridge.execute，队列留空：若门控失效误发
            // close_browser，execute 会因脚本耗尽 panic 而让测试失败
            script: std::sync::Mutex::new(VecDeque::new()),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(true),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.plan = LoginAttemptPlan::Script(Box::new(script_plan()));
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        let methods = bridge.methods.lock().unwrap();
        assert!(
            !methods.iter().any(|m| m == "close_browser"),
            "脚本会话终态不得关闭共享 Worker 上的浏览器: {methods:?}"
        );
    }

    /// 对照组：浏览器渠道会话终态在 has_live_worker()=true 时应照常发
    /// close_browser（门控只挡直连/脚本渠道，不挡浏览器渠道自身的收尾）
    #[tokio::test(start_paused = true)]
    async fn test_browser_session_terminal_closes_browser_when_worker_live() {
        let bridge = Arc::new(ScriptedBridge {
            // 第 1 条：登录尝试本身返回 UnknownError（终态失败）；第 2 条：
            // 终态收尾的 close_browser 响应（emit 忽略其内容）
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (false, "unknown_error", "意外错误"),
                (true, "success", "已关闭"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(true),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        let methods = bridge.methods.lock().unwrap();
        assert!(
            methods.iter().any(|m| m == "close_browser"),
            "浏览器渠道终态应照常关闭浏览器: {methods:?}"
        );
    }

    // ============ bridge.execute Err 分流（WorkerCrashed 可重试） ============

    /// Worker 崩溃（Err(WorkerCrashed)）必须走重试路径而非一次即终态失败：
    /// max_retries=2 时应试满 3 次并以「重试耗尽」终态收尾，错误详情进入
    /// 终态消息；每次重试前经归属感知回收已死 Worker（幂等安全）。
    #[tokio::test(start_paused = true)]
    async fn test_worker_crashed_error_is_retryable_until_exhausted() {
        let bridge = Arc::new(FailingBridge {
            make_error: Box::new(|| BridgeError::WorkerCrashed {
                reason: "管道关闭".to_string(),
            }),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            3,
            "max_retries=2 时 WorkerCrashed 应试满 3 次"
        );
        assert_eq!(
            bridge.recycled.load(Ordering::SeqCst),
            2,
            "每次重试前应回收已死 Worker"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 3);
        assert!(result.message.contains("重试耗尽"), "{}", result.message);
        assert!(
            result.message.contains("Worker"),
            "错误详情应进入终态消息: {}",
            result.message
        );
    }

    /// 对照组：Timeout 等其余 Err 保持终态失败，不消耗重试预算、不回收 Worker
    #[tokio::test(start_paused = true)]
    async fn test_bridge_timeout_error_stays_terminal_without_retry() {
        let bridge = Arc::new(FailingBridge {
            make_error: Box::new(|| BridgeError::Timeout),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            1,
            "Timeout 属终态失败，不得重试"
        );
        assert_eq!(bridge.recycled.load(Ordering::SeqCst), 0);
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 1);
        assert!(
            result.message.contains("Bridge 执行失败"),
            "{}",
            result.message
        );
    }

    /// WorkerBusy（调试会话/重定向检测占用槽位）是瞬时冲突：必须走可重试
    /// 路径消耗重试预算，而非一次即终态失败（此前调试期间自动登录被静默
    /// 压制且历史被 WorkerBusy 刷屏）。
    #[tokio::test(start_paused = true)]
    async fn test_bridge_worker_busy_retries_until_budget_exhausted() {
        let bridge = Arc::new(FailingBridge {
            make_error: Box::new(|| BridgeError::WorkerBusy),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        // max_retries=2：初次 + 2 次重试全部打满（对照 Timeout 用例的 calls==1）
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            3,
            "WorkerBusy 属可重试失败，应消耗完整重试预算"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 3);
        assert!(result.message.contains("Worker 忙"), "{}", result.message);
    }

    // ============ 失败信封 outcome 恢复（C1） ============

    /// C1：失败信封（success=false）的 data.outcome 必须被恢复——首导航超时
    /// 属可重试失败，不得被终态化成一次即放弃。脚本第 2 次尝试返回成功，
    /// 断言会话确实发起了第二次尝试、终态记录 2 次尝试（首次失败未终态化）。
    /// 注：计划声明 success_condition 以跳过登录后真实网络探测（测试环境
    /// 探测结果随环境波动，同 test_retry_exhaustion 注释），终态确定性为 Success。
    #[tokio::test(start_paused = true)]
    async fn test_failure_envelope_navigation_timeout_is_retryable() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (false, "navigation_timeout", "导航超时: #auth-page"),
                (true, "success", "步骤执行成功"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.plan = LoginAttemptPlan::Browser(serde_json::json!({
            "task_config": { "success_condition": "logged_in" }
        }));
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        let mut result_rx = result_rx;
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            2,
            "失败信封中的 navigation_timeout 应恢复为可重试并发起第二次尝试"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(
            result.attempts, 2,
            "首次失败不得终态化，终态应记录 2 次尝试"
        );
        assert_eq!(result.terminal, LoginTerminal::Success);
    }

    /// C1：失败信封恢复出 NetworkError 后，重试前应触发既有的
    /// `should_force_recycle` 强制回收（该路径在失败信封上此前从未生效）。
    /// max_retries=1：首试失败 → 回收 → 重试一次 → 预算耗尽终态失败。
    #[tokio::test(start_paused = true)]
    async fn test_failure_envelope_network_error_triggers_force_recycle() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (false, "network_error", "门户不可达"),
                (false, "network_error", "门户不可达"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = 1;
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        let mut result_rx = result_rx;
        session.run().await;
        assert_eq!(bridge.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            bridge.recycled.load(Ordering::SeqCst),
            1,
            "NetworkError 重试前应强制回收 Worker（仅首试失败后一次）"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 2);
        assert!(result.message.contains("重试耗尽"), "{}", result.message);
        assert!(
            result.message.contains("门户不可达"),
            "失败详情应进入终态消息: {}",
            result.message
        );
    }

    /// C1：失败信封恢复出 manual_required 后仍走 classify 终态失败分支——
    /// 恢复 outcome 不得把本应终态的业务失败改成可重试。
    #[tokio::test(start_paused = true)]
    async fn test_failure_envelope_manual_required_is_terminal_failed() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![(
                false,
                "manual_required",
                "门户要求人工完成验证",
            )])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        let mut result_rx = result_rx;
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            1,
            "manual_required 必须一次即终态，不得重试"
        );
        assert_eq!(bridge.recycled.load(Ordering::SeqCst), 0);
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 1);
        assert!(result.message.contains("门户要求人工完成验证"));
    }

    /// C1 防御：失败信封的 data 宣称 success 属矛盾信封，一律不采信并回退
    /// UnknownError 终态失败——否则损坏/恶意回包可伪造「恢复成功」绕过防线。
    #[tokio::test(start_paused = true)]
    async fn test_failure_envelope_data_claiming_success_falls_back_to_unknown() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![(false, "success", "伪造成功")])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, result_rx) = tokio::sync::watch::channel(None);
        let session = LoginSession::new(
            make_params(),
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        let mut result_rx = result_rx;
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            1,
            "矛盾信封必须立即终态，不得进入成功验证/重试路径"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 1);
        // 回退消息取自 result.error；ScriptedBridge 恒置 None → 兜底文案
        assert!(
            result.message.contains("Worker 返回失败"),
            "应回退 UnknownError 兜底文案: {}",
            result.message
        );
    }

    /// C1：失败信封 `data` 缺失（Value::Null）→ 回退 UnknownError，消息取
    /// `result.error`（与旧失败分支行为一致）。
    #[test]
    fn test_parse_ipc_response_failure_envelope_data_missing_falls_back_to_unknown() {
        let resp = crate::bridge::IpcResponse {
            id: 1,
            result: crate::bridge::IpcResult {
                success: false,
                data: Value::Null,
                error: Some("导航超时".to_string()),
            },
        };
        let s = LoginSession::parse_ipc_response(resp);
        assert_eq!(s.outcome, Outcome::UnknownError);
        assert_eq!(s.message, "导航超时");
        assert_eq!(s.duration_ms, 0);
    }

    /// C1：失败信封 `data` 可解析且未宣称 Success → 恢复 outcome 与耗时；
    /// message 为空时回填 `result.error`（WorkerError 路径的 error 与 data.message
    /// 同源，兜底覆盖异常构造的信封）。
    #[test]
    fn test_parse_ipc_response_failure_envelope_recovers_outcome_and_fills_message() {
        let resp = crate::bridge::IpcResponse {
            id: 1,
            result: crate::bridge::IpcResult {
                success: false,
                data: serde_json::json!({
                    "outcome": "navigation_timeout",
                    "message": "",
                    "duration_ms": 1200,
                }),
                error: Some("导航超时: #auth-page".to_string()),
            },
        };
        let s = LoginSession::parse_ipc_response(resp);
        assert_eq!(s.outcome, Outcome::NavigationTimeout);
        assert_eq!(s.message, "导航超时: #auth-page");
        assert_eq!(s.duration_ms, 1200);
    }

    // ============ 终态事件发布（事件源接入批三：emit 单点发射） ============

    /// 在暂停时钟的单线程 runtime 内跑一轮会话，返回 (mock bridge, 终态结果, 收到的事件)
    ///
    /// 与 [`run_session_with_script`] 同装配，另注入小事件总线并订阅收集；
    /// 声明 success_condition 跳过登录后真实网络验证，终态确定性不随环境波动。
    async fn run_session_collecting_events(
        script: VecDeque<(bool, &'static str, &'static str)>,
        max_retries: u32,
    ) -> (
        Arc<ScriptedBridge>,
        LoginResult,
        Vec<crate::events::EventEnvelope>,
    ) {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(script),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let events = Arc::new(crate::events::EventBus::new());
        let mut rx = events.subscribe();
        let deps = make_deps_with_events(bridge.clone(), events).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = max_retries;
        params.plan = LoginAttemptPlan::Browser(serde_json::json!({
            "task_config": { "success_condition": "logged_in" }
        }));
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let result = result_rx
            .borrow_and_update()
            .clone()
            .expect("会话 run() 后必须有终态结果");
        let mut got = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            got.push(envelope);
        }
        (bridge, result, got)
    }

    /// 事件①：成功终态 → 恰一次 `LoginSucceeded`，载荷 channel/profile/attempt 正确、
    /// 成功不带 reason（attempt 从 1 起直映射 LoginResult.attempts）
    #[tokio::test(start_paused = true)]
    async fn test_success_terminal_publishes_login_succeeded_once() {
        let (_bridge, result, events) = run_session_collecting_events(
            VecDeque::from(vec![(true, "success", "步骤执行成功")]),
            2,
        )
        .await;
        assert_eq!(result.terminal, LoginTerminal::Success);
        assert_eq!(
            events.len(),
            1,
            "成功终态应恰发布一次事件，实际 {:?}",
            events
        );
        assert_eq!(events[0].event.kind(), "login_succeeded");
        match &events[0].event {
            AppEvent::LoginSucceeded(s) => {
                assert_eq!(s.profile_id, "default");
                assert_eq!(s.channel, "browser");
                assert_eq!(s.attempt, Some(1));
                assert_eq!(s.reason, None);
            }
            other => panic!("应为 LoginSucceeded，实际 {other:?}"),
        }
    }

    /// 事件②：重试耗尽终态 → 恰一次 `LoginFailed`，reason 为失败消息首行摘要、
    /// attempt 映射总尝试次数
    #[tokio::test(start_paused = true)]
    async fn test_retry_exhausted_publishes_login_failed_once() {
        let (_bridge, result, events) = run_session_collecting_events(
            VecDeque::from(vec![
                (true, "selector_failed", "按钮未找到"),
                (true, "selector_failed", "按钮未找到"),
                (true, "selector_failed", "按钮未找到"),
            ]),
            2,
        )
        .await;
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 3);
        assert_eq!(
            events.len(),
            1,
            "重试耗尽终态应恰发布一次事件，实际 {:?}",
            events
        );
        assert_eq!(events[0].event.kind(), "login_failed");
        match &events[0].event {
            AppEvent::LoginFailed(s) => {
                assert_eq!(s.profile_id, "default");
                assert_eq!(s.channel, "browser");
                assert_eq!(s.attempt, Some(3));
                let reason = s.reason.as_deref().expect("失败事件应携带 reason");
                assert!(reason.contains("重试耗尽"), "reason 应含耗尽短语: {reason}");
                assert!(!reason.contains('\n'), "reason 应取首行摘要: {reason}");
            }
            other => panic!("应为 LoginFailed，实际 {other:?}"),
        }
    }

    /// 事件③：manual_required 终态停止 → 恰一次 `LoginFailed`（一次即终态，随发随停）
    #[tokio::test(start_paused = true)]
    async fn test_manual_required_publishes_login_failed() {
        let (_bridge, result, events) = run_session_collecting_events(
            VecDeque::from(vec![(false, "manual_required", "门户要求人工完成验证")]),
            2,
        )
        .await;
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 1);
        assert_eq!(events.len(), 1, "manual_required 终态应恰发布一次事件");
        match &events[0].event {
            AppEvent::LoginFailed(s) => {
                assert_eq!(s.attempt, Some(1));
                let reason = s.reason.as_deref().unwrap_or_default();
                assert!(
                    reason.contains("门户要求人工完成验证"),
                    "reason 应含终态失败短语: {reason}"
                );
            }
            other => panic!("应为 LoginFailed，实际 {other:?}"),
        }
    }

    /// 事件⑤：重试预算内的中间失败（可重试 AssertionFailed）不发布——首试
    /// assertion_failed 走重试、第二次 manual_required 终态，全程恰一次事件
    #[tokio::test(start_paused = true)]
    async fn test_retryable_intermediate_failure_does_not_publish() {
        let (_bridge, result, events) = run_session_collecting_events(
            VecDeque::from(vec![
                (true, "assertion_failed", "断言不匹配"),
                (false, "manual_required", "门户要求人工完成验证"),
            ]),
            2,
        )
        .await;
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 2, "首试失败应走重试而非终态");
        assert_eq!(
            events.len(),
            1,
            "中间失败不得发布，仅终态一次，实际 {:?}",
            events
        );
        assert_eq!(events[0].event.kind(), "login_failed");
    }

    /// 事件补充：取消终态不发布（既非成功也非最终失败，事件流不覆盖）
    #[tokio::test(start_paused = true)]
    async fn test_cancelled_terminal_does_not_publish() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::new()),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let events = Arc::new(crate::events::EventBus::new());
        let mut rx = events.subscribe();
        let deps = make_deps_with_events(bridge, events).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let cancel = CancellationToken::new();
        // 预先取消：run 开头即走 finish_with_cancelled
        cancel.cancel();
        let session = LoginSession::new(
            make_params(),
            cancel,
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let result = result_rx.borrow_and_update().clone().expect("应有取消终态");
        assert_eq!(result.terminal, LoginTerminal::Cancelled);
        assert!(rx.try_recv().is_err(), "取消终态不发布事件");
    }

    /// 渠道标识映射：与 LoginChannel 的 serde snake_case 序列化同值
    #[test]
    fn test_login_channel_code_matches_serde() {
        for (channel, expect) in [
            (LoginChannel::Browser, "browser"),
            (LoginChannel::Http, "http"),
            (LoginChannel::Script, "script"),
        ] {
            assert_eq!(login_channel_code(channel), expect);
            assert_eq!(
                serde_json::to_value(channel).unwrap(),
                serde_json::json!(expect),
                "事件载荷渠道标识必须与 serde 序列化同值"
            );
        }
    }

    // ============ 随机信封属性测试（proptest） ============
    //
    // 注：`arb_json_value` 与 src/bridge/process.rs 测试模块中的同名策略互为镜像，
    // 各自独立维护（跨模块共用会引入生产文件上的 cfg(test) 桥）。

    use proptest::prelude::*;

    /// 属性测试统一口径：512 cases（CI 亲和，无秒级超时；随机失败必可 shrink 复现）
    fn prop_config() -> proptest::test_runner::Config {
        proptest::test_runner::Config::with_cases(512)
    }

    /// 递归 JSON 值策略核心：对象 / 数组 / 字符串（任意 Unicode）/ 数值 /
    /// 布尔 / null，嵌套至多 4 层；键名混入协议关键字以提高「形似信封」密度。
    /// `allow_floats=false` 时数值叶子仅限整数（供 roundtrip 类属性使用，
    /// 见 [`arb_json_value_exact`] 文档）。
    fn arb_json_value_impl(
        allow_floats: bool,
    ) -> proptest::strategy::BoxedStrategy<serde_json::Value> {
        // 数值叶子：allow_floats 时混入任意有限浮点（非有限浮点进不了 serde_json）
        let numeric: proptest::strategy::BoxedStrategy<serde_json::Value> = if allow_floats {
            any::<f64>()
                .prop_filter("非有限浮点不可序列化", |f| f.is_finite())
                .prop_map(|f| serde_json::json!(f))
                .boxed()
        } else {
            any::<i64>().prop_map(|n| serde_json::json!(n)).boxed()
        };
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            numeric,
            any::<String>().prop_map(serde_json::Value::String),
            // 协议关键字键名（对象字段撞名时提高信封相似度）
            prop_oneof![
                Just("id".to_string()),
                Just("result".to_string()),
                Just("event".to_string()),
                Just("data".to_string()),
                Just("success".to_string()),
                Just("error".to_string()),
            ]
            .prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(
            4,  // 嵌套深度
            64, // 期望规模
            8,  // 分支大小
            |inner| {
                prop_oneof![
                    // 对象（键可重复，Map 收敛为 last-wins，属合法 JSON 语义）
                    proptest::collection::vec((any::<String>(), inner.clone()), 0..8).prop_map(
                        |entries| serde_json::Value::Object(entries.into_iter().collect())
                    ),
                    // 数组
                    proptest::collection::vec(inner, 0..8).prop_map(serde_json::Value::Array),
                ]
            },
        )
        .boxed()
    }

    /// 递归 JSON 值策略（含任意有限浮点）：用于不要求数值位保真的
    /// 信封契约类属性
    fn arb_json_value() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
        arb_json_value_impl(true)
    }

    /// roundtrip 安全变体（不含任意浮点）。
    ///
    /// 属性测试实测：serde_json 对部分 f64 的序列化→解析往返存在 1 ulp 保真
    /// 损失（其解析器非全区间正确舍入）。数值位保真不属 IPC 契约（数据透传以
    /// serde_json 为界），故逐字段相等类属性（S2）用整数域版本钉住其余结构的
    /// 无损性；浮点路径的信封契约覆盖由 [`arb_json_value`] 承担。
    fn arb_json_value_exact() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
        arb_json_value_impl(false)
    }

    /// 全部 10 个声明 outcome 变体的均匀策略。Outcome 为 `#[non_exhaustive]`，
    /// 未来新增变体时本策略需同步（同 crate 内 S4 的 match 穷尽检查会编译失败兜底）。
    fn arb_outcome() -> impl proptest::strategy::Strategy<Value = Outcome> {
        prop_oneof![
            Just(Outcome::Success),
            Just(Outcome::Cancelled),
            Just(Outcome::NavigationTimeout),
            Just(Outcome::SelectorFailed),
            Just(Outcome::AssertionFailed),
            Just(Outcome::CaptchaFailed),
            Just(Outcome::InvalidCredential),
            Just(Outcome::ManualRequired),
            Just(Outcome::NetworkError),
            Just(Outcome::UnknownError),
        ]
    }

    proptest! {
        #![proptest_config(prop_config())]

        /// 【不变量 S1】任意 (success, data, error) 信封解析永不 panic，且满足契约：
        /// 成功信封 data 可解析则逐字段透传，否则 UnknownError +「结果解析失败」；
        /// 失败信封绝不产出 Success（矛盾信封防御）、可恢复且未宣称 Success 则
        /// 精确恢复 outcome、否则 UnknownError 兜底且消息必非空
        #[test]
        fn prop_parse_ipc_response_envelope_contract(
            success in any::<bool>(),
            data in arb_json_value(),
            error in any::<Option<String>>(),
        ) {
            let resp = IpcResponse {
                id: 1,
                result: crate::bridge::IpcResult {
                    success,
                    data: data.clone(),
                    error: error.clone(),
                },
            };
            let s = LoginSession::parse_ipc_response(resp);
            if success {
                match serde_json::from_value::<StructuredResult>(data.clone()) {
                    Ok(expect) => {
                        prop_assert_eq!(s.outcome, expect.outcome, "成功信封应逐字段透传");
                        prop_assert_eq!(&s.message, &expect.message);
                        prop_assert_eq!(&s.data, &expect.data);
                        prop_assert_eq!(&s.screenshot_url, &expect.screenshot_url);
                        prop_assert_eq!(s.duration_ms, expect.duration_ms);
                    }
                    Err(_) => {
                        prop_assert_eq!(
                            s.outcome,
                            Outcome::UnknownError,
                            "不可解析的成功信封必须回退 UnknownError"
                        );
                        prop_assert!(
                            s.message.starts_with("结果解析失败"),
                            "回退消息口径: {}",
                            s.message
                        );
                        prop_assert_eq!(s.duration_ms, 0);
                    }
                }
            } else {
                // 矛盾信封防御：失败信封任何情况下不得产出 Success
                prop_assert_ne!(s.outcome, Outcome::Success);
                let recoverable = serde_json::from_value::<StructuredResult>(data.clone())
                    .ok()
                    .filter(|s| s.outcome != Outcome::Success);
                match recoverable {
                    Some(expect) => {
                        prop_assert_eq!(s.outcome, expect.outcome, "失败信封应精确恢复 outcome");
                        let expect_msg = if expect.message.is_empty() {
                            // 回退口径：error 非空串才采信，空串/缺失一律兜底文案
                            error
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| "Worker 返回失败".into())
                        } else {
                            expect.message
                        };
                        prop_assert_eq!(&s.message, &expect_msg);
                        prop_assert_eq!(s.duration_ms, expect.duration_ms);
                        prop_assert_eq!(&s.screenshot_url, &expect.screenshot_url);
                    }
                    None => {
                        prop_assert_eq!(s.outcome, Outcome::UnknownError);
                        prop_assert_eq!(
                            &s.message,
                            &error
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| "Worker 返回失败".into())
                        );
                        prop_assert_eq!(s.duration_ms, 0);
                    }
                }
                prop_assert!(!s.message.is_empty(), "失败信封消息必非空");
            }
        }

        /// 【不变量 S2】成功信封对任意 StructuredResult 无损透传（round-trip 完备性）：
        /// 任意 outcome / message / data / screenshot_url / duration_ms 组合经序列化
        /// 成信封后逐字段还原（data 用整数域策略：serde_json 浮点往返保真限制
        /// 见 arb_json_value_exact）
        #[test]
        fn prop_success_envelope_passthrough(
            outcome in arb_outcome(),
            message in any::<String>(),
            data in arb_json_value_exact(),
            screenshot_url in any::<Option<String>>(),
            duration_ms in any::<u64>(),
        ) {
            let s = StructuredResult { outcome, message, data, screenshot_url, duration_ms };
            let resp = IpcResponse {
                id: 1,
                result: crate::bridge::IpcResult {
                    success: true,
                    data: serde_json::to_value(&s).unwrap(),
                    error: None,
                },
            };
            let parsed = LoginSession::parse_ipc_response(resp);
            prop_assert_eq!(parsed.outcome, s.outcome);
            prop_assert_eq!(parsed.message, s.message);
            prop_assert_eq!(parsed.data, s.data);
            prop_assert_eq!(parsed.screenshot_url, s.screenshot_url);
            prop_assert_eq!(parsed.duration_ms, s.duration_ms);
        }

        /// 【不变量 S3】失败信封恢复 outcome 后必须保持 classify 语义：恢复出的
        /// outcome 与直接拿到该 outcome 的重试/终态/回收判定完全一致——
        /// manual_required / invalid_credential / unknown_error 恢复后仍是终态失败，
        /// 可重试 outcome 恢复后仍可重试（C1 修复的核心回归口径）
        #[test]
        fn prop_failure_envelope_recovery_preserves_classification(
            outcome in arb_outcome().prop_filter("Success 由矛盾信封分支覆盖", |o| *o != Outcome::Success),
            error in any::<Option<String>>(),
        ) {
            // 可恢复失败信封：data 为合法 StructuredResult 且未宣称 Success
            let data = serde_json::json!({
                "outcome": serde_json::to_value(outcome).unwrap(),
                "message": "",
                "duration_ms": 1234u64,
            });
            let resp = IpcResponse {
                id: 1,
                result: crate::bridge::IpcResult { success: false, data, error: error.clone() },
            };
            let s = LoginSession::parse_ipc_response(resp);
            prop_assert_eq!(s.outcome, outcome, "可恢复失败信封必须精确恢复 outcome");
            prop_assert_eq!(
                s.message,
                error
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Worker 返回失败".into()),
                "空 message 必须回填非空 error（空串错误视为缺失，兜底文案）"
            );
            prop_assert_eq!(s.duration_ms, 1234);
            prop_assert_eq!(
                classify(s.outcome),
                classify(outcome),
                "恢复不得改判重试/终态语义"
            );
            prop_assert_eq!(
                should_force_recycle(s.outcome),
                should_force_recycle(outcome),
                "恢复不得改判 Worker 回收语义"
            );
        }

        /// 【不变量 S4】classify 全变体不变量：可重试集合恰为
        /// {CaptchaFailed, NavigationTimeout, SelectorFailed, AssertionFailed,
        /// NetworkError}；manual_required / invalid_credential / unknown_error
        /// 终态失败（manual_required 终态不得被改判可重试）；Success/Cancelled
        /// 各归其终态；should_force_recycle 仅 NetworkError
        #[test]
        fn prop_classify_invariants(outcome in arb_outcome()) {
            match outcome {
                Outcome::Success => {
                    prop_assert_eq!(classify(outcome), ResultAction::Terminal(LoginTerminal::Success));
                }
                Outcome::Cancelled => {
                    prop_assert_eq!(classify(outcome), ResultAction::Terminal(LoginTerminal::Cancelled));
                }
                Outcome::ManualRequired | Outcome::InvalidCredential | Outcome::UnknownError => {
                    prop_assert_eq!(classify(outcome), ResultAction::Terminal(LoginTerminal::Failed));
                }
                Outcome::CaptchaFailed
                | Outcome::NavigationTimeout
                | Outcome::SelectorFailed
                | Outcome::AssertionFailed
                | Outcome::NetworkError => {
                    prop_assert_eq!(classify(outcome), ResultAction::Retry);
                }
            }
            prop_assert_eq!(should_force_recycle(outcome), outcome == Outcome::NetworkError);
        }
    }
    /// 可重试 outcome 池（协议串与 Outcome 的 snake_case 序列化一致；
    /// 下标 4 = NetworkError，供回收计数断言取用）
    const RETRYABLE_OUTCOMES: [&str; 5] = [
        "navigation_timeout",
        "selector_failed",
        "assertion_failed",
        "captcha_failed",
        "network_error",
    ];

    /// 终态 outcome 池（协议串 → 期望终态；success 依赖 success_condition
    /// 跳过登录后网络验证，保证测试环境下的终态确定性）
    const TERMINAL_OUTCOMES: [(&str, LoginTerminal); 4] = [
        ("success", LoginTerminal::Success),
        ("invalid_credential", LoginTerminal::Failed),
        ("manual_required", LoginTerminal::Failed),
        ("unknown_error", LoginTerminal::Failed),
    ];

    /// 在暂停时钟的单线程 runtime 内跑一轮会话，返回 (mock bridge, 终态结果)
    async fn run_session_with_script(
        script: VecDeque<(bool, &'static str, &'static str)>,
        max_retries: u32,
    ) -> (Arc<ScriptedBridge>, LoginResult) {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(script),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = max_retries;
        // 声明 success_condition：跳过登录后真实网络验证，终态确定性不随环境波动
        params.plan = LoginAttemptPlan::Browser(serde_json::json!({
            "task_config": { "success_condition": "logged_in" }
        }));
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let result = result_rx
            .borrow_and_update()
            .clone()
            .expect("会话 run() 后必须有终态结果");
        (bridge, result)
    }

    proptest! {
        #![proptest_config(prop_config())]

        /// 【不变量 S5】随机可重试失败序列下会话精确执行「失败 N 次 + 终态一次」：
        /// 每次 attempt 恰好消费一条 Worker 回包，终态 outcome 一出现立即停止
        /// （同一会话只发射一次终态、不得再多试）；预算恰够全部可重试时，
        /// 每个触发重试的 NetworkError 重试前恰好回收一次 Worker
        ///
        /// 注：proptest 宏把函数体包装为恒返回 Ok 的闭包体，prop_assert 的
        /// early-return 需穿越 async block——故断言块返回 `Result<(), TestCaseError>`
        /// 并经 `check?` 从 runner 闭包传播 Err。
        #[test]
        fn prop_session_first_terminal_outcome_stops_session(
            seq in proptest::collection::vec(0usize..5, 0..=4),
            terminal in 0usize..4,
        ) {
            let ne_count = seq.iter().filter(|&&i| i == 4).count();
            let (terminal_str, expect_terminal) = TERMINAL_OUTCOMES[terminal];
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap();
            let check = rt.block_on(async {
                let mut script: VecDeque<(bool, &'static str, &'static str)> = seq
                    .iter()
                    .map(|&i| (true, RETRYABLE_OUTCOMES[i], "失败详情"))
                    .collect();
                script.push_back((true, terminal_str, "终态详情"));
                // 预算恰够全部可重试（max_retries = N）：终态在第 N+1 次 attempt 到达
                let (bridge, result) =
                    run_session_with_script(script, seq.len() as u32).await;
                prop_assert_eq!(
                    bridge.calls.load(std::sync::atomic::Ordering::SeqCst),
                    (seq.len() + 1) as u32,
                    "终态 outcome 必须立即结束会话，不得再多试一次"
                );
                prop_assert_eq!(result.attempts, (seq.len() + 1) as u32);
                prop_assert_eq!(result.terminal, expect_terminal);
                prop_assert_eq!(
                    bridge.recycled.load(std::sync::atomic::Ordering::SeqCst),
                    ne_count as u32,
                    "每个触发重试的 NetworkError 应回收一次 Worker"
                );
                Ok(())
            });
            check?;
        }

        /// 【不变量 S6】重试预算恰在可重试序列中途耗尽时精确停在 Exhausted：
        /// 共执行 max_retries+1 次尝试、终态 Failed、消息含「重试耗尽」，
        /// 且 Exhausted 分支（最后一次失败）不再触发 Worker 回收
        #[test]
        fn prop_session_retry_exhaustion_stops_at_budget(
            seq in proptest::collection::vec(0usize..5, 2..=5),
            budget_pick in 0usize..4,
        ) {
            // 预算必须小于序列长度才能在序列中途耗尽：budget ∈ [1, len-1]
            let budget = 1 + budget_pick % (seq.len() - 1);
            let retried_ne = seq[..budget].iter().filter(|&&i| i == 4).count();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap();
            let check = rt.block_on(async {
                let script: VecDeque<(bool, &'static str, &'static str)> = seq
                    .iter()
                    .map(|&i| (true, RETRYABLE_OUTCOMES[i], "失败详情"))
                    .collect();
                let (bridge, result) = run_session_with_script(script, budget as u32).await;
                prop_assert_eq!(
                    bridge.calls.load(std::sync::atomic::Ordering::SeqCst),
                    (budget + 1) as u32,
                    "预算耗尽必须恰执行 max_retries+1 次尝试"
                );
                prop_assert_eq!(result.attempts, (budget + 1) as u32);
                prop_assert_eq!(result.terminal, LoginTerminal::Failed);
                prop_assert!(
                    result.message.contains("重试耗尽"),
                    "耗尽终态消息口径: {}",
                    result.message
                );
                prop_assert_eq!(
                    bridge.recycled.load(std::sync::atomic::Ordering::SeqCst),
                    retried_ne as u32,
                    "仅触发重试的 NetworkError 回收；Exhausted 分支不回收"
                );
                Ok(())
            });
            check?;
        }
    }

    // ============ 持久会话自动登录（T3-3b）：派发与 outcome 三形态 ============

    /// 参数捕获 mock：固定回包 + 记录每次 execute 的 method 与 params。
    /// 与 [`ScriptedBridge`] 分开维护：后者被十余个既有用例按字段全量构造，
    /// 加 params 字段会牵连全部装配；本 mock 仅服务派发载荷断言。
    struct RecordingBridge {
        /// 固定回包 (success, outcome, message)，每次调用返回同一值
        response: (bool, &'static str, &'static str),
        methods: std::sync::Mutex<Vec<String>>,
        params_log: std::sync::Mutex<Vec<Value>>,
        calls: std::sync::atomic::AtomicU32,
        recycled: std::sync::atomic::AtomicU32,
        live: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl crate::bridge::BridgeApi for RecordingBridge {
        async fn execute(
            &self,
            method: &str,
            params: Value,
        ) -> Result<crate::bridge::IpcResponse, BridgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.methods.lock().unwrap().push(method.to_string());
            self.params_log.lock().unwrap().push(params.clone());
            let (success, outcome, message) = self.response;
            Ok(crate::bridge::IpcResponse {
                id: 1,
                result: crate::bridge::IpcResult {
                    success,
                    data: serde_json::json!({
                        "outcome": outcome,
                        "message": message,
                        "duration_ms": 1,
                    }),
                    error: None,
                },
            })
        }

        fn cancel(&self, _cancel_id: &str) {}

        async fn execute_with_timeout(
            &self,
            method: &str,
            params: Value,
            _timeout: Duration,
        ) -> Result<crate::bridge::IpcResponse, BridgeError> {
            self.execute(method, params).await
        }

        async fn force_recycle(&self) {
            self.recycled.fetch_add(1, Ordering::SeqCst);
        }

        fn has_live_worker(&self) -> bool {
            self.live.load(Ordering::SeqCst)
        }

        async fn recycle_if_running(&self) {}

        async fn shutdown(&self) {}
    }

    /// 持久自动登录载荷样例（编排侧装配形状：user_data_dir / portal_url /
    /// recorded_actions / browser_settings）
    fn persistent_params() -> Value {
        serde_json::json!({
            "user_data_dir": "/tmp/browser_profiles/default",
            "portal_url": "http://portal.example/auth",
            "recorded_actions": [
                { "type": "click", "selector": "text=登录", "strategy": "text", "ts": 1 },
                { "type": "change", "selector": "#isp", "strategy": "id", "ts": 2, "value": "cmcc" }
            ],
            "browser_settings": { "timeout": 45 }
        })
    }

    /// 同 [`make_deps`]，但禁用全部网络探测（成功 outcome 的网络验证兜底
    /// 确定性返回 Unknown → RetryableFailure，不随测试机环境波动）
    async fn make_deps_verification_disabled(
        bridge: Arc<dyn crate::bridge::BridgeApi>,
    ) -> SessionDeps {
        let dir = tempfile::TempDir::new().unwrap();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
        let config = ConfigService::new(dir.path().to_path_buf(), reload_tx)
            .await
            .expect("ConfigService 构造失败");
        let mut settings = config.load_settings();
        settings.global.monitor.tcp_enabled = false;
        settings.global.monitor.http_enabled = false;
        settings.global.monitor.url_enabled = false;
        settings.global.monitor.post_login_delay = 0;
        config.save_settings(&settings).await.unwrap();
        config.reload().await.expect("配置重载失败");
        SessionDeps {
            bridge,
            monitor: Arc::new(
                crate::monitor::MonitorService::new(
                    config.clone(),
                    crate::network::detect::create_detector(),
                    Some(crate::utils::metrics::Metrics::new()),
                )
                .expect("MonitorService 构造失败"),
            ),
            config_service: config,
            status_manager: Arc::new(StatusManager::new()),
            history_service: Arc::new(LoginHistoryService::new(dir.path())),
            script_runner: Arc::new(StubScriptRunner),
            events: Arc::new(crate::events::EventBus::new()),
            metrics: None,
        }
    }

    /// 派发载荷：BrowserPersistent 计划走 profile_auto_login 通道，
    /// cancel_id 由会话层注入，其余字段原样透传
    #[tokio::test(start_paused = true)]
    async fn test_persistent_dispatch_payload_and_channel() {
        let bridge = Arc::new(RecordingBridge {
            response: (true, "assertion_failed", "断言不匹配"),
            methods: std::sync::Mutex::new(Vec::new()),
            params_log: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = 1;
        params.plan = LoginAttemptPlan::BrowserPersistent(persistent_params());
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let methods = bridge.methods.lock().unwrap();
        assert_eq!(
            methods.first().map(String::as_str),
            Some("profile_auto_login"),
            "持久计划必须经 profile_auto_login 通道派发: {methods:?}"
        );
        let sent = bridge.params_log.lock().unwrap().remove(0);
        assert_eq!(sent["user_data_dir"], "/tmp/browser_profiles/default");
        assert_eq!(sent["portal_url"], "http://portal.example/auth");
        assert_eq!(
            sent["recorded_actions"],
            persistent_params()["recorded_actions"],
            "录制动作必须原样透传"
        );
        let timeout = sent["browser_settings"]["timeout"].as_u64().unwrap();
        assert!(timeout >= 45, "步骤预算必须满足 ≥45s 口径，实际 {timeout}");
        assert!(
            sent["cancel_id"].as_str().is_some_and(|s| !s.is_empty()),
            "会话层必须注入 cancel_id 供取消传播命中"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
    }

    /// outcome 形态①：Worker Success ≠ 登录成功——必须走既有网络验证兜底，
    /// 验证未通过（探测全禁用 → Unknown）转可重试路径发起第二次尝试
    #[tokio::test(start_paused = true)]
    async fn test_persistent_success_goes_through_network_verification() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (true, "success", "回放完成"),
                (false, "manual_required", "门户要求人工完成验证"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps_verification_disabled(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = 1;
        params.plan = LoginAttemptPlan::BrowserPersistent(persistent_params());
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            2,
            "Worker Success 不得直接终态成功，应经网络验证兜底后重试"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert_eq!(result.attempts, 2);
        assert!(
            result.message.contains("门户要求人工完成验证"),
            "第二次尝试的终态消息: {}",
            result.message
        );
    }

    /// outcome 形态②：assertion_failed 可重试、不回收 Worker；重试耗尽的
    /// 终态失败追加持久会话失效提示（T3-4 最小面）
    #[tokio::test(start_paused = true)]
    async fn test_persistent_assertion_failed_exhaustion_appends_hint() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (true, "assertion_failed", "断言不匹配"),
                (true, "assertion_failed", "断言不匹配"),
                (true, "assertion_failed", "断言不匹配"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = 2;
        params.plan = LoginAttemptPlan::BrowserPersistent(persistent_params());
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            3,
            "assertion_failed 应消耗完整重试预算"
        );
        assert_eq!(
            bridge.recycled.load(Ordering::SeqCst),
            0,
            "assertion_failed 不得触发 Worker 回收"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert!(
            result.message.contains("重试耗尽"),
            "耗尽终态消息: {}",
            result.message
        );
        assert!(
            result
                .message
                .contains("持久会话疑似失效，请重新打开浏览器配置"),
            "终态失败应追加失效提示: {}",
            result.message
        );
    }

    /// outcome 形态③：cancelled 终态停止（Worker 取消语义 = success 信封 +
    /// cancelled outcome），不再消耗重试预算
    #[tokio::test(start_paused = true)]
    async fn test_persistent_cancelled_is_terminal() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![(true, "cancelled", "任务已取消")])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = 2;
        params.plan = LoginAttemptPlan::BrowserPersistent(persistent_params());
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            1,
            "取消终态必须立即停止，不得重试"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有取消终态");
        assert_eq!(result.terminal, LoginTerminal::Cancelled);
        assert_eq!(result.attempts, 1);
    }

    /// 非持久对照①：开关关闭的 Browser 计划走原 execute_login_attempt 通道
    /// （零变化），且终态失败不带持久会话失效提示
    #[tokio::test(start_paused = true)]
    async fn test_non_persistent_plan_uses_login_attempt_and_no_hint() {
        let bridge = Arc::new(ScriptedBridge {
            script: std::sync::Mutex::new(VecDeque::from(vec![
                (true, "assertion_failed", "断言不匹配"),
                (true, "assertion_failed", "断言不匹配"),
                (true, "assertion_failed", "断言不匹配"),
            ])),
            methods: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::atomic::AtomicU32::new(0),
            recycled: std::sync::atomic::AtomicU32::new(0),
            live: std::sync::atomic::AtomicBool::new(false),
        });
        let deps = make_deps(bridge.clone()).await;
        let (result_tx, mut result_rx) = tokio::sync::watch::channel(None);
        let mut params = make_params();
        params.max_retries = 2;
        // 维持 make_params 的 Browser 计划（开关关闭路径）
        let session = LoginSession::new(
            params,
            CancellationToken::new(),
            Arc::new(crate::login::LoginHandleInner { result_tx }),
            Arc::new(arc_swap::ArcSwapOption::new(None)),
            CancellationToken::new(),
            Arc::new(std::sync::Mutex::new(None)),
            deps,
        );
        session.run().await;
        let methods = bridge.methods.lock().unwrap();
        assert_eq!(
            methods.first().map(String::as_str),
            Some("execute_login_attempt"),
            "非持久模式必须走原浏览器任务通道: {methods:?}"
        );
        let result = result_rx.borrow_and_update().clone().expect("应有终态结果");
        assert_eq!(result.terminal, LoginTerminal::Failed);
        assert!(
            !result.message.contains("持久会话"),
            "非持久模式不得出现失效提示: {}",
            result.message
        );
    }
}
