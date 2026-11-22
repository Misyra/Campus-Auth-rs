//! 一键网络诊断：并发逐项探测 + 结构化报告（Web 层按需构造，不入容器拓扑）
//!
//! 与 [`super`]（MonitorService 周期监测）的分工：周期监测面向自动恢复决策，
//! 证据口径服务于 Engine；本模块面向用户手动触发的单次诊断，逐项给出
//! ok/warn/fail/skip 结论与总结论。探测实现一律复用 monitor/network 既有基建
//! （TcpProbe 竞速、PreparedEgress 出口租约、网卡枚举），不重写探测器。
//!
//! 可测试性：探测动作收敛在私有 `ProbeBackend` trait 之后，生产实现走系统
//! 调用与真实网络，单元测试注入桩（见模块测试）；五项检查并发执行，整体受
//! [`TOTAL_TIMEOUT`] 硬约束——任一项超限都以 fail 收敛返回，调用绝不悬挂。
//!
//! 脱敏红线：报告中不出现凭据、token 或带查询串的完整 URL——门户地址统一经
//! [`sanitize_portal_url`] 剥除查询串 / fragment / 内嵌凭据后再进入结果。
//!
//! 本模块同时承载「一键导出诊断包」（[`build_diagnostics_bundle`]，Web 层经
//! `GET /api/diagnostics/bundle` 下载）：manifest + 深度脱敏配置快照 +
//! 事件流 JSONL + 运行日志 + 环境观测文本，白名单式收集、总量 20MB 软上限，
//! 敏感字段清单见 [`BUNDLE_SANITIZED_FIELD_PATHS`]。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDate, Utc};
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

// ============ 诊断包导出（zip） ============
//
// 与上方「一键网络诊断」（单次探测）的分工：诊断包面向用户随 bug 上传的
// 离线证据包，收录 manifest / 脱敏配置快照 / 事件流 JSONL / 运行日志 /
// 环境观测文本五类内容。收集全程白名单式（只按谓词收 events/*.jsonl 与
// logs/app.log*），`.auth_token` / `.auth_bootstrap` / `update/` 安装包 /
// `python_worker` venv / 其他二进制大文件一概不会进入包内。

/// 诊断包总软上限（20MB）：events 与 logs 按「新→旧」优先共享剩余额度，
/// 超限截断并在包内 manifest 注明；manifest / config / environment 文本
/// 天然有界（KB 级）优先保障
pub const BUNDLE_MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;

/// events 收集窗口（天）：与事件流落盘保留期 `EVENT_RETENTION_DAYS` 同口径
pub const BUNDLE_EVENT_DAYS: i64 = 30;

/// logs 收集窗口（天）：日志默认保留 7 天（LoggingSettings::default 口径）
pub const BUNDLE_LOG_DAYS: i64 = 7;

/// 脱敏占位值：字段级置空后写入的统一占位（键与结构保留）
pub const BUNDLE_REDACTED: &str = "[已脱敏]";

/// config.sanitized.json 的敏感字段清单（manifest 原样注明；与
/// [`sanitize_config_snapshot`] 的实现和模块测试逐字段一一钉住）。
///
/// 枚举即事实源——新增敏感字段时先在此登记，再补实现与测试：
/// - settings.global.app.management_password（管理密码明文）
/// - settings.global.notification.channels[].webhook.url（可携带 token 的自建
///   地址；剥 query/fragment/userinfo 而非整体置空，保留 host/路径排障价值）
/// - settings.global.notification.channels[].webhook.headers[].value（可含密钥头）
/// - settings.global.notification.channels[].bark.device_key
/// - settings.global.notification.channels[].bark.server（自建服务器地址，同 URL 口径）
/// - settings.global.notification.channels[].serverchan.send_key
/// - settings.global.notification.channels[].telegram.bot_token
/// - settings.global.browser.bind_proxy（旧代理字段，URL 口径）
/// - settings.global.browser.extra_headers_json（浏览器附加请求头，可含 Cookie/Authorization）
/// - settings.global.updater.proxy_url（更新代理，URL 口径）
/// - active_profile.password（方案密码：内存明文或磁盘 ENC: 密文，一律置空）
/// - active_profile.network.proxy_url（HTTP/SOCKS5 代理可含 Basic 认证，URL 口径）
/// - active_profile.recorded_actions[].value（持久会话录制动作的值：fill/change
///   是录制时的真实输入——含录制器启发式漏判时落入的明文密码，一律置空；
///   navigate 的落地页 URL 剥 query/fragment/userinfo、host/path 保留排障，
///   click 无值键不动）
/// - llm.api_key_enc / llm.api_keys_enc.*（AI 任务生成的 API key 密文，按服务商槽位）
///
/// 有意保留：profile.username（诊断必需的账号线索，非密钥）、telegram.chat_id
/// （推送目标非凭据）、webhook.body_template（占位符模板无凭据）、auth_url
/// （门户地址，与既有 feedback-bundle / logs-export meta 同口径）。
pub const BUNDLE_SANITIZED_FIELD_PATHS: &[&str] = &[
    "settings.global.app.management_password",
    "settings.global.notification.channels[].webhook.url",
    "settings.global.notification.channels[].webhook.headers[].value",
    "settings.global.notification.channels[].bark.device_key",
    "settings.global.notification.channels[].bark.server",
    "settings.global.notification.channels[].serverchan.send_key",
    "settings.global.notification.channels[].telegram.bot_token",
    "settings.global.browser.bind_proxy",
    "settings.global.browser.extra_headers_json",
    "settings.global.updater.proxy_url",
    "active_profile.password",
    "active_profile.network.proxy_url",
    "active_profile.recorded_actions[].value",
    "llm.api_key_enc",
    "llm.api_keys_enc.*",
];

/// manifest 注明的排除项：白名单收集语义之外的一律不进包
pub const BUNDLE_EXCLUDED_PATHS: &[&str] = &[
    ".auth_token / .auth_bootstrap（鉴权凭据）",
    "update/ 下的安装包与 pending 状态",
    "python_worker/.venv（虚拟环境，大小也不统计）",
    "python_worker venv 之外的二进制大文件",
];

/// 诊断包分项统计（manifest 各节与截断标记的数据源）
#[derive(Debug, Default)]
pub struct BundleSection {
    /// 收入包内的文件数
    pub files: usize,
    /// 收入包内的字节数
    pub bytes: u64,
    /// 是否有文件因总量超限被截断跳过
    pub truncated: bool,
}

/// events/logs 白名单收集产物
#[derive(Debug)]
pub struct BundleDataFiles {
    /// 待入包文件（包内相对路径 + 内容），顺序：events（新→旧）→ logs（新→旧）
    pub files: Vec<(String, Vec<u8>)>,
    /// events 分项统计
    pub events: BundleSection,
    /// logs 分项统计
    pub logs: BundleSection,
    /// 跳过/读取失败说明（manifest skipped_files，逐条注明原因）
    pub skipped: Vec<String>,
}

/// 在总量预算内读取单个文件；超限截断 / 失败写说明，绝不中断收集
fn read_file_within_budget(
    path: &std::path::Path,
    rel: &str,
    remaining: &mut usize,
    section: &mut BundleSection,
    skipped: &mut Vec<String>,
) -> Option<(String, Vec<u8>)> {
    let size = match std::fs::metadata(path) {
        // 先读 metadata 防止把超限大文件整块读入内存
        Ok(m) => m.len() as usize,
        Err(e) => {
            skipped.push(format!("{rel}（读取元数据失败: {e}）"));
            return None;
        }
    };
    if size > *remaining {
        section.truncated = true;
        skipped.push(format!("{rel}（超总量上限截断）"));
        return None;
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            // 以实际读到的字节数扣预算（metadata 与读取间文件可能增长，竞态下宁可少收）
            *remaining = remaining.saturating_sub(bytes.len());
            section.bytes += bytes.len() as u64;
            section.files += 1;
            Some((rel.to_string(), bytes))
        }
        Err(e) => {
            skipped.push(format!("{rel}（读取失败: {e}）"));
            None
        }
    }
}

/// 收集诊断包的 events 与 logs 文件（纯同步，供 spawn_blocking 与单测直调）
///
/// - `events/YYYY-MM-DD.jsonl`：文件名日期在 [`BUNDLE_EVENT_DAYS`] 窗口内；
///   命名不规范或超窗的文件不收（与 `events::sink` 轮转清理同一谓词口径）
/// - `logs/app.log`（当前）+ `logs/app.log.YYYY-MM-DD`：日期在 [`BUNDLE_LOG_DAYS`]
///   窗口内，当前文件无日期后缀、始终收录
/// - 目录缺失返回空集（manifest 记 files=0、truncated=false，不视为错误）
pub fn collect_bundle_data_files(
    base: &std::path::Path,
    max_total_bytes: usize,
) -> BundleDataFiles {
    let mut out = BundleDataFiles {
        files: Vec::new(),
        events: BundleSection::default(),
        logs: BundleSection::default(),
        skipped: Vec::new(),
    };
    let mut remaining = max_total_bytes;

    // ---- events：文件名即 UTC 日期（与落盘切分同口径），按日期降序（新→旧） ----
    let event_cutoff = Utc::now().date_naive() - chrono::Duration::days(BUNDLE_EVENT_DAYS);
    let mut event_files: Vec<(NaiveDate, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(crate::utils::paths::events_dir(base))
        .into_iter()
        .flatten()
    {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(date_text) = name.strip_suffix(".jsonl") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if date >= event_cutoff {
            event_files.push((date, entry.path()));
        }
    }
    event_files.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, path) in &event_files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(item) = read_file_within_budget(
            path,
            &format!("events/{name}"),
            &mut remaining,
            &mut out.events,
            &mut out.skipped,
        ) {
            out.files.push(item);
        }
    }

    // ---- logs：当前 app.log 优先，其余按日期降序（本地日期，与 tracing-appender 同口径） ----
    let logs_dir = crate::utils::paths::logs_dir(base);
    let log_cutoff = Local::now().date_naive() - chrono::Duration::days(BUNDLE_LOG_DAYS);
    let mut log_files: Vec<(NaiveDate, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&logs_dir).into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(date_text) = name.strip_prefix("app.log.") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if date >= log_cutoff {
            log_files.push((date, entry.path()));
        }
    }
    log_files.sort_by_key(|a| std::cmp::Reverse(a.0));
    // 当前 app.log（正在写的今天日志）无日期后缀，排在最前；存在才收
    let current_log = logs_dir.join("app.log");
    let ordered = std::iter::once(current_log)
        .filter(|p| p.is_file())
        .chain(log_files.into_iter().map(|(_, p)| p));
    for path in ordered {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(item) = read_file_within_budget(
            &path,
            &format!("logs/{name}"),
            &mut remaining,
            &mut out.logs,
            &mut out.skipped,
        ) {
            out.files.push(item);
        }
    }
    out
}

/// 字符串字段级置空：值替换为 [`BUNDLE_REDACTED`]，键与结构保留（缺字段不动）
fn redact_string_field(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    if obj.contains_key(key) {
        obj.insert(
            key.to_string(),
            serde_json::Value::String(BUNDLE_REDACTED.into()),
        );
    }
}

/// URL 字段脱敏：剥 query / fragment / userinfo（密钥常放这三处），保留
/// scheme://host:port/path 的排障价值。复用推送通知 [`crate::notification::sender::sanitize_url`]
/// 范式；空值原样保留（该函数对解析失败才产出占位串，空串不生产占位）。
fn sanitize_url_field(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    if let Some(serde_json::Value::String(url)) = obj.get(key)
        && !url.is_empty()
    {
        let sanitized = crate::notification::sender::sanitize_url(url);
        obj.insert(key.to_string(), serde_json::Value::String(sanitized));
    }
}

/// 录制动作数组的值脱敏：`fill`/`change` 的值是录制时的真实输入（含录制器
/// 启发式漏判时落入的明文密码，见 P1-1 组合链），一律替换 [`BUNDLE_REDACTED`]；
/// `navigate` 落地页 URL 走 [`sanitize_url_field`] 口径剥 query/fragment/userinfo
/// （录制器抓的是整条 `location.href`，门户重定向常把一次性令牌放进 query 或
/// fragment——诊断包是外发排障件，host/path 的排障价值保留、令牌段不进包）；
/// `click` 无值键不动（结构保留口径同上）
fn redact_recorded_action_values(actions: &mut [serde_json::Value]) {
    for action in actions {
        let Some(obj) = action.as_object_mut() else {
            continue;
        };
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("fill") | Some("change") => {
                redact_string_field(obj, "value");
            }
            Some("navigate") => {
                sanitize_url_field(obj, "value");
            }
            _ => {}
        }
    }
}

/// 构造 `config.sanitized.json`：当前配置快照深度脱敏（纯同步，供单测直调）
///
/// 结构 = `{ settings, active_profile, llm }` 三节（settings.json 全局配置 +
/// 活跃 Profile 文件 + `config/llm.json` AI 配置，后两者独立落盘故平铺展示）。
/// 脱敏字段清单见 [`BUNDLE_SANITIZED_FIELD_PATHS`]：字段级置空、值替换为
/// [`BUNDLE_REDACTED`]、键与结构保留；URL 类字段剥 query/fragment/userinfo。
pub fn sanitize_config_snapshot(
    settings: &crate::config::SettingsData,
    profile: &crate::config::ProfileData,
    llm: &crate::ai::LlmSettings,
) -> serde_json::Value {
    let mut settings_value = serde_json::to_value(settings).unwrap_or(serde_json::Value::Null);
    if let Some(global) = settings_value
        .get_mut("global")
        .and_then(|v| v.as_object_mut())
    {
        if let Some(app) = global.get_mut("app").and_then(|v| v.as_object_mut()) {
            redact_string_field(app, "management_password");
        }
        if let Some(browser) = global.get_mut("browser").and_then(|v| v.as_object_mut()) {
            sanitize_url_field(browser, "bind_proxy");
            // 附加请求头整串置空：串内可能内嵌 Cookie/Authorization 等凭据
            redact_string_field(browser, "extra_headers_json");
        }
        if let Some(updater) = global.get_mut("updater").and_then(|v| v.as_object_mut()) {
            sanitize_url_field(updater, "proxy_url");
        }
        if let Some(channels) = global
            .get_mut("notification")
            .and_then(|v| v.as_object_mut())
            .and_then(|v| v.get_mut("channels"))
            .and_then(|v| v.as_array_mut())
        {
            // 通道是 serde tagged 枚举：序列化后按 type 判别字段分派脱敏
            for channel in channels {
                let Some(obj) = channel.as_object_mut() else {
                    continue;
                };
                match obj.get("type").and_then(|v| v.as_str()) {
                    Some("webhook") => {
                        sanitize_url_field(obj, "url");
                        if let Some(headers) = obj.get_mut("headers").and_then(|v| v.as_array_mut())
                        {
                            for header in headers.iter_mut() {
                                if let Some(header_obj) = header.as_object_mut() {
                                    redact_string_field(header_obj, "value");
                                }
                            }
                        }
                    }
                    Some("bark") => {
                        redact_string_field(obj, "device_key");
                        sanitize_url_field(obj, "server");
                    }
                    Some("serverchan") => redact_string_field(obj, "send_key"),
                    Some("telegram") => redact_string_field(obj, "bot_token"),
                    _ => {}
                }
            }
        }
    }

    let mut profile_value = serde_json::to_value(profile).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = profile_value.as_object_mut() {
        redact_string_field(obj, "password");
        if let Some(network) = obj.get_mut("network").and_then(|v| v.as_object_mut()) {
            sanitize_url_field(network, "proxy_url");
        }
        if let Some(actions) = obj
            .get_mut("recorded_actions")
            .and_then(|v| v.as_array_mut())
        {
            redact_recorded_action_values(actions);
        }
    }

    let mut llm_value = serde_json::to_value(llm).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = llm_value.as_object_mut() {
        redact_string_field(obj, "api_key_enc");
        if let Some(keys) = obj.get_mut("api_keys_enc").and_then(|v| v.as_object_mut()) {
            // 按服务商槽位逐一置空：键（服务商标识）保留、值统一占位
            for value in keys.values_mut() {
                *value = serde_json::Value::String(BUNDLE_REDACTED.into());
            }
        }
    }

    let mut root = serde_json::Map::new();
    root.insert("settings".into(), settings_value);
    root.insert("active_profile".into(), profile_value);
    root.insert("llm".into(), llm_value);
    serde_json::Value::Object(root)
}

/// 递归统计目录占用（字节、文件数）；`skip` 命名的子目录整体跳过（venv），
/// 条目数超 `max_entries` 即停并返回 capped 标记（防异常深树拖死导出）
fn measure_dir(
    dir: &std::path::Path,
    skip: Option<&str>,
    max_entries: usize,
) -> (u64, usize, bool) {
    let mut total = 0u64;
    let mut count = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            count += 1;
            if count > max_entries {
                return (total, count, true);
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if skip.is_some_and(|name| entry.file_name().to_str() == Some(name)) {
                    continue;
                }
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    (total, count, false)
}

/// 字节数的人类可读展示（environment.txt 用）
fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// 渲染诊断包 `environment.txt`：环境就绪状态 + 数据目录清单与大小
///
/// 全部取自现有观测面（[`crate::environment::EnvironmentApi::status`] 与盘面
/// metadata），不新造探测；目录仅列清单与占用，文件内容不进诊断包。
pub fn render_environment_text(
    status: &crate::environment::EnvironmentStatus,
    python_path: &std::path::Path,
    base: &std::path::Path,
) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    let _ = writeln!(text, "== 运行环境状态（EnvironmentApi 观测面）==");
    let _ = writeln!(text, "uv_ready: {}", status.uv_ready);
    let _ = writeln!(text, "python_ready: {}", status.python_ready);
    let _ = writeln!(text, "worker_ready: {}", status.worker_ready);
    let _ = writeln!(text, "manifest_current: {}", status.manifest_current);
    let _ = writeln!(text, "playwright_ready: {}", status.playwright_ready);
    let _ = writeln!(
        text,
        "system_browser_ready: {}",
        status.system_browser_ready
    );
    let _ = writeln!(text, "ocr_enabled: {}", status.ocr_enabled);
    let _ = writeln!(text, "ocr_ready: {}", status.ocr_ready);
    let _ = writeln!(text, "capability_ready: {}", status.capability_ready);
    let _ = writeln!(text, "stage: {:?}", status.stage);
    let _ = writeln!(text, "progress: {:?}", status.progress);
    let _ = writeln!(
        text,
        "last_error: {}",
        status.last_error.as_deref().unwrap_or("(无)")
    );
    let _ = writeln!(text, "python_path: {}", python_path.display());

    let _ = writeln!(text);
    let _ = writeln!(text, "== 数据目录清单与大小（base={}）==", base.display());
    let _ = writeln!(text, "仅列清单与占用大小，文件内容不进诊断包：");
    // 与 utils::paths 的目录常量同源：新增用户数据目录时此处跟着同步
    let dirs: &[(&str, Option<&str>)] = &[
        (crate::utils::paths::CONFIG_DIR, None),
        (crate::utils::paths::TASKS_DIR, None),
        (crate::utils::paths::LOGS_DIR, None),
        (crate::utils::paths::EVENTS_DIR, None),
        (crate::utils::paths::LOGIN_HISTORY_DIR, None),
        (crate::utils::paths::ENV_DIR, None),
        (crate::utils::paths::UPDATE_DIR, None),
        // Worker 工程目录跳过 .venv：几万个小文件的遍历代价高且无诊断价值
        (
            crate::utils::paths::WORKER_PROJECT_DIR,
            Some(crate::utils::paths::VENV_DIR),
        ),
    ];
    for (name, skip) in dirs {
        let (bytes, count, capped) = measure_dir(&base.join(name), *skip, 50_000);
        let cap_note = if capped {
            "，条目数超限仅部分统计"
        } else {
            ""
        };
        let _ = writeln!(
            text,
            "{name}/  {}  ({count} 个文件{cap_note})",
            format_size(bytes)
        );
    }
    let venv = base
        .join(crate::utils::paths::WORKER_PROJECT_DIR)
        .join(crate::utils::paths::VENV_DIR);
    let _ = writeln!(
        text,
        "python_worker/.venv  {}（虚拟环境体积不统计、不收录）",
        if venv.is_dir() { "存在" } else { "不存在" }
    );
    text
}

/// 组装完成的诊断包（zip 字节 + 建议下载文件名）
#[derive(Debug)]
pub struct DiagnosticsBundle {
    /// zip 字节流（HTTP 响应体）
    pub bytes: Vec<u8>,
    /// 下载文件名 `campus-auth-diagnostics-<时间戳>.zip`
    pub filename: String,
}

/// 构建诊断包（纯同步阻塞：文件 I/O + zip 打包，handler 须放 spawn_blocking）
///
/// 包内布局：`manifest.json` + `config.sanitized.json` + `environment.txt` +
/// `events/*.jsonl` + `logs/app.log*`。收集失败的分项写 manifest 说明而非
/// 整包报错；任何步骤都不触碰 [`BUNDLE_EXCLUDED_PATHS`] 所列内容。
pub fn build_diagnostics_bundle(
    base: &std::path::Path,
    settings: &crate::config::SettingsData,
    profile: &crate::config::ProfileData,
    llm: &crate::ai::LlmSettings,
    environment_txt: String,
    uptime_seconds: u64,
) -> Result<DiagnosticsBundle, String> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let config_value = sanitize_config_snapshot(settings, profile, llm);
    let config_bytes =
        serde_json::to_vec_pretty(&config_value).map_err(|e| format!("配置快照序列化失败: {e}"))?;
    // events/logs 分享剩余额度：config 与 environment 文本天然有界（KB 级）优先保障
    let data_budget = BUNDLE_MAX_TOTAL_BYTES
        .saturating_sub(config_bytes.len())
        .saturating_sub(environment_txt.len());
    let data = collect_bundle_data_files(base, data_budget);

    let manifest = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "uptime_seconds": uptime_seconds,
        "exported_at": Local::now().to_rfc3339(),
        "total_limit_bytes": BUNDLE_MAX_TOTAL_BYTES,
        "sections": {
            "config": {
                "file": "config.sanitized.json",
                "sanitized_fields": BUNDLE_SANITIZED_FIELD_PATHS,
                "note": "凭据/密钥/token 字段级置空（值替换为占位串），结构与键保留；URL 类字段剥 query/fragment/userinfo",
            },
            "events": {
                "files": data.events.files,
                "bytes": data.events.bytes,
                "truncated": data.events.truncated,
                "window_days": BUNDLE_EVENT_DAYS,
                "note": "事件流 JSONL 原样收录；事件类型为白名单式载荷，设计上无敏感字段（信任链见 events 模块 doc）",
            },
            "logs": {
                "files": data.logs.files,
                "bytes": data.logs.bytes,
                "truncated": data.logs.truncated,
                "window_days": BUNDLE_LOG_DAYS,
                "note": "运行日志按设计不含凭据；仅收录最近窗口内的 app.log* 文件",
            },
            "environment": {
                "file": "environment.txt",
                "note": "环境就绪状态与数据目录清单/大小，取自 EnvironmentApi 观测面与盘面 metadata",
            },
        },
        "skipped_files": data.skipped,
        "excluded": BUNDLE_EXCLUDED_PATHS,
    });
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("manifest 序列化失败: {e}"))?;

    let mut buf = Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let mut add = |name: &str, bytes: &[u8]| -> Result<(), String> {
            zw.start_file(name, opts).map_err(|e| e.to_string())?;
            zw.write_all(bytes).map_err(|e| e.to_string())
        };
        add("manifest.json", &manifest_bytes)?;
        add("config.sanitized.json", &config_bytes)?;
        add("environment.txt", environment_txt.as_bytes())?;
        for (rel, bytes) in &data.files {
            add(rel, bytes)?;
        }
        zw.finish().map_err(|e| format!("zip 收尾失败: {e}"))?;
    }
    let filename = format!(
        "campus-auth-diagnostics-{}.zip",
        Local::now().format("%Y%m%d-%H%M%S")
    );
    Ok(DiagnosticsBundle {
        bytes: buf.into_inner(),
        filename,
    })
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

    // ============ 诊断包：脱敏 ============

    use crate::ai::LlmSettings;
    use crate::config::schema::RecordedAction;
    use crate::config::{NotificationChannel, NotificationHeader, ProfileData, SettingsData};
    use crate::network::settings::{NetworkMode, NetworkSettings};

    /// 含全部敏感字段的配置夹具（与 BUNDLE_SANITIZED_FIELD_PATHS 一一对应），
    /// 敏感原值常量供逐字段断言复用
    const PW_MGMT: &str = "mgmt-pw-123";
    const PW_PROFILE_ENC: &str = "ENC:profile-cipher";
    const WH_URL: &str = "https://botuser:botpass@hooks.example.com/chAT?token=tok123";
    const WH_HEADER_VALUE: &str = "Bearer sk-live-99";
    const BARK_KEY: &str = "bark-device-key-1";
    const SEND_KEY: &str = "SCT123456";
    const TG_TOKEN: &str = "123456:ABC-DEF";
    const PROXY_UPDATER: &str = "http://upduser:updpass@127.0.0.1:7890";
    const PROXY_BIND: &str = "socks5://binduser:bindpass@10.0.0.9:1080";
    const EXTRA_HEADERS: &str = r#"{"Cookie":"session=abc123","Authorization":"Bearer x"}"#;
    const PROXY_NETWORK: &str = "http://netuser:netpass@10.0.0.8:3128";
    const AI_KEY_GLM: &str = "ENC:glm-key-cipher";
    const AI_KEY_DSK: &str = "ENC:dsk-key-cipher";
    /// 录制 fill 值中的明文敏感输入（模拟录制器启发式漏判形态，P1-1 组合链）
    const RECORDED_FILL_SECRET: &str = "rec-plain-pw-777";
    /// 录制 change 值（选择项同样可能内嵌真实输入）
    const RECORDED_CHANGE_SECRET: &str = "rec-isp-choice-888";
    /// 录制 navigate 落地页 URL（门户重定向常把一次性令牌放进 query/fragment，
    /// 脱敏应剥掉这两段、host/path 保留排障）
    const RECORDED_NAVIGATE_URL: &str =
        "http://portal.example.edu/login?token=one-time-999&next=%2Fhome#sso";

    fn full_sensitive_settings() -> SettingsData {
        let mut settings = SettingsData::default();
        settings.global.app.management_password = PW_MGMT.into();
        settings.global.updater.proxy_url = PROXY_UPDATER.into();
        settings.global.browser.bind_proxy = PROXY_BIND.into();
        settings.global.browser.extra_headers_json = EXTRA_HEADERS.into();
        settings.global.notification.channels = vec![
            NotificationChannel::Webhook {
                id: "ch-wh".into(),
                name: "自建".into(),
                enabled: true,
                url: WH_URL.into(),
                headers: vec![NotificationHeader {
                    name: "Authorization".into(),
                    value: WH_HEADER_VALUE.into(),
                }],
                body_template: String::new(),
                timeout_seconds: 10,
            },
            NotificationChannel::Bark {
                id: "ch-bark".into(),
                name: String::new(),
                enabled: true,
                server: "https://api.day.app".into(),
                device_key: BARK_KEY.into(),
                timeout_seconds: 10,
            },
            NotificationChannel::Serverchan {
                id: "ch-sct".into(),
                name: String::new(),
                enabled: true,
                send_key: SEND_KEY.into(),
                timeout_seconds: 10,
            },
            NotificationChannel::Telegram {
                id: "ch-tg".into(),
                name: String::new(),
                enabled: true,
                bot_token: TG_TOKEN.into(),
                chat_id: "@campus".into(),
                timeout_seconds: 10,
            },
        ];
        settings
    }

    fn full_sensitive_profile() -> ProfileData {
        ProfileData {
            username: "stu2024001".into(),
            password: PW_PROFILE_ENC.into(),
            network: Some(NetworkSettings {
                mode: NetworkMode::Proxy,
                interface_id: String::new(),
                proxy_url: PROXY_NETWORK.into(),
                dns_servers: Vec::new(),
            }),
            recorded_actions: vec![
                RecordedAction {
                    action_type: "fill".into(),
                    selector: "#password".into(),
                    strategy: "id".into(),
                    ts: 1,
                    value: Some(RECORDED_FILL_SECRET.into()),
                    frame: None,
                },
                RecordedAction {
                    action_type: "change".into(),
                    selector: "#isp".into(),
                    strategy: "id".into(),
                    ts: 2,
                    value: Some(RECORDED_CHANGE_SECRET.into()),
                    frame: None,
                },
                RecordedAction {
                    action_type: "navigate".into(),
                    selector: String::new(),
                    strategy: "structure".into(),
                    ts: 3,
                    value: Some(RECORDED_NAVIGATE_URL.into()),
                    frame: None,
                },
                RecordedAction {
                    action_type: "click".into(),
                    selector: "#login-btn".into(),
                    strategy: "id".into(),
                    ts: 4,
                    value: None,
                    frame: None,
                },
            ],
            ..ProfileData::default()
        }
    }

    fn full_sensitive_llm() -> LlmSettings {
        let mut llm = LlmSettings::default();
        llm.api_keys_enc.insert("glm".into(), AI_KEY_GLM.into());
        llm.api_keys_enc
            .insert("deepseek".into(), AI_KEY_DSK.into());
        llm
    }

    /// 敏感字段逐字段剥除：所有原值不出现在快照文本中，占位与结构保留
    #[test]
    fn 脱敏_全敏感字段逐字段剥除且结构保留() {
        let value = sanitize_config_snapshot(
            &full_sensitive_settings(),
            &full_sensitive_profile(),
            &full_sensitive_llm(),
        );
        let text = serde_json::to_string(&value).unwrap();
        for forbidden in [
            PW_MGMT,
            PW_PROFILE_ENC,
            "botuser",
            "botpass",
            "token=tok123",
            WH_HEADER_VALUE,
            BARK_KEY,
            SEND_KEY,
            TG_TOKEN,
            "upduser",
            "updpass",
            "binduser",
            "bindpass",
            "session=abc123",
            "netuser",
            "netpass",
            AI_KEY_GLM,
            AI_KEY_DSK,
            RECORDED_FILL_SECRET,
            RECORDED_CHANGE_SECRET,
        ] {
            assert!(
                !text.contains(forbidden),
                "快照泄漏敏感片段 {forbidden}: {text}"
            );
        }
        // 结构保留：三节齐全，通道 4 个且 type 判别字段原样
        assert_eq!(
            value["settings"]["global"]["app"]["management_password"],
            BUNDLE_REDACTED
        );
        assert_eq!(value["active_profile"]["password"], BUNDLE_REDACTED);
        // 录制动作结构保留：4 条动作齐全且 type 原样（值脱敏见专项测试）
        let actions = value["active_profile"]["recorded_actions"]
            .as_array()
            .expect("录制动作数组结构保留");
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0]["type"], "fill");
        assert_eq!(actions[1]["type"], "change");
        assert_eq!(actions[2]["type"], "navigate");
        assert_eq!(actions[3]["type"], "click");
        // 网络代理走 URL 口径：userinfo 剥除、host:port 保留
        assert_eq!(
            value["active_profile"]["network"]["proxy_url"],
            "http://10.0.0.8:3128/"
        );
        let channels = value["settings"]["global"]["notification"]["channels"]
            .as_array()
            .expect("通道数组结构保留");
        assert_eq!(channels.len(), 4);
        assert_eq!(channels[0]["type"], "webhook");
        assert_eq!(channels[1]["type"], "bark");
        assert_eq!(channels[2]["type"], "serverchan");
        assert_eq!(channels[3]["type"], "telegram");
    }

    /// URL 类字段剥 query/fragment/userinfo 但保留 host 路径；非 URL 凭据字段值替换占位
    #[test]
    fn 脱敏_url字段剥凭据保留host_密钥字段值置占位() {
        let value = sanitize_config_snapshot(
            &full_sensitive_settings(),
            &full_sensitive_profile(),
            &full_sensitive_llm(),
        );
        let channels = value["settings"]["global"]["notification"]["channels"]
            .as_array()
            .unwrap();
        // webhook url：剥 userinfo 与 query，保留 host + 路径
        assert_eq!(channels[0]["url"], "https://hooks.example.com/chAT");
        // 自定义头：name 保留、value 占位
        assert_eq!(channels[0]["headers"][0]["name"], "Authorization");
        assert_eq!(channels[0]["headers"][0]["value"], BUNDLE_REDACTED);
        // bark：device_key 占位、官方服务器地址（无凭据）原样
        assert_eq!(channels[1]["device_key"], BUNDLE_REDACTED);
        assert_eq!(channels[1]["server"], "https://api.day.app/");
        assert_eq!(channels[2]["send_key"], BUNDLE_REDACTED);
        // telegram：bot_token 占位、chat_id 保留（非凭据）
        assert_eq!(channels[3]["bot_token"], BUNDLE_REDACTED);
        assert_eq!(channels[3]["chat_id"], "@campus");
        // 全局与网络代理：host:port 保留、userinfo 剥除（special scheme 的 Url 规范化补路径斜杠，
        // socks5 等 non-special scheme 不补）
        assert_eq!(
            value["settings"]["global"]["updater"]["proxy_url"],
            "http://127.0.0.1:7890/"
        );
        assert_eq!(
            value["settings"]["global"]["browser"]["bind_proxy"],
            "socks5://10.0.0.9:1080"
        );
        assert_eq!(
            value["settings"]["global"]["browser"]["extra_headers_json"],
            BUNDLE_REDACTED
        );
        // llm：槽位键保留、密文值占位
        assert_eq!(value["llm"]["api_keys_enc"]["glm"], BUNDLE_REDACTED);
        assert_eq!(value["llm"]["api_keys_enc"]["deepseek"], BUNDLE_REDACTED);
        // 有意保留的账号线索
        assert_eq!(value["active_profile"]["username"], "stu2024001");
    }

    /// URL 字段为空/无凭据时的边界：空串保持空串，干净 URL 原样保留
    #[test]
    fn 脱敏_空url与干净url保持原样() {
        let mut settings = SettingsData::default();
        settings.global.updater.proxy_url = String::new();
        settings.global.browser.bind_proxy = "http://127.0.0.1:7890".into();
        let profile = ProfileData::default();
        let value = sanitize_config_snapshot(&settings, &profile, &LlmSettings::default());
        assert_eq!(value["settings"]["global"]["updater"]["proxy_url"], "");
        assert_eq!(
            value["settings"]["global"]["browser"]["bind_proxy"],
            "http://127.0.0.1:7890/"
        );
    }

    /// P3-18：录制动作值脱敏——fill/change 值（录制时的真实输入，含启发式
    /// 漏判落入的明文密码）一律占位；navigate 落地页 URL 剥 query/fragment
    /// （录制器抓整条 location.href，门户重定向令牌不进外发排障件，host/path
    /// 保留）；click 无值键不动；selector/strategy 等结构字段原样保留排障
    #[test]
    fn 脱敏_录制动作fill_change置占位navigate剥参() {
        let value = sanitize_config_snapshot(
            &SettingsData::default(),
            &full_sensitive_profile(),
            &LlmSettings::default(),
        );
        let actions = value["active_profile"]["recorded_actions"]
            .as_array()
            .expect("录制动作数组结构保留");
        // fill：值占位、其余字段原样
        assert_eq!(actions[0]["value"], BUNDLE_REDACTED);
        assert_eq!(actions[0]["selector"], "#password");
        assert_eq!(actions[0]["strategy"], "id");
        // change：值占位
        assert_eq!(actions[1]["value"], BUNDLE_REDACTED);
        // navigate：一次性令牌 query/fragment 剥除，host/path 保留排障
        assert_eq!(actions[2]["value"], "http://portal.example.edu/login");
        // click：无值键保持缺失（结构口径不变）
        assert_eq!(actions[3].get("value"), None);
    }

    /// P3-18 边界：recorded_actions 为空数组或缺失时脱敏为 no-op
    #[test]
    fn 脱敏_录制动作空数组与缺省不受影响() {
        let empty = sanitize_config_snapshot(
            &SettingsData::default(),
            &ProfileData::default(),
            &LlmSettings::default(),
        );
        // ProfileData::default 的 recorded_actions 为空数组：结构原样、无值可脱
        assert_eq!(
            empty["active_profile"]["recorded_actions"],
            serde_json::json!([])
        );
    }

    // ============ 诊断包：收集与打包 ============

    fn write_file(path: &std::path::Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// 事件流文件名（相对今天偏移 day_offset 天）
    fn events_file_name(day_offset: i64) -> String {
        format!(
            "{}.jsonl",
            (Utc::now().date_naive() - chrono::Duration::days(day_offset))
        )
    }

    /// 运行日志文件名（本地日期口径，与 tracing-appender 同名规则）
    fn logs_file_name(day_offset: i64) -> String {
        format!(
            "app.log.{}",
            (Local::now().date_naive() - chrono::Duration::days(day_offset))
        )
    }

    #[test]
    fn 收集_events缺失目录为空集_窗口外与非规范命名不收() {
        let tmp = tempfile::tempdir().unwrap();
        // 今天 + 窗口内旧文件收；31 天前超窗；非 .jsonl 与非日期命名不收；空文件照收
        write_file(
            &tmp.path().join("events").join(events_file_name(0)),
            b"{\"id\":\"a\"}\n",
        );
        write_file(
            &tmp.path().join("events").join(events_file_name(29)),
            b"{\"id\":\"b\"}\n",
        );
        write_file(
            &tmp.path().join("events").join(events_file_name(31)),
            b"{\"id\":\"too-old\"}\n",
        );
        write_file(&tmp.path().join("events").join("not-a-date.jsonl"), b"{}\n");
        write_file(
            &tmp.path().join("events").join("2020-01-01.txt"),
            b"binary?\n",
        );
        write_file(&tmp.path().join("events").join(events_file_name(2)), b"");

        let out = collect_bundle_data_files(tmp.path(), 1024 * 1024);
        assert_eq!(out.events.files, 3);
        assert!(!out.events.truncated);
        assert!(out.skipped.is_empty());
        let names: Vec<&str> = out.files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&&format!("events/{}", events_file_name(0))[..]));
        assert!(names.contains(&&format!("events/{}", events_file_name(29))[..]));
        assert!(names.contains(&&format!("events/{}", events_file_name(2))[..]));
        assert!(!names.iter().any(|n| n.contains("too-old")));
        assert!(!names.iter().any(|n| n.contains("not-a-date")));
        // 空文件也在列（文件名计数已含），字节为零
        assert_eq!(out.events.bytes, 22);
    }

    #[test]
    fn 收集_events目录缺失为空集() {
        let tmp = tempfile::tempdir().unwrap();
        let out = collect_bundle_data_files(tmp.path(), 1024 * 1024);
        assert_eq!(out.events.files, 0);
        assert_eq!(out.logs.files, 0);
        assert!(!out.events.truncated);
        assert!(out.skipped.is_empty());
        assert!(out.files.is_empty());
    }

    #[test]
    fn 收集_logs七日窗口与当前文件() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(&tmp.path().join("logs").join("app.log"), b"current\n");
        write_file(
            &tmp.path().join("logs").join(logs_file_name(3)),
            b"three days ago\n",
        );
        write_file(
            &tmp.path().join("logs").join(logs_file_name(10)),
            b"ten days ago\n",
        );
        // 无关文件不进包
        write_file(&tmp.path().join("logs").join("not-a-log.txt"), b"x\n");

        let out = collect_bundle_data_files(tmp.path(), 1024 * 1024);
        assert_eq!(out.logs.files, 2, "当前 app.log + 3 天前轮转，10 天前超窗");
        let names: Vec<&str> = out.files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"logs/app.log"));
        assert!(names.contains(&&format!("logs/{}", logs_file_name(3))[..]));
        assert!(
            !names
                .iter()
                .any(|n| n.contains("ten days ago") || n.contains(&logs_file_name(10)))
        );
    }

    #[test]
    fn 收集_总量超限截断新文件优先() {
        let tmp = tempfile::tempdir().unwrap();
        // events 两份：新文件 800B 收入后仅剩 200B，旧文件 900B 超限截断
        write_file(
            &tmp.path().join("events").join(events_file_name(0)),
            &vec![b'a'; 800],
        );
        write_file(
            &tmp.path().join("events").join(events_file_name(1)),
            &vec![b'b'; 900],
        );
        write_file(&tmp.path().join("logs").join("app.log"), &[b'c'; 100]);

        let out = collect_bundle_data_files(tmp.path(), 1000);
        assert_eq!(out.events.files, 1, "仅最新的事件文件入包");
        assert!(out.events.truncated, "旧文件超限应标记截断");
        assert_eq!(out.logs.files, 1, "logs 分享剩余预算，放得下就收");
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].contains(&events_file_name(1)));
        assert!(out.skipped[0].contains("超总量上限截断"));
    }

    #[tokio::test]
    async fn 诊断包_端到端条目齐全且manifest注明截断与排除项() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            &tmp.path().join("events").join(events_file_name(0)),
            b"{\"id\":\"e1\"}\n",
        );
        write_file(&tmp.path().join("logs").join("app.log"), b"log line\n");

        let bundle = build_diagnostics_bundle(
            tmp.path(),
            &full_sensitive_settings(),
            &full_sensitive_profile(),
            &full_sensitive_llm(),
            "== 环境文本 ==\n".into(),
            3600,
        )
        .unwrap();
        assert!(bundle.filename.starts_with("campus-auth-diagnostics-"));
        assert!(bundle.filename.ends_with(".zip"));

        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bundle.bytes)).unwrap();
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "config.sanitized.json".to_string(),
                "environment.txt".to_string(),
                format!("events/{}", events_file_name(0)),
                "logs/app.log".to_string(),
                "manifest.json".to_string(),
            ]
        );

        let mut manifest = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("manifest.json").unwrap(), &mut manifest)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["os"], std::env::consts::OS);
        assert_eq!(manifest["arch"], std::env::consts::ARCH);
        assert_eq!(manifest["uptime_seconds"], 3600);
        assert!(manifest["exported_at"].as_str().is_some());
        assert_eq!(manifest["sections"]["events"]["files"], 1);
        assert_eq!(manifest["sections"]["events"]["truncated"], false);
        assert_eq!(manifest["sections"]["logs"]["files"], 1);
        assert_eq!(
            manifest["sections"]["config"]["sanitized_fields"],
            serde_json::json!(BUNDLE_SANITIZED_FIELD_PATHS)
        );
        assert_eq!(
            manifest["excluded"],
            serde_json::json!(BUNDLE_EXCLUDED_PATHS)
        );

        // 端到端脱敏：包内 config.sanitized.json 不含任何敏感原值
        let mut config = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("config.sanitized.json").unwrap(),
            &mut config,
        )
        .unwrap();
        for forbidden in [
            PW_MGMT,
            PW_PROFILE_ENC,
            BARK_KEY,
            SEND_KEY,
            TG_TOKEN,
            AI_KEY_GLM,
            RECORDED_FILL_SECRET,
            RECORDED_CHANGE_SECRET,
        ] {
            assert!(
                !config.contains(forbidden),
                "包内泄漏 {forbidden}: {config}"
            );
        }
    }

    #[test]
    fn 环境文本_含状态字段与目录清单() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(&tmp.path().join("config").join("settings.json"), b"{}\n");
        write_file(
            &tmp.path().join("python_worker").join(".venv").join("py"),
            b"x",
        );
        let status = crate::environment::EnvironmentStatus {
            uv_ready: true,
            python_ready: true,
            worker_ready: false,
            manifest_current: true,
            playwright_ready: false,
            system_browser_ready: true,
            ocr_enabled: true,
            ocr_ready: false,
            capability_ready: false,
            stage: crate::environment::BootstrapStage::Idle,
            progress: None,
            last_error: Some("浏览器安装失败".into()),
        };
        let text =
            render_environment_text(&status, std::path::Path::new("/venv/python"), tmp.path());
        assert!(text.contains("uv_ready: true"));
        assert!(text.contains("worker_ready: false"));
        assert!(text.contains("last_error: 浏览器安装失败"));
        assert!(text.contains("python_path: /venv/python"));
        // 目录清单：config 统计大小；venv 只报存在性
        assert!(text.contains("config/"), "{text}");
        assert!(text.contains("events/"), "{text}");
        assert!(text.contains("python_worker/.venv  存在"), "{text}");
    }
}
