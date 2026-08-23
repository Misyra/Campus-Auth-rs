//! 登录编排：LoginOrchestrator 公共接口 + 子模块 re-export
//!
//! 本模块实现登录统一入口：配置校验 → auth_url 预检 → 去重/抢占判断 → 创建会话 →
//! 驱动状态机 → 返回 [`LoginHandle`]。活跃会话由 `Mutex<OrchestratorState>` 保护，
//! 同一时刻最多一个。底层 Worker 执行经 `BridgeSupervisor` 完成，历史经
//! [`LoginHistoryService`] 落盘。
//!
//! 全部依赖在 `new` 构造时注入（无 setter），依赖关系在组装期即确定。

pub mod history;
pub mod preemption;
pub mod session;

use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::net::TcpStream;
use tokio::sync::{watch, Mutex as AsyncMutex};
use tokio::time::timeout as tokio_timeout;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::bridge::BridgeSupervisor;
use crate::config::runtime::ProfileSnapshot;
use crate::config::runtime::RuntimeConfig;
use crate::config::ConfigService;
use crate::environment::EnvironmentManager;
use crate::status::{LoginStatus, PartialSnapshot, StatusManager};
use crate::tasks::TaskManager;
use crate::utils::metrics::Metrics;

/// 结果共享槽（句柄与状态机共享，支持 [`LoginHandle`] 可克隆）
///
/// 使用 `watch` channel 替代 `Notify`，避免无等待者时通知丢失导致 `await_result` 永久挂起。
#[derive(Debug)]
pub(crate) struct LoginHandleInner {
    /// watch sender：写入 `Some(result)` 即表示终态就绪
    result_tx: watch::Sender<Option<LoginResult>>,
}

impl LoginHandleInner {
    /// 写入终态结果（所有已订阅的 `await_result` 等待者会被唤醒）
    ///
    /// 使用 `send_modify` 而非 `send`，即使当前无 receiver 也不会失败，
    /// 后续 `subscribe()` 仍能看到已写入的值。
    pub(crate) fn set_result(&self, r: LoginResult) {
        self.result_tx.send_modify(|val| *val = Some(r));
    }

    /// 非阻塞读取当前结果（None 表示尚未完成）
    pub(crate) fn peek(&self) -> Option<LoginResult> {
        self.result_tx.borrow().clone()
    }
}

/// 中毒锁恢复：锁被 Poison 时取回内部数据而非 panic
pub(crate) fn recover_lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 登录提交后返回的控制句柄（可克隆，多次复用共享同一终态结果）
#[derive(Clone, Debug)]
pub struct LoginHandle {
    /// 登录来源
    source: LoginSource,
    /// 会话级取消令牌
    cancel_token: CancellationToken,
    /// 结果共享槽
    inner: Arc<LoginHandleInner>,
}

impl LoginHandle {
    /// 触发取消（会话状态机退出后，`await_result` 收到取消终态）
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }

    /// 结果是否已就绪（非阻塞）
    pub fn done(&self) -> bool {
        self.inner.peek().is_some()
    }

    /// 异步等待终态结果
    ///
    /// 通过 `watch::Receiver::wait_for` 等待，不存在通知丢失的竞态问题。
    /// 若 watch channel 关闭（会话 task 异常退出未写入终态），不再 panic，
    /// 而是返回一个失败结果，让调用方按“未成功”处理。
    pub async fn await_result(&self) -> LoginResult {
        let mut rx = self.inner.result_tx.subscribe();
        // 如果结果已就绪则立即返回；否则阻塞等待直到收到 Some。
        // wait_for 在 sender 全部 drop 时返回 Err —— 此处用 match 替代双重 unwrap，
        // channel 关闭时回退为 None，由 unwrap_or_else 兜底为失败结果。
        match rx.wait_for(Option::is_some).await {
            Ok(v) => v.clone(),
            Err(_) => None,
        }
        .unwrap_or_else(|| LoginResult {
            success: false,
            message: "登录结果通道关闭".into(),
            source: self.source,
            duration: Duration::ZERO,
            attempts: 0,
        })
    }

    /// 返回此句柄的来源
    pub fn source(&self) -> LoginSource {
        self.source
    }

    /// 由既有终态结果构造「立即终态」句柄
    ///
    /// 结果创建时已就绪：`await_result` 立即返回、`cancel` 无副作用。
    /// 供校验失败等无需真正执行的场景，及测试 mock 构造（M1）。
    pub fn immediate(result: LoginResult) -> Self {
        let source = result.source;
        let (result_tx, _rx) = watch::channel(Some(result));
        Self {
            source,
            cancel_token: CancellationToken::new(),
            inner: Arc::new(LoginHandleInner { result_tx }),
        }
    }
}

/// 活跃会话记录（位于 `OrchestratorState` 中）
struct ActiveSession {
    /// 登录来源
    source: LoginSource,
    /// 会话唯一 ID（用于完成时回填校验）
    session_id: u64,
    /// 会话级取消令牌
    cancel_token: CancellationToken,
    /// 当前在途 attempt 的 cancel_id（供取消传播读取）
    attempt_cancel_id: Arc<ArcSwapOption<String>>,
    /// 取消原因（供终态消息使用）
    cancel_reason: Arc<StdMutex<Option<String>>>,
    /// 对外句柄（去重时克隆返回）
    handle: LoginHandle,
}

/// 编排器内部状态（由 `tokio::sync::Mutex` 保护，跨 await 安全）
struct OrchestratorState {
    /// 当前活跃会话（同一时刻最多一个）
    active_session: Option<ActiveSession>,
    /// 会话 ID 自增计数器
    next_session_id: u64,
}

/// Web 层消费的登录编排抽象（M1 细粒度 state 第二域）
///
/// handler 通过 `State<Arc<dyn LoginApi>>` 提取依赖，测试可注入内存实现
/// （配合 [`LoginHandle::immediate`] 构造立即终态句柄）。
#[async_trait::async_trait]
pub trait LoginApi: Send + Sync {
    /// 提交登录会话，返回控制句柄（校验失败等以立即终态句柄体现，不返回 Result）
    async fn submit(
        &self,
        source: LoginSource,
        task_id: Option<String>,
        profile_id: Option<String>,
    ) -> LoginHandle;

    /// 取消当前登录会话（等待状态锁，不静默丢弃取消）
    async fn cancel_current(&self);
}

#[async_trait::async_trait]
impl LoginApi for LoginOrchestrator {
    async fn submit(
        &self,
        source: LoginSource,
        task_id: Option<String>,
        profile_id: Option<String>,
    ) -> LoginHandle {
        LoginOrchestrator::submit(self, source, task_id, profile_id).await
    }

    async fn cancel_current(&self) {
        LoginOrchestrator::cancel_current(self).await
    }
}

/// 登录统一入口（编排器）
pub struct LoginOrchestrator {
    /// 配置服务（读取运行时配置快照）
    config: Arc<ConfigService>,
    /// 历史服务（终态写入）
    history: Arc<LoginHistoryService>,
    /// 状态管理器（广播登录状态）
    status: Arc<StatusManager>,
    /// Bridge 句柄（构造注入）
    bridge: Arc<BridgeSupervisor>,
    /// 环境管理器（浏览器能力预检用）
    environment: Arc<EnvironmentManager>,
    /// 任务管理器（提供浏览器任务的步骤配置）
    tasks: Arc<TaskManager>,
    /// 网络监测服务（登录后网络验证用）
    monitor: Arc<crate::monitor::MonitorService>,
    /// 内部状态（活跃会话 + ID 计数器）
    state: Arc<AsyncMutex<OrchestratorState>>,
    /// 运行指标（可选）
    metrics: Option<Arc<Metrics>>,
    /// 应用级 shutdown 信号（会话在 shutdown 时立即退出）
    shutdown_token: CancellationToken,
}

impl LoginOrchestrator {
    /// 构造登录编排器
    ///
    /// 全部依赖通过构造注入（无 setter）：依赖关系在组装期即确定，
    /// 避免运行时未注入导致的隐性空值路径。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<ConfigService>,
        history: Arc<LoginHistoryService>,
        status: Arc<StatusManager>,
        bridge: Arc<BridgeSupervisor>,
        environment: Arc<EnvironmentManager>,
        tasks: Arc<TaskManager>,
        monitor: Arc<crate::monitor::MonitorService>,
        shutdown_token: CancellationToken,
        metrics: Option<Arc<Metrics>>,
    ) -> Self {
        Self {
            config,
            history,
            status,
            bridge,
            environment,
            tasks,
            monitor,
            state: Arc::new(AsyncMutex::new(OrchestratorState {
                active_session: None,
                next_session_id: 0,
            })),
            metrics,
            shutdown_token,
        }
    }

    /// 提交一次登录，返回控制句柄
    ///
    /// 流程：配置校验 → auth_url 预检（仅 manual/login_once）→ 去重/抢占判断 →
    /// 创建会话并 `spawn` 状态机 → 返回 [`LoginHandle`]。本方法不返回 `Result`：
    /// 校验失败等会以“立即终态失败”的句柄体现。
    pub async fn submit(
        &self,
        source: LoginSource,
        task_id: Option<String>,
        profile_id: Option<String>,
    ) -> LoginHandle {
        // 读取最新运行时配置（每次 submit 重新读取，不缓存）
        let rt = self.config.runtime().load_full();
        // 解析凭据来源 Profile：profile_id 指定时加载该 Profile 快照，否则用全局活跃 Profile。
        // 多 Profile 场景下的定时浏览器任务可借此使用各自独立的账号凭据。
        let resolved_profile: ProfileSnapshot = match &profile_id {
            Some(pid) if !pid.is_empty() => match self.config.runtime_config_for_profile(pid) {
                Ok(rc) => rc.profile,
                Err(e) => {
                    warn!("加载指定 Profile {pid} 失败，回退全局活跃 Profile: {e}");
                    rt.profile.clone()
                }
            },
            _ => rt.profile.clone(),
        };
        let profile = &resolved_profile;

        // 全局唯一起活跃任务：任务页「使用」设置的 `.order.json.active`（TaskManager 层）。
        // 手动/自动/CLI 登录（task_id 为空）统一走它；定时任务各自携带独立 task_id，不受影响。
        let global_active_task = self.tasks.get_active_task().await;

        // 1. 配置完整性校验
        let mut missing = Vec::new();
        if profile.username.is_empty() {
            missing.push("username");
        }
        if profile.password.as_str().is_empty() {
            missing.push("password");
        }
        if profile.auth_url.is_empty() {
            missing.push("auth_url");
        }
        if !matches!(source, LoginSource::Browser) && task_id.is_none() && global_active_task.is_empty() {
            missing.push("active_task");
        }
        if source == LoginSource::Browser && task_id.is_none() && global_active_task.is_empty() {
            missing.push("task");
        }
        if !missing.is_empty() {
            let msg = missing.join(", ");
            warn!("登录配置不完整，缺少字段: {msg}（source={source:?}）");
            return self.immediate_handle(source, false, format!("配置不完整: {msg}"), profile.id.clone()).await;
        }

        // 浏览器来源要求环境能力就绪
        if source == LoginSource::Browser && !self.environment.capability_ready() {
            warn!("浏览器能力未就绪，无法执行定时任务");
            return self.immediate_handle(
                source,
                false,
                "浏览器能力未就绪，无法执行定时任务".into(),
                profile.id.clone(),
            )
            .await;
        }

        // 2. auth_url TCP 预检（仅 manual / login_once）
        if matches!(source, LoginSource::Manual | LoginSource::LoginOnce) {
            let timeout = Duration::from_secs(rt.monitor.auth_url_timeout as u64);
            if self.check_auth_url(&profile.auth_url, timeout).await.is_err() {
                warn!("auth_url 预检不可达: {}", profile.auth_url);
                return self.immediate_handle(
                    source,
                    false,
                    format!("auth_url 不可达: {}", profile.auth_url),
                    profile.id.clone(),
                )
                .await;
            }
        }

        // 3. 去重/抢占判断（决策与 take 在同一把锁内完成，避免 TOCTOU 竞态）
        let old_session = {
            let mut guard = self.state.lock().await;
            let decision = match &guard.active_session {
                None => PreemptionDecision::Create,
                Some(active) => decide(source, Some(active.source), Some(active.handle.clone())),
            };
            match decision {
                PreemptionDecision::Reuse(handle) => return handle,
                PreemptionDecision::Preempt => guard.active_session.take(),
                PreemptionDecision::Create => None,
            }
        };
        // 锁已释放，安全执行异步取消
        if let Some(old) = old_session {
            old.cancel_token.cancel();
            *recover_lock(old.cancel_reason.as_ref()) = Some("被更高优先级登录抢占".to_string());
            if let Some(cid) = old.attempt_cancel_id.load_full() {
                self.bridge.cancel(cid.as_str());
            }
            let _ = tokio_timeout(Duration::from_secs(5), old.handle.await_result()).await;
        }

        // 4. 创建新会话
        // 统计登录次数
        if let Some(m) = &self.metrics {
            m.inc_login();
        }
        let session_id = {
            let mut g = self.state.lock().await;
            let id = g.next_session_id;
            g.next_session_id += 1;
            id
        };
        let cancel_token = CancellationToken::new();
        let attempt_cancel_id = Arc::new(ArcSwapOption::<String>::new(None));
        let cancel_reason = Arc::new(StdMutex::new(None::<String>));
        let shutdown_token = self.shutdown_token.clone();
        let (result_tx, _rx) = watch::channel(None);
        let result_slot = Arc::new(LoginHandleInner { result_tx });
        let handle = LoginHandle {
            source,
            cancel_token: cancel_token.clone(),
            inner: result_slot.clone(),
        };

        let effective_task_id = task_id.or_else(|| {
            if global_active_task.is_empty() {
                None
            } else {
                Some(global_active_task.clone())
            }
        });

        let worker_config = self
            .build_worker_config(&rt, &resolved_profile, effective_task_id.as_deref().unwrap_or(""))
            .await;

        let session = LoginSession::new(
            source,
            effective_task_id,
            cancel_token.clone(),
            rt.retry.max_retries,
            Duration::from_secs(rt.retry.retry_interval as u64),
            Duration::from_secs(rt.browser.login_timeout as u64),
            profile.id.clone(),
            worker_config,
            result_slot,
            attempt_cancel_id.clone(),
            shutdown_token,
            cancel_reason.clone(),
            Some(self.bridge.clone()),
            Some(self.monitor.clone()),
            self.config.clone(),
            self.status.clone(),
            self.history.clone(),
            self.metrics.clone(),
        );

        // 写入活跃会话；若抢占等待期间已有并发会话写入，则放弃本会话，避免泄漏
        let became_active = {
            let mut g = self.state.lock().await;
            if g.active_session.is_none() {
                g.active_session = Some(ActiveSession {
                    source,
                    session_id,
                    cancel_token,
                    attempt_cancel_id,
                    cancel_reason,
                    handle: handle.clone(),
                });
                true
            } else {
                false
            }
        };

        // 5. 仅当成功占据活跃会话槽位时才 spawn 状态机 task
        if became_active {
            let state_arc = self.state.clone();
            tokio::spawn(async move {
                session.run().await;
                let mut g = state_arc.lock().await;
                let should_clear =
                    matches!(&g.active_session, Some(a) if a.session_id == session_id);
                if should_clear {
                    g.active_session = None;
                }
            });
        } else {
            // 活跃槽位已被占用，立即写入终态（避免 await_result 永久挂起）
            handle.inner.set_result(LoginResult {
                success: false,
                message: "被更新的登录请求取代".into(),
                source,
                duration: Duration::ZERO,
                attempts: 0,
            });
        }

        // 6. 返回句柄
        handle
    }

/// 取消当前在途登录（Web API `POST /api/login/cancel`）
    ///
    /// 使用 `lock().await` 等待锁（锁窗口极短），避免撞上 `submit` 持锁窗口时
    /// 用户取消被静默丢弃（原 `try_lock` 会跳过取消，表现为点取消没反应）。
    pub async fn cancel_current(&self) {
        let guard = self.state.lock().await;
        if let Some(active) = &guard.active_session {
            active.cancel_token.cancel();
            *recover_lock(active.cancel_reason.as_ref()) = Some("用户取消".to_string());
            if let Some(cid) = active.attempt_cancel_id.load_full() {
                self.bridge.cancel(cid.as_str());
            }
        }
    }

    /// 仅取消 `source=Auto` 的活跃会话（Engine 崩溃清理）
    ///
    /// 遍历活跃会话，仅对 `Auto` 来源触发取消；manual/login_once/browser 不受影响。
    /// 使用异步 `lock().await` 避免锁竞争时静默跳过（原 `try_lock` 在 Engine 崩溃时
    /// 可能漏取消 auto 会话）。
    pub async fn cancel_auto_pending(&self, reason: &str) {
        let guard = self.state.lock().await;
        if let Some(active) = &guard.active_session {
            if active.source == LoginSource::Auto {
                active.cancel_token.cancel();
                *recover_lock(active.cancel_reason.as_ref()) = Some(reason.to_string());
                if let Some(cid) = active.attempt_cancel_id.load_full() {
                    self.bridge.cancel(cid.as_str());
                }
            }
        }
    }

    /// 查询当前登录状态
    pub fn status(&self) -> LoginStatus {
        self.status.borrow().login_status
    }

    /// 构造“立即终态”的句柄（用于校验失败等无需真正执行的场景）
    ///
    /// L2：async 化并直接 await 历史写入，避免 spawn 后台任务在
    /// graceful_shutdown 时被截断导致历史丢失。
    async fn immediate_handle(
        &self,
        source: LoginSource,
        success: bool,
        message: String,
        profile_id: String,
    ) -> LoginHandle {
        // 与 LoginSession::finish 终态广播保持一致：立即终态同样要合并到
        // StatusManager，否则配置校验失败等场景前端状态停留在 Idle/Running，
        // 用户无从得知失败原因（M4：状态更新协议统一，所有终态必经广播）
        let status = if success {
            LoginStatus::Success
        } else {
            LoginStatus::Failed
        };
        self.status.merge(PartialSnapshot::Login {
            status,
            source: Some(source),
            message: Some(message.clone()),
            retry_count: 0,
        });
        let entry = LoginHistoryEntry {
            timestamp: chrono::Local::now(),
            source,
            profile_id,
            result: if success {
                HistoryResult::Success
            } else {
                HistoryResult::Failed
            },
            message: message.clone(),
            duration_secs: 0.0,
        };
        if let Err(e) = self.history.record(&entry).await {
            warn!("登录历史写入失败: {e}");
        }
        LoginHandle::immediate(LoginResult {
            success,
            message,
            source,
            duration: Duration::ZERO,
            attempts: 0,
        })
    }

    /// 构造发送给 Worker 的配置字典（凭证、auth_url、浏览器设置等）
    ///
    /// 浏览器设置整体序列化 [`RuntimeConfig::browser`]（`BrowserSettings`）注入
    /// `browser_settings` 键——这是跨 IPC 边界与 Python Worker 约定的键名
    /// （Rust 内部字段名为 `browser`），覆盖原手动拼字段的丢失问题。
    /// `bind_proxy`（浏览器代理）由 `BrowserSettings` 携带，随配置一并下发 Worker。
    /// 构造发送给 Worker 的配置字典（凭证、auth_url、浏览器设置、任务步骤等）
    ///
    /// 浏览器设置整体序列化 [`RuntimeConfig::browser`]（`BrowserSettings`）注入
    /// `browser_settings` 键——这是跨 IPC 边界与 Python Worker 约定的键名
    /// （Rust 内部字段名为 `browser`），覆盖原手动拼字段的丢失问题。
    /// `bind_proxy`（浏览器代理）由 `BrowserSettings` 携带，随配置一并下发 Worker。
    ///
    /// **关键修复**：按 `task_id` 从 [`TaskManager`] 加载浏览器任务的 [`TaskConfig`]，
    /// 序列化为 `task_config` 键一并发送。Python Worker 仅依据 `task_config.steps`
    /// 执行步骤——缺失该键会导致浏览器打开认证页却不执行任何输入（假登录）。
    /// 加载失败或任务非浏览器类型时仅告警、不嵌入，由 Python 侧按空步骤处理。
    async fn build_worker_config(
        &self,
        rt: &RuntimeConfig,
        profile: &ProfileSnapshot,
        task_id: &str,
    ) -> serde_json::Value {
        let mut cfg = serde_json::json!({
            "username": profile.username,
            "password": profile.password.as_str(),
            "auth_url": profile.auth_url,
            "isp": profile.isp,
            "gateway_ip": profile.gateway_ip,
            "wifi_ssid": profile.wifi_ssid,
            "active_task": task_id,
        });
        // 整体序列化 BrowserSettings，避免字段遗漏
        if let Ok(browser) = serde_json::to_value(&rt.browser) {
            cfg["browser_settings"] = browser;
        }
        // 加载浏览器任务配置并嵌入 task_config（Worker 执行步骤的唯一依据）。
        // 失败时 embed_task_config 内部告警，不嵌入（Worker 按空步骤处理）
        self.tasks.embed_task_config(task_id, &mut cfg).await;
        cfg
    }

    /// auth_url TCP 预检：解析 host:port 并限时连接
    async fn check_auth_url(&self, auth_url: &str, timeout: Duration) -> Result<(), ()> {
        let Some(addr) = parse_host_port(auth_url) else {
            return Err(());
        };
        match tokio_timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(_)) => Ok(()),
            _ => Err(()),
        }
    }
}

/// 从 URL 解析 `host:port`（无端口时按协议推断 80/443）
///
/// 正确处理 IPv6 方括号格式（如 `http://[::1]:8080/login`）。
fn parse_host_port(url: &str) -> Option<String> {
    let without_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hostport = without_scheme.split('/').next().unwrap_or(without_scheme);
    // 处理 IPv6 方括号格式：[host]:port
    if let Some(rest) = hostport.strip_prefix('[') {
        if let Some((host, port_str)) = rest.split_once("]:") {
            if port_str.parse::<u16>().is_ok() {
                return Some(format!("[{host}]:{port_str}"));
            }
        }
        // 有方括号但无端口，去除方括号后拼接推断端口
        let host = rest.strip_suffix(']').unwrap_or(rest);
        let is_https = url.starts_with("https://");
        let port: u16 = if is_https { 443 } else { 80 };
        return Some(format!("[{host}]:{port}"));
    }
    if let Some((host, port)) = hostport.rsplit_once(':') {
        if port.parse::<u16>().is_ok() {
            return Some(format!("{host}:{port}"));
        }
    }
    let is_https = url.starts_with("https://");
    let port: u16 = if is_https { 443 } else { 80 };
    Some(format!("{hostport}:{port}"))
}

// 重新导出公共类型，供 `crate::login::*` 引用
pub use crate::status::LoginSource;
pub use crate::bridge::{Outcome as LoginOutcome, StructuredResult};
pub use history::{HistoryResult, LoginHistoryEntry, LoginHistoryService, HistoryStore};
pub use preemption::{decide, PreemptionDecision};
pub use session::{LoginResult, LoginSession, LoginState, ResultAction, TerminalKind};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::LoginSource;
    use tokio_util::sync::CancellationToken;

    // ============ parse_host_port 纯函数测试 ============

    #[test]
    fn test_parse_host_port_http_default_80() {
        // http 无显式端口 → 80
        assert_eq!(
            parse_host_port("http://example.com/login"),
            Some("example.com:80".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_https_default_443() {
        // https 无显式端口 → 443
        assert_eq!(
            parse_host_port("https://example.com"),
            Some("example.com:443".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_explicit_port_with_path() {
        // 显式端口 + 路径 + 查询参数：仅保留 host:port
        assert_eq!(
            parse_host_port("http://example.com:8080/login?next=/home"),
            Some("example.com:8080".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_no_scheme_with_port() {
        // 无 scheme 但含端口 → 按显式端口解析
        assert_eq!(
            parse_host_port("example.com:8080"),
            Some("example.com:8080".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_no_scheme_no_port_defaults_80() {
        // 无 scheme 无端口 → 默认 80（非 https）
        assert_eq!(
            parse_host_port("example.com"),
            Some("example.com:80".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_ipv6_with_port() {
        // IPv6 方括号 + 端口
        assert_eq!(
            parse_host_port("http://[::1]:8080/login"),
            Some("[::1]:8080".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_ipv6_no_port_http() {
        // IPv6 方括号无端口 → http 80
        assert_eq!(
            parse_host_port("http://[::1]/login"),
            Some("[::1]:80".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_ipv6_no_port_https() {
        // IPv6 方括号无端口 → https 443
        assert_eq!(
            parse_host_port("https://[2001:db8::1]"),
            Some("[2001:db8::1]:443".to_string())
        );
    }

    #[test]
    fn test_parse_host_port_https_with_explicit_port() {
        // https + 显式端口：保留显式端口（不强制 443）
        assert_eq!(
            parse_host_port("https://example.com:8443/api"),
            Some("example.com:8443".to_string())
        );
    }

    // ============ decide Reuse 分支测试（需 LoginHandle，字段为 mod.rs 私有） ============

    /// 构造测试用 LoginHandle（复用 watch channel）
    fn make_handle(source: LoginSource) -> LoginHandle {
        let (result_tx, _rx) = watch::channel(None);
        LoginHandle {
            source,
            cancel_token: CancellationToken::new(),
            inner: Arc::new(LoginHandleInner { result_tx }),
        }
    }

    #[test]
    fn test_decide_same_auto_with_handle_reuses() {
        // 同来源 Auto + 有句柄 → Reuse（去重）
        let handle = make_handle(LoginSource::Auto);
        let d = decide(
            LoginSource::Auto,
            Some(LoginSource::Auto),
            Some(handle.clone()),
        );
        match d {
            PreemptionDecision::Reuse(h) => assert_eq!(h.source(), LoginSource::Auto),
            other => panic!("期望 Reuse，得到 {:?}", other),
        }
    }

    #[test]
    fn test_decide_lower_priority_with_handle_reuses() {
        // 新来源优先级更低 + 有句柄 → Reuse（让高优先级会话继续）
        // Auto(1) vs Manual(2)
        let handle = make_handle(LoginSource::Manual);
        let d = decide(
            LoginSource::Auto,
            Some(LoginSource::Manual),
            Some(handle.clone()),
        );
        assert!(matches!(d, PreemptionDecision::Reuse(_)));
        // Manual(2) vs Browser(3)
        let handle2 = make_handle(LoginSource::Browser);
        let d2 = decide(
            LoginSource::Manual,
            Some(LoginSource::Browser),
            Some(handle2),
        );
        assert!(matches!(d2, PreemptionDecision::Reuse(_)));
    }

    #[test]
    fn test_decide_cross_source_lower_priority_never_preempts() {
        // 跨来源且新优先级更低 → 必为 Reuse 或 Create，绝不 Preempt
        let handle = make_handle(LoginSource::LoginOnce);
        let d = decide(
            LoginSource::Auto,
            Some(LoginSource::LoginOnce),
            Some(handle),
        );
        assert!(!matches!(d, PreemptionDecision::Preempt));
    }

    // ============ recover_lock 中毒恢复测试 ============

    #[test]
    fn test_recover_lock_normal_access() {
        // 正常锁可访问内部数据
        let m = StdMutex::new(42u32);
        let g = recover_lock(&m);
        assert_eq!(*g, 42);
    }

    // ============ B2: cancel_current 可等待锁 ============

    /// 构造一个最小可用的 LoginOrchestrator 测试实例。
    ///
    /// 仅依赖 config/status/bridge 等可空构造的依赖，其余用 dummy 填充；
    /// 测试只调用 cancel_current，不触碰其它字段。
    async fn make_orchestrator() -> Arc<LoginOrchestrator> {
        use crate::config::{ConfigService, ProfileService};
        use crate::environment::EnvironmentManager;
        use crate::login::history::LoginHistoryService;
        use crate::monitor::MonitorService;
        use crate::network::detect::create_detector;
        use crate::tasks::TaskManager;
        use crate::utils::metrics::Metrics;

        let dir = tempfile::TempDir::new().unwrap();
        let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
        let config = ConfigService::new(dir.path().to_path_buf(), reload_tx)
            .await
            .expect("ConfigService 构造失败");
        let status = Arc::new(StatusManager::new());
        let bridge = crate::bridge::BridgeSupervisor::new(
            dir.path().to_path_buf(),
            config.clone(),
            status.clone(),
            None,
        );
        let environment = EnvironmentManager::new(
            dir.path().to_path_buf(),
            status.clone(),
            config.runtime().load().app.developer_mode,
        );
        let tasks = TaskManager::new(dir.path(), config.clone());
        let history = LoginHistoryService::new(dir.path());
        let detector = create_detector();
        let monitor = Arc::new(
            MonitorService::new(config.clone(), detector.clone(), None, Some(Metrics::new()))
                .expect("MonitorService 构造失败"),
        );
        let _ = Arc::new(ProfileService::new(config.clone()));

        Arc::new(LoginOrchestrator::new(
            config,
            Arc::new(history),
            status,
            bridge,
            environment,
            tasks,
            monitor,
            CancellationToken::new(),
            Some(Metrics::new()),
        ))
    }

    #[tokio::test]
    async fn test_cancel_current_waits_for_lock() {
        // B2：cancel_current 使用 lock().await，持锁状态下能等到锁而非放弃。
        // 模拟：先持锁（模拟 submit 持锁窗口），再在独立任务中调用 cancel_current，
        // 释放锁后取消应成功传播到活跃会话。
        let orch = make_orchestrator().await;

        // 预置活跃会话
        {
            let mut state = orch.state.lock().await;
            state.active_session = Some(ActiveSession {
                session_id: 1,
                source: LoginSource::Manual,
                cancel_token: CancellationToken::new(),
                cancel_reason: Arc::new(StdMutex::new(None)),
                attempt_cancel_id: Arc::new(arc_swap::ArcSwapOption::new(Some(
                    Arc::new("cid-1".to_string()),
                ))),
                handle: make_handle(LoginSource::Manual),
            });
        }

        // 模拟 submit 持锁窗口：持有锁，再在独立任务中调用 cancel_current
        let lock_guard = orch.state.lock().await;
        let orch_for_task = orch.clone();
        let cancel_task = tokio::spawn(async move {
            // cancel_current 会 await 锁，因此此处不会立即返回，必须等锁释放
            orch_for_task.cancel_current().await;
            true
        });
        // 短暂等待，确认 cancel_task 尚未完成（在等待锁）
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!cancel_task.is_finished(), "cancel_current 应等待锁而非放弃");
        // 释放锁，cancel_current 应能拿到锁并完成取消
        drop(lock_guard);
        let done = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            cancel_task,
        )
        .await
        .expect("cancel_current 应在锁释放后完成")
        .unwrap();
        assert!(done);

        // 验证取消已传播到活跃会话
        let state = orch.state.lock().await;
        let active = state.active_session.as_ref().unwrap();
        assert!(active.cancel_token.is_cancelled());
        assert_eq!(
            *active.cancel_reason.as_ref().lock().unwrap_or_else(|e| e.into_inner()),
            Some("用户取消".to_string())
        );
    }
}
