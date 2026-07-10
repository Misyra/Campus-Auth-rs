//! 登录会话状态机：单次登录的尝试循环、结果分类、重试与取消传播
//!
//! [`LoginSession`] 由 [`crate::login::LoginOrchestrator::submit`] 创建并 `tokio::spawn`，
//! 独立运行直至终态。每一轮尝试通过 `BridgeSupervisor::execute` 驱动 Python Worker，
//! 根据返回的 [`Outcome`] 分类为终态或可重试；可重试时检查预算并 `sleep` 等待（期间监听取消）。

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use serde_json::{json, Value};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bridge::{BridgeSupervisor, IpcResponse, Outcome, StructuredResult};
use crate::config::ConfigService;
use crate::login::history::{HistoryResult, LoginHistoryEntry, LoginHistoryService};
use crate::login::{recover_lock, LoginHandleInner};
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
/// - `InvalidCredential` / `CaptchaFailed` / `UnknownError` → 终态（失败，不重试）
/// - `NavigationTimeout` / `SelectorFailed` / `NetworkError` → 可重试
///
/// `Outcome` 为 `#[non_exhaustive]`，未知变体兜底为终态（失败）。
#[allow(unreachable_patterns)]
pub fn classify(outcome: Outcome) -> ResultAction {
    match outcome {
        Outcome::Success => ResultAction::Terminal(TerminalKind::Success),
        Outcome::Cancelled => ResultAction::Terminal(TerminalKind::Cancelled),
        Outcome::InvalidCredential => ResultAction::Terminal(TerminalKind::Failed),
        Outcome::CaptchaFailed => ResultAction::Terminal(TerminalKind::Failed),
        Outcome::UnknownError => ResultAction::Terminal(TerminalKind::Failed),
        Outcome::NavigationTimeout => ResultAction::Retry,
        Outcome::SelectorFailed => ResultAction::Retry,
        Outcome::NetworkError => ResultAction::Retry,
        // 前向兼容：未知 outcome 兜底为终态（失败），避免无限重试
        _ => ResultAction::Terminal(TerminalKind::Failed),
    }
}

/// 判断某次失败结果是否需要强制回收 Worker
///
/// `UnknownError` 语义不明，浏览器上下文可能已损坏，保守回收；
/// `NetworkError` 可能伴随 Worker 网络栈异常（与 IPC 枚举文档一致：网络错误强制回收），
/// 回收后由 `ensure_worker` 重新 spawn，避免已损坏上下文被复用导致后续重试持续失败。
/// 其余可重试结果（NavigationTimeout / SelectorFailed）上下文未损坏，复用即可。
pub fn should_force_recycle(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::UnknownError | Outcome::NetworkError)
}

/// 登录会话：持有配置快照派生参数、服务引用与取消原语，驱动单次登录状态机
pub struct LoginSession {
    /// 登录来源
    source: LoginSource,
    /// 浏览器任务 ID（仅 `Browser` 来源有值），为空时回退到 Profile 的 `active_task`
    task_id: Option<String>,
    /// 会话级取消令牌（整个会话生命周期）
    cancel_token: CancellationToken,
    /// 最大重试次数（不含首次尝试）
    max_retries: u32,
    /// 重试间隔
    retry_interval: Duration,
    /// 单次登录会话总超时
    login_timeout: Duration,
    /// 关联 Profile ID（写入历史）
    profile_id: String,
    /// 发送给 Worker 的配置字典（凭证、auth_url、浏览器设置等）
    worker_config: Value,
    /// 结果共享槽（与 [`crate::login::LoginHandle`] 共享）
    result_slot: Arc<LoginHandleInner>,
    /// 当前在途 attempt 的 cancel_id（供取消传播读取）
    attempt_cancel_id: Arc<ArcSwapOption<String>>,
    /// 应用级 shutdown 信号（触发时会话立即以取消终态退出）
    shutdown_token: CancellationToken,
    /// 取消原因（由 `cancel_current` / `cancel_auto_pending` 设置，供终态消息使用）
    cancel_reason: Arc<StdMutex<Option<String>>>,
    /// Bridge 句柄（可能为 None，仅当编排器未注入时）
    bridge: Option<Arc<BridgeSupervisor>>,
    /// 网络监测服务（可能为 None，仅当编排器未注入时；登录后网络验证使用）
    monitor: Option<Arc<crate::monitor::MonitorService>>,
    /// 配置服务（保留引用，供后续扩展按需重读；当前未使用）
    #[allow(dead_code)]
    config_service: Arc<ConfigService>,
    /// 状态管理器（广播登录状态）
    status_manager: Arc<StatusManager>,
    /// 历史服务（终态写入）
    history_service: Arc<LoginHistoryService>,
    /// 运行指标（可选）
    metrics: Option<Arc<Metrics>>,
    /// 内部状态机当前状态
    state: StdMutex<LoginState>,
}

impl LoginSession {
    /// 构造登录会话
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: LoginSource,
        task_id: Option<String>,
        cancel_token: CancellationToken,
        max_retries: u32,
        retry_interval: Duration,
        login_timeout: Duration,
        profile_id: String,
        worker_config: Value,
        result_slot: Arc<LoginHandleInner>,
        attempt_cancel_id: Arc<ArcSwapOption<String>>,
        shutdown_token: CancellationToken,
        cancel_reason: Arc<StdMutex<Option<String>>>,
        bridge: Option<Arc<BridgeSupervisor>>,
        monitor: Option<Arc<crate::monitor::MonitorService>>,
        config_service: Arc<ConfigService>,
        status_manager: Arc<StatusManager>,
        history_service: Arc<LoginHistoryService>,
        metrics: Option<Arc<Metrics>>,
    ) -> Self {
        Self {
            source,
            task_id,
            cancel_token,
            max_retries,
            retry_interval,
            login_timeout,
            profile_id,
            worker_config,
            result_slot,
            attempt_cancel_id,
            shutdown_token,
            cancel_reason,
            bridge,
            monitor,
            config_service,
            status_manager,
            history_service,
            metrics,
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
        self.status_manager.merge(PartialSnapshot::Login {
            status: LoginStatus::Running,
            source: Some(self.source),
            message: Some("登录中...".into()),
            retry_count: 0,
        });

        let bridge = match &self.bridge {
            Some(b) => b.clone(),
            None => {
                warn!("LoginSession 缺少 BridgeSupervisor，无法执行登录");
                self.emit(
                    self.make_result(false, "Bridge 未初始化，无法执行登录".into(), session_start, 0),
                    HistoryResult::Failed,
                )
                .await;
                return;
            }
        };

        let total_attempts = self.max_retries + 1;
        let mut attempts_used: u32 = 0;

        loop {
            // 取消检查（状态机任意阶段）
            if self.cancel_token.is_cancelled() {
                self.emit(
                    self.make_cancelled_result(session_start, attempts_used),
                    HistoryResult::Cancelled,
                )
                .await;
                return;
            }

            // 会话总超时检查
            if session_start.elapsed() > self.login_timeout {
                if let Some(cid) = self.attempt_cancel_id.load_full() {
                    bridge.cancel(cid.as_str());
                }
                self.emit(
                    self.make_result(false, "登录超时".into(), session_start, attempts_used),
                    HistoryResult::Failed,
                )
                .await;
                return;
            }

            // 生成本轮 attempt 的 cancel_id（UUID v4）
            let cancel_id = uuid::Uuid::new_v4().to_string();
            self.attempt_cancel_id
                .store(Some(Arc::new(cancel_id.clone())));

            let attempt_no = attempts_used + 1;
            self.status_manager.merge(PartialSnapshot::Login {
                status: LoginStatus::Running,
                source: Some(self.source),
                message: Some(format!("尝试 {attempt_no}/{total_attempts}")),
                retry_count: attempts_used,
            });

            // 根据来源选择 Bridge 命令
            let method = match self.source {
                LoginSource::Browser => "execute_browser_task",
                _ => "execute_login_attempt",
            };

            let mut params = self.worker_config.clone();
            params["cancel_id"] = json!(cancel_id.clone());
            if let Some(tid) = &self.task_id {
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
                    .login_timeout
                    .saturating_sub(session_start.elapsed());
                tokio::select! {
                    biased;
                    _ = ct.cancelled() => {
                        bridge.cancel(&cancel_id);
                        self.emit(
                            self.make_cancelled_result(session_start, attempts_used),
                            HistoryResult::Cancelled,
                        )
                        .await;
                        return;
                    }
                    _ = self.shutdown_token.cancelled() => {
                        bridge.cancel(&cancel_id);
                        *recover_lock(self.cancel_reason.as_ref()) =
                            Some("应用关闭".to_string());
                        self.emit(
                            self.make_cancelled_result(session_start, attempts_used),
                            HistoryResult::Cancelled,
                        )
                        .await;
                        return;
                    }
                    _ = sleep(remaining) => {
                        bridge.cancel(&cancel_id);
                        self.emit(
                            self.make_result(
                                false,
                                "登录超时".into(),
                                session_start,
                                attempts_used,
                            ),
                            HistoryResult::Failed,
                        )
                        .await;
                        return;
                    }
                    res = bridge.execute(method, params) => res,
                }
            };

            let structured = match exec {
                Ok(resp) => self.parse_response(resp),
                Err(e) => {
                    warn!("Bridge 执行失败: {e}");
                    self.emit(
                        self.make_result(
                            false,
                            format!("Bridge 执行失败: {e}"),
                            session_start,
                            attempts_used,
                        ),
                        HistoryResult::Failed,
                    )
                    .await;
                    return;
                }
            };

            // 本轮 attempt 结束，清除在途 cancel_id
            self.attempt_cancel_id.store(None);

            match classify(structured.outcome) {
                ResultAction::Terminal(kind) => match kind {
                    TerminalKind::Success => {
                        // 步骤全部成功后做真实网络验证：避免 Worker 假成功（步骤未抛异常
                        // 但页面实际未登录成功）被误报。参考老实现 _check_success：
                        // 等待 5s 让认证生效 → check_once → 仅 Online 才算真成功。
                        let net_ok = self.verify_network_after_login().await;
                        if net_ok {
                            self.emit(
                                self.make_result(
                                    true,
                                    "登录成功".into(),
                                    session_start,
                                    attempts_used,
                                ),
                                HistoryResult::Success,
                            )
                            .await;
                            return;
                        }
                        // 网络验证未通过：不直接判终态失败，而是走可重试路径。
                        // 理由：网络探测可能因瞬时波动误判，重试一次登录比直接判死更稳妥。
                        // 策略与 Worker 返回 NetworkError 时一致（classify → Retry）。
                        warn!(
                            "Worker 返回 Success 但网络验证未通过，转入重试（attempt {}）",
                            attempts_used + 1
                        );
                        // 构造一个 NetworkError 的 structured 以复用下方 Retry 分支
                        let retry_structured = StructuredResult {
                            outcome: Outcome::NetworkError,
                            message: "网络验证未通过".into(),
                            data: structured.data.clone(),
                            screenshot_url: structured.screenshot_url.clone(),
                            duration_ms: structured.duration_ms,
                        };
                        if !self.try_retry(&retry_structured, &mut attempts_used, session_start)
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
                        self.emit(
                            self.make_result(
                                false,
                                structured.message.clone(),
                                session_start,
                                attempts_used,
                            ),
                            HistoryResult::Failed,
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
                    unreachable!("classify() never returns Exhausted")
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
        if *attempts_used >= self.max_retries {
            self.emit(
                self.make_result(
                    false,
                    format!("重试耗尽（共 {} 次）", self.max_retries),
                    session_start,
                    *attempts_used,
                ),
                HistoryResult::Failed,
            )
            .await;
            return false;
        }
        *attempts_used += 1;
        *recover_lock(&self.state) = LoginState::Retrying {
            attempt: *attempts_used,
        };
        self.status_manager.merge(PartialSnapshot::Login {
            status: LoginStatus::Running,
            source: Some(self.source),
            message: Some(format!("重试中 {attempts_used}/{}", self.max_retries)),
            retry_count: *attempts_used,
        });
        if should_force_recycle(structured.outcome) {
            // 强制回收 Worker：直接 kill 当前子进程并标记 Error，
            // 下一次 bridge.execute() 内部的 ensure_worker 会自动重新 spawn。
            // 同步 await（而非 spawn）确保 kill 在重试间隔之前完成，避免下一轮
            // ensure_worker 复用即将被 kill 的 Worker（force_recycle 与 retry 竞态）。
            if let Some(b) = &self.bridge {
                warn!("登录结果 {:?} 触发 Worker 强制回收", structured.outcome);
                b.force_recycle().await;
            }
        }
        let retry_interval = self.retry_interval.max(Duration::from_secs(1));
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

    /// 登录后真实网络验证：等待 5s 让认证生效，再调用 MonitorService 做一次完整探测。
    ///
    /// 仅当探测结果为 [`NetworkStatus::Online`] 时返回 true，其余（CaptivePortal /
    /// Offline / Paused / 探测异常 / Monitor 未注入）均返回 false。
    ///
    /// 与老实现 `BrowserTaskRunner._network_detection_check` 等价：防止 Worker 步骤
    /// 全部成功但页面实际未登录成功（如填入字面量 `{{USERNAME}}` 却没点登录按钮）。
    async fn verify_network_after_login(&self) -> bool {
        let Some(monitor) = self.monitor.as_ref() else {
            warn!("MonitorService 未注入，跳过登录后网络验证（按失败处理）");
            return false;
        };
        // 等待认证服务器处理请求并放行流量
        tokio::time::sleep(Duration::from_secs(5)).await;
        match monitor.check_once().await {
            Ok(report) => {
                use crate::status::NetworkStatus;
                let ok = matches!(report.status, NetworkStatus::Online);
                if ok {
                    info!("登录后网络验证通过：Online");
                } else {
                    warn!(
                        "登录后网络验证未通过：status={:?}, tcp={:?}, http={:?}, url={:?}",
                        report.status, report.tcp_outcome, report.http_outcome, report.url_outcome
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
            source: self.source,
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
            source: self.source,
            duration: start.elapsed(),
            attempts: attempts_used + 1,
        }
    }

    /// 终态收尾：写入共享结果槽 → 广播状态 → 记录历史 → 更新内部状态
    async fn emit(&self, result: LoginResult, history: HistoryResult) {
        self.result_slot.set_result(result.clone());

        // 统计登录终态指标
        if let Some(m) = &self.metrics {
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
        self.status_manager.merge(PartialSnapshot::Login {
            status,
            source: None,
            message: Some(msg),
            retry_count: result.attempts.saturating_sub(1),
        });

        let entry = LoginHistoryEntry {
            timestamp: chrono::Local::now(),
            source: result.source,
            profile_id: self.profile_id.clone(),
            result: history,
            message: result.message.clone(),
            duration_secs: result.duration.as_secs_f64(),
        };
        if let Err(e) = self.history_service.record(&entry).await {
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
        debug!("登录会话结束: source={:?} success={}", result.source, result.success);
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
    fn test_classify_captcha_failed_is_terminal_failed() {
        // 验证码失败当前硬编码为终态（DOC-2）
        assert_eq!(
            classify(Outcome::CaptchaFailed),
            ResultAction::Terminal(TerminalKind::Failed)
        );
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
    fn test_should_force_recycle_unknown_error() {
        // UnknownError 语义不明，保守回收
        assert!(should_force_recycle(Outcome::UnknownError));
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
}
