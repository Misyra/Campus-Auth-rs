//! 网络监测：TCP/HTTP/URL 三类探测 + 状态判定
//!
//! [`MonitorService`] 编排三类探测，将结果汇总为 [`ProbeReport`]（含 [`crate::status::NetworkStatus`]）
//! 供 Engine 决策是否触发登录。配置每次探测前从 [`crate::config::ConfigService`] 热读取，
//! 保证运行期修改即时生效。

pub mod decision;
pub mod probes;

pub use decision::evaluate;
pub use probes::{PerProbeDetail, ProbeKind, ProbeOutcome};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use futures::future::{join_all, BoxFuture};
use reqwest::redirect::Policy;
use reqwest::Client;
use tokio::net::TcpStream;
use tracing::{debug, info, instrument, warn};

use crate::config::runtime::RuntimeConfig;
use crate::config::ConfigService;
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
    /// 配置读取失败
    #[error("配置读取失败: {0}")]
    ConfigLoad(String),

    /// 网卡检测失败
    #[error("网卡检测失败: {0}")]
    InterfaceDetect(String),

    /// reqwest 客户端构建失败
    #[error("reqwest 客户端构建失败: {0}")]
    ClientBuild(String),

    /// 探测超时
    #[error("探测超时: {kind:?} ({timeout_ms}ms)")]
    ProbeTimeout {
        /// 超时的探测类别
        kind: ProbeKind,
        /// 超时毫秒数
        timeout_ms: u64,
    },

    /// 所有探测类型均已禁用
    #[error("所有探测类型均已禁用")]
    AllProbesDisabled,
}

/// 从 RuntimeConfig 提取的监测配置子集
///
/// 每次 `check_once()` 开头重新构建，保证配置热更新生效。
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

/// 一次完整探测周期的返回结果
#[derive(Debug, Clone)]
pub struct ProbeReport {
    /// 最终网络状态结论
    pub status: NetworkStatus,
    /// auth_url 是否可达（仅 CaptivePortal 时有值）
    pub auth_url_reachable: Option<bool>,
    /// TCP 探测结果
    pub tcp_outcome: ProbeOutcome,
    /// HTTP 探测结果
    pub http_outcome: ProbeOutcome,
    /// URL 探测结果
    pub url_outcome: ProbeOutcome,
    /// 整体探测耗时（毫秒）
    pub latency_ms: u64,
    /// 累计检测次数
    pub check_number: u64,
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
    http_client: Arc<ArcSwap<Client>>,
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
        let http_client = Self::build_client(proxy)?;
        Ok(Self {
            config_service: config,
            network_detect: detect,
            http_client: Arc::new(ArcSwap::from_pointee(http_client)),
            metrics,
            check_count: AtomicU64::new(0),
            all_disabled_warned: AtomicBool::new(false),
        })
    }

    /// 构建 reqwest 客户端（redirect=none、忽略证书错误、禁用系统代理、连接池复用）
    ///
    /// 禁用系统代理（no_proxy）与原版 `set_block_proxy()` 一致：网络检测应走直连，
    /// 避免系统代理干扰探测结果（代理挂了误判 Offline）。
    fn build_client(proxy: Option<&str>) -> Result<Client, MonitorError> {
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .pool_idle_timeout(HTTP_POOL_IDLE_TIMEOUT);
        if let Some(p) = proxy {
            let proxy = reqwest::Proxy::all(p).map_err(|e| MonitorError::ClientBuild(e.to_string()))?;
            builder = builder.proxy(proxy);
        }
        builder.build().map_err(|e| MonitorError::ClientBuild(e.to_string()))
    }

    /// 执行一次完整探测周期，返回 [`ProbeReport`]。
    ///
    /// 暂停时段由 Engine 在调用前检查，本方法不重复判断。
    #[instrument(skip_all)]
    pub async fn check_once(&self) -> Result<ProbeReport, MonitorError> {
        let rt = self.config_service.runtime().load();
        let cfg = MonitorConfig::from_runtime(&rt);

        // 检测周期开始：以 INFO 记录启用类型与计数，保证默认 info 级别下可见（此前均为 debug 被过滤）
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
        info!(
            "网络检测 #{} 开始：启用探测 [{}]，间隔 {}s",
            check_no,
            enabled_list.join("/"),
            cfg.check_interval
        );

        // 步骤 1：全部禁用检查（首次告警，后续降级为 DEBUG）
        if !cfg.tcp_enabled && !cfg.http_enabled && !cfg.url_enabled {
            if self.all_disabled_warned.swap(true, Ordering::Relaxed) {
                debug!("所有探测类型均已禁用，本轮返回 Offline");
            } else {
                warn!("所有探测类型均已禁用，本轮返回 Offline（后续同类告警降级为 DEBUG）");
            }
            return Ok(self.finalize_report(
                NetworkStatus::Offline,
                ProbeOutcome::Disabled,
                ProbeOutcome::Disabled,
                ProbeOutcome::Disabled,
                0,
                None,
            ));
        }

        // 步骤 2：物理网卡连接检查（由 local_check_enabled 控制）
        // 逻辑：存在在线网卡表明链路已连接；网卡全失联时直接判 Offline，跳过后续探测。
        if cfg.local_check_enabled {
            match tokio::time::timeout(INTERFACE_CHECK_TIMEOUT, self.network_detect.list_interfaces()).await {
                Ok(Ok(list)) => {
                    debug!("网卡检测通过：发现 {} 个网卡", list.len());
                    if list.is_empty() {
                        return Ok(self.finalize_report(
                            NetworkStatus::Offline,
                            ProbeOutcome::Disabled,
                            ProbeOutcome::Disabled,
                            ProbeOutcome::Disabled,
                            0,
                            None,
                        ));
                    }
                }
                Ok(Err(e)) => {
                    warn!("网卡检测失败: {e}");
                    return Ok(self.finalize_report(
                        NetworkStatus::Offline,
                        ProbeOutcome::Disabled,
                        ProbeOutcome::Disabled,
                        ProbeOutcome::Disabled,
                        0,
                        None,
                    ));
                }
                Err(_) => {
                    warn!("网卡检测超时");
                    return Ok(self.finalize_report(
                        NetworkStatus::Offline,
                        ProbeOutcome::Disabled,
                        ProbeOutcome::Disabled,
                        ProbeOutcome::Disabled,
                        0,
                        None,
                    ));
                }
            }
        }

        // 步骤 3：并发执行已启用的三类探测（不绑定出口网卡，走系统默认路由）
        let client = self.http_client.load();
        let start = Instant::now();

        let mut tasks: Vec<BoxFuture<(ProbeKind, ProbeOutcome, Vec<PerProbeDetail>)>> = Vec::new();

        if cfg.tcp_enabled {
            info!("TCP 探测启动：目标 {:?}，超时 {:?}", cfg.tcp_targets, cfg.tcp_timeout);
            let targets = cfg.tcp_targets.clone();
            let timeout = cfg.tcp_timeout;
            tasks.push(Box::pin(async move {
                let (o, d) = probes::TcpProbe::run(&targets, timeout).await;
                (ProbeKind::Tcp, o, d)
            }));
        }
        if cfg.http_enabled {
            info!("HTTP 探测启动：目标 {:?}，超时 {:?}", cfg.http_targets, cfg.http_timeout);
            let targets = cfg.http_targets.clone();
            let timeout = cfg.http_timeout;
            let c = client.clone();
            tasks.push(Box::pin(async move {
                let (o, d) = probes::HttpProbe::run(&c, &targets, timeout).await;
                (ProbeKind::Http, o, d)
            }));
        }
        if cfg.url_enabled {
            info!("URL 探测启动：目标 {:?}，超时 {:?}", cfg.url_targets, cfg.url_timeout);
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
        let latency = start.elapsed().as_millis() as u64;

        // 收集各类结果（逐目标明细日志）
        let mut tcp_outcome = ProbeOutcome::Disabled;
        let mut http_outcome = ProbeOutcome::Disabled;
        let mut url_outcome = ProbeOutcome::Disabled;
        let mut results: Vec<(ProbeKind, ProbeOutcome)> = Vec::new();
        for (kind, outcome, details) in completed {
            match kind {
                ProbeKind::Tcp => tcp_outcome = outcome,
                ProbeKind::Http => http_outcome = outcome,
                ProbeKind::Url => url_outcome = outcome,
            }
            results.push((kind, outcome));
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
            // 单类探测整体 Fail 时提升为告警
            if matches!(outcome, ProbeOutcome::Fail) {
                warn!("{kind:?} 探测整体失败（所有目标均不可达）");
            }
        }

        // 步骤 4：综合判定
        let status = evaluate(&results);

        // 步骤 5：CaptivePortal 时前置检查 auth_url 可达性
        let mut auth_url_reachable = None;
        if status == NetworkStatus::CaptivePortal && !rt.profile.auth_url.is_empty() {
            auth_url_reachable = Some(self.check_auth_url(&rt.profile.auth_url, cfg.auth_url_timeout).await);
        }

        let report = self.finalize_report(status, tcp_outcome, http_outcome, url_outcome, latency, auth_url_reachable);
        // 增强完成日志：补充各探测 outcome、auth_url 可达性与启用的探测类型
        let enabled = [
            (ProbeKind::Tcp, cfg.tcp_enabled),
            (ProbeKind::Http, cfg.http_enabled),
            (ProbeKind::Url, cfg.url_enabled),
        ]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(k, _)| format!("{k:?}"))
        .collect::<Vec<_>>()
        .join(",");
        info!(
            "探测完成 #{}: status={:?}, latency={}ms, tcp={:?}, http={:?}, url={:?}, auth_url={}, enabled=[{}]",
            report.check_number, report.status, report.latency_ms,
            report.tcp_outcome, report.http_outcome, report.url_outcome,
            match report.auth_url_reachable {
                Some(true) => "reachable",
                Some(false) => "unreachable",
                None => "N/A",
            },
            enabled
        );
        Ok(report)
    }

    /// 检查 auth_url 的 TCP 可达性（仅在 CaptivePortal 时调用）
    #[instrument(skip(self))]
    pub async fn check_auth_url(&self, auth_url: &str, timeout: Duration) -> bool {
        let (host, port) = match parse_auth_host_port(auth_url) {
            Some(hp) => hp,
            None => {
                debug!("auth_url 解析失败: {auth_url}");
                return false;
            }
        };
        let result = match tokio::time::timeout(timeout, TcpStream::connect((host.as_str(), port))).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        };
        debug!("auth_url 可达性: {host}:{port} -> {result}");
        result
    }

    /// 组装 ProbeReport 并递增计数/指标
    fn finalize_report(
        &self,
        status: NetworkStatus,
        tcp_outcome: ProbeOutcome,
        http_outcome: ProbeOutcome,
        url_outcome: ProbeOutcome,
        latency_ms: u64,
        auth_url_reachable: Option<bool>,
    ) -> ProbeReport {
        let n = self.check_count.fetch_add(1, Ordering::Relaxed) + 1;
        // 记录探测次数与平均耗时（通过 Metrics 方法而非直接操作原子字段）
        if let Some(m) = &self.metrics {
            m.record_probe(latency_ms);
        }
        ProbeReport {
            status,
            auth_url_reachable,
            tcp_outcome,
            http_outcome,
            url_outcome,
            latency_ms,
            check_number: n,
        }
    }
}

/// 解析 auth_url（http/https）为 (host, port)，不依赖 `url` crate
fn parse_auth_host_port(auth_url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = if let Some(idx) = auth_url.find("://") {
        (&auth_url[..idx], &auth_url[idx + 3..])
    } else {
        ("http", auth_url)
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
        None => (authority.to_string(), if scheme == "https" { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}
