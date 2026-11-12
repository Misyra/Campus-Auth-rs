//! 一键网络诊断：并发逐项探测 + 结构化报告（Web 层按需构造，不入容器拓扑）
//!
//! 与 [`super`]（MonitorService 周期监测）的分工：周期监测面向自动恢复决策，
//! 证据口径服务于 Engine；本模块面向用户手动触发的单次诊断，逐项给出
//! ok/warn/fail/skip 结论与总结论。探测实现一律复用 monitor/network 既有基建
//! （TcpProbe 竞速、PreparedEgress 出口租约、网卡枚举），不重写探测器。
//!
//! 可测试性：探测动作收敛在私有 [`ProbeBackend`] trait 之后，生产实现走系统
//! 调用与真实网络，单元测试注入桩（见模块测试）；五项检查并发执行，整体受
//! [`TOTAL_TIMEOUT`] 硬约束——任一项超限都以 fail 收敛返回，调用绝不悬挂。
//!
//! 脱敏红线：报告中不出现凭据、token 或带查询串的完整 URL——门户地址统一经
//! [`sanitize_portal_url`] 剥除查询串 / fragment / 内嵌凭据后再进入结果。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::json;

use crate::login::history::{HistoryResult, HistoryStore, LoginHistoryEntry};
use crate::monitor::probes::{PerProbeDetail, ProbeOutcome, TcpProbe, parse_url_host_port};
use crate::network::adapters::EgressAdapter;
use crate::network::egress::PreparedEgress;

/// 单次诊断总超时上限：五项检查并发执行，整体不得超过该时长
///
/// 各探测自有子超时（网关 4s / DNS 5s / 门户 8s / 网卡枚举内部 8s），
/// 并发下整体最坏耗时 ≈ max(子超时) < 15s；该项同时是测试注入短超时的收敛锚点。
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);

/// 网关 TCP 探测候选端口（竞速，任一连通即视为网关可达）
const GATEWAY_PROBE_PORTS: [u16; 3] = [80, 443, 8080];

/// 网关单端口 TCP 连接超时
const GATEWAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// DNS 解析超时
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// 门户 HTTP 探测超时
const PORTAL_HTTP_TIMEOUT: Duration = Duration::from_secs(8);

/// 登录失败归因回看窗口（天）
pub const FAILURE_LOOKBACK_DAYS: i64 = 7;

/// 拉取登录历史的条数上限（近期窗口内取最新 N 条再筛失败）
const HISTORY_FETCH_LIMIT: usize = 20;

/// 失败归因最多考虑的最近失败条数
const FAILURE_SAMPLE: usize = 5;

/// 归因细节中失败消息的最大字符数（消息为自由文本，截断防泄意外内容）
const FAILURE_MESSAGE_MAX_CHARS: usize = 120;

/// 单项检查结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    /// 通过
    Ok,
    /// 有警告（链路可用但需留意）
    Warn,
    /// 失败
    Fail,
    /// 不适用 / 未执行（如认证地址为 IP 直连时跳过 DNS）
    Skip,
}

/// 诊断总结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticVerdict {
    /// 全部通过（跳过项不计入）
    Healthy,
    /// 存在警告项
    Degraded,
    /// 存在失败项
    Faulty,
}

/// 单项检查结果
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticCheck {
    /// 检查项标识（机器可读：interfaces / gateway / dns / portal / history）
    pub key: &'static str,
    /// 检查项中文名（前端直接展示）
    pub name: &'static str,
    /// 结论
    pub status: DiagnosticStatus,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
    /// 一句话摘要
    pub summary: String,
    /// 结构化细节（已脱敏）
    pub details: serde_json::Value,
}

/// 一次诊断的完整报告
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticReport {
    /// 逐项结果（顺序固定：interfaces → gateway → dns → portal → history）
    pub checks: Vec<DiagnosticCheck>,
    /// 总结论
    pub verdict: DiagnosticVerdict,
    /// 总结论一句话
    pub conclusion: String,
    /// 整体耗时（毫秒）
    pub total_ms: u64,
}

/// 门户 HTTP 探测失败归类（与 monitor 探测证据结构的错误字段风格一致）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalProbeError {
    /// 请求超时（子超时或总限时内无响应）
    Timeout(String),
    /// 连接层失败（TCP/TLS；DNS 已由独立检查项覆盖）
    Connect(String),
    /// 其他请求错误
    Other(String),
}

impl PortalProbeError {
    /// 摘要前缀（一句话结论用）
    fn summary(&self) -> String {
        let detail = self.detail_text();
        match self {
            Self::Timeout(_) => format!("门户请求超时（{detail}）"),
            Self::Connect(_) => format!("门户连接失败（{detail}）"),
            Self::Other(_) => format!("门户请求失败（{detail}）"),
        }
    }

    /// 细节文本
    fn detail_text(&self) -> &str {
        match self {
            Self::Timeout(s) | Self::Connect(s) | Self::Other(s) => s,
        }
    }

    /// 序列化用的机器可读类别
    fn kind(&self) -> &'static str {
        match self {
            Self::Timeout(_) => "timeout",
            Self::Connect(_) => "connect",
            Self::Other(_) => "other",
        }
    }
}

/// 诊断输入：handler 从运行时快照与出口租约按需构造，测试可全量自造
pub struct DiagnosticInputs {
    /// 登录导航有效地址（auth_url；重定向/兜底时为触发地址）——探测与展示前
    /// 统一经 [`sanitize_portal_url`] 脱敏，原文不出现在任何结果里
    pub portal_url: String,
    /// 门户地址来源标记：auth_url = 用户配置的固定登录网址；fallback_trigger = 兜底触发地址
    pub portal_source: &'static str,
    /// 出口 reqwest 客户端构建结果（Err = 出口配置无法生效的原因，门户检查落失败）
    pub http_client: Result<reqwest::Client, String>,
    /// 出口租约（网关 TCP 竞速按方案绑定网卡；None = 出口不可用，回退系统路由直连）
    pub egress: Option<Arc<PreparedEgress>>,
}

/// 门户地址脱敏展示：剥除查询串、fragment 与内嵌凭据，保留 scheme://host:port/path
///
/// 兼容三种输入形态：完整 URL、裸 `host:port`（补 http scheme）、裸 IPv6 字面量
/// （补方括号）。解析失败返回 None，调用方以「认证地址格式无效」收敛。
pub fn sanitize_portal_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let mut url = match reqwest::Url::parse(raw) {
        Ok(u) => u,
        // 裸 host:port / 裸 IPv6：parse_url_host_port 认得而 Url::parse 不认得，
        // 补全 scheme 后重解析（与门户探测共用同一份地址解析语义）
        Err(_) => {
            let (host, port) = parse_url_host_port(raw)?;
            let authority = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            reqwest::Url::parse(&format!("http://{authority}")).ok()?
        }
    };
    // 凭据先于用户名清空：url crate 要求 password 的存在以 username 非空为前提
    if !url.username().is_empty() || url.password().is_some() {
        let _ = url.set_password(None);
        let _ = url.set_username("");
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

/// 探测后端抽象：生产实现走系统调用与真实网络，测试注入桩
#[async_trait::async_trait]
trait ProbeBackend: Send + Sync {
    /// 本机可用网卡清单（复用 network::adapters）
    async fn list_adapters(&self) -> std::io::Result<Vec<EgressAdapter>>;

    /// 默认网关 IP 列表（复用 network::detect）
    async fn default_gateways(&self) -> Result<Vec<IpAddr>, String>;

    /// TCP 竞速探测（复用 monitor TcpProbe 的 race 语义与明细结构）
    async fn tcp_race(
        &self,
        targets: Vec<String>,
        timeout: Duration,
        egress: Option<Arc<PreparedEgress>>,
    ) -> (ProbeOutcome, Vec<PerProbeDetail>);

    /// DNS 解析（系统解析器）
    async fn dns_lookup(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, String>;

    /// 门户 HTTP GET（重定向不跟随，按状态码归类）
    async fn portal_get(
        &self,
        client: &reqwest::Client,
        url: &str,
        timeout: Duration,
    ) -> Result<u16, PortalProbeError>;

    /// 最近登录历史（升序）
    async fn recent_history(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
        limit: usize,
    ) -> Result<Vec<LoginHistoryEntry>, String>;
}

/// 生产探测后端：真实系统调用 + 调用方传入的 HistoryStore
struct SystemBackend<'a> {
    /// 登录历史存储（来自 AppState 细粒度注入）
    history: &'a dyn HistoryStore,
}

#[async_trait::async_trait]
impl ProbeBackend for SystemBackend<'_> {
    async fn list_adapters(&self) -> std::io::Result<Vec<EgressAdapter>> {
        crate::network::adapters::list_adapters().await
    }

    async fn default_gateways(&self) -> Result<Vec<IpAddr>, String> {
        crate::network::create_detector()
            .default_gateways()
            .await
            .map(|v| v.into_iter().map(IpAddr::V4).collect())
            .map_err(|e| e.to_string())
    }

    async fn tcp_race(
        &self,
        targets: Vec<String>,
        timeout: Duration,
        egress: Option<Arc<PreparedEgress>>,
    ) -> (ProbeOutcome, Vec<PerProbeDetail>) {
        TcpProbe::run_via(&targets, timeout, egress).await
    }

    async fn dns_lookup(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
        tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| "DNS 解析超时".to_string())?
            .map(|addrs| addrs.map(|a| a.ip()).collect())
            .map_err(|e| e.to_string())
    }

    async fn portal_get(
        &self,
        client: &reqwest::Client,
        url: &str,
        timeout: Duration,
    ) -> Result<u16, PortalProbeError> {
        match client.get(url).timeout(timeout).send().await {
            // 只取状态码，不读 body：门户页面可能携带一次性 token，不进诊断结果
            Ok(resp) => Ok(resp.status().as_u16()),
            Err(e) if e.is_timeout() => Err(PortalProbeError::Timeout(e.to_string())),
            Err(e) if e.is_connect() => Err(PortalProbeError::Connect(e.to_string())),
            Err(e) => Err(PortalProbeError::Other(e.to_string())),
        }
    }

    async fn recent_history(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
        limit: usize,
    ) -> Result<Vec<LoginHistoryEntry>, String> {
        self.history
            .query_latest(from, to, limit)
            .await
            .map_err(|e| e.to_string())
    }
}

/// 执行一次完整诊断（生产入口）：五项并发，整体受 [`TOTAL_TIMEOUT`] 约束
pub async fn run(inputs: DiagnosticInputs, history: &dyn HistoryStore) -> DiagnosticReport {
    let backend = SystemBackend { history };
    run_with_backend(inputs, &backend, TOTAL_TIMEOUT).await
}

/// 可注入后端与总超时的编排核心（测试入口）
async fn run_with_backend(
    inputs: DiagnosticInputs,
    backend: &dyn ProbeBackend,
    total: Duration,
) -> DiagnosticReport {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + total;
    // 全部检查并发启动：整体耗时 ≈ max(单项耗时) 而非 sum，
    // 每项再受总限时硬约束（timeout_at），超时项以 fail 收敛
    let (interfaces, gateway, dns, portal, history_check) = tokio::join!(
        check_interfaces(backend, deadline),
        check_gateway(&inputs, backend, deadline),
        check_dns(&inputs, backend, deadline),
        check_portal(&inputs, backend, deadline),
        check_history(backend, deadline),
    );
    let checks = vec![interfaces, gateway, dns, portal, history_check];
    let (verdict, conclusion) = summarize_checks(&checks);
    DiagnosticReport {
        checks,
        verdict,
        conclusion,
        total_ms: started.elapsed().as_millis() as u64,
    }
}

/// 按检查项状态汇总总结论：fail 优先，其次 warn，skip 不降级
fn summarize_checks(checks: &[DiagnosticCheck]) -> (DiagnosticVerdict, String) {
    let names = |status: DiagnosticStatus| -> Vec<&str> {
        checks
            .iter()
            .filter(|c| c.status == status)
            .map(|c| c.name)
            .collect()
    };
    let failed = names(DiagnosticStatus::Fail);
    let warned = names(DiagnosticStatus::Warn);
    let skipped = names(DiagnosticStatus::Skip);
    let skip_suffix = if skipped.is_empty() {
        String::new()
    } else {
        format!("（{} 项跳过）", skipped.len())
    };
    if !failed.is_empty() {
        (
            DiagnosticVerdict::Faulty,
            format!(
                "发现 {} 项异常（{}），请优先处理失败项",
                failed.len(),
                failed.join("、")
            ),
        )
    } else if !warned.is_empty() {
        (
            DiagnosticVerdict::Degraded,
            format!(
                "链路可用，但存在 {} 项警告（{}）{skip_suffix}",
                warned.len(),
                warned.join("、")
            ),
        )
    } else {
        (
            DiagnosticVerdict::Healthy,
            format!("各项检查均通过{skip_suffix}"),
        )
    }
}

/// 检查项一：本机网卡清单（复用 network::adapters）
async fn check_interfaces(
    backend: &dyn ProbeBackend,
    deadline: tokio::time::Instant,
) -> DiagnosticCheck {
    let started = Instant::now();
    let fut = async {
        match backend.list_adapters().await {
            Ok(adapters) if adapters.is_empty() => DiagnosticCheck {
                key: "interfaces",
                name: "本机网卡",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: "未发现具有有效 IPv4 的已连接网卡".into(),
                details: json!({ "count": 0 }),
            },
            Ok(adapters) => {
                let count = adapters.len();
                let items: Vec<_> = adapters
                    .iter()
                    .map(|a| {
                        json!({
                            "name": a.name,
                            "ipv4": a.ipv4.to_string(),
                            "dns_servers": a.dns_servers,
                        })
                    })
                    .collect();
                DiagnosticCheck {
                    key: "interfaces",
                    name: "本机网卡",
                    status: DiagnosticStatus::Ok,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    summary: format!("发现 {count} 块可用网卡"),
                    details: json!({ "count": count, "adapters": items }),
                }
            }
            Err(e) => DiagnosticCheck {
                key: "interfaces",
                name: "本机网卡",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: format!("网卡枚举失败：{e}"),
                details: json!({ "error": e.to_string() }),
            },
        }
    };
    with_total_deadline("interfaces", "本机网卡", started, deadline, fut).await
}

/// 检查项二：默认网关可达性（复用 network::detect + monitor TcpProbe 竞速）
///
/// 连接被拒绝（RST）同样证明网关 IP 有响应——端口未开放但链路可达，按通过
/// 处理并如实标注，避免把「网关活着但不开 80/443/8080」误报成断网。
async fn check_gateway(
    inputs: &DiagnosticInputs,
    backend: &dyn ProbeBackend,
    deadline: tokio::time::Instant,
) -> DiagnosticCheck {
    let started = Instant::now();
    let fut = async {
        match backend.default_gateways().await {
            Err(e) => DiagnosticCheck {
                key: "gateway",
                name: "默认网关",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: format!("无法获取默认网关：{e}"),
                details: json!({ "error": e }),
            },
            Ok(gateways) if gateways.is_empty() => DiagnosticCheck {
                key: "gateway",
                name: "默认网关",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: "未检测到默认网关（本机可能未接入任何网络）".into(),
                details: json!({ "gateways": [] }),
            },
            Ok(gateways) => {
                let targets: Vec<String> = gateways
                    .iter()
                    .flat_map(|gw| {
                        GATEWAY_PROBE_PORTS
                            .iter()
                            .map(move |port| format!("{gw}:{port}"))
                    })
                    .collect();
                let (outcome, details) = backend
                    .tcp_race(targets, GATEWAY_CONNECT_TIMEOUT, inputs.egress.clone())
                    .await;
                let probes: Vec<_> = details
                    .iter()
                    .map(|d| {
                        json!({
                            "target": d.target,
                            "success": d.success,
                            "elapsed_ms": d.elapsed_ms,
                            "error": d.error,
                        })
                    })
                    .collect();
                let first_error = details
                    .iter()
                    .find_map(|d| d.error.as_deref())
                    .unwrap_or("全部探测端口连接失败");
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let gateway_list: Vec<String> = gateways.iter().map(|g| g.to_string()).collect();
                match outcome {
                    ProbeOutcome::Pass => {
                        let winner = details.iter().find(|d| d.success);
                        let hit = winner.map(|d| d.target.as_str()).unwrap_or("");
                        DiagnosticCheck {
                            key: "gateway",
                            name: "默认网关",
                            status: DiagnosticStatus::Ok,
                            elapsed_ms,
                            summary: format!("网关 {} 可达（TCP {} 连通）", gateways[0], hit),
                            details: json!({
                                "gateways": gateway_list,
                                "probes": probes,
                            }),
                        }
                    }
                    ProbeOutcome::Fail
                        if details.iter().any(|d| is_refused(d.error.as_deref())) =>
                    {
                        DiagnosticCheck {
                            key: "gateway",
                            name: "默认网关",
                            status: DiagnosticStatus::Ok,
                            elapsed_ms,
                            summary: format!(
                                "网关 {} 有响应（探测端口未开放，连接被拒绝即链路可达）",
                                gateways[0]
                            ),
                            details: json!({
                                "gateways": gateway_list,
                                "probes": probes,
                            }),
                        }
                    }
                    _ => DiagnosticCheck {
                        key: "gateway",
                        name: "默认网关",
                        status: DiagnosticStatus::Fail,
                        elapsed_ms,
                        summary: format!("网关 {} 不可达：{first_error}", gateways[0]),
                        details: json!({
                            "gateways": gateway_list,
                            "probes": probes,
                        }),
                    },
                }
            }
        }
    };
    with_total_deadline("gateway", "默认网关", started, deadline, fut).await
}

/// 连接拒绝识别：error 文本匹配 refused 或常见平台的 os error 码
///
/// TcpProbe 明细里错误已折成字符串（保留 io::ErrorKind 需要改探测基建签名），
/// 这里按文本特征识别——覆盖 Unix（os error 111/61）与 Windows（os error 1225）
/// 的英文与本地化文案（错误文本恒带 `os error N` 尾巴）。
fn is_refused(error: Option<&str>) -> bool {
    let Some(text) = error else {
        return false;
    };
    let lower = text.to_lowercase();
    lower.contains("refused")
        || lower.contains("os error 111")
        || lower.contains("os error 61")
        || lower.contains("os error 1225")
}

/// 检查项三：DNS 解析（解析门户地址的 host；IP 直连则跳过）
async fn check_dns(
    inputs: &DiagnosticInputs,
    backend: &dyn ProbeBackend,
    deadline: tokio::time::Instant,
) -> DiagnosticCheck {
    let started = Instant::now();
    let fut = async {
        let Some((host, port)) = parse_url_host_port(&inputs.portal_url) else {
            return DiagnosticCheck {
                key: "dns",
                name: "DNS 解析",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: "认证地址格式无效，无法解析主机名".into(),
                details: json!({ "error": "invalid_url" }),
            };
        };
        if host.parse::<IpAddr>().is_ok() {
            return DiagnosticCheck {
                key: "dns",
                name: "DNS 解析",
                status: DiagnosticStatus::Skip,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: format!("认证地址 {host} 为 IP 直连，不经过 DNS 解析"),
                details: json!({ "host": host, "reason": "ip_literal" }),
            };
        }
        match backend.dns_lookup(&host, port).await {
            Ok(ips) if ips.is_empty() => DiagnosticCheck {
                key: "dns",
                name: "DNS 解析",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: format!("主机 {host} 未解析到任何地址"),
                details: json!({ "host": host, "ips": [] }),
            },
            Ok(ips) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let ip_texts: Vec<String> = ips.iter().map(|ip| ip.to_string()).collect();
                DiagnosticCheck {
                    key: "dns",
                    name: "DNS 解析",
                    status: DiagnosticStatus::Ok,
                    elapsed_ms,
                    summary: format!(
                        "主机 {host} 解析到 {} 个地址（{elapsed_ms}ms）",
                        ip_texts.len()
                    ),
                    details: json!({
                        "host": host,
                        "port": port,
                        "ips": ip_texts,
                        "url_source": inputs.portal_source,
                    }),
                }
            }
            Err(e) => DiagnosticCheck {
                key: "dns",
                name: "DNS 解析",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: format!("主机 {host} 解析失败：{e}"),
                details: json!({ "host": host, "error": e }),
            },
        }
    };
    with_total_deadline("dns", "DNS 解析", started, deadline, fut).await
}

/// 检查项四：门户可达性（HTTP 探测脱敏后的门户地址，重定向不跟随）
async fn check_portal(
    inputs: &DiagnosticInputs,
    backend: &dyn ProbeBackend,
    deadline: tokio::time::Instant,
) -> DiagnosticCheck {
    let started = Instant::now();
    let fut = async {
        // 展示与请求统一用脱敏地址：查询串 / fragment / 内嵌凭据不进结果
        let Some(url) = sanitize_portal_url(&inputs.portal_url) else {
            return DiagnosticCheck {
                key: "portal",
                name: "认证门户",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: "认证地址格式无效，无法发起探测".into(),
                details: json!({ "error": "invalid_url" }),
            };
        };
        let Ok(client) = inputs.http_client.as_ref() else {
            let reason = inputs.http_client.as_ref().unwrap_err().clone();
            return DiagnosticCheck {
                key: "portal",
                name: "认证门户",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: format!("出口客户端构建失败：{reason}"),
                details: json!({ "url": url, "error": reason }),
            };
        };
        match backend.portal_get(client, &url, PORTAL_HTTP_TIMEOUT).await {
            Ok(status) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let (status_kind, summary) = if (200..400).contains(&status) {
                    (
                        DiagnosticStatus::Ok,
                        format!("门户可达（HTTP {status}，{elapsed_ms}ms）"),
                    )
                } else {
                    (
                        DiagnosticStatus::Warn,
                        format!("门户响应异常（HTTP {status}）：链路可达，但门户服务可能未就绪"),
                    )
                };
                DiagnosticCheck {
                    key: "portal",
                    name: "认证门户",
                    status: status_kind,
                    elapsed_ms,
                    summary,
                    details: json!({
                        "url": url,
                        "http_status": status,
                        "url_source": inputs.portal_source,
                    }),
                }
            }
            Err(e) => DiagnosticCheck {
                key: "portal",
                name: "认证门户",
                status: DiagnosticStatus::Fail,
                elapsed_ms: started.elapsed().as_millis() as u64,
                summary: e.summary(),
                details: json!({
                    "url": url,
                    "error": e.detail_text(),
                    "kind": e.kind(),
                    "url_source": inputs.portal_source,
                }),
            },
        }
    };
    with_total_deadline("portal", "认证门户", started, deadline, fut).await
}

/// 检查项五：近期登录失败归因（复用 LoginHistoryService 近期查询）
async fn check_history(
    backend: &dyn ProbeBackend,
    deadline: tokio::time::Instant,
) -> DiagnosticCheck {
    let started = Instant::now();
    let fut = async {
        let to = Local::now();
        let from = to - chrono::Duration::days(FAILURE_LOOKBACK_DAYS);
        let (status, summary, details) =
            match backend.recent_history(from, to, HISTORY_FETCH_LIMIT).await {
                Err(e) => (
                    DiagnosticStatus::Warn,
                    format!("登录历史读取失败，无法归因：{e}"),
                    json!({ "error": e }),
                ),
                Ok(entries) => attribute_failures(&entries),
            };
        DiagnosticCheck {
            key: "history",
            name: "近期登录失败",
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            summary,
            details,
        }
    };
    with_total_deadline("history", "近期登录失败", started, deadline, fut).await
}

/// 单项检查的总限时包装：子超时之外再受整体 deadline 约束，超时以 fail 收敛
async fn with_total_deadline<F>(
    key: &'static str,
    name: &'static str,
    started: Instant,
    deadline: tokio::time::Instant,
    fut: F,
) -> DiagnosticCheck
where
    F: std::future::Future<Output = DiagnosticCheck>,
{
    match tokio::time::timeout_at(deadline, fut).await {
        Ok(check) => check,
        Err(_) => DiagnosticCheck {
            key,
            name,
            status: DiagnosticStatus::Fail,
            elapsed_ms: started.elapsed().as_millis() as u64,
            summary: "诊断总限时内未完成，已收敛返回".into(),
            details: json!({ "reason": "total_timeout" }),
        },
    }
}

/// 登录失败归类（关键字口径与 `login::session::login_failure_hint` 保持一致，
/// 演化时须同步）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureCategory {
    /// Python 环境 / 浏览器未就绪
    EnvNotReady,
    /// 验证码 / OCR 异常
    Captcha,
    /// 凭据无效（账号 / 密码 / 认证失败）
    InvalidCredential,
    /// 认证页结构变化（选择器失效）
    PageSelector,
    /// 网络 / 门户超时
    Timeout,
    /// 网络不可达
    Network,
    /// 其他原因
    Other,
}

impl FailureCategory {
    /// 中文名（进结论与分类统计）
    fn label(&self) -> &'static str {
        match self {
            Self::EnvNotReady => "运行环境未就绪",
            Self::Captcha => "验证码/OCR 异常",
            Self::InvalidCredential => "凭据无效",
            Self::PageSelector => "认证页结构变化",
            Self::Timeout => "网络/门户超时",
            Self::Network => "网络不可达",
            Self::Other => "其他原因",
        }
    }

    /// 一句话处置建议
    fn hint(&self) -> &'static str {
        match self {
            Self::EnvNotReady => "可先在设置页初始化 Python 环境或检查浏览器配置",
            Self::Captcha => "门户出现验证码或 OCR 异常，可改用手动登录",
            Self::InvalidCredential => "请核对方案中的账号与密码",
            Self::PageSelector => "认证页结构可能已变化，需更新登录任务步骤",
            Self::Timeout => "弱网或门户响应慢，可稍后重试",
            Self::Network => "请检查本机网络与网关连通性",
            Self::Other => "可查看完整日志或手动打开认证页确认",
        }
    }
}

/// 按失败消息关键字归类（顺序与 login_failure_hint 一致：环境 → 验证码 →
/// 凭据 → 选择器 → 超时 → 网络 → 兜底，先命中先归类）
fn classify_failure(message: &str) -> FailureCategory {
    if message.contains("未就绪")
        || message.contains("初始化")
        || message.contains("Worker 未安装")
        || message.contains("浏览器")
    {
        FailureCategory::EnvNotReady
    } else if message.contains("验证码") || message.contains("OCR") || message.contains("captcha")
    {
        FailureCategory::Captcha
    } else if message.contains("密码")
        || message.contains("账号")
        || message.contains("用户名")
        || message.contains("认证失败")
    {
        FailureCategory::InvalidCredential
    } else if message.contains("选择器") || message.contains("selector") || message.contains("元素")
    {
        FailureCategory::PageSelector
    } else if message.contains("超时") || message.to_lowercase().contains("timeout") {
        FailureCategory::Timeout
    } else if message.contains("网络")
        || message.contains("连接")
        || message.contains("DNS")
        || message.to_lowercase().contains("dns")
    {
        FailureCategory::Network
    } else {
        FailureCategory::Other
    }
}

/// 失败归因：从升序历史里取最近 FAILURE_SAMPLE 条失败，给出（状态, 一句话, 细节）
///
/// - 无记录 / 无失败 → skip（历史失败属参考信息，不直接降级当前网络结论）
/// - 最近失败同属一类 → 「最近 N 次登录失败均为「X」；建议」
/// - 混合 → 以出现最多的类别为主因，附最新一次消息（截断）
fn attribute_failures(
    entries: &[LoginHistoryEntry],
) -> (DiagnosticStatus, String, serde_json::Value) {
    let failures: Vec<&LoginHistoryEntry> = entries
        .iter()
        .filter(|e| e.result == HistoryResult::Failed)
        .collect();
    let sample_start = failures.len().saturating_sub(FAILURE_SAMPLE);
    let recent = &failures[sample_start..];
    if recent.is_empty() {
        let summary = if entries.is_empty() {
            format!("最近 {FAILURE_LOOKBACK_DAYS} 天无登录记录")
        } else {
            format!("最近 {FAILURE_LOOKBACK_DAYS} 天无登录失败记录")
        };
        return (
            DiagnosticStatus::Skip,
            summary,
            json!({ "window_days": FAILURE_LOOKBACK_DAYS, "failure_count": 0 }),
        );
    }
    let n = recent.len();
    let latest = recent[n - 1];
    let latest_category = classify_failure(&latest.message);
    // 分类计数（保持时间顺序遍历，平票时首个达到最大计数的类别胜出）
    let mut counts: Vec<(FailureCategory, usize)> = Vec::new();
    for entry in recent {
        let category = classify_failure(&entry.message);
        match counts.iter_mut().find(|(c, _)| *c == category) {
            Some((_, count)) => *count += 1,
            None => counts.push((category, 1)),
        }
    }
    let dominant = counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(c, _)| *c)
        .unwrap_or(FailureCategory::Other);
    let categories_json: serde_json::Map<String, serde_json::Value> = counts
        .iter()
        .map(|(c, count)| (c.label().to_string(), json!(count)))
        .collect();
    let latest_json = json!({
        "timestamp": latest.timestamp.to_rfc3339(),
        "channel": latest.channel,
        "message": truncate_chars(&latest.message, FAILURE_MESSAGE_MAX_CHARS),
        "category": latest_category.label(),
    });
    let details = json!({
        "window_days": FAILURE_LOOKBACK_DAYS,
        "failure_count": n,
        "categories": categories_json,
        "latest": latest_json,
    });
    let summary = if counts.len() == 1 {
        if n == 1 {
            format!(
                "最近一次登录失败：{}；{}",
                latest_category.label(),
                latest_category.hint()
            )
        } else {
            format!(
                "最近 {n} 次登录失败均为「{}」；{}",
                dominant.label(),
                dominant.hint()
            )
        }
    } else {
        let distinct = counts.len();
        format!(
            "最近 {n} 次登录失败涉及 {distinct} 类原因，主要为「{}」；最新一次：{}（{}）",
            dominant.label(),
            truncate_chars(&latest.message, FAILURE_MESSAGE_MAX_CHARS),
            latest_category.hint()
        )
    };
    (DiagnosticStatus::Warn, summary, details)
}

/// 按字符数截断（char 安全，不劈开多字节字符）
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::LoginSource;

    // ============ 桩后端 ============

    /// 全可配置的探测桩：字段为 Option 时该项返回 Err；`slow` 列表中的检查项
    /// 先 sleep `delay`（用于总超时收敛测试，sleep future 会被 timeout 丢弃）
    struct StubBackend {
        /// 错误以 String 承载（io::Error 不可 Clone），trait 实现处再转回 io::Error
        adapters: Option<Result<Vec<EgressAdapter>, String>>,
        gateways: Option<Result<Vec<IpAddr>, String>>,
        tcp: (ProbeOutcome, Vec<PerProbeDetail>),
        dns: Option<Result<Vec<IpAddr>, String>>,
        portal: Option<Result<u16, PortalProbeError>>,
        history: Option<Result<Vec<LoginHistoryEntry>, String>>,
        slow: &'static [&'static str],
        delay: Duration,
    }

    impl Default for StubBackend {
        fn default() -> Self {
            Self {
                adapters: Some(Ok(vec![sample_adapter()])),
                gateways: Some(Ok(vec![IpAddr::from([192, 168, 1, 1])])),
                tcp: (ProbeOutcome::Pass, vec![ok_detail("192.168.1.1:80", 3)]),
                dns: Some(Ok(vec![IpAddr::from([10, 10, 1, 1])])),
                portal: Some(Ok(200)),
                history: Some(Ok(vec![])),
                slow: &[],
                delay: Duration::from_secs(1),
            }
        }
    }

    fn sample_adapter() -> EgressAdapter {
        EgressAdapter {
            id: "guid-1".into(),
            name: "以太网".into(),
            index: 1,
            ipv4: std::net::Ipv4Addr::new(10, 0, 0, 2),
            mac: String::new(),
            dns_servers: vec!["10.0.0.1".to_string()],
        }
    }

    fn ok_detail(target: &str, elapsed_ms: u64) -> PerProbeDetail {
        PerProbeDetail {
            target: target.to_string(),
            success: true,
            elapsed_ms,
            http_status: None,
            error: None,
        }
    }

    fn fail_detail(target: &str, error: &str) -> PerProbeDetail {
        PerProbeDetail {
            target: target.to_string(),
            success: false,
            elapsed_ms: 1,
            http_status: None,
            error: Some(error.to_string()),
        }
    }

    impl StubBackend {
        async fn stall(&self, key: &str) {
            if self.slow.contains(&key) {
                tokio::time::sleep(self.delay).await;
            }
        }
    }

    #[async_trait::async_trait]
    impl ProbeBackend for StubBackend {
        async fn list_adapters(&self) -> std::io::Result<Vec<EgressAdapter>> {
            self.stall("interfaces").await;
            match &self.adapters {
                Some(Ok(list)) => Ok(list.clone()),
                Some(Err(e)) => Err(std::io::Error::other(e.clone())),
                None => panic!("stub adapters 必须配置"),
            }
        }

        async fn default_gateways(&self) -> Result<Vec<IpAddr>, String> {
            self.stall("gateway").await;
            self.gateways.clone().expect("stub gateways 必须配置")
        }

        async fn tcp_race(
            &self,
            targets: Vec<String>,
            _timeout: Duration,
            _egress: Option<Arc<PreparedEgress>>,
        ) -> (ProbeOutcome, Vec<PerProbeDetail>) {
            self.stall("gateway").await;
            // 回填真实目标，保证汇总细节与编排一致
            let details = if self.tcp.1.is_empty() {
                self.tcp.1.clone()
            } else {
                self.tcp
                    .1
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let mut d = d.clone();
                        if let Some(t) = targets.get(i) {
                            d.target = t.clone();
                        }
                        d
                    })
                    .collect()
            };
            (self.tcp.0, details)
        }

        async fn dns_lookup(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, String> {
            self.stall("dns").await;
            self.dns.clone().expect("stub dns 必须配置")
        }

        async fn portal_get(
            &self,
            _client: &reqwest::Client,
            _url: &str,
            _timeout: Duration,
        ) -> Result<u16, PortalProbeError> {
            self.stall("portal").await;
            self.portal.clone().expect("stub portal 必须配置")
        }

        async fn recent_history(
            &self,
            _from: DateTime<Local>,
            _to: DateTime<Local>,
            _limit: usize,
        ) -> Result<Vec<LoginHistoryEntry>, String> {
            self.stall("history").await;
            self.history.clone().expect("stub history 必须配置")
        }
    }

    // ============ 输入构造 ============

    /// 带查询串与内嵌凭据的门户地址：同时服务编排测试与脱敏断言
    const LEAKY_PORTAL_URL: &str = "http://admin:s3cret@10.0.0.55:8080/login?token=xyz&next=/pay";

    /// 带凭据与查询串的主机名门户地址：host 非 IP 字面量，DNS 检查不被跳过
    const LEAKY_HOST_URL: &str =
        "http://admin:s3cret@portal.campus.edu:8080/login?token=xyz&next=/pay";

    fn leaky_inputs() -> DiagnosticInputs {
        DiagnosticInputs {
            portal_url: LEAKY_PORTAL_URL.to_string(),
            portal_source: "auth_url",
            http_client: Ok(reqwest::Client::builder().no_proxy().build().unwrap()),
            egress: None,
        }
    }

    /// 同 [`leaky_inputs`]，但门户地址为主机名形态（DNS 检查真正执行）
    fn host_inputs() -> DiagnosticInputs {
        DiagnosticInputs {
            portal_url: LEAKY_HOST_URL.to_string(),
            ..leaky_inputs()
        }
    }

    async fn report_of(backend: StubBackend, inputs: DiagnosticInputs) -> DiagnosticReport {
        run_with_backend(inputs, &backend, TOTAL_TIMEOUT).await
    }

    fn check_of<'a>(report: &'a DiagnosticReport, key: &str) -> &'a DiagnosticCheck {
        report
            .checks
            .iter()
            .find(|c| c.key == key)
            .unwrap_or_else(|| panic!("缺少检查项 {key}"))
    }

    // ============ 探测编排 ============

    #[tokio::test]
    async fn 编排全绿_五项有序且结论健康() {
        let report = report_of(StubBackend::default(), host_inputs()).await;
        let keys: Vec<&str> = report.checks.iter().map(|c| c.key).collect();
        assert_eq!(
            keys,
            vec!["interfaces", "gateway", "dns", "portal", "history"]
        );
        // 探测类四项全 Ok；history 项无失败记录恒为 Skip（不降级总结论）
        let non_history = report
            .checks
            .iter()
            .filter(|c| c.key != "history")
            .all(|c| c.status == DiagnosticStatus::Ok);
        assert!(non_history);
        assert_eq!(check_of(&report, "history").status, DiagnosticStatus::Skip);
        assert_eq!(report.verdict, DiagnosticVerdict::Healthy);
        assert!(report.conclusion.contains("各项检查均通过"));
        assert!(report.conclusion.contains("1 项跳过"));
        assert!(report.total_ms < TOTAL_TIMEOUT.as_millis() as u64);
        // 桩数据透传：DNS 细节携带解析出的 IP，门户细节带状态码与脱敏地址
        assert_eq!(check_of(&report, "dns").details["ips"][0], "10.10.1.1");
        assert_eq!(check_of(&report, "portal").details["http_status"], 200);
    }

    #[tokio::test]
    async fn 编排失败项进结论_总结论判脏() {
        let backend = StubBackend {
            gateways: Some(Ok(vec![])),
            tcp: (ProbeOutcome::Fail, vec![]),
            dns: Some(Err("解析超时".into())),
            portal: Some(Ok(503)),
            ..StubBackend::default()
        };
        let report = report_of(backend, host_inputs()).await;
        assert_eq!(check_of(&report, "gateway").status, DiagnosticStatus::Fail);
        assert_eq!(check_of(&report, "dns").status, DiagnosticStatus::Fail);
        assert_eq!(check_of(&report, "portal").status, DiagnosticStatus::Warn);
        assert_eq!(report.verdict, DiagnosticVerdict::Faulty);
        assert!(report.conclusion.contains("发现 2 项异常"));
        assert!(report.conclusion.contains("默认网关"));
        assert!(report.conclusion.contains("DNS 解析"));
    }

    #[tokio::test]
    async fn 跳过项不降级总结论() {
        // IP 直连门户 → DNS 跳过；空历史 → history 也 Skip；两处跳过均不降级
        let inputs = DiagnosticInputs {
            portal_url: "http://10.0.0.55:8080/login".into(),
            portal_source: "auth_url",
            ..leaky_inputs()
        };
        let report = report_of(StubBackend::default(), inputs).await;
        assert_eq!(check_of(&report, "dns").status, DiagnosticStatus::Skip);
        assert_eq!(check_of(&report, "history").status, DiagnosticStatus::Skip);
        assert_eq!(report.verdict, DiagnosticVerdict::Healthy);
        assert!(report.conclusion.contains("2 项跳过"));
    }

    // ============ 总超时收敛 ============

    #[tokio::test]
    async fn 总超时收敛_全部慢探测有界返回() {
        let backend = StubBackend {
            slow: &["interfaces", "gateway", "dns", "portal", "history"],
            delay: Duration::from_secs(30),
            ..StubBackend::default()
        };
        let started = Instant::now();
        let report = run_with_backend(host_inputs(), &backend, Duration::from_millis(200)).await;
        let wall = started.elapsed();
        assert!(
            wall < Duration::from_secs(3),
            "整体必须在总限时附近收敛返回（实际 {wall:?}）"
        );
        // 五项全部 fail 且标记总限时
        for check in &report.checks {
            assert_eq!(check.status, DiagnosticStatus::Fail, "项 {}", check.key);
            assert_eq!(check.details["reason"], "total_timeout");
            assert!(check.elapsed_ms < 1000, "超时项耗时应有界: {check:?}");
        }
        assert_eq!(report.verdict, DiagnosticVerdict::Faulty);
    }

    #[tokio::test]
    async fn 总超时收敛_只有慢项被标记() {
        let backend = StubBackend {
            slow: &["dns"],
            delay: Duration::from_secs(30),
            ..StubBackend::default()
        };
        let report = run_with_backend(host_inputs(), &backend, Duration::from_millis(250)).await;
        assert_eq!(check_of(&report, "dns").status, DiagnosticStatus::Fail);
        assert_eq!(check_of(&report, "dns").details["reason"], "total_timeout");
        assert_eq!(check_of(&report, "interfaces").status, DiagnosticStatus::Ok);
        assert_eq!(check_of(&report, "portal").status, DiagnosticStatus::Ok);
        assert_eq!(report.verdict, DiagnosticVerdict::Faulty);
    }

    // ============ 网关检查分支 ============

    #[tokio::test]
    async fn 网关连接被拒视为可达() {
        let backend = StubBackend {
            tcp: (
                ProbeOutcome::Fail,
                vec![
                    fail_detail("192.168.1.1:80", "Connection refused (os error 111)"),
                    fail_detail("192.168.1.1:443", "Connection refused (os error 111)"),
                ],
            ),
            ..StubBackend::default()
        };
        let report = report_of(backend, leaky_inputs()).await;
        let gateway = check_of(&report, "gateway");
        assert_eq!(gateway.status, DiagnosticStatus::Ok);
        assert!(gateway.summary.contains("有响应"));
    }

    #[tokio::test]
    async fn 网关超时按失败处理() {
        let backend = StubBackend {
            tcp: (
                ProbeOutcome::Fail,
                vec![fail_detail("192.168.1.1:80", "tcp connect timeout")],
            ),
            ..StubBackend::default()
        };
        let report = report_of(backend, leaky_inputs()).await;
        let gateway = check_of(&report, "gateway");
        assert_eq!(gateway.status, DiagnosticStatus::Fail);
        assert!(gateway.summary.contains("不可达"));
    }

    // ============ 门户检查分支 ============

    #[tokio::test]
    async fn 门户非2xx3xx按警告处理() {
        let backend = StubBackend {
            portal: Some(Ok(403)),
            ..StubBackend::default()
        };
        let report = report_of(backend, leaky_inputs()).await;
        let portal = check_of(&report, "portal");
        assert_eq!(portal.status, DiagnosticStatus::Warn);
        assert!(portal.summary.contains("403"));
    }

    #[tokio::test]
    async fn 门户超时归类为失败() {
        let backend = StubBackend {
            portal: Some(Err(PortalProbeError::Timeout("timeout".into()))),
            ..StubBackend::default()
        };
        let report = report_of(backend, leaky_inputs()).await;
        let portal = check_of(&report, "portal");
        assert_eq!(portal.status, DiagnosticStatus::Fail);
        assert!(portal.summary.contains("超时"));
        assert_eq!(portal.details["kind"], "timeout");
    }

    #[tokio::test]
    async fn 出口客户端构建失败门户落失败() {
        let inputs = DiagnosticInputs {
            http_client: Err("代理地址无法解析".into()),
            ..leaky_inputs()
        };
        let report = report_of(StubBackend::default(), inputs).await;
        let portal = check_of(&report, "portal");
        assert_eq!(portal.status, DiagnosticStatus::Fail);
        assert!(portal.summary.contains("出口客户端构建失败"));
        // 其余检查不受出口失败影响
        assert_eq!(check_of(&report, "gateway").status, DiagnosticStatus::Ok);
    }

    // ============ 失败归因分支 ============

    fn entry(result: HistoryResult, message: &str, offset_secs: i64) -> LoginHistoryEntry {
        LoginHistoryEntry {
            timestamp: Local::now() - chrono::Duration::seconds(offset_secs),
            source: LoginSource::Auto,
            channel: Some(crate::config::LoginChannel::Browser),
            profile_id: "default".into(),
            result,
            message: message.to_string(),
            duration_secs: 1.0,
        }
    }

    #[test]
    fn 归因_无记录与无失败均跳过() {
        let (status, summary, details) = attribute_failures(&[]);
        assert_eq!(status, DiagnosticStatus::Skip);
        assert!(summary.contains("无登录记录"));
        assert_eq!(details["failure_count"], 0);

        let ok_only = vec![entry(HistoryResult::Success, "登录成功", 60)];
        let (status, summary, _) = attribute_failures(&ok_only);
        assert_eq!(status, DiagnosticStatus::Skip);
        assert!(summary.contains("无登录失败记录"));
    }

    #[test]
    fn 归因_连续同类失败给单一结论() {
        let entries = vec![
            entry(HistoryResult::Success, "登录成功", 300),
            entry(HistoryResult::Failed, "门户返回：用户名或密码错误", 200),
            entry(HistoryResult::Failed, "认证失败：密码不正确", 100),
            entry(HistoryResult::Failed, "账号或密码有误", 10),
        ];
        let (status, summary, details) = attribute_failures(&entries);
        assert_eq!(status, DiagnosticStatus::Warn);
        assert!(
            summary.contains("最近 3 次登录失败均为「凭据无效」"),
            "{summary}"
        );
        assert!(summary.contains("核对方案中的账号与密码"));
        assert_eq!(details["failure_count"], 3);
        assert_eq!(details["latest"]["category"], "凭据无效");
    }

    #[test]
    fn 归因_单一失败用最近一次措辞() {
        let entries = vec![entry(HistoryResult::Failed, "门户响应超时: 30000ms", 30)];
        let (status, summary, _) = attribute_failures(&entries);
        assert_eq!(status, DiagnosticStatus::Warn);
        assert!(
            summary.contains("最近一次登录失败：网络/门户超时"),
            "{summary}"
        );
    }

    #[test]
    fn 归因_混合失败报主因与最新一次() {
        let entries = vec![
            entry(HistoryResult::Failed, "输入元素操作超时: #password", 300),
            entry(HistoryResult::Failed, "门户返回：用户名或密码错误", 200),
            entry(HistoryResult::Failed, "认证失败", 100),
            entry(
                HistoryResult::Failed,
                "浏览器能力未就绪，自动初始化失败",
                10,
            ),
        ];
        let (status, summary, details) = attribute_failures(&entries);
        assert_eq!(status, DiagnosticStatus::Warn);
        assert!(summary.contains("涉及 3 类原因"), "{summary}");
        assert!(summary.contains("主要为「凭据无效」"), "{summary}");
        assert!(summary.contains("最新一次"), "{summary}");
        // 最新一次消息（环境未就绪）进入细节，且带类别标签
        assert_eq!(details["latest"]["category"], "运行环境未就绪");
        assert_eq!(details["categories"]["凭据无效"], 2);
    }

    #[test]
    fn 归因_只看最近若干条失败() {
        // 超出 FAILURE_SAMPLE 的旧失败不参与归因
        let mut entries = vec![entry(HistoryResult::Success, "登录成功", 10)];
        for i in 0..(FAILURE_SAMPLE + 3) as i64 {
            entries.push(entry(HistoryResult::Failed, "门户响应超时", 60 * (i + 1)));
        }
        let (_, summary, details) = attribute_failures(&entries);
        assert_eq!(details["failure_count"], FAILURE_SAMPLE);
        assert!(summary.contains("最近 5 次登录失败"));
    }

    #[test]
    fn 失败消息关键字归类分支() {
        assert_eq!(
            classify_failure("浏览器能力未就绪，自动初始化失败"),
            FailureCategory::EnvNotReady
        );
        assert_eq!(
            classify_failure("OCR 识别超时（>60s）"),
            FailureCategory::Captcha
        );
        assert_eq!(
            classify_failure("门户返回：用户名或密码错误"),
            FailureCategory::InvalidCredential
        );
        assert_eq!(
            classify_failure("操作超时: 30000ms"),
            FailureCategory::Timeout
        );
        assert_eq!(classify_failure("网络验证未通过"), FailureCategory::Network);
        assert_eq!(classify_failure("boom"), FailureCategory::Other);
    }

    #[test]
    fn 归因消息超长截断() {
        let long = "x".repeat(300);
        let entries = vec![entry(HistoryResult::Failed, &long, 10)];
        let (_, _, details) = attribute_failures(&entries);
        let message = details["latest"]["message"].as_str().unwrap();
        assert!(message.chars().count() <= FAILURE_MESSAGE_MAX_CHARS + 1);
        assert!(message.ends_with('…'));
    }

    // ============ 脱敏断言 ============

    #[test]
    fn 门户地址脱敏剥除凭据查询串与fragment() {
        assert_eq!(
            sanitize_portal_url(LEAKY_PORTAL_URL).as_deref(),
            Some("http://10.0.0.55:8080/login")
        );
        // 裸 host:port 与裸 IPv6 兼容（默认端口 80 在 Url 规范化时被省略）
        assert_eq!(
            sanitize_portal_url("10.0.0.1:80").as_deref(),
            Some("http://10.0.0.1/")
        );
        assert_eq!(sanitize_portal_url("::1").as_deref(), Some("http://[::1]/"));
        assert_eq!(
            sanitize_portal_url("https://portal.edu/login#main").as_deref(),
            Some("https://portal.edu/login")
        );
        assert_eq!(sanitize_portal_url("not a url @@@"), None);
    }

    #[tokio::test]
    async fn 报告全文不含凭据与查询串() {
        let backend = StubBackend {
            // 失败路径的错误文本也要走脱敏地址，覆盖 fail 分支
            portal: Some(Err(PortalProbeError::Connect("connect error".into()))),
            ..StubBackend::default()
        };
        let report = report_of(backend, leaky_inputs()).await;
        let text = serde_json::to_string(&report).unwrap();
        for forbidden in ["s3cret", "admin:", "token=xyz", "next=/pay"] {
            assert!(
                !text.contains(forbidden),
                "报告泄漏敏感片段 {forbidden}: {text}"
            );
        }
        // 脱敏后的门户地址正常出现（可排障）
        assert!(text.contains("http://10.0.0.55:8080/login"));
    }

    #[tokio::test]
    async fn 报告不含用户名密码原始形态() {
        // DNS host 只取 host 部分；门户 URL 走脱敏——两端都不回传 userinfo
        let report = report_of(StubBackend::default(), leaky_inputs()).await;
        let text = serde_json::to_string(&report).unwrap();
        assert!(
            !text.contains("admin"),
            "host 提取不得夹带 userinfo: {text}"
        );
        assert_eq!(check_of(&report, "dns").details["host"], "10.0.0.55");
    }

    // ============ 总结论汇总 ============

    #[test]
    fn 总结论_fail优先于warn_skip不降级() {
        let check = |status| DiagnosticCheck {
            key: "k",
            name: "项",
            status,
            elapsed_ms: 1,
            summary: String::new(),
            details: serde_json::Value::Null,
        };
        let (verdict, conclusion) = summarize_checks(&[
            check(DiagnosticStatus::Ok),
            check(DiagnosticStatus::Warn),
            check(DiagnosticStatus::Skip),
        ]);
        assert_eq!(verdict, DiagnosticVerdict::Degraded);
        assert!(conclusion.contains("1 项警告"));
        assert!(conclusion.contains("1 项跳过"));

        let (verdict, conclusion) =
            summarize_checks(&[check(DiagnosticStatus::Warn), check(DiagnosticStatus::Fail)]);
        assert_eq!(verdict, DiagnosticVerdict::Faulty);
        assert!(conclusion.contains("1 项异常"));
    }
}
