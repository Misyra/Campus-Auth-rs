//! 网络监测：TCP/HTTP/URL 三类探测 + 状态判定
//!
//! [`MonitorService`] 编排三类探测，将结果汇总为 [`ProbeReport`]（含 [`crate::status::NetworkStatus`]）
//! 供 Engine 决策是否触发登录。配置每次探测前从 [`crate::config::ConfigService`] 热读取，
//! 保证运行期修改即时生效。

pub mod decision;
pub mod model;
pub mod portal;
pub mod probes;

pub use decision::{
    apply_auth_endpoint, apply_lenient_trigger, assess_connectivity, lenient_trigger_candidate,
};
pub use model::{
    AssessmentConfidence, AssessmentReason, AuthEndpointState, ConnectivityAssessment,
    LocalLinkState, ProbeEvidence, ProbeReport, RecoveryAdvice,
};
pub use portal::{PortalDetectResult, PortalDetectStatus, detect_portal};
pub use probes::{PerProbeDetail, ProbeKind, ProbeOutcome, parse_url_host_port};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use futures::future::{BoxFuture, join_all};
use reqwest::Client;
use reqwest::redirect::Policy;
use tokio::net::TcpStream;
use tracing::{debug, instrument, warn};

use crate::config::ConfigService;
use crate::config::runtime::RuntimeConfig;
use crate::network::NetworkDetect;
use crate::status::NetworkStatus;
use crate::utils::metrics::Metrics;

/// reqwest 连接池空闲超时（秒）
const HTTP_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// 物理网卡检测超时（秒）
const INTERFACE_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// 监测相关错误
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    /// reqwest 客户端构建失败
    #[error("reqwest 客户端构建失败: {0}")]
    ClientBuild(String),
}

/// 从 RuntimeConfig 提取的监测配置子集
///
/// 每种检测入口执行前都会重新构建，保证配置热更新生效。
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// 监测间隔（秒）
    pub check_interval: u64,
    /// 是否启用 TCP 探测
    pub tcp_enabled: bool,
    /// 是否启用 HTTP 探测
    pub http_enabled: bool,
    /// 是否启用 URL 探测
    pub url_enabled: bool,
    /// TCP 探测目标（host:port）
    pub tcp_targets: Vec<String>,
    /// HTTP 探测目标（URL）
    pub http_targets: Vec<String>,
    /// URL 探测目标
    pub url_targets: Vec<String>,
    /// URL 期望响应（URL -> 期望包含的标题片段）
    pub url_expected_responses: HashMap<String, String>,
    /// 是否启用物理网卡连接检查
    pub local_check_enabled: bool,
    /// 严格登录模式：仅在拿到明确门户结论时才建议自动登录（默认开启）
    ///
    /// 关闭后退化为宽松触发：自动监测会额外采集本地链路证据（`list_interfaces`），
    /// 并在严格判定未给出门户结论时升级为 `AttemptLogin`
    /// （见 [`decision::apply_lenient_trigger`]）。
    pub strict_login_mode: bool,
    /// TCP 连接超时
    pub tcp_timeout: Duration,
    /// HTTP 请求超时
    pub http_timeout: Duration,
    /// URL 请求超时
    pub url_timeout: Duration,
    /// auth_url 可达性检查超时
    pub auth_url_timeout: Duration,
}

impl MonitorConfig {
    /// 从运行时配置提取监测子集
    pub fn from_runtime(rt: &RuntimeConfig) -> Self {
        let m = &rt.monitor;
        MonitorConfig {
            check_interval: m.check_interval as u64,
            tcp_enabled: m.tcp_enabled,
            http_enabled: m.http_enabled,
            url_enabled: m.url_enabled,
            tcp_targets: m.tcp_targets.clone(),
            http_targets: m.http_targets.clone(),
            url_targets: m.url_targets.clone(),
            url_expected_responses: m.url_expected_responses.clone(),
            local_check_enabled: m.local_check_enabled,
            strict_login_mode: m.strict_login_mode,
            tcp_timeout: Duration::from_secs(m.tcp_timeout as u64),
            http_timeout: Duration::from_secs(m.http_timeout as u64),
            url_timeout: Duration::from_secs(m.url_timeout as u64),
            auth_url_timeout: Duration::from_secs(m.auth_url_timeout as u64),
        }
    }
}

/// 一次检测的用途
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckPurpose {
    /// 周期监测：补充认证入口证据并生成自动恢复建议
    AutoMonitor,
    /// 用户主动诊断：可采集本地链路，但绝不生成自动恢复动作
    ManualDiagnostic,
    /// 登录后验证：只确认公网是否恢复
    PostLoginVerification,
}

/// 网络监测服务
///
/// 持有长生命周期 reqwest 连接池与原子计数器。配置每次探测前从 ConfigService 热读取。
pub struct MonitorService {
    /// 配置服务（用于热读取 RuntimeConfig）
    config_service: Arc<ConfigService>,
    /// 物理网络检测器
    network_detect: Arc<dyn NetworkDetect>,
    /// HTTP/URL 探测共用的长生命周期客户端（连接池复用）
    ///
    /// 代理/证书策略按当前设置热重建（见 `ensure_client`），不再需重启生效
    http_client: Arc<ArcSwap<Client>>,
    /// 上次构建客户端时的 `disable_proxy` 快照（热重建比对用）
    last_disable_proxy: std::sync::atomic::AtomicBool,
    /// 上次构建客户端时的 `ignore_https_errors` 快照（热重建比对用）
    last_ignore_certs: std::sync::atomic::AtomicBool,
    /// 构造时传入的网卡绑定代理（热重建时复用）
    bind_proxy: std::sync::Mutex<Option<String>>,
    /// 运行指标（可选）
    metrics: Option<Arc<Metrics>>,
    /// 累计检测次数
    check_count: AtomicU64,
    /// “所有探测类型均已禁用”告警是否已降级（首次 warn，后续 debug）
    all_disabled_warned: AtomicBool,
}

impl MonitorService {
    /// 构造监测服务并构建 reqwest 客户端。
    ///
    /// `proxy` 为可选的 SOCKS5 代理地址（网卡绑定场景）；构建失败返回 [`MonitorError::ClientBuild`]。
    pub fn new(
        config: Arc<ConfigService>,
        detect: Arc<dyn NetworkDetect>,
        proxy: Option<&str>,
        metrics: Option<Arc<Metrics>>,
    ) -> Result<Self, MonitorError> {
        let settings = config.load_settings();
        let disable_proxy = settings.global.monitor.disable_proxy;
        let ignore_certs = settings.global.browser.ignore_https_errors;
        let http_client = Self::build_client(proxy, disable_proxy, ignore_certs)?;
        Ok(Self {
            config_service: config,
            network_detect: detect,
            http_client: Arc::new(ArcSwap::from_pointee(http_client)),
            last_disable_proxy: std::sync::atomic::AtomicBool::new(disable_proxy),
            last_ignore_certs: std::sync::atomic::AtomicBool::new(ignore_certs),
            bind_proxy: std::sync::Mutex::new(proxy.map(|s| s.to_string())),
            metrics,
            check_count: AtomicU64::new(0),
            all_disabled_warned: AtomicBool::new(false),
        })
    }

    /// 探测前热重建客户端：代理/证书开关变化即重建，无需重启
    fn ensure_client(&self) {
        let settings = self.config_service.load_settings();
        let disable_proxy = settings.global.monitor.disable_proxy;
        let ignore_certs = settings.global.browser.ignore_https_errors;
        let last_disable = self
            .last_disable_proxy
            .load(std::sync::atomic::Ordering::Relaxed);
        let last_ignore = self
            .last_ignore_certs
            .load(std::sync::atomic::Ordering::Relaxed);
        if disable_proxy == last_disable && ignore_certs == last_ignore {
            return;
        }
        let bind = self.bind_proxy.lock().ok().and_then(|g| g.clone());
        match Self::build_client(bind.as_deref(), disable_proxy, ignore_certs) {
            Ok(client) => {
                self.http_client.store(std::sync::Arc::new(client));
                self.last_disable_proxy
                    .store(disable_proxy, std::sync::atomic::Ordering::Relaxed);
                self.last_ignore_certs
                    .store(ignore_certs, std::sync::atomic::Ordering::Relaxed);
                tracing::info!("检测客户端已按新配置重建");
            }
            Err(e) => tracing::warn!("检测客户端重建失败，沿用旧客户端: {e}"),
        }
    }

    /// 构建 reqwest 客户端（redirect=none、忽略证书错误、连接池复用）
    ///
    /// `disable_system_proxy=true`（监测设置"禁用代理"默认值）时显式 no_proxy——
    /// 网络检测直连，避免系统代理故障时误判 Offline（与原版 `set_block_proxy()`
    /// 一致）；false 时跟随系统代理。
    fn build_client(
        proxy: Option<&str>,
        disable_system_proxy: bool,
        ignore_certs: bool,
    ) -> Result<Client, MonitorError> {
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .danger_accept_invalid_certs(ignore_certs)
            .pool_idle_timeout(HTTP_POOL_IDLE_TIMEOUT);
        if disable_system_proxy {
            builder = builder.no_proxy();
        }
        if let Some(p) = proxy {
            let proxy =
                reqwest::Proxy::all(p).map_err(|e| MonitorError::ClientBuild(e.to_string()))?;
            builder = builder.proxy(proxy);
        }
        builder
            .build()
            .map_err(|e| MonitorError::ClientBuild(e.to_string()))
    }

    /// 执行一次完整探测周期，返回 [`ProbeReport`]。
    ///
    /// 暂停时段由 Engine 在调用前检查，本方法不重复判断。
    /// 执行自动监测：公网探测后按需补充认证入口证据与恢复建议。
    pub async fn check_auto_monitor(&self) -> Result<ProbeReport, MonitorError> {
        self.check_once(CheckPurpose::AutoMonitor).await
    }

    /// 执行用户主动诊断：允许采集本地链路，不触发任何自动恢复动作。
    pub async fn diagnose_once(&self) -> Result<ProbeReport, MonitorError> {
        self.check_once(CheckPurpose::ManualDiagnostic).await
    }

    /// 执行登录后公网验证：不采集本地链路，也不额外探测认证入口。
    pub async fn verify_internet(&self) -> Result<ProbeReport, MonitorError> {
        self.check_once(CheckPurpose::PostLoginVerification).await
    }

    /// 按用途执行一次检测；暂停属于 Engine 调度策略。
    #[instrument(skip_all)]
    async fn check_once(&self, purpose: CheckPurpose) -> Result<ProbeReport, MonitorError> {
        self.ensure_client();
        let rt = self.config_service.runtime().load();
        let cfg = MonitorConfig::from_runtime(&rt);

        // 检测周期开始：每轮探测的高频事件，降为 debug 避免默认 info 级别刷屏
        //（网络状态变化由 Engine 的"网络状态变化" info 单点记录）
        let check_no = self.check_count.load(Ordering::Relaxed) + 1;
        let mut enabled_list: Vec<&str> = Vec::new();
        if cfg.tcp_enabled {
            enabled_list.push("TCP");
        }
        if cfg.http_enabled {
            enabled_list.push("HTTP");
        }
        if cfg.url_enabled {
            enabled_list.push("URL");
        }
        debug!(
            "网络检测 #{} 开始：启用探测 [{}]，间隔 {}s",
            check_no,
            enabled_list.join("/"),
            cfg.check_interval
        );

        // 全部禁用时仍走统一判定，最终返回 Unknown + NoProbeEvidence；
        // 没有测量不能伪装成确认离线。
        if !cfg.tcp_enabled && !cfg.http_enabled && !cfg.url_enabled {
            if self.all_disabled_warned.swap(true, Ordering::Relaxed) {
                debug!("所有探测类型均已禁用，本轮返回 Unknown");
            } else {
                warn!("所有探测类型均已禁用，本轮返回 Unknown 且禁止自动恢复");
            }
        } else {
            self.all_disabled_warned.store(false, Ordering::Relaxed);
        }

        // 并发执行已启用的三类公网探测（不绑定出口网卡，走系统默认路由）
        // 客户端已按当前配置热重建（ensure_client）
        let client = self.http_client.load();
        let start = Instant::now();

        let mut tasks: Vec<BoxFuture<(ProbeKind, ProbeOutcome, Vec<PerProbeDetail>)>> = Vec::new();

        if cfg.tcp_enabled {
            debug!(
                "TCP 探测启动：目标 {:?}，超时 {:?}",
                cfg.tcp_targets, cfg.tcp_timeout
            );
            let targets = cfg.tcp_targets.clone();
            let timeout = cfg.tcp_timeout;
            tasks.push(Box::pin(async move {
                let (o, d) = probes::TcpProbe::run(&targets, timeout).await;
                (ProbeKind::Tcp, o, d)
            }));
        }
        if cfg.http_enabled {
            debug!(
                "HTTP 探测启动：目标 {:?}，超时 {:?}",
                cfg.http_targets, cfg.http_timeout
            );
            let targets = cfg.http_targets.clone();
            let timeout = cfg.http_timeout;
            let c = client.clone();
            tasks.push(Box::pin(async move {
                let (o, d) = probes::HttpProbe::run(&c, &targets, timeout).await;
                (ProbeKind::Http, o, d)
            }));
        }
        if cfg.url_enabled {
            debug!(
                "URL 探测启动：目标 {:?}，超时 {:?}",
                cfg.url_targets, cfg.url_timeout
            );
            let targets = cfg.url_targets.clone();
            let expected = cfg.url_expected_responses.clone();
            let timeout = cfg.url_timeout;
            let c = client.clone();
            tasks.push(Box::pin(async move {
                let (o, d) = probes::UrlProbe::run(&c, &targets, &expected, timeout).await;
                (ProbeKind::Url, o, d)
            }));
        }

        let completed = join_all(tasks).await;

        // 收集各类结果（逐目标明细日志）
        let mut tcp_outcome = ProbeOutcome::Disabled;
        let mut http_outcome = ProbeOutcome::Disabled;
        let mut url_outcome = ProbeOutcome::Disabled;
        for (kind, outcome, details) in completed {
            match kind {
                ProbeKind::Tcp => tcp_outcome = outcome,
                ProbeKind::Http => http_outcome = outcome,
                ProbeKind::Url => url_outcome = outcome,
            }
            // 逐目标输出探测明细：成功仅 DEBUG（避免刷屏），失败降为 DEBUG（单目标失败是正常竞态行为）
            for d in &details {
                if d.success {
                    debug!(
                        "探测明细 {:?} | target={} | success={} | elapsed={}ms | status={:?}",
                        kind, d.target, d.success, d.elapsed_ms, d.http_status
                    );
                } else {
                    debug!(
                        "探测明细 {:?} | target={} | elapsed={}ms | status={:?} | error={:?}",
                        kind, d.target, d.elapsed_ms, d.http_status, d.error
                    );
                }
            }
            // 单类探测整体 Fail 仅记 debug：断网期间每轮重复告警会刷屏，
            // 真正的状态转换已由 Engine 的"网络状态变化" info 记录
            if matches!(outcome, ProbeOutcome::Fail) {
                debug!("{kind:?} 探测整体失败（所有目标均不可达）");
            }
        }

        let mut evidence = ProbeEvidence::new(tcp_outcome, http_outcome, url_outcome);
        let mut assessment = assess_connectivity(&evidence);

        if purpose == CheckPurpose::AutoMonitor
            && assessment.recovery_advice != RecoveryAdvice::NoProbeEvidence
            && assessment.status != NetworkStatus::Online
        {
            let auth_endpoint = if !rt.profile.trigger_url.is_empty() {
                AuthEndpointState::SkippedRedirectMode
            } else if rt.profile.auth_url.trim().is_empty() {
                AuthEndpointState::Missing
            } else {
                self.inspect_auth_endpoint(&rt.profile.auth_url, cfg.auth_url_timeout)
                    .await
            };
            assessment = apply_auth_endpoint(assessment, auth_endpoint);
        } else if purpose != CheckPurpose::AutoMonitor {
            assessment.auth_endpoint = AuthEndpointState::NotChecked;
            assessment.recovery_advice = RecoveryAdvice::NotEvaluated;
        }

        // 本地链路证据按需采集（串行，在公网探测之后）：
        // - 手动诊断：按用户开关采集，是诊断说明的一部分；
        // - 自动监测：仅当严格模式**关闭**（宽松口径）且严格判定未确认在线时才采集
        //   ——此时它是「升级还是等待」的唯一变量；已确认在线或已由严格判定接管
        //   （配置错误/无有效探测）时采集毫无用处，而 `list_interfaces` 要 spawn
        //   系统命令子进程，不该在在线稳态下每轮白跑；
        // - 登录后验证：不采集。
        // 串行而非并行：并行的代价是在线稳态下仍会白跑（结果被丢弃），收益只是
        // 失败路径省去一次「枚举网卡」的耗时（≤3s），而失败路径紧接着要拉起
        // 浏览器，这点延迟可忽略。
        let want_local_link = match purpose {
            CheckPurpose::ManualDiagnostic => cfg.local_check_enabled,
            CheckPurpose::AutoMonitor => {
                !cfg.strict_login_mode && lenient_trigger_candidate(&assessment)
            }
            CheckPurpose::PostLoginVerification => false,
        };
        if want_local_link {
            evidence.local_link = self.probe_local_link().await;
        }

        // 宽松登录触发（严格模式关闭时）必须在严格判定（含认证入口补充）**之后**
        // 应用：它是对「未确认在线」的兜底升级，而不是替换严格证据。顺序颠倒会让
        // apply_auth_endpoint 的 FixConfiguration 被覆盖掉。
        if purpose == CheckPurpose::AutoMonitor && !cfg.strict_login_mode {
            let before = assessment.recovery_advice;
            assessment = apply_lenient_trigger(assessment, evidence.local_link);
            if assessment.recovery_advice == RecoveryAdvice::AttemptLogin
                && before != RecoveryAdvice::AttemptLogin
            {
                debug!(
                    local_link = ?evidence.local_link,
                    previous_advice = ?before,
                    "已关闭严格登录模式：本地链路可用且未确认在线，升级为建议登录"
                );
            }
        }

        let latency = start.elapsed().as_millis() as u64;
        let report = self.finalize_report(evidence, assessment, latency);
        debug!(
            status = ?report.assessment.status,
            confidence = ?report.assessment.confidence,
            reason = ?report.assessment.reason,
            recovery = ?report.assessment.recovery_advice,
            auth_endpoint = ?report.assessment.auth_endpoint,
            latency_ms = report.latency_ms,
            tcp = ?report.evidence.tcp,
            http = ?report.evidence.http,
            url = ?report.evidence.url,
            local_link = ?report.evidence.local_link,
            "探测完成 #{}",
            report.check_number
        );
        Ok(report)
    }

    /// 检查认证入口的 TCP 可达性，并区分配置错误与网络不可达。
    ///
    /// 必须直连（`TcpStream::connect`），禁止走系统代理/`http_client`：
    /// `auth_url` 指向校园内网认证服务器（常见 `10.x`/`172.16.x` 或校内域名的私网 IP），
    /// Captive 态下尚未获得公网访问能力，公网代理此时不可达且不会回源内网；若走代理
    /// 会把认证入口误判为不可达，干扰监测层的恢复建议。外网三类探测已由
    /// `build_client(disable_proxy=true → no_proxy)` 屏蔽代理，本方法将该原则贯彻到
    /// 内网：内网更不应经过代理。
    ///
    /// 地址解析统一走 [`probes::parse_url_host_port`] 单点实现（G2）：
    /// 支持 IPv6 方括号与裸地址形式，返回的 host 已剥除方括号。
    #[instrument(skip(self))]
    pub async fn inspect_auth_endpoint(
        &self,
        auth_url: &str,
        timeout: Duration,
    ) -> AuthEndpointState {
        let (host, port) = match parse_url_host_port(auth_url) {
            Some(hp) => hp,
            None => {
                warn!("auth_url 解析失败: {auth_url}");
                return AuthEndpointState::Invalid;
            }
        };
        let reachable =
            match tokio::time::timeout(timeout, TcpStream::connect((host.as_str(), port))).await {
                Ok(Ok(_)) => true,
                Ok(Err(_)) | Err(_) => false,
            };
        debug!("auth_url 可达性: {host}:{port} -> {reachable}");
        if reachable {
            AuthEndpointState::Reachable
        } else {
            AuthEndpointState::Unreachable
        }
    }

    /// 读取本机主用接口的地址（IPv4 + MAC），供直连脚本 `ctx.local_ip` /
    /// `ctx.local_mac` 使用。
    ///
    /// 部分校园门户（如 eportal / Dr.COM）的字段加密密钥由**来源 IP** 推导，
    /// 没有本机 IP 就无法在直连渠道复现。查询失败或平台不支持时返回空值而非
    /// 报错：地址只是脚本入参的一部分，缺了它脚本自身可回退到从页面提取。
    ///
    /// 网卡为空时返回「无地址」而不是错误——脚本据此判断拿不到本机 IP。
    pub async fn local_address(&self) -> crate::network::LocalAddress {
        match tokio::time::timeout(
            INTERFACE_CHECK_TIMEOUT,
            self.network_detect.list_interfaces(),
        )
        .await
        {
            Ok(Ok(list)) => crate::network::local_address_from(&list),
            Ok(Err(e)) => {
                debug!("查询本机地址失败（脚本将收到空 local_ip）: {e}");
                crate::network::LocalAddress::default()
            }
            Err(_) => {
                debug!("查询本机地址超时（脚本将收到空 local_ip）");
                crate::network::LocalAddress::default()
            }
        }
    }

    /// 采集本地链路证据：至少一块非虚拟网卡持有非链路本地 IPv4 即判 `Available`。
    ///
    /// 注意语义边界：它只判**链路层是否连着**，不判「是否有网」——插着网线但对端
    /// 未通、连着 WiFi 但网关不响应 DHCP，同样会得到 `Available`。真正判断「是否有
    /// 网」的是公网探测（204/URL/TCP）；本方法只在宽松模式下作为**比「有网」更弱**
    /// 的兜底信号使用（见 [`decision::apply_lenient_trigger`]），因此不得用于替代探测。
    ///
    /// 失败与超时均不影响公网状态判定：返回 `ProbeFailed`，宽松触发据此不升级。
    async fn probe_local_link(&self) -> LocalLinkState {
        match tokio::time::timeout(
            INTERFACE_CHECK_TIMEOUT,
            self.network_detect.list_interfaces(),
        )
        .await
        {
            Ok(Ok(list)) if list.is_empty() => {
                debug!("网卡检查未发现有效物理接口");
                LocalLinkState::Unavailable
            }
            Ok(Ok(list)) => {
                debug!("网卡检查通过：发现 {} 个有效物理接口", list.len());
                LocalLinkState::Available
            }
            Ok(Err(error)) => {
                warn!("网卡检查失败，不影响公网状态判定: {error}");
                LocalLinkState::ProbeFailed
            }
            Err(_) => {
                warn!("网卡检查超时，不影响公网状态判定");
                LocalLinkState::ProbeFailed
            }
        }
    }

    /// 读取累计指标快照（G23）
    ///
    /// 返回 `(probe_total, login_total)` 的当前计数器值，供 Engine 在探测状态
    /// merge 的同一位置推送 `PartialSnapshot::Totals`。probe_total 由本服务的
    /// `check_once` 完成路径单点递增（周期/手动/启动检测均计入）；
    /// login_total 由登录侧递增，此处只读。未注入 Metrics 时返回 None。
    pub fn metrics_totals(&self) -> Option<(u64, u64)> {
        let m = self.metrics.as_ref()?;
        Some((
            m.probe_total.load(Ordering::Relaxed),
            m.login_total.load(Ordering::Relaxed),
        ))
    }

    /// 组装 ProbeReport 并递增计数/指标
    ///
    /// `Metrics::probe_total` 的唯一递增点（G23）：`check_once` 的所有出口
    /// （含全部禁用 / 网卡检查提前返回）都经此方法收尾，因此周期检测、
    /// 启动/恢复触发的立即检测与手动 TestNetwork 全部计入探测总数。
    fn finalize_report(
        &self,
        evidence: ProbeEvidence,
        assessment: ConnectivityAssessment,
        latency_ms: u64,
    ) -> ProbeReport {
        let n = self.check_count.fetch_add(1, Ordering::Relaxed) + 1;
        // 记录探测次数与平均耗时（通过 Metrics 方法而非直接操作原子字段）
        if let Some(m) = &self.metrics {
            m.record_probe(latency_ms);
        }
        ProbeReport {
            evidence,
            assessment,
            latency_ms,
            check_number: n,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigService;
    use crate::network::detect::{InterfaceInfo, NetworkError};
    use async_trait::async_trait;
    use std::net::Ipv4Addr;

    /// 固定返回单块有线网卡的检测器（本地链路必判 Available），并记录调用次数
    struct WiredDetect {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl WiredDetect {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl NetworkDetect for WiredDetect {
        async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(vec![InterfaceInfo {
                name: "以太网".into(),
                ipv4: Ipv4Addr::new(192, 168, 1, 100),
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                is_wifi: false,
                ssid: None,
                mac: Some("00:1a:2b:3c:4d:5e".into()),
            }])
        }
        async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
            Ok(vec![Ipv4Addr::new(192, 168, 1, 1)])
        }
        async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
            Ok(None)
        }
    }

    /// 204 恒直通（判 Online）的本地服务地址，用于验证「已在线时不采集网卡」
    async fn spawn_always_204() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                });
            }
        });
        format!("http://{addr}/generate_204")
    }

    /// 无可用网卡的检测器（本地链路必判 Unavailable）
    struct NoLinkDetect;

    #[async_trait]
    impl NetworkDetect for NoLinkDetect {
        async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
            Ok(vec![])
        }
        async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
            Ok(vec![])
        }
        async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
            Ok(None)
        }
    }

    /// 构造监测服务；探测目标指向必然失败的地址（全 Fail → Offline）。
    ///
    /// `strict_mode=false` 即「关闭严格模式」＝启用宽松触发（被测行为）。
    ///
    /// `auth_url` 配成不可达地址（`127.0.0.1:9`，discard 端口通常无人监听）而非留空：
    /// - 留空会走 `AuthEndpointState::Missing` → 严格判定给 `FixConfiguration`，
    ///   而此时宽松触发按设计不覆盖（配置错误须用户先修正），测不出目标行为；
    /// - 不可达才是用户真实场景（认证入口预检失败），严格判定给
    ///   `WaitForNetwork`，正是宽松触发要接管的那条路径。
    ///
    /// `trigger_url` 留空以避免走 `SkippedRedirectMode`。
    async fn monitor_with(
        tmp: &tempfile::TempDir,
        detect: Arc<dyn NetworkDetect>,
        strict_mode: bool,
    ) -> Arc<MonitorService> {
        monitor_with_http_target(tmp, detect, strict_mode, "http://127.0.0.1:9/generate_204").await
    }

    /// 同 [`monitor_with`]，但可指定 204 探测目标（用于构造 Online 场景）
    async fn monitor_with_http_target(
        tmp: &tempfile::TempDir,
        detect: Arc<dyn NetworkDetect>,
        strict_mode: bool,
        http_target: &str,
    ) -> Arc<MonitorService> {
        use tokio::sync::mpsc;
        let (reload_tx, _reload_rx) = mpsc::channel(8);
        let config = ConfigService::new(tmp.path().to_path_buf(), reload_tx)
            .await
            .unwrap();
        let mut settings = config.load_settings();
        settings.global.monitor.tcp_enabled = false;
        settings.global.monitor.url_enabled = false;
        settings.global.monitor.http_enabled = true;
        settings.global.monitor.http_targets = vec![http_target.to_string()];
        settings.global.monitor.http_timeout = 1;
        settings.global.monitor.local_check_enabled = false;
        settings.global.monitor.strict_login_mode = strict_mode;
        settings.global.monitor.auth_url_timeout = 1;
        config.save_settings(&settings).await.unwrap();
        assert_eq!(
            config.load_settings().global.monitor.strict_login_mode,
            strict_mode,
            "严格模式开关应先真实落盘"
        );

        let profiles = crate::config::ProfileService::new(config.clone());
        let mut profile = profiles.get_profile("default").unwrap();
        profile.auth_url = "http://127.0.0.1:9/login".into();
        profile.trigger_url = String::new();
        profiles.update_profile("default", profile).await.unwrap();
        assert_eq!(
            profiles.get_profile("default").unwrap().auth_url,
            "http://127.0.0.1:9/login",
            "认证地址需真实写入活跃方案，否则走的是 Missing 分支"
        );
        config.reload().await.unwrap();

        Arc::new(MonitorService::new(config, detect, None, None).unwrap())
    }

    /// 严格模式关闭（宽松口径）+ 网卡可用：全 Fail 的 Offline 必须被升级为建议登录，
    /// 且网卡证据确实被采集（自动监测路径在严格模式下恒为 NotChecked）。
    #[tokio::test]
    async fn test_lenient_trigger_upgrades_auto_monitor_when_link_available() {
        let tmp = tempfile::tempdir().unwrap();
        let detect = Arc::new(WiredDetect::new());
        let monitor = monitor_with(&tmp, detect.clone(), false).await;
        let report = monitor.check_auto_monitor().await.unwrap();
        assert_eq!(
            report.evidence.local_link,
            LocalLinkState::Available,
            "宽松口径必须采集网卡证据，否则无法判定链路"
        );
        assert_eq!(
            detect.calls(),
            1,
            "未确认在线时恰好采集一次网卡（串行路径的唯一一次调用）"
        );
        assert_eq!(report.assessment.status, NetworkStatus::CaptivePortal);
        assert_eq!(
            report.assessment.reason,
            AssessmentReason::LinkUpLoginAssumed
        );
        assert_eq!(
            report.assessment.recovery_advice,
            RecoveryAdvice::AttemptLogin,
            "Engine 只认 AttemptLogin，其余建议都不会触发自动登录"
        );
    }

    /// 已确认在线时即便严格模式关闭也不采集网卡：在线稳态下每轮白跑
    /// `list_interfaces`（spawn `ipconfig`/`ip addr`）没有意义。
    #[tokio::test]
    async fn test_online_skips_link_probe_even_when_strict_mode_off() {
        let tmp = tempfile::tempdir().unwrap();
        let target = spawn_always_204().await;
        let detect = Arc::new(WiredDetect::new());
        let monitor = monitor_with_http_target(&tmp, detect.clone(), false, &target).await;
        let report = monitor.check_auto_monitor().await.unwrap();
        assert_eq!(report.assessment.status, NetworkStatus::Online);
        assert_eq!(report.assessment.recovery_advice, RecoveryAdvice::NoAction);
        assert_eq!(
            report.evidence.local_link,
            LocalLinkState::NotChecked,
            "已确认在线时不该采集网卡证据"
        );
        assert_eq!(detect.calls(), 0, "在线稳态下不得 spawn 网卡枚举子进程");
    }

    /// 默认（严格模式开启）：同一探测条件下保持原有等待语义，
    /// 且不采集网卡证据（避免无谓 spawn 系统命令子进程）。
    #[tokio::test]
    async fn test_strict_mode_keeps_waiting_without_link_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let detect = Arc::new(WiredDetect::new());
        let monitor = monitor_with(&tmp, detect.clone(), true).await;
        let report = monitor.check_auto_monitor().await.unwrap();
        assert_eq!(
            report.evidence.local_link,
            LocalLinkState::NotChecked,
            "严格模式不应采集网卡证据"
        );
        assert_eq!(detect.calls(), 0);
        assert_eq!(report.assessment.status, NetworkStatus::Offline);
        assert_eq!(
            report.assessment.recovery_advice,
            RecoveryAdvice::WaitForNetwork,
            "回归锚点：默认（严格）行为与改动前一致"
        );
    }

    /// 认证地址缺失（Missing → FixConfiguration）时不采集网卡：
    /// 宽松触发按设计不接管配置错误，采集结果也无人使用。
    #[tokio::test]
    async fn test_configuration_error_skips_link_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let detect = Arc::new(WiredDetect::new());
        let monitor = monitor_with(&tmp, detect.clone(), false).await;
        // 清空认证地址与触发地址 → 严格判定给 FixConfiguration
        let config = monitor.config_service.clone();
        let profiles = crate::config::ProfileService::new(config.clone());
        let mut profile = profiles.get_profile("default").unwrap();
        profile.auth_url = String::new();
        profiles.update_profile("default", profile).await.unwrap();
        config.reload().await.unwrap();

        let report = monitor.check_auto_monitor().await.unwrap();
        assert_eq!(
            report.assessment.recovery_advice,
            RecoveryAdvice::FixConfiguration
        );
        assert_eq!(
            report.evidence.local_link,
            LocalLinkState::NotChecked,
            "配置错误时不该采集网卡证据"
        );
        assert_eq!(detect.calls(), 0);
    }

    /// 宽松口径但网卡不可用：不得升级——网卡没连上时登录没有意义。
    #[tokio::test]
    async fn test_lenient_trigger_skipped_when_link_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let monitor = monitor_with(&tmp, Arc::new(NoLinkDetect), false).await;
        let report = monitor.check_auto_monitor().await.unwrap();
        assert_eq!(report.evidence.local_link, LocalLinkState::Unavailable);
        assert_eq!(report.assessment.status, NetworkStatus::Offline);
        assert_eq!(
            report.assessment.recovery_advice,
            RecoveryAdvice::WaitForNetwork
        );
    }
}
