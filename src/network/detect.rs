//! NetworkDetect trait 与跨平台实现

use std::net::Ipv4Addr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 网络检测相关错误
#[derive(Debug, Error)]
pub enum NetworkError {
    /// 子进程执行失败
    #[error("子进程执行失败: {command}")]
    SubprocessFailed {
        command: String,
        #[source]
        source: std::io::Error,
    },

    /// 子进程超时
    #[error("子进程超时: {command} ({timeout_secs}s)")]
    SubprocessTimeout {
        command: String,
        timeout_secs: u64,
    },

    /// 解析系统命令输出失败
    #[error("解析系统命令输出失败: {command}")]
    ParseFailed {
        command: String,
        #[source]
        source: anyhow::Error,
    },

    /// IO 错误（如绑定 SOCKS5 端口）
    #[error("IO 错误: {0}")]
    Io(#[source] std::io::Error),

    /// SOCKS5 端口被占用
    #[error("SOCKS5 端口 {port} 被占用，重试 {retries} 次后仍失败")]
    Socks5PortBusy { port: u16, retries: u8 },

    /// SOCKS5 转发器异常退出
    #[error("SOCKS5 转发器异常退出: {reason}")]
    Socks5Crashed { reason: String },

    /// 网络检测不支持当前平台
    #[error("网络检测不支持当前平台")]
    UnsupportedPlatform,
}

/// 网络检测 trait：各平台提供具体实现
#[async_trait]
pub trait NetworkDetect: Send + Sync {
    /// 列出所有有效网络接口（已过滤 loopback / 无 IP / 虚拟接口）
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError>;

    /// 获取所有默认路由的网关 IP
    async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError>;

    /// 获取当前连接的 WiFi SSID（无 WiFi 接口时返回 None）
    async fn current_ssid(&self) -> Result<Option<String>, NetworkError>;
}

/// 单个网络接口信息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceInfo {
    /// 接口名称，如 "以太网"、"WLAN"
    pub name: String,
    /// IPv4 地址
    pub ipv4: Ipv4Addr,
    /// 网关 IP（可能没有）
    pub gateway: Option<Ipv4Addr>,
    /// 是否为 WiFi 接口（尽力推断）
    pub is_wifi: bool,
    /// WiFi SSID（仅 WiFi 接口）
    pub ssid: Option<String>,
}

/// 网关与 SSID 汇总信息
#[derive(Debug, Clone, Serialize)]
pub struct GatewayInfo {
    /// 所有默认路由网关
    pub gateways: Vec<Ipv4Addr>,
    /// 当前 WiFi SSID
    pub ssid: Option<String>,
}

/// 执行系统命令并返回 stdout，带超时
async fn run_command(program: &str, args: &[&str]) -> Result<String, NetworkError> {
    let timeout = Duration::from_secs(SUBPROCESS_TIMEOUT_SECS);
    let fut = tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    let output = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| NetworkError::SubprocessTimeout {
            command: program.to_string(),
            timeout_secs: SUBPROCESS_TIMEOUT_SECS,
        })?
        .map_err(|source| NetworkError::SubprocessFailed {
            command: program.to_string(),
            source,
        })?;
    if !output.status.success() {
        return Err(NetworkError::ParseFailed {
            command: program.to_string(),
            source: anyhow::anyhow!("命令退出码 {:?}", output.status.code()),
        });
    }
    Ok(decode_console_output(&output.stdout))
}

/// 解码控制台命令输出：优先 UTF-8，失败则按 GBK（中文 Windows 默认代码页 936）解码。
///
/// Windows 上 `ipconfig`、`route` 等命令的输出编码取决于控制台代码页：
/// - 代码页 65001 (UTF-8)：输出为 UTF-8，`from_utf8` 可直接解析
/// - 代码页 936 (GBK)：输出为 GBK，`from_utf8` 会将中文乱码化
///
/// 用 GBK 兜底解码保证中文标签（如"适配器"、"IPv4 地址"）能被正确解析。
fn decode_console_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::GBK.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

/// 解析 `ipconfig /all` 输出，提取各网络接口信息（过滤 loopback / 无 IP / 虚拟接口）
fn parse_ipconfig(text: &str) -> Vec<InterfaceInfo> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        // 适配器标题行：形如 “以太网适配器 以太网:” / “Wireless LAN adapter WLAN:”
        let is_header = (trimmed.contains("适配器") || trimmed.to_ascii_lowercase().contains("adapter"))
            && trimmed.ends_with(':');
        if is_header {
            if in_block && !current.is_empty() {
                if let Some(info) = parse_adapter_block(&current) {
                    result.push(info);
                }
            }
            current.clear();
            in_block = true;
        }
        if in_block {
            current.push_str(line);
            current.push('\n');
        }
    }
    if in_block && !current.is_empty() {
        if let Some(info) = parse_adapter_block(&current) {
            result.push(info);
        }
    }
    result
}

/// 从单个适配器块解析出 InterfaceInfo（无有效 IPv4 时返回 None）
fn parse_adapter_block(block: &str) -> Option<InterfaceInfo> {
    let mut header: &str = "";
    let mut ipv4: Option<Ipv4Addr> = None;
    let mut gateway: Option<Ipv4Addr> = None;
    let mut media_disconnected = false;
    for line in block.lines() {
        let trimmed = line.trim();
        if (trimmed.contains("适配器") || trimmed.to_ascii_lowercase().contains("adapter"))
            && trimmed.ends_with(':')
        {
            header = trimmed;
        }
        // IPv4 地址（中英文标签兼容）
        if trimmed.contains("IPv4 地址") || trimmed.contains("IPv4 Address") {
            if let Some(ip) = extract_ipv4(trimmed) {
                ipv4 = Some(ip);
            }
        }
        // 默认网关（中英文标签兼容）
        if trimmed.contains("默认网关") || trimmed.contains("Default Gateway") {
            gateway = extract_ipv4(trimmed);
        }
        // 已断开的适配器（"媒体状态: 已断开" / "Media State: Media disconnected"）
        if (trimmed.contains("媒体状态") || trimmed.to_ascii_lowercase().contains("media state"))
            && (trimmed.contains("已断开")
                || trimmed.to_ascii_lowercase().contains("media disconnected"))
        {
            media_disconnected = true;
        }
    }
    if media_disconnected {
        return None;
    }
    let name = adapter_name(header);
    if name.is_empty() || is_virtual_interface(&name) {
        return None;
    }
    let ipv4 = ipv4?;
    // 过滤回环地址
    if ipv4 == Ipv4Addr::LOCALHOST {
        return None;
    }
    let is_wifi = {
        let n = name.to_ascii_lowercase();
        n.contains("wlan") || n.contains("wi-fi") || block.contains("无线")
    };
    Some(InterfaceInfo {
        name,
        ipv4,
        gateway,
        is_wifi,
        ssid: None,
    })
}

/// 从一行文本中提取冒号后的 IPv4 地址（忽略 (首选)/(Preferred) 后缀）
fn extract_ipv4(line: &str) -> Option<Ipv4Addr> {
    let value = line.rfind(':')?;
    let v = line[value + 1..].trim();
    if v.is_empty() {
        return None;
    }
    // 截去括号及之后的说明文字
    let ip_str = v.split(['(', '（']).next()?.trim();
    ip_str.parse::<Ipv4Addr>().ok()
}

/// 从适配器标题行提取接口名（去掉 “适配器 ” / “adapter ” 前缀与结尾冒号）
fn adapter_name(header: &str) -> String {
    let h = header.trim_end_matches(':').trim();
    if let Some(idx) = h.rfind("适配器 ") {
        h[idx + "适配器 ".len()..].trim().to_string()
    } else if let Some(idx) = h.to_ascii_lowercase().rfind("adapter ") {
        h[idx + "adapter ".len()..].trim().to_string()
    } else {
        h.to_string()
    }
}

/// 解析 `netsh wlan show interfaces` 输出，提取首个有效的 WiFi SSID（无则 None）
///
/// 健壮性要点：
/// - 仅匹配以 `SSID` 为字段键的行（其后必须紧跟空白/数字/冒号），避免误命中其它文本
/// - 以第一个冒号为键值分隔符，取其后的全部内容（SSID 本身可含冒号）
/// - 跳过空值（未连接或字段缺失）
fn parse_netsh_ssid(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        // 非 SSID 字段键的行直接跳过
        let rest = match line.strip_prefix("SSID") {
            Some(r) => r,
            None => continue,
        };
        // 字段键后必须是空白、数字或冒号（如 "SSID" / "SSID 5" / "SSID :"）
        if !rest.starts_with(|c: char| c.is_whitespace() || c.is_ascii_digit() || c == ':') {
            continue;
        }
        let idx = match rest.find(':') {
            Some(i) => i,
            None => continue,
        };
        let ssid = rest[idx + 1..].trim();
        if !ssid.is_empty() {
            return Some(ssid.to_string());
        }
    }
    None
}

/// 是否为虚拟/隧道接口（不参与联网状态判断）
fn is_virtual_interface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("vmware")
        || n.contains("virtualbox")
        || n.contains("vethernet")
        || n.contains("docker")
        || n.contains("npcap")
        || n.contains("隧道")
        || n.contains("tunnel")
}

/// Windows 网络检测器
pub struct WindowsDetect;

#[async_trait]
impl NetworkDetect for WindowsDetect {
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
        let out = run_command("ipconfig", &["/all"]).await?;
        // 解析后兜底过滤：排除虚拟/回环/链路本地/未指定地址
        Ok(crate::network::interfaces::filter_interfaces(parse_ipconfig(
            &out,
        )))
    }

    async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
        let out = run_command("route", &["print", "0.0.0.0"]).await?;
        let mut gateways = Vec::new();
        for line in out.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // 0.0.0.0 行：第一列是目标网络，第二列是网关
            if cols.len() >= 3 && cols[0] == "0.0.0.0" {
                if let Ok(ip) = cols[2].parse::<Ipv4Addr>() {
                    gateways.push(ip);
                }
            }
        }
        Ok(gateways)
    }

    async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
        match run_command("netsh", &["wlan", "show", "interfaces"]).await {
            Ok(out) => Ok(parse_netsh_ssid(&out)),
            Err(_) => Ok(None),
        }
    }
}

/// Linux 虚拟接口前缀
const LINUX_VIRTUAL_PREFIXES: &[&str] = &[
    "docker", "veth", "br-", "virbr", "tun", "tap", "lo", "bond", "dummy",
];

/// Linux 网络检测器
pub struct LinuxDetect;

#[async_trait]
impl NetworkDetect for LinuxDetect {
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
        let out = run_command("ip", &["addr", "show"]).await?;
        // 解析后兜底过滤：排除虚拟/回环/链路本地/未指定地址
        Ok(crate::network::interfaces::filter_interfaces(parse_ip_addr(
            &out,
        )))
    }

    async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
        let out = run_command("ip", &["route", "show", "default"]).await?;
        let mut gateways = Vec::new();
        for line in out.lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("default") && parts.next() == Some("via") {
                if let Some(gw) = parts.next() {
                    if let Ok(ip) = gw.parse::<Ipv4Addr>() {
                        gateways.push(ip);
                    }
                }
            }
        }
        Ok(gateways)
    }

    async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
        match run_command("iwgetid", &["-r"]).await {
            Ok(out) => {
                let ssid = out.trim();
                if ssid.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(ssid.to_string()))
                }
            }
            Err(_) => Ok(None),
        }
    }
}

/// macOS 网络检测器
pub struct MacosDetect;

#[async_trait]
impl NetworkDetect for MacosDetect {
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
        let out = run_command("ifconfig", &[]).await?;
        // 解析后兜底过滤：排除虚拟/回环/链路本地/未指定地址
        Ok(crate::network::interfaces::filter_interfaces(
            parse_ifconfig(&out),
        ))
    }

    async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
        let out = run_command("route", &["-n", "get", "default"]).await?;
        for line in out.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("gateway:") {
                let gw = rest.trim();
                if let Ok(ip) = gw.parse::<Ipv4Addr>() {
                    return Ok(vec![ip]);
                }
            }
        }
        Ok(Vec::new())
    }

    async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
        // 先获取 WiFi 设备名（通常为 en0 或 en1）
        let hw_out = match run_command("networksetup", &["-listallhardwareports"]).await {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        let mut wifi_device: Option<String> = None;
        let mut lines = hw_out.lines().peekable();
        while let Some(line) = lines.next() {
            if line.contains("Hardware Port:") && line.to_ascii_lowercase().contains("wi-fi") {
                // 下一行是 Device: enX
                if let Some(dev_line) = lines.next() {
                    if let Some(rest) = dev_line.strip_prefix("Device:") {
                        wifi_device = Some(rest.trim().to_string());
                        break;
                    }
                }
            }
        }
        let device = match wifi_device {
            Some(d) => d,
            None => return Ok(None),
        };
        // 获取当前 WiFi 网络
        match run_command("networksetup", &["-getairportnetwork", &device]).await {
            Ok(out) => {
                // 输出格式: "Current Wi-Fi Network: MyWiFi" 或 "You are not associated with an AirPort network."
                if let Some(rest) = out.strip_prefix("Current Wi-Fi Network:") {
                    let ssid = rest.trim();
                    if !ssid.is_empty() {
                        return Ok(Some(ssid.to_string()));
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }
}

/// 解析 `ip addr show` 输出（Linux），提取各网络接口信息
fn parse_ip_addr(text: &str) -> Vec<InterfaceInfo> {
    let mut result = Vec::new();
    let mut current_name = String::new();
    let mut current_ipv4: Option<Ipv4Addr> = None;
    let mut current_is_up = false;
    let mut in_block = false;

    for line in text.lines() {
        // 接口标题行: "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> ..."
        if let Some(rest) = line.strip_prefix(|c: char| c.is_ascii_digit()) {
            if let Some(rest) = rest.strip_prefix(": ") {
                // 提取接口名（去掉 @ 后缀）
                let name = rest.split(':').next().unwrap_or("").split('@').next().unwrap_or("").trim();
                // 保存上一个接口
                if in_block {
                    if let Some(ipv4) = current_ipv4 {
                        if !is_linux_virtual(&current_name) && current_is_up {
                            let is_wifi = current_name.starts_with("wl");
                            result.push(InterfaceInfo {
                                name: current_name.clone(),
                                ipv4,
                                gateway: None,
                                is_wifi,
                                ssid: None,
                            });
                        }
                    }
                }
                current_name = name.to_string();
                current_ipv4 = None;
                current_is_up = line.contains("UP");
                in_block = true;
            }
        }
        if !in_block {
            continue;
        }
        // IPv4 行: "    inet 192.168.1.100/24 brd 192.168.1.255 scope global eth0"
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("inet ") {
            if let Some(ip_part) = rest.split('/').next() {
                if let Ok(ip) = ip_part.parse::<Ipv4Addr>() {
                    if ip != Ipv4Addr::LOCALHOST {
                        current_ipv4 = Some(ip);
                    }
                }
            }
        }
    }
    // 最后一个接口
    if in_block {
        if let Some(ipv4) = current_ipv4 {
            if !is_linux_virtual(&current_name) && current_is_up {
                let is_wifi = current_name.starts_with("wl");
                result.push(InterfaceInfo {
                    name: current_name,
                    ipv4,
                    gateway: None,
                    is_wifi,
                    ssid: None,
                });
            }
        }
    }
    result
}

/// Linux 虚拟接口判断
fn is_linux_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    LINUX_VIRTUAL_PREFIXES.iter().any(|p| n.starts_with(p))
}

/// 解析 `ifconfig` 输出（macOS），提取各网络接口信息
fn parse_ifconfig(text: &str) -> Vec<InterfaceInfo> {
    let mut result = Vec::new();
    let mut current_name = String::new();
    let mut current_ipv4: Option<Ipv4Addr> = None;
    let mut current_is_up = false;
    let mut in_block = false;

    for line in text.lines() {
        // 接口标题行: "en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500"
        if !line.starts_with(' ') && !line.is_empty() && line.contains("flags=") {
            // 保存上一个接口
            if in_block {
                if let Some(ipv4) = current_ipv4 {
                    if !is_macos_virtual(&current_name) && current_is_up {
                        let is_wifi = current_name == "en0" || current_name == "en1";
                        result.push(InterfaceInfo {
                            name: current_name.clone(),
                            ipv4,
                            gateway: None,
                            is_wifi,
                            ssid: None,
                        });
                    }
                }
            }
            current_name = line.split(':').next().unwrap_or("").trim().to_string();
            current_ipv4 = None;
            current_is_up = line.contains("UP");
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        // IPv4 行: "	inet 192.168.1.100 netmask 0xffffff00 broadcast 192.168.1.255"
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("inet ") {
            if let Some(ip_part) = rest.split_whitespace().next() {
                if let Ok(ip) = ip_part.parse::<Ipv4Addr>() {
                    if ip != Ipv4Addr::LOCALHOST {
                        current_ipv4 = Some(ip);
                    }
                }
            }
        }
    }
    // 最后一个接口
    if in_block {
        if let Some(ipv4) = current_ipv4 {
            if !is_macos_virtual(&current_name) && current_is_up {
                let is_wifi = current_name == "en0" || current_name == "en1";
                result.push(InterfaceInfo {
                    name: current_name,
                    ipv4,
                    gateway: None,
                    is_wifi,
                    ssid: None,
                });
            }
        }
    }
    result
}

/// macOS 虚拟接口判断
fn is_macos_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("bridge")
        || n.starts_with("vboxnet")
        || n.starts_with("vmnet")
        || n == "lo0"
        || n.starts_with("utun")
        || n.starts_with("awdl")
}

/// 根据编译目标创建对应平台的检测器
pub fn create_detector() -> std::sync::Arc<dyn NetworkDetect> {
    #[cfg(target_os = "windows")]
    {
        std::sync::Arc::new(WindowsDetect)
    }
    #[cfg(target_os = "linux")]
    {
        std::sync::Arc::new(LinuxDetect)
    }
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(MacosDetect)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        std::sync::Arc::new(UnsupportedDetect)
    }
}

/// 未支持平台的占位检测器
pub struct UnsupportedDetect;

#[async_trait]
impl NetworkDetect for UnsupportedDetect {
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
        Err(NetworkError::UnsupportedPlatform)
    }

    async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
        Err(NetworkError::UnsupportedPlatform)
    }

    async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
        Err(NetworkError::UnsupportedPlatform)
    }
}

/// 系统命令超时（秒）
pub const SUBPROCESS_TIMEOUT_SECS: u64 = 10;

#[cfg(test)]
mod tests {
    use super::*;

    // ============ extract_ipv4 测试 ============

    #[test]
    fn test_extract_ipv4_basic() {
        // 基本 IPv4 提取
        let line = "IPv4 地址 . . . . . . . . . . . . : 192.168.1.100";
        assert_eq!(extract_ipv4(line), Some(Ipv4Addr::new(192, 168, 1, 100)));
    }

    #[test]
    fn test_extract_ipv4_with_preferred_suffix() {
        // 带 (首选) 后缀的 IPv4 提取
        let line = "IPv4 Address. . . . . . . . . . . : 10.0.0.5(首选)";
        assert_eq!(extract_ipv4(line), Some(Ipv4Addr::new(10, 0, 0, 5)));
    }

    #[test]
    fn test_extract_ipv4_english_label() {
        // 英文标签的 IPv4 提取
        let line = "IPv4 Address. . . . . . . . . . . : 172.16.0.1 (Preferred)";
        assert_eq!(extract_ipv4(line), Some(Ipv4Addr::new(172, 16, 0, 1)));
    }

    #[test]
    fn test_extract_ipv4_empty_value() {
        // 空值应返回 None
        let line = "IPv4 地址 . . . . . . . . . . . . : ";
        assert_eq!(extract_ipv4(line), None);
    }

    #[test]
    fn test_extract_ipv4_no_colon() {
        // 无冒号的行应返回 None
        let line = "no colon here";
        assert_eq!(extract_ipv4(line), None);
    }

    #[test]
    fn test_extract_ipv4_gateway_line() {
        // 默认网关行的 IPv4 提取
        let line = "默认网关 . . . . . . . . . . . . : 192.168.1.1";
        assert_eq!(extract_ipv4(line), Some(Ipv4Addr::new(192, 168, 1, 1)));
    }

    // ============ adapter_name 测试 ============

    #[test]
    fn test_adapter_name_chinese() {
        let header = "以太网适配器 以太网:";
        assert_eq!(adapter_name(header), "以太网");
    }

    #[test]
    fn test_adapter_name_english() {
        let header = "Wireless LAN adapter WLAN:";
        assert_eq!(adapter_name(header), "WLAN");
    }

    #[test]
    fn test_adapter_name_no_prefix() {
        let header = "SomeHeader:";
        assert_eq!(adapter_name(header), "SomeHeader");
    }

    // ============ is_virtual_interface 测试 ============

    #[test]
    fn test_is_virtual_vmware() {
        assert!(is_virtual_interface("VMware Network Adapter"));
        assert!(is_virtual_interface("vmware8"));
    }

    #[test]
    fn test_is_virtual_vbox() {
        assert!(is_virtual_interface("VirtualBox Host-Only Ethernet Adapter"));
    }

    #[test]
    fn test_is_virtual_docker() {
        assert!(is_virtual_interface("DockerNAT"));
        assert!(is_virtual_interface("docker0"));
    }

    #[test]
    fn test_is_virtual_tunnel() {
        assert!(is_virtual_interface("隧道适配器 Tunnel"));
        assert!(is_virtual_interface("Tunnel Adapter"));
    }

    #[test]
    fn test_is_virtual_npcap() {
        assert!(is_virtual_interface("Npcap Loopback Adapter"));
    }

    #[test]
    fn test_is_not_virtual_normal_interface() {
        assert!(!is_virtual_interface("以太网"));
        assert!(!is_virtual_interface("WLAN"));
        assert!(!is_virtual_interface("Wi-Fi"));
    }

    // ============ parse_netsh_ssid 测试（Windows 格式） ============

    #[test]
    fn test_parse_netsh_ssid_basic() {
        let input = "State                  : connected\n\
                     SSID                   : MyHomeWiFi\n\
                     BSSID                  : aa:bb:cc:dd:ee:ff";
        assert_eq!(parse_netsh_ssid(input), Some("MyHomeWiFi".to_string()));
    }

    #[test]
    fn test_parse_netsh_ssid_with_index() {
        // SSID 后带序号（多 SSID 场景）
        let input = "SSID 5                 : OfficeNet\n";
        assert_eq!(parse_netsh_ssid(input), Some("OfficeNet".to_string()));
    }

    #[test]
    fn test_parse_netsh_ssid_with_colon_in_value() {
        // SSID 值本身含冒号（MAC 型），应完整保留
        let input = "SSID                   : AA:BB:CC\n";
        assert_eq!(parse_netsh_ssid(input), Some("AA:BB:CC".to_string()));
    }

    #[test]
    fn test_parse_netsh_ssid_empty_value_skipped() {
        // 已连接但 SSID 为空时应跳过并继续
        let input = "SSID                   : \n\
                     SSID 5                 : RealNet";
        assert_eq!(parse_netsh_ssid(input), Some("RealNet".to_string()));
    }

    #[test]
    fn test_parse_netsh_ssid_not_matched() {
        // 不含 SSID 字段（未连接）
        let input = "State                  : disconnected\n\
                     Name                   : Wi-Fi";
        assert_eq!(parse_netsh_ssid(input), None);
    }

    #[test]
    fn test_parse_netsh_ssid_rejects_embedded() {
        // "SSID" 作为其它词的一部分不应误匹配
        let input = "BSSID                  : aa:bb:cc:dd:ee:ff";
        assert_eq!(parse_netsh_ssid(input), None);
    }

    // ============ parse_ipconfig 测试（Windows 格式） ============

    #[test]
    fn test_parse_ipconfig_basic() {
        let input = r#"Windows IP 配置

以太网适配器 以太网:

   连接特定的 DNS 后缀 . . . . . . . :
   描述. . . . . . . . . . . . . . . . : Intel Ethernet
   IPv4 地址 . . . . . . . . . . . . : 192.168.1.100(首选)
   子网掩码  . . . . . . . . . . . . : 255.255.255.0
   默认网关 . . . . . . . . . . . . : 192.168.1.1

无线局域网适配器 WLAN:

   连接特定的 DNS 后缀 . . . . . . . :
   描述. . . . . . . . . . . . . . . . : Intel Wi-Fi
   IPv4 地址 . . . . . . . . . . . . : 10.0.0.5(首选)
   子网掩码  . . . . . . . . . . . . : 255.255.255.0
   默认网关 . . . . . . . . . . . . : 10.0.0.1
"#;
        let interfaces = parse_ipconfig(input);
        assert_eq!(interfaces.len(), 2);
        // 第一个接口
        assert_eq!(interfaces[0].name, "以太网");
        assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(interfaces[0].gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!interfaces[0].is_wifi);
        // 第二个接口
        assert_eq!(interfaces[1].name, "WLAN");
        assert_eq!(interfaces[1].ipv4, Ipv4Addr::new(10, 0, 0, 5));
        assert!(interfaces[1].is_wifi);
    }

    #[test]
    fn test_parse_ipconfig_filters_virtual() {
        // 虚拟接口（如 Npcap）应被过滤
        let input = r#"Npcap Loopback Adapter:

   IPv4 地址 . . . . . . . . . . . . : 192.168.88.1(首选)
   默认网关 . . . . . . . . . . . . :

以太网适配器 以太网:

   IPv4 地址 . . . . . . . . . . . . : 10.0.0.10(首选)
   默认网关 . . . . . . . . . . . . : 10.0.0.1
"#;
        let interfaces = parse_ipconfig(input);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name, "以太网");
    }

    #[test]
    fn test_parse_ipconfig_english_labels() {
        // 英文标签的 ipconfig 输出
        let input = r#"Ethernet adapter Ethernet:

   IPv4 Address. . . . . . . . . . . : 10.10.10.5
   Subnet Mask . . . . . . . . . . . : 255.255.255.0
   Default Gateway . . . . . . . . . : 10.10.10.1
"#;
        let interfaces = parse_ipconfig(input);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(10, 10, 10, 5));
        assert_eq!(interfaces[0].gateway, Some(Ipv4Addr::new(10, 10, 10, 1)));
    }

    #[test]
    fn test_parse_ipconfig_empty() {
        // 空输入应返回空列表
        let interfaces = parse_ipconfig("");
        assert!(interfaces.is_empty());
    }

    #[test]
    fn test_parse_ipconfig_no_ipv4_skipped() {
        // 无 IPv4 的适配器应被跳过
        let input = r#"以太网适配器 断开的连接:

   媒体状态  . . . . . . . . . . . . : 已断开
   描述. . . . . . . . . . . . . . . . : Disconnected NIC
"#;
        let interfaces = parse_ipconfig(input);
        assert!(interfaces.is_empty());
    }

    #[test]
    fn test_parse_ipconfig_gbk_chinese_labels() {
        // 模拟实际 ipconfig /all 输出（GBK 编码，中文标签）
        // "以太网适配器 以太网:" 和 "IPv4 地址" 等中文标签在 GBK 下是合法字节但非 UTF-8
        let gbk_bytes: &[u8] = b"Windows IP \xC5\xE4\xD6\xC3\r\n\r\n\
\xD2\xD4\xCC\xAB\xCD\xF8\xCA\xCA\xC5\xE4\xC6\xF7 \xD2\xD4\xCC\xAB\xCD\xF8:\r\n\r\n\
   \xC3\xBD\xCC\xE5\xD7\xB4\xCC\xAC  . . . . . . . . . . . . : \xC3\xBD\xCC\xE5\xD2\xD1\xB6\xCF\xBF\xAA\r\n\
   \xC3\xE8\xCA\xF6. . . . . . . . . . . . . . . . : Realtek PCIe GbE\r\n\r\n\
\xCE\xDE\xCF\xDF\xD0\xC7\xD3\xF2\xCD\xF8\xCA\xCA\xC5\xE4\xC6\xF7 WLAN 2:\r\n\r\n\
   \xC3\xE8\xCA\xF6. . . . . . . . . . . . . . . . : Intel Wi-Fi\r\n\
   IPv4 \xB5\xD8\xD6\xB7 . . . . . . . . . . . . : 192.168.31.178(\xCA\xD7\xD1\xA1)\r\n\
   \xD7\xD3\xCD\xF8\xD1\xDA\xC2\xEB  . . . . . . . . . . . . : 255.255.255.0\r\n\
   \xC4\xAC\xC8\xCF\xCD\xF8\xB9\xD8 . . . . . . . . . . . . : 192.168.31.1\r\n";
        // 使用 decode_console_output 解码（而非 from_utf8_lossy）
        let decoded = decode_console_output(gbk_bytes);
        // 确认解码后包含中文标签
        assert!(decoded.contains("适配器"), "decode 应正确解码 '适配器'");
        assert!(decoded.contains("IPv4 地址"), "decode 应正确解码 'IPv4 地址'");
        assert!(decoded.contains("WLAN 2:"), "decode 应保留 'WLAN 2:'");
        let interfaces = parse_ipconfig(&decoded);
        assert_eq!(
            interfaces.len(),
            1,
            "应解析出 WLAN 2 接口, 实际: {:?}",
            interfaces
        );
        assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(192, 168, 31, 178));
    }

    #[tokio::test]
    async fn test_run_ipconfig_actual() {
        // 实际调用 ipconfig /all 验证 run_command 是否正常工作
        let result = run_command("ipconfig", &["/all"]).await;
        match result {
            Ok(out) => {
                eprintln!("ipconfig output length: {}", out.len());
                eprintln!(
                    "First 200 chars: {:?}",
                    &out.chars().take(200).collect::<String>()
                );
                assert!(!out.is_empty(), "ipconfig 输出不应为空");
                // 中英文 Windows 均包含冒号（用于键值分隔，如 "IPv4 地址 : x.x.x.x"）
                assert!(out.contains(':'), "输出应包含冒号（键值分隔符）");
                let interfaces = parse_ipconfig(&out);
                eprintln!("Parsed {} interfaces", interfaces.len());
                assert!(
                    !interfaces.is_empty(),
                    "应至少解析出一个网络接口, 输出前500字符: {:?}",
                    &out.chars().take(500).collect::<String>()
                );
            }
            Err(e) => {
                panic!("run_command(ipconfig) 失败: {}", e);
            }
        }
    }

    // ============ parse_ip_addr 测试（Linux 格式） ============

    #[test]
    fn test_parse_ip_addr_basic() {
        let input = r#"1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN
    inet 127.0.0.1/8 scope host lo
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP
    inet 192.168.1.100/24 brd 192.168.1.255 scope global eth0
3: wl0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP
    inet 10.0.0.5/24 brd 10.0.0.255 scope global wl0
"#;
        let interfaces = parse_ip_addr(input);
        // lo 应被过滤（回环 + 虚拟前缀）
        assert_eq!(interfaces.len(), 2);
        assert_eq!(interfaces[0].name, "eth0");
        assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(192, 168, 1, 100));
        assert!(!interfaces[0].is_wifi);
        assert_eq!(interfaces[1].name, "wl0");
        assert!(interfaces[1].is_wifi);
    }

    #[test]
    fn test_parse_ip_addr_filters_docker() {
        // docker 接口应被过滤
        let input = r#"1: docker0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500
    inet 172.17.0.1/16 brd 172.17.255.255 scope global docker0
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500
    inet 192.168.1.100/24 scope global eth0
"#;
        let interfaces = parse_ip_addr(input);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name, "eth0");
    }

    #[test]
    fn test_parse_ip_addr_down_interface_filtered() {
        // 状态非 UP 的接口应被过滤
        let input = r#"1: eth1: <BROADCAST,MULTICAST> mtu 1500 state DOWN
    inet 10.0.0.5/24 scope global eth1
"#;
        let interfaces = parse_ip_addr(input);
        assert!(interfaces.is_empty());
    }

    #[test]
    fn test_parse_ip_addr_empty() {
        let interfaces = parse_ip_addr("");
        assert!(interfaces.is_empty());
    }

    // ============ parse_ifconfig 测试（macOS 格式） ============

    #[test]
    fn test_parse_ifconfig_basic() {
        let input = r#"lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
	inet 127.0.0.1 netmask 0xff000000
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 192.168.1.100 netmask 0xffffff00 broadcast 192.168.1.255
en1: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 10.0.0.5 netmask 0xffffff00 broadcast 10.0.0.255
"#;
        let interfaces = parse_ifconfig(input);
        // lo0 应被过滤
        assert_eq!(interfaces.len(), 2);
        assert_eq!(interfaces[0].name, "en0");
        assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(192, 168, 1, 100));
        assert!(interfaces[0].is_wifi); // en0 在 macOS 上视为 WiFi
        assert_eq!(interfaces[1].name, "en1");
    }

    #[test]
    fn test_parse_ifconfig_filters_bridge() {
        // bridge 接口应被过滤
        let input = r#"bridge0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 192.168.2.1 netmask 0xffffff00 broadcast 192.168.2.255
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 10.0.0.5 netmask 0xffffff00 broadcast 10.0.0.255
"#;
        let interfaces = parse_ifconfig(input);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name, "en0");
    }

    #[test]
    fn test_parse_ifconfig_empty() {
        let interfaces = parse_ifconfig("");
        assert!(interfaces.is_empty());
    }

    // ============ is_linux_virtual 测试 ============

    #[test]
    fn test_is_linux_virtual_various_prefixes() {
        assert!(is_linux_virtual("docker0"));
        assert!(is_linux_virtual("veth12345"));
        assert!(is_linux_virtual("br-abcdef"));
        assert!(is_linux_virtual("virbr0"));
        assert!(is_linux_virtual("tun0"));
        assert!(is_linux_virtual("tap0"));
        assert!(is_linux_virtual("lo"));
        assert!(is_linux_virtual("bond0"));
        assert!(is_linux_virtual("dummy0"));
    }

    #[test]
    fn test_is_linux_virtual_normal() {
        assert!(!is_linux_virtual("eth0"));
        assert!(!is_linux_virtual("wlan0"));
        assert!(!is_linux_virtual("enp0s3"));
    }

    // ============ is_macos_virtual 测试 ============

    #[test]
    fn test_is_macos_virtual_various() {
        assert!(is_macos_virtual("bridge0"));
        assert!(is_macos_virtual("vboxnet0"));
        assert!(is_macos_virtual("vmnet8"));
        assert!(is_macos_virtual("lo0"));
        assert!(is_macos_virtual("utun0"));
        assert!(is_macos_virtual("awdl0"));
    }

    #[test]
    fn test_is_macos_virtual_normal() {
        assert!(!is_macos_virtual("en0"));
        assert!(!is_macos_virtual("en1"));
    }

    // ============ NetworkError Display 测试 ============

    #[test]
    fn test_network_error_display_subprocess_failed() {
        let err = NetworkError::SubprocessFailed {
            command: "ipconfig".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        assert!(format!("{err}").contains("ipconfig"));
    }

    #[test]
    fn test_network_error_display_timeout() {
        let err = NetworkError::SubprocessTimeout {
            command: "route".to_string(),
            timeout_secs: 10,
        };
        assert!(format!("{err}").contains("route"));
        assert!(format!("{err}").contains("10"));
    }

    #[test]
    fn test_network_error_display_unsupported() {
        let err = NetworkError::UnsupportedPlatform;
        assert!(format!("{err}").contains("不支持"));
    }
}
