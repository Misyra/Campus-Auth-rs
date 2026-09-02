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
    SubprocessTimeout { command: String, timeout_secs: u64 },

    /// 解析系统命令输出失败
    #[error("解析系统命令输出失败: {command}")]
    ParseFailed {
        command: String,
        #[source]
        source: anyhow::Error,
    },

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

/// 执行系统命令并返回 stdout，带超时
async fn run_command(program: &str, args: &[&str]) -> Result<String, NetworkError> {
    let timeout = Duration::from_secs(SUBPROCESS_TIMEOUT_SECS);
    let start = std::time::Instant::now();
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Windows：设置 CREATE_NO_WINDOW，避免 ipconfig/netsh 等系统命令弹出黑色控制台窗口
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let fut = cmd.output();
    let output = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| {
            // 网络探测失败默认对上层表现为空结果，留 debug 便于区分「命令异常」与「真无网络」
            tracing::debug!(
                command = program,
                elapsed_ms = start.elapsed().as_millis() as u64,
                "网络探测命令执行超时"
            );
            NetworkError::SubprocessTimeout {
                command: program.to_string(),
                timeout_secs: SUBPROCESS_TIMEOUT_SECS,
            }
        })?
        .map_err(|source| {
            tracing::debug!(
                command = program,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %source,
                "网络探测命令启动失败"
            );
            NetworkError::SubprocessFailed {
                command: program.to_string(),
                source,
            }
        })?;
    if !output.status.success() {
        tracing::debug!(
            command = program,
            elapsed_ms = start.elapsed().as_millis() as u64,
            exit_code = ?output.status.code(),
            "网络探测命令非零退出"
        );
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
        Err(_) => encoding_rs::GBK
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
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
        let is_header = (trimmed.contains("适配器")
            || trimmed.to_ascii_lowercase().contains("adapter"))
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
    // 虚拟网卡特征统一查 interfaces::is_excluded 的单一权威表（A3）：
    // 原 detect.rs 私有的 7 条 is_virtual_interface 已并入该表
    if name.is_empty() || crate::network::interfaces::is_excluded(&name) {
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
            return Some(decode_netsh_ssid_hex(ssid));
        }
    }
    None
}

/// 还原 netsh 对非 ASCII SSID 的 hex 转义输出
///
/// `netsh wlan show interfaces` 在 SSID 含非 ASCII 字符时会把整个 SSID 输出为
/// hex 字符串（UTF-8 字节序列，如 "4369616C6C6FEFBD9E..." → "Ciallo～..."），
/// 原样透传会导致 Profile 的 wifi_ssid 匹配永远失败。仅当字符串为偶数长度的
/// 合法 hex、可解码为 UTF-8、且解码结果含非 ASCII 可打印字符时才还原，避免把
/// 名字恰好是 hex 形态的普通 SSID（如 "12345678"、"41424344"）误转换。
fn decode_netsh_ssid_hex(ssid: &str) -> String {
    let looks_hex =
        ssid.len() >= 8 && ssid.len() % 2 == 0 && ssid.bytes().all(|b| b.is_ascii_hexdigit());
    if !looks_hex {
        return ssid.to_string();
    }
    let bytes: Vec<u8> = match (0..ssid.len() / 2)
        .map(|i| u8::from_str_radix(&ssid[i * 2..i * 2 + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
    {
        Ok(b) => b,
        Err(_) => return ssid.to_string(),
    };
    let decoded = match String::from_utf8(bytes) {
        Ok(d) => d,
        Err(_) => return ssid.to_string(),
    };
    let has_non_ascii = !decoded.is_ascii();
    let all_printable = decoded.chars().all(|c| !c.is_control());
    if has_non_ascii && all_printable {
        decoded
    } else {
        ssid.to_string()
    }
}

/// Windows 网络检测器
pub struct WindowsDetect;

#[async_trait]
impl NetworkDetect for WindowsDetect {
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
        let out = run_command("ipconfig", &["/all"]).await?;
        // 解析后兜底过滤：排除虚拟/回环/链路本地/未指定地址
        Ok(crate::network::interfaces::filter_interfaces(
            parse_ipconfig(&out),
        ))
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
            Err(e) => {
                // 命令失败（无无线网卡等）与「已连接但无 SSID」不同，留 debug 区分
                tracing::debug!(
                    command = "netsh wlan show interfaces",
                    error = %e,
                    "查询 WiFi SSID 的命令执行失败，视为无 WiFi"
                );
                Ok(None)
            }
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
        Ok(crate::network::interfaces::filter_interfaces(
            parse_ip_addr(&out),
        ))
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
            Err(e) => {
                // iwgetid 缺失/无权限等命令级失败与「未连接」不同，留 debug 区分
                tracing::debug!(
                    command = "iwgetid -r",
                    error = %e,
                    "查询 WiFi SSID 的命令执行失败，视为无 WiFi"
                );
                Ok(None)
            }
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
            Err(e) => {
                tracing::debug!(
                    command = "networksetup -listallhardwareports",
                    error = %e,
                    "查询 WiFi 硬件端口的命令执行失败，视为无 WiFi"
                );
                return Ok(None);
            }
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
            Err(e) => {
                tracing::debug!(
                    command = "networksetup -getairportnetwork",
                    device = %device,
                    error = %e,
                    "查询当前 WiFi 网络的命令执行失败，视为无 WiFi"
                );
                Ok(None)
            }
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
        // 接口标题行: "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> ..."。
        // 序号可为多位数字（接口数 ≥10 时出现 "10:"、"255:"），原先
        // strip_prefix(is_ascii_digit) 只剥一个数字，"10: eth0:" 无法识别且
        // 其 inet 行被并入上一个接口（G10）。改为按首个冒号切分，校验冒号
        // 前段非空且全为数字才认定是标题行。
        let header_rest = line
            .split_once(':')
            .filter(|(idx, _)| !idx.is_empty() && idx.bytes().all(|b| b.is_ascii_digit()));
        if let Some((_, rest)) = header_rest {
            // 提取接口名（去掉第二个冒号后的标志位与 @ifindex 后缀）
            let name = rest
                .split(':')
                .next()
                .unwrap_or("")
                .split('@')
                .next()
                .unwrap_or("")
                .trim();
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

/// 根据编译目标创建对应平台的检测器（外层套 30s TTL 缓存）
pub fn create_detector() -> std::sync::Arc<dyn NetworkDetect> {
    #[cfg(target_os = "windows")]
    {
        std::sync::Arc::new(CachingDetector::new(std::sync::Arc::new(WindowsDetect)))
    }
    #[cfg(target_os = "linux")]
    {
        std::sync::Arc::new(CachingDetector::new(std::sync::Arc::new(LinuxDetect)))
    }
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(CachingDetector::new(std::sync::Arc::new(MacosDetect)))
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

/// 网卡枚举缓存 TTL（秒）：避免每次探测周期 / Web 请求都新起 ipconfig/netsh/route 子进程
const DETECT_CACHE_TTL_SECS: u64 = 30;

/// 单项缓存条目
struct Cached<V> {
    fetched_at: std::time::Instant,
    value: V,
}

/// 带 30s TTL 的网络检测器包装（7.3）
///
/// `list_interfaces` / `default_gateways` / `current_ssid` 各自缓存，TTL 内直接返回缓存，
/// 避免 monitor 每探测周期 + 每个 Web 请求（list_network_interfaces / detect）都重新 spawn
/// ipconfig、netsh、route 子进程（各 10s 超时，开销大）。
pub(crate) struct CachingDetector {
    inner: std::sync::Arc<dyn NetworkDetect>,
    interfaces: std::sync::Mutex<Option<Cached<Vec<InterfaceInfo>>>>,
    gateways: std::sync::Mutex<Option<Cached<Vec<Ipv4Addr>>>>,
    ssid: std::sync::Mutex<Option<Cached<Option<String>>>>,
}

impl CachingDetector {
    fn new(inner: std::sync::Arc<dyn NetworkDetect>) -> Self {
        Self {
            inner,
            interfaces: std::sync::Mutex::new(None),
            gateways: std::sync::Mutex::new(None),
            ssid: std::sync::Mutex::new(None),
        }
    }

    /// 读缓存：未过期则返回 Some（克隆值），否则返回 None（触发重新探测）
    fn fresh<T: Clone>(entry: &std::sync::Mutex<Option<Cached<T>>>) -> Option<T> {
        let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .filter(|c| c.fetched_at.elapsed().as_secs() < DETECT_CACHE_TTL_SECS)
            .map(|c| c.value.clone())
    }

    /// 写缓存
    fn store<T>(target: &std::sync::Mutex<Option<Cached<T>>>, value: T) {
        let mut guard = target.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(Cached {
            fetched_at: std::time::Instant::now(),
            value,
        });
    }
}

#[async_trait]
impl NetworkDetect for CachingDetector {
    async fn list_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkError> {
        if let Some(v) = Self::fresh(&self.interfaces) {
            return Ok(v);
        }
        let value = self.inner.list_interfaces().await?;
        Self::store(&self.interfaces, value.clone());
        Ok(value)
    }

    async fn default_gateways(&self) -> Result<Vec<Ipv4Addr>, NetworkError> {
        if let Some(v) = Self::fresh(&self.gateways) {
            return Ok(v);
        }
        let value = self.inner.default_gateways().await?;
        Self::store(&self.gateways, value.clone());
        Ok(value)
    }

    async fn current_ssid(&self) -> Result<Option<String>, NetworkError> {
        if let Some(v) = Self::fresh(&self.ssid) {
            return Ok(v);
        }
        let value = self.inner.current_ssid().await?;
        Self::store(&self.ssid, value.clone());
        Ok(value)
    }
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
