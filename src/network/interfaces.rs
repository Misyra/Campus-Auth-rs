//! InterfaceInfo 模型 + 网卡枚举 + 过滤

use crate::network::detect::InterfaceInfo;

/// Windows 虚拟网卡特征模式（全小写，用于 case-insensitive contains 匹配）
///
/// 单一权威特征表（A3）：原 detect.rs 的 `is_virtual_interface`（7 条）已删除，
/// 差异特征（npcap / docker / 隧道）并入本表；并补充 tap / tun / wireguard /
/// clash 等常见 VPN 与虚拟化接口模式。
pub const VIRTUAL_IF_PATTERNS_WINDOWS: &[&str] = &[
    "vethernet",  // Hyper-V Virtual Ethernet
    "hyper-v",    // Hyper-V Virtual Ethernet Adapter
    "vmware",     // VMware Network Adapter
    "virtualbox", // VirtualBox Host-Only Network
    "loopback",   // Loopback Pseudo-Interface
    "virtual",    // Virtual Ethernet / Virtual Adapter
    "pseudo",     // Pseudo-Interface
    "tunnel",     // Tunnel / Tunneling
    "miniport",   // WAN Miniport
    "teredo",     // Teredo Tunneling
    "npcap",      // Npcap Loopback Adapter（自 detect.rs 并入）
    "docker",     // DockerNAT / Docker Virtual NIC（自 detect.rs 并入）
    "隧道",       // 中文"隧道适配器"（自 detect.rs 并入）
    "tap",        // TAP-Windows Adapter（OpenVPN 等）
    "tun",        // Wintun（WireGuard 系隧道）
    "wireguard",  // WireGuard Tunnel
    "clash",      // Clash / Clash Verge 虚拟网卡
];
/// Linux 虚拟网卡特征模式（补充 wireguard / clash 系隧道，A3）
pub const VIRTUAL_IF_PATTERNS_LINUX: &[&str] = &[
    "docker",
    "veth",
    "br-",
    "virbr",
    "tun",
    "tap",
    "bond",
    "dummy",
    "vmnet",
    "vboxnet",
    "wireguard",
    "wg",
    "clash",
];
/// macOS 虚拟网卡特征模式（补充 wireguard / clash 系隧道，A3；utun/awdl 由
/// detect.rs 的 `is_macos_virtual` 前缀判定覆盖，此处为兜底过滤补充）
pub const VIRTUAL_IF_PATTERNS_MACOS: &[&str] =
    &["bridge", "vboxnet", "vmnet", "wireguard", "clash"];

/// 返回当前平台的虚拟网卡特征模式
pub fn virtual_if_patterns() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        VIRTUAL_IF_PATTERNS_WINDOWS
    }
    #[cfg(target_os = "linux")]
    {
        VIRTUAL_IF_PATTERNS_LINUX
    }
    #[cfg(target_os = "macos")]
    {
        VIRTUAL_IF_PATTERNS_MACOS
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        &[]
    }
}

/// 判断接口名是否属于虚拟网卡（应被排除）
/// 大小写不敏感的 contains 匹配
pub fn is_excluded(name: &str) -> bool {
    let lower = name.to_lowercase();
    virtual_if_patterns().iter().any(|p| lower.contains(p))
}

/// 过滤掉虚拟网卡、loopback、链路本地、未指定地址等无效接口
///
/// 作为各平台 `list_interfaces` 解析后的兜底过滤：解析器已过滤虚拟/回环，
/// 此处再排除 `0.0.0.0`（未指定）与 `169.254.x.x`（链路本地，无真实连通性）。
pub fn filter_interfaces(interfaces: Vec<InterfaceInfo>) -> Vec<InterfaceInfo> {
    interfaces
        .into_iter()
        .filter(|i| {
            !is_excluded(&i.name)
                && !i.ipv4.is_unspecified() // 0.0.0.0
                && !i.ipv4.is_loopback() // 127.0.0.0/8
                && !i.ipv4.is_link_local() // 169.254.0.0/16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_filter_excludes_unspecified_and_link_local() {
        // 兜底过滤应排除 0.0.0.0 与链路本地地址，保留真实 IPv4
        let interfaces = vec![
            InterfaceInfo {
                name: "以太网".into(),
                ipv4: Ipv4Addr::new(192, 168, 1, 100),
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                is_wifi: false,
                ssid: None,
            },
            InterfaceInfo {
                name: "未指定".into(),
                ipv4: Ipv4Addr::UNSPECIFIED,
                gateway: None,
                is_wifi: false,
                ssid: None,
            },
            InterfaceInfo {
                name: "链路本地".into(),
                ipv4: Ipv4Addr::new(169, 254, 1, 2),
                gateway: None,
                is_wifi: false,
                ssid: None,
            },
        ];
        let filtered = filter_interfaces(interfaces);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "以太网");
    }

    #[test]
    fn test_filter_excludes_loopback() {
        // "lo" 经 filter_interfaces 的 is_loopback 兜底排除，跨平台一致
        let interfaces = vec![InterfaceInfo {
            name: "lo".into(),
            ipv4: Ipv4Addr::LOCALHOST,
            gateway: None,
            is_wifi: false,
            ssid: None,
        }];
        assert!(filter_interfaces(interfaces).is_empty());
    }

    // "vEthernet" 仅存在于 Windows 特征表（Hyper-V），仅 Windows 编译
    #[cfg(windows)]
    #[test]
    fn test_filter_excludes_windows_virtual_adapter() {
        let interfaces = vec![InterfaceInfo {
            name: "vEthernet (Default Switch)".into(),
            ipv4: Ipv4Addr::new(172, 16, 0, 1),
            gateway: None,
            is_wifi: false,
            ssid: None,
        }];
        assert!(filter_interfaces(interfaces).is_empty());
    }
}
