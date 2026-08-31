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
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::time::timeout as tokio_timeout;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::bridge::BridgeSupervisor;

use crate::config::ConfigService;
use crate::config::runtime::ProfileSnapshot;
use crate::config::runtime::RuntimeConfig;
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
    /// 会话任务完全收尾通知（`session.run()` 返回后 `notify_one`）
    ///
    /// 区别于 `handle.await_result()`（emit 中 `set_result` 后即返回，随后的
    /// `close_browser` 仍在途）：本通知在**全部收尾动作**（含 close_browser）
    /// 完成后触发，供抢占方等待旧会话完全退出再放行新会话（历史遗留 F6）。
    /// `Notify::notify_one` 无等待者时会存储许可，旧会话先于等待结束的场景
    /// 不丢失通知、也不会挂起（防死锁）。
    finished: Arc<tokio::sync::Notify>,
}

impl ActiveSession {
    /// 取消传播三连：会话取消令牌 → 记录取消原因 → 取消在途 attempt
    ///
    /// 收敛 submit 抢占 / cancel_current / cancel_auto_pending 三处同构样板
    /// （原三处逐字重复 cancel + recover_lock + attempt_cancel_id → bridge.cancel）。
    fn propagate_cancel(&self, bridge: &BridgeSupervisor, reason: &str) {
        self.cancel_token.cancel();
        *recover_lock(self.cancel_reason.as_ref()) = Some(reason.to_string());
        if let Some(cid) = self.attempt_cancel_id.load_full() {
            bridge.cancel(cid.as_str());
        }
    }
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
    /// submit 串行门：覆盖“抢占决策 → 等旧会话完全收尾 → 新会话占槽”的整个窗口。
    ///
    /// 与 `state` 锁分离，因此等待旧会话的最长 13s 期间不会阻塞 cancel_current；
    /// 只阻止其他 submit 趁 active_session 被 take 后的空窗插队，避免优先级反转。
    submit_gate: AsyncMutex<()>,
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
            submit_gate: AsyncMutex::new(()),
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
        if !matches!(source, LoginSource::Browser)
            && task_id.is_none()
            && global_active_task.is_empty()
        {
            missing.push("active_task");
        }
        if source == LoginSource::Browser && task_id.is_none() && global_active_task.is_empty() {
            missing.push("task");
        }
        if !missing.is_empty() {
            let msg = missing.join(", ");
            warn!("登录配置不完整，缺少字段: {msg}（source={source:?}）");
            return self
                .immediate_handle(
                    source,
                    false,
                    format!("配置不完整: {msg}"),
                    profile.id.clone(),
                )
                .await;
        }

        // 浏览器来源要求环境能力就绪：未就绪时自动触发 uv sync 初始化（经 BootstrapGate 幂等），
        // 仍未就绪则以失败终态返回（携带 last_error 便于前端提示并引导至“初始化 Python 环境”按钮）。
        // Manual 场景不走此分支——其 Worker 缺失会在会话内 Bridge 执行阶段以 WorkerNotInstalled 失败，
        // 已在 session 层统一处理；此处仅守 Browser 定时任务路径。
        if source == LoginSource::Browser && !self.environment.capability_ready() {
            tracing::info!("浏览器能力未就绪，尝试自动初始化环境...");
            if let Err(e) = self.environment.ensure_capability().await {
                let detail = self
                    .environment
                    .status()
                    .last_error
                    .unwrap_or_else(|| e.to_string());
                warn!("浏览器任务环境自动初始化失败: {detail}");
                return self
                    .immediate_handle(
                        source,
                        false,
                        format!("浏览器能力未就绪，自动初始化失败: {detail}"),
                        profile.id.clone(),
                    )
                    .await;
            }
            if !self.environment.capability_ready() {
                let detail = self
                    .environment
                    .status()
                    .last_error
                    .unwrap_or_else(|| "未知原因".to_string());
                warn!("环境初始化完成但仍未就绪: {detail}");
                return self
                    .immediate_handle(
                        source,
                        false,
                        format!("浏览器能力未就绪（初始化后仍未就绪）: {detail}"),
                        profile.id.clone(),
                    )
                    .await;
            }
            tracing::info!("浏览器任务环境自动初始化成功，继续执行登录");
        }

        // 手动登录同样自动初始化：未安装时直接拒绝会让全新安装用户无从操作。
        // 复用同一 BootstrapGate，显式按钮与登录并发时只跑一次 uv sync。
        if matches!(source, LoginSource::Manual | LoginSource::LoginOnce)
            && !self.environment.capability_ready()
        {
            tracing::info!("手动登录触发环境自动初始化...");
            self.status.merge(crate::status::PartialSnapshot::Login {
                status: crate::status::LoginStatus::Running,
                source: Some(source),
                message: Some("正在初始化 Python 环境...".into()),
                retry_count: 0,
            });
            if let Err(e) = self.environment.ensure_capability().await {
                let detail = self
                    .environment
                    .status()
                    .last_error
                    .unwrap_or_else(|| e.to_string());
                warn!("手动登录环境自动初始化失败: {detail}");
                return self
                    .immediate_handle(
                        source,
                        false,
                        format!("环境未就绪，自动初始化失败: {detail}"),
                        profile.id.clone(),
                    )
                    .await;
            }
            if !self.environment.capability_ready() {
                let detail = self
                    .environment
                    .status()
                    .last_error
                    .unwrap_or_else(|| "未知原因".to_string());
                return self
                    .immediate_handle(
                        source,
                        false,
                        format!("环境初始化后仍未就绪: {detail}"),
                        profile.id.clone(),
                    )
                    .await;
            }
            tracing::info!("手动登录环境自动初始化成功，继续执行登录");
        }

        // 2. auth_url TCP 预检（仅 manual / login_once）
        // 地址解析统一走 MonitorService 的单点实现（parse_url_host_port，
        // 支持 IPv6 方括号与裸地址），登录侧不再维护私有副本
        if matches!(source, LoginSource::Manual | LoginSource::LoginOnce) {
            let timeout = Duration::from_secs(rt.monitor.auth_url_timeout as u64);
            if !self
                .monitor
                .check_auth_url(&profile.auth_url, timeout)
                .await
            {
                warn!("auth_url 预检不可达: {}", profile.auth_url);
                return self
                    .immediate_handle(
                        source,
                        false,
                        format!("auth_url 不可达: {}", profile.auth_url),
                        profile.id.clone(),
                    )
                    .await;
            }
        }

        // 3. 从抢占决策开始串行化所有 submit，直到新会话真正占据 active_session。
        // 不能只依赖 state 锁：抢占会 take 旧会话后释放 state 锁并 await 最长 13s，
        // 若无本 gate，低优先级 submit 可趁空槽抢先写入，反而把高优先级请求挤掉。
        let _submit_guard = self.submit_gate.lock().await;

        // 4. 去重/抢占判断（决策与 take 在同一把 state 锁内完成，避免 TOCTOU 竞态）
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
        // state 锁已释放，安全执行异步取消与收尾等待；submit_gate 仍持有，
        // 因此其他 submit 无法利用 active_session 的临时空窗插队。
        if let Some(old) = old_session {
            old.propagate_cancel(&self.bridge, "被更高优先级登录抢占");
            self.wait_old_session_finished(old).await;
        }

        // 5. 创建新会话（计数延后到 became_active 判定后，避免抢占失败的“被取代”请求污染 login_total）
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
        // 会话完全收尾通知（F6）：spawn 的会话任务在 run() 返回后触发
        let finished = Arc::new(tokio::sync::Notify::new());
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
            .build_worker_config(
                &rt,
                &resolved_profile,
                effective_task_id.as_deref().unwrap_or(""),
            )
            .await;

        let session = LoginSession::new(
            session::SessionParams {
                source,
                task_id: effective_task_id,
                max_retries: rt.retry.max_retries,
                retry_interval: Duration::from_secs(rt.retry.retry_interval as u64),
                login_timeout: Duration::from_secs((rt.browser.login_timeout as u64).max(1)),
                profile_id: profile.id.clone(),
                worker_config,
            },
            cancel_token.clone(),
            result_slot,
            attempt_cancel_id.clone(),
            shutdown_token,
            cancel_reason.clone(),
            session::SessionDeps {
                bridge: self.bridge.clone(),
                monitor: self.monitor.clone(),
                config_service: self.config.clone(),
                status_manager: self.status.clone(),
                history_service: self.history.clone(),
                metrics: self.metrics.clone(),
            },
        );

        // 写入活跃会话。submit_gate 保证此窗口没有其他 submit 可写入；
        // 仍保留 is_none 防御性检查，避免未来新增非 submit 写路径时泄漏会话。
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
                    finished: finished.clone(),
                });
                true
            } else {
                false
            }
        };

        // 6. 仅当成功占据活跃会话槽位时才计数并 spawn 状态机 task
        if became_active {
            if let Some(m) = &self.metrics {
                m.inc_login();
                self.status.merge(PartialSnapshot::Totals {
                    probe_total: m.probe_total.load(std::sync::atomic::Ordering::Relaxed),
                    login_total: m.login_total.load(std::sync::atomic::Ordering::Relaxed),
                });
            }
            let state_arc = self.state.clone();
            let finished_notifier = finished.clone();
            tokio::spawn(async move {
                session.run().await;
                // F6：run() 返回即全部收尾动作（含 emit 的 close_browser）完成，
                // 触发通知供抢占方放行新会话；无等待者时存储许可，不丢失
                finished_notifier.notify_one();
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

        // 7. 返回句柄；_submit_guard 随函数返回释放，后续 submit 此时只能看到
        // 已安装的新 active_session，不再能看到抢占过程中的临时空槽。
        handle
    }

    /// 等待被抢占的旧会话**完全收尾**（历史遗留 F6）
    ///
    /// 旧实现只等 `await_result`（5s）：emit 先 `set_result` 再 `close_browser`
    /// （≤8s），抢占方一返回就建新会话复用同一 Worker——旧会话仍在途的
    /// close_browser 可能关掉新会话刚复用/重建的浏览器，或其 ensure_browser
    /// 复位掉新会话的上下文。
    ///
    /// 时序设计：改为等待 `finished` 通知（run() 返回 = set_result + 指标 +
    /// 状态广播 + 历史落盘 + close_browser 全部完成）。总预算 13s = 5s 等终态
    /// 结果 + 8s close_browser 上限（与 emit 内 close_browser 的命令级超时对齐）。
    /// 超时兜底：`force_recycle` Worker——kill 掉旧会话可能仍挂起的
    /// close_browser/execute（pending 被 drain、取消令牌全部触发），确保旧会话
    /// 失去对 Worker 的一切影响后再放行新会话；旧会话随后收尾时
    /// `has_live_worker()` 为 false（或属新会话的 Worker 已重建），不再互相干扰。
    ///
    /// 防死锁：`Notify::notify_one` 无等待者时存储许可——旧会话先于本等待
    /// 完成时 `notified()` 立即返回，不挂起。
    async fn wait_old_session_finished(&self, old: ActiveSession) {
        // F6 总预算：5s（旧版等结果预算）+ 8s（emit 内 close_browser 上限）
        const PREEMPT_WAIT_BUDGET: Duration = Duration::from_secs(13);
        match tokio_timeout(PREEMPT_WAIT_BUDGET, old.finished.notified()).await {
            Ok(()) => {
                // 完全收尾：结果必然已写入（notify 在 run() 返回后触发），
                // 无需再等 await_result
            }
            Err(_) => {
                warn!(
                    "等待旧会话完全收尾超时（{}s），强制回收 Worker 后放行新会话",
                    PREEMPT_WAIT_BUDGET.as_secs()
                );
                self.bridge.force_recycle().await;
            }
        }
    }

    /// 取消当前在途登录（Web API `POST /api/login/cancel`）
    ///
    /// 使用 `lock().await` 等待锁（锁窗口极短），避免撞上 `submit` 持锁窗口时
    /// 用户取消被静默丢弃（原 `try_lock` 会跳过取消，表现为点取消没反应）。
    pub async fn cancel_current(&self) {
        let guard = self.state.lock().await;
        if let Some(active) = &guard.active_session {
            active.propagate_cancel(&self.bridge, "用户取消");
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
                active.propagate_cancel(&self.bridge, reason);
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
}

// 重新导出公共类型，供 `crate::login::*` 引用
pub use crate::bridge::{Outcome as LoginOutcome, StructuredResult};
pub use crate::status::LoginSource;
pub use history::{HistoryResult, HistoryStore, LoginHistoryEntry, LoginHistoryService};
pub use preemption::{PreemptionDecision, decide};
pub use session::{LoginResult, LoginSession, LoginState, ResultAction, TerminalKind};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::LoginSource;
    use tokio_util::sync::CancellationToken;

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

    // ============ submit gate：抢占切换窗口串行化 ============

    #[tokio::test]
    async fn test_submit_gate_serializes_submit_window() {
        let orch = make_orchestrator().await;
        let guard = orch.submit_gate.lock().await;
        let orch2 = orch.clone();
        let waiter = tokio::spawn(async move {
            let _next = orch2.submit_gate.lock().await;
            true
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "第二个 submit 窗口必须等待 gate");
        drop(guard);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .expect("释放 gate 后应立即放行")
                .unwrap()
        );
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
                attempt_cancel_id: Arc::new(arc_swap::ArcSwapOption::new(Some(Arc::new(
                    "cid-1".to_string(),
                )))),
                handle: make_handle(LoginSource::Manual),
                finished: Arc::new(tokio::sync::Notify::new()),
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
        assert!(
            !cancel_task.is_finished(),
            "cancel_current 应等待锁而非放弃"
        );
        // 释放锁，cancel_current 应能拿到锁并完成取消
        drop(lock_guard);
        let done = tokio::time::timeout(std::time::Duration::from_secs(2), cancel_task)
            .await
            .expect("cancel_current 应在锁释放后完成")
            .unwrap();
        assert!(done);

        // 验证取消已传播到活跃会话
        let state = orch.state.lock().await;
        let active = state.active_session.as_ref().unwrap();
        assert!(active.cancel_token.is_cancelled());
        assert_eq!(
            *active
                .cancel_reason
                .as_ref()
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            Some("用户取消".to_string())
        );
    }

    // ============ F6：抢占等待旧会话完全收尾 ============

    /// 构造测试用 ActiveSession（finished 通知可外部控制）
    fn make_active(source: LoginSource) -> ActiveSession {
        ActiveSession {
            source,
            session_id: 1,
            cancel_token: CancellationToken::new(),
            cancel_reason: Arc::new(StdMutex::new(None)),
            attempt_cancel_id: Arc::new(arc_swap::ArcSwapOption::new(None)),
            handle: make_handle(source),
            finished: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// F6：旧会话已先于等待完成（notify 许可已存储）→ 立即返回，不挂起（防死锁）
    #[tokio::test]
    async fn f6_旧会话已完成_立即返回不挂起() {
        let orch = make_orchestrator().await;
        let old = make_active(LoginSource::Manual);
        // 会话任务已退出并触发通知（无等待者时存储许可）
        old.finished.notify_one();
        let start = std::time::Instant::now();
        orch.wait_old_session_finished(old).await;
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "旧会话已收尾应立即返回"
        );
    }

    /// F6：旧会话延迟收尾（模拟 set_result 后 close_browser 仍在途）→
    /// 抢占方等到**完全收尾**（notify 触发）才返回，且不触发 force_recycle
    #[tokio::test]
    async fn f6_旧会话延迟收尾_等待完全完成后放行() {
        let orch = make_orchestrator().await;
        let old = make_active(LoginSource::Manual);
        let finished = old.finished.clone();
        // 模拟旧会话 emit：set_result 后仍需 150ms 才完成 close_browser
        old.handle.inner.set_result(LoginResult {
            success: false,
            message: "被抢占".into(),
            source: LoginSource::Manual,
            duration: Duration::ZERO,
            attempts: 1,
        });
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            finished.notify_one();
        });
        let start = std::time::Instant::now();
        orch.wait_old_session_finished(old).await;
        // 等待覆盖了 close_browser 在途时间（旧实现 await_result 即返回 ≈0ms）
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(140),
            "应等待旧会话完全收尾而非仅终态结果"
        );
        // 未超时 → 不应 force_recycle（Worker 状态保持非 Error）
        let ws = orch.bridge.worker_status();
        assert!(
            !matches!(ws, crate::status::WorkerStatus::Error),
            "正常收尾不应触发 Worker 强制回收，实际 {ws:?}"
        );
    }

    /// F6：旧会话超预算未收尾 → force_recycle 兜底后放行
    /// （start_paused 让 13s 预算瞬间耗尽；force_recycle 将 Worker 置 Error 可观测）
    #[tokio::test(start_paused = true)]
    async fn f6_旧会话超时未收尾_强制回收后放行() {
        let orch = make_orchestrator().await;
        let old = make_active(LoginSource::Manual);
        // 不触发 finished：模拟收尾挂死
        orch.wait_old_session_finished(old).await;
        // force_recycle（无真实进程）仍会把 worker_state 置为 Error
        let ws = orch.bridge.worker_status();
        assert!(
            matches!(ws, crate::status::WorkerStatus::Error),
            "超时兜底应强制回收 Worker，实际 {ws:?}"
        );
    }
}
