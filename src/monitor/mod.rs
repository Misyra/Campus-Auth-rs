//! 网络监测：TCP/HTTP/URL 三类探测 + 状态判定
//!
//! [`MonitorService`] 编排三类探测，将结果汇总为 [`ProbeReport`]（含 [`crate::status::NetworkStatus`]）
//! 供 Engine 决策是否触发登录。配置每次探测前从 [`crate::config::ConfigService`] 热读取，
//! 保证运行期修改即时生效。

pub mod decision;
pub mod model;
pub mod portal;
pub mod probes;

pub use decision::{apply_auth_endpoint, assess_connectivity};
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

        let local_probe = async {
            if purpose != CheckPurpose::ManualDiagnostic || !cfg.local_check_enabled {
                return LocalLinkState::NotChecked;
            }
            match tokio::time::timeout(
                INTERFACE_CHECK_TIMEOUT,
                self.network_detect.list_interfaces(),
            )
            .await
            {
                Ok(Ok(list)) if list.is_empty() => {
                    warn!("网卡诊断未发现有效物理接口；该结果不参与公网状态判定");
                    LocalLinkState::Unavailable
                }
                Ok(Ok(list)) => {
                    debug!("网卡诊断通过：发现 {} 个有效物理接口", list.len());
                    LocalLinkState::Available
                }
                Ok(Err(error)) => {
                    warn!("网卡诊断失败，不影响公网状态判定: {error}");
                    LocalLinkState::ProbeFailed
                }
                Err(_) => {
                    warn!("网卡诊断超时，不影响公网状态判定");
                    LocalLinkState::ProbeFailed
                }
            }
        };
        let (completed, local_link) = tokio::join!(join_all(tasks), local_probe);

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
        evidence.local_link = local_link;
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
