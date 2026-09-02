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

use crate::bridge::{IpcResponse, Outcome, StructuredResult};
use crate::config::ConfigService;
use crate::login::history::{HistoryResult, LoginHistoryEntry, LoginHistoryService};
use crate::login::{LoginHandleInner, recover_lock};
use crate::status::{LoginSource, LoginStatus, PartialSnapshot, StatusManager};
use crate::utils::metrics::Metrics;

/// 会话内部状态机枚举（用于内部观测，前端可见状态经 `StatusManager` 广播）
#[derive(Debug, Clone)]
pub enum LoginState {
    /// 初始状态，尚未开始执行
    Idle,
    /// 正在执行 `bridge.execute()`
    Running,
    /// 重试间隔 `sleep` 中
    Retrying {
        /// 当前重试次数（从 1 计）
        attempt: u32,
    },
    /// 终态：成功
    Success,
    /// 终态：失败
    Failed {
        /// 失败原因
        reason: String,
    },
    /// 终态：取消
    Cancelled {
        /// 取消原因
        reason: String,
    },
}

/// 终态种类（成功 / 取消 / 失败）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
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
    Terminal(TerminalKind),
    /// 可重试（导航超时 / 选择器失败 / 网络错误）
    Retry,
    /// 重试预算耗尽
    Exhausted,
}

/// 登录终态结果（句柄 `await_result` 返回、历史与调度器读取）
#[derive(Debug, Clone)]
pub struct LoginResult {
    /// 是否成功
    pub success: bool,
    /// 结果消息（成功提示 / 失败原因 / 取消原因）
    pub message: String,
    /// 登录来源
    pub source: LoginSource,
    /// 总耗时
    pub duration: Duration,
    /// 尝试次数（含首次）
    pub attempts: u32,
}

/// 单次尝试结果分类（纯函数）
///
/// 分类规则：
/// - `Success` → 终态（成功）
/// - `Cancelled` → 终态（取消）
/// - `InvalidCredential` / `UnknownError` → 终态（失败，不重试）
/// - `CaptchaFailed` / `NavigationTimeout` / `SelectorFailed` / `NetworkError` → 可重试
///
/// 验证码失败（OCR 误识别）与网络/导航失败同属瞬时性失败，难以可靠区分具体成因，
/// 故统一重试整个登录流程（受 `max_retries` 预算约束），达到上限才以失败终态结束（历史遗留 #6）。
///
/// `Outcome` 为 `#[non_exhaustive]`，未知变体兜底为终态（失败）。
#[allow(unreachable_patterns)]
pub fn classify(outcome: Outcome) -> ResultAction {
    match outcome {
        Outcome::Success => ResultAction::Terminal(TerminalKind::Success),
        Outcome::Cancelled => ResultAction::Terminal(TerminalKind::Cancelled),
        Outcome::InvalidCredential => ResultAction::Terminal(TerminalKind::Failed),
        Outcome::UnknownError => ResultAction::Terminal(TerminalKind::Failed),
        Outcome::CaptchaFailed => ResultAction::Retry,
        Outcome::NavigationTimeout => ResultAction::Retry,
        Outcome::SelectorFailed => ResultAction::Retry,
        // P11：断言失败（assert_text 超时/不匹配）可重试、不回收 Worker
        Outcome::AssertionFailed => ResultAction::Retry,
        Outcome::NetworkError => ResultAction::Retry,
        // 前向兼容：未知 outcome 兜底为终态（失败），避免无限重试
        _ => ResultAction::Terminal(TerminalKind::Failed),
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
    /// 运行指标（可选）
    pub metrics: Option<std::sync::Arc<Metrics>>,
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
    /// 发送给 Worker 的配置字典（凭证、auth_url、浏览器设置等）
    pub worker_config: Value,
}

/// 登录会话：持有会话参数、取消原语与服务依赖，驱动单次登录状态机
pub struct LoginSession {
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
    /// 内部状态机当前状态
    state: StdMutex<LoginState>,
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
            params,
            cancel_token,
            result_slot,
            attempt_cancel_id,
            shutdown_token,
            cancel_reason,
            deps,
            state: StdMutex::new(LoginState::Idle),
        }
    }

    /// 返回当前内部状态机状态
    pub fn current_state(&self) -> LoginState {
        recover_lock(&self.state).clone()
    }

    /// 会话主循环：执行 → 分类 → 重试/终态
    pub async fn run(self) {
        let session_start = Instant::now();
        *recover_lock(&self.state) = LoginState::Running;
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

        // A-2：依赖 trait 化且非 Option——编排器构造会话时必然注入
        let bridge = self.deps.bridge.clone();

        let total_attempts = self.params.max_retries + 1;
        let mut attempts_used: u32 = 0;

        loop {
            // 取消检查（状态机任意阶段）
            if self.cancel_token.is_cancelled() {
                self.finish_with_cancelled(session_start, attempts_used, None)
                    .await;
                return;
            }

            // 会话总超时检查（login_timeout 至少 1s，见 SessionParams 构造 clamp）
            if session_start.elapsed() > self.params.login_timeout.max(Duration::from_secs(1)) {
                if let Some(cid) = self.attempt_cancel_id.load_full() {
                    bridge.cancel(cid.as_str());
                }
                self.finish_with_failure(session_start, attempts_used, "登录超时".into())
                    .await;
                return;
            }

            // 生成本轮 attempt 的 cancel_id（UUID v4）
            let cancel_id = uuid::Uuid::new_v4().to_string();
            self.attempt_cancel_id
                .store(Some(Arc::new(cancel_id.clone())));

            let attempt_no = attempts_used + 1;
            self.deps.status_manager.merge(PartialSnapshot::Login {
                status: LoginStatus::Running,
                source: Some(self.params.source),
                message: Some(format!("尝试 {attempt_no}/{total_attempts}")),
                retry_count: attempts_used,
            });

            // 根据来源选择 Bridge 命令
            let method = match self.params.source {
                LoginSource::Browser => "execute_browser_task",
                _ => "execute_login_attempt",
            };

            let mut params = self.params.worker_config.clone();
            params["cancel_id"] = json!(cancel_id.clone());
            if let Some(tid) = &self.params.task_id {
                params["task_id"] = json!(tid.clone());
            }

            // 等待 Bridge 响应，期间监听会话级取消、应用 shutdown 与会话总超时
            //
            // bridge.execute() 内部最长阻塞 300s。此处用 tokio::select! (biased) 同时监听：
            // - cancel_token 触发（用户/系统取消，最高优先级）
            // - shutdown_token 触发（应用关闭，立即以取消终态退出）
            // - 会话总超时到期（防止 Worker 卡死导致整会话永不退出）
            // - execute 完成（正常路径）
            // biased 保证取消类信号先于 execute/timeout 生效，避免取消被延迟。
            let exec = {
                let ct = self.cancel_token.clone();
                let remaining = self
                    .params
                    .login_timeout
                    .max(Duration::from_secs(1))
                    .saturating_sub(session_start.elapsed());
                tokio::select! {
                    biased;
                    _ = ct.cancelled() => {
                        bridge.cancel(&cancel_id);
                        self.finish_with_cancelled(session_start, attempts_used, None).await;
                        return;
                    }
                    _ = self.shutdown_token.cancelled() => {
                        bridge.cancel(&cancel_id);
                        self.finish_with_cancelled(
                            session_start,
                            attempts_used,
                            Some("应用关闭".to_string()),
                        )
                        .await;
                        return;
                    }
                    _ = sleep(remaining) => {
                        bridge.cancel(&cancel_id);
                        self.finish_with_failure(session_start, attempts_used, "登录超时".into())
                            .await;
                        return;
                    }
                    res = bridge.execute(method, params) => res,
                }
            };

            let structured = match exec {
                Ok(resp) => self.parse_response(resp),
                Err(e) => {
                    error!(attempt = attempts_used + 1, "Bridge 执行失败: {e}");
                    self.finish_with_failure(
                        session_start,
                        attempts_used,
                        format!("Bridge 执行失败: {e}"),
                    )
                    .await;
                    return;
                }
            };

            // 本轮 attempt 结束，清除在途 cancel_id
            self.attempt_cancel_id.store(None);

            // 分类结果：可重试但重试预算已耗尽时归入 Exhausted（避免进入 try_retry）
            // 后再次判断，保持"预算耗尽"这一决策与 classify 一起表达
            let action = classify(structured.outcome);
            let action =
                if action == ResultAction::Retry && attempts_used >= self.params.max_retries {
                    ResultAction::Exhausted
                } else {
                    action
                };
            match action {
                ResultAction::Terminal(kind) => match kind {
                    TerminalKind::Success => {
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
                            self.emit(
                                self.make_result(true, msg, session_start, attempts_used),
                                HistoryResult::Success,
                            )
                            .await;
                            return;
                        }
                        // 步骤全部成功后做真实网络验证：避免 Worker 假成功（步骤未抛异常
                        // 但页面实际未登录成功）被误报。参考老实现 _check_success：
                        // 等待 post_login_delay 让认证生效 → check_once → 仅 Online 才算真成功。
                        let net_ok = self.verify_network_after_login().await;
                        if net_ok {
                            self.emit(
                                self.make_result(true, msg, session_start, attempts_used),
                                HistoryResult::Success,
                            )
                            .await;
                            return;
                        }
                        // 网络验证未通过：不直接判终态失败，而是走可重试路径。
                        // 理由：网络探测可能因瞬时波动误判，重试一次登录比直接判死更稳妥。
                        // 策略与 Worker 返回 NetworkError 时一致（classify → Retry）。
                        warn!(
                            attempt = attempts_used + 1,
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
                        if !self
                            .try_retry(&retry_structured, &mut attempts_used, session_start)
                            .await
                        {
                            return;
                        }
                    }
                    TerminalKind::Cancelled => {
                        self.emit(
                            self.make_result(
                                false,
                                "登录已取消".into(),
                                session_start,
                                attempts_used,
                            ),
                            HistoryResult::Cancelled,
                        )
                        .await;
                        return;
                    }
                    TerminalKind::Failed => {
                        self.finish_with_failure(
                            session_start,
                            attempts_used,
                            format!("{}{}", structured.message, dialog_note(&structured.data)),
                        )
                        .await;
                        return;
                    }
                },
                ResultAction::Retry => {
                    if !self
                        .try_retry(&structured, &mut attempts_used, session_start)
                        .await
                    {
                        return;
                    }
                }
                ResultAction::Exhausted => {
                    // 可重试结果但重试预算已耗尽：以"重试耗尽"终态收尾，
                    // 不再进入 try_retry 重复尝试（由下方 classify 前预算预检产生）。
                    // 附带最后一次失败的步骤错误与页面弹窗文案，便于定位真实原因。
                    self.finish_with_failure(
                        session_start,
                        attempts_used,
                        format!(
                            "重试耗尽（共 {} 次）: {}{}",
                            self.params.max_retries,
                            structured.message,
                            dialog_note(&structured.data)
                        ),
                    )
                    .await;
                    return;
                }
            }
        }
    }

    /// 尝试重试：更新状态、按需回收 Worker、等待重试间隔。
    ///
    /// 返回 `true` 表示继续下一轮循环，`false` 表示已 emit 终态结果（重试耗尽或被取消）。
    async fn try_retry(
        &self,
        structured: &StructuredResult,
        attempts_used: &mut u32,
        session_start: Instant,
    ) -> bool {
        if *attempts_used >= self.params.max_retries {
            self.finish_with_failure(
                session_start,
                *attempts_used,
                format!(
                    "重试耗尽（共 {} 次）: {}{}",
                    self.params.max_retries,
                    structured.message,
                    dialog_note(&structured.data)
                ),
            )
            .await;
            return false;
        }
        *attempts_used += 1;
        *recover_lock(&self.state) = LoginState::Retrying {
            attempt: *attempts_used,
        };
        self.deps.status_manager.merge(PartialSnapshot::Login {
            status: LoginStatus::Running,
            source: Some(self.params.source),
            message: Some(format!(
                "重试中 {attempts_used}/{}",
                self.params.max_retries
            )),
            retry_count: *attempts_used,
        });
        if should_force_recycle(structured.outcome) {
            // 强制回收 Worker：直接 kill 当前子进程并标记 Error，
            // 下一次 bridge.execute() 内部的 ensure_worker 会自动重新 spawn。
            // 同步 await（而非 spawn）确保 kill 在重试间隔之前完成，避免下一轮
            // ensure_worker 复用即将被 kill 的 Worker（force_recycle 与 retry 竞态）。
            warn!("登录结果 {:?} 触发 Worker 强制回收", structured.outcome);
            self.deps.bridge.force_recycle().await;
        }
        let retry_interval = self.params.retry_interval.max(Duration::from_secs(1));
        let ct = self.cancel_token.clone();
        tokio::select! {
            biased;
            _ = ct.cancelled() => {
                self.emit(
                    self.make_cancelled_result(session_start, *attempts_used),
                    HistoryResult::Cancelled,
                )
                .await;
                false
            }
            _ = self.shutdown_token.cancelled() => {
                *recover_lock(self.cancel_reason.as_ref()) = Some("应用关闭".to_string());
                self.emit(
                    self.make_cancelled_result(session_start, *attempts_used),
                    HistoryResult::Cancelled,
                )
                .await;
                false
            }
            _ = sleep(retry_interval) => true,
        }
    }

    /// 任务是否声明了 `success_condition`（以变量真值判定登录成功）
    ///
    /// 声明时 Worker 已用变量真值判定过成功，登录路径应跳过网络检测兜底
    /// （对齐原项目 v4.2.3 `login_attempt` 的 `has_explicit_condition` 分支）。
    fn has_explicit_success_condition(&self) -> bool {
        self.params
            .worker_config
            .get("task_config")
            .and_then(|t| t.get("success_condition"))
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    /// 登录后真实网络验证：等待 post_login_delay 让认证生效，再调用 MonitorService 做一次完整探测。
    ///
    /// 仅当探测结果为 [`NetworkStatus::Online`] 时返回 true，其余（CaptivePortal /
    /// Offline / Paused / 探测异常 / Monitor 未注入）均返回 false。
    ///
    /// 与老实现 `BrowserTaskRunner._network_detection_check` 等价：防止 Worker 步骤
    /// 全部成功但页面实际未登录成功（如填入字面量 `{{USERNAME}}` 却没点登录按钮）。
    async fn verify_network_after_login(&self) -> bool {
        let monitor = &self.deps.monitor;
        // 登录后等待 portal 生效的延迟（可配置，默认 5s）
        let delay = self
            .deps
            .config_service
            .runtime()
            .load()
            .monitor
            .post_login_delay;
        tokio::time::sleep(Duration::from_secs(delay as u64)).await;
        match monitor.check_once().await {
            Ok(report) => {
                use crate::status::NetworkStatus;
                let ok = matches!(report.status, NetworkStatus::Online);
                if ok {
                    info!("登录后网络验证通过：Online");
                } else {
                    warn!(
                        status = ?report.status,
                        tcp = ?report.tcp_outcome,
                        http = ?report.http_outcome,
                        url = ?report.url_outcome,
                        "登录后网络验证未通过"
                    );
                }
                ok
            }
            Err(e) => {
                warn!("登录后网络验证异常: {e}");
                false
            }
        }
    }

    /// 构建终态结果（携带耗时与尝试次数）
    fn make_result(
        &self,
        success: bool,
        message: String,
        start: Instant,
        attempts_used: u32,
    ) -> LoginResult {
        LoginResult {
            success,
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
            success: false,
            message: reason,
            source: self.params.source,
            duration: start.elapsed(),
            attempts: attempts_used + 1,
        }
    }

    /// 终态收尾：写入共享结果槽 → 广播状态 → 记录历史 → 更新内部状态
    async fn emit(&self, result: LoginResult, history: HistoryResult) {
        self.result_slot.set_result(result.clone());

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

        let entry = LoginHistoryEntry {
            timestamp: chrono::Local::now(),
            source: result.source,
            profile_id: self.params.profile_id.clone(),
            result: history,
            message: result.message.clone(),
            duration_secs: result.duration.as_secs_f64(),
        };
        if let Err(e) = self.deps.history_service.record(&entry).await {
            warn!("登录历史写入失败: {e}");
        }

        *recover_lock(&self.state) = match history {
            HistoryResult::Success => LoginState::Success,
            HistoryResult::Cancelled => LoginState::Cancelled {
                reason: result.message.clone(),
            },
            HistoryResult::Failed => LoginState::Failed {
                reason: result.message.clone(),
            },
        };
        // 会话终态后关闭浏览器（对齐原版 BrowserContextManager 的会话级生命周期）：
        // 会话内重试复用同一浏览器，终态即关闭；Worker 进程保留，下次登录由
        // ensure_browser 重建。进程已被回收（force_recycle / 空闲超时）时跳过，
        // 避免仅为关浏览器而重新 spawn 一个 Worker。
        {
            let b = &self.deps.bridge;
            if b.has_live_worker() {
                // 超时与 Python 侧 close_browser 内部超时（8s）对齐：
                // 命令级超时兜底由 bridge.execute_with_timeout 负责，失败仅告警不阻塞收尾
                if let Err(e) = b
                    .execute_with_timeout("close_browser", json!({}), Duration::from_secs(8))
                    .await
                {
                    warn!("登录终态关闭浏览器失败（忽略）: {e}");
                }
            }
        }
        info!(
            source = ?result.source,
            success = result.success,
            "登录会话结束: {}",
            result.message
        );
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
        self.emit(
            self.make_cancelled_result(session_start, attempts_used),
            HistoryResult::Cancelled,
        )
        .await;
    }

    /// 以「失败」终态收尾：emit 失败结果 → 写历史（收敛 `run`/`try_retry` 内样板，C4）。
    async fn finish_with_failure(
        &self,
        session_start: Instant,
        attempts_used: u32,
        message: String,
    ) {
        self.emit(
            self.make_result(false, message, session_start, attempts_used),
            HistoryResult::Failed,
        )
        .await;
    }

    /// 将 `IpcResponse` 解析为 [`StructuredResult`]
    ///
    /// 成功时从 `result.data` 反序列化；失败时构造 `UnknownError` 兜底结果。
    fn parse_response(&self, resp: IpcResponse) -> StructuredResult {
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
            StructuredResult {
                outcome: Outcome::UnknownError,
                message: resp
                    .result
                    .error
                    .clone()
                    .unwrap_or_else(|| "Worker 返回失败".into()),
                data: Value::Null,
                screenshot_url: None,
                duration_ms: 0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::Outcome;
    use std::sync::atomic::Ordering;

    // ============ classify 纯函数测试 ============

    #[test]
    fn test_classify_success_is_terminal_success() {
        assert_eq!(
            classify(Outcome::Success),
            ResultAction::Terminal(TerminalKind::Success)
        );
    }

    #[test]
    fn test_classify_cancelled_is_terminal_cancelled() {
        assert_eq!(
            classify(Outcome::Cancelled),
            ResultAction::Terminal(TerminalKind::Cancelled)
        );
    }

    #[test]
    fn test_classify_invalid_credential_is_terminal_failed() {
        // 凭证无效属于终态失败，重试无意义
        assert_eq!(
            classify(Outcome::InvalidCredential),
            ResultAction::Terminal(TerminalKind::Failed)
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
            ResultAction::Terminal(TerminalKind::Failed)
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
            false
        }

        async fn recycle_if_running(&self) {}

        async fn shutdown(&self) {}
    }

    /// 构造带脚本 Bridge 的会话依赖集（真实 ConfigService/StatusManager/History）
    async fn make_deps(bridge: Arc<ScriptedBridge>) -> SessionDeps {
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
                    None,
                    Some(crate::utils::metrics::Metrics::new()),
                )
                .expect("MonitorService 构造失败"),
            ),
            config_service: config,
            status_manager: Arc::new(StatusManager::new()),
            history_service: Arc::new(LoginHistoryService::new(dir.path())),
            metrics: None,
        }
    }

    fn make_params() -> SessionParams {
        SessionParams {
            source: LoginSource::Auto,
            task_id: None,
            max_retries: 2,
            retry_interval: Duration::from_secs(0),
            login_timeout: Duration::from_secs(30),
            profile_id: "default".to_string(),
            worker_config: serde_json::json!({}),
        }
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
        assert!(!result.success, "预算耗尽应为失败终态");
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
        assert!(!result.success);
    }
}
