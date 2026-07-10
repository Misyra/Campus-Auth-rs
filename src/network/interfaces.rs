//! InterfaceInfo 模型 + 网卡枚举 + 过滤

use crate::network::detect::InterfaceInfo;

/// Windows 虚拟网卡特征模式（全小写，用于 case-insensitive contains 匹配）
pub const VIRTUAL_IF_PATTERNS_WINDOWS: &[&str] = &[
    "vethernet",    // Hyper-V Virtual Ethernet
    "hyper-v",      // Hyper-V Virtual Ethernet Adapter
    "vmware",       // VMware Network Adapter
    "virtualbox",   // VirtualBox Host-Only Network
    "loopback",     // Loopback Pseudo-Interface
    "virtual",      // Virtual Ethernet / Virtual Adapter
    "pseudo",       // Pseudo-Interface
    "tunnel",       // Tunnel / Tunneling
    "miniport",     // WAN Miniport
    "teredo",       // Teredo Tunneling
];
/// Linux 虚拟网卡特征模式
pub const VIRTUAL_IF_PATTERNS_LINUX: &[&str] =
    &["docker", "veth", "br-", "virbr", "tun", "tap", "bond", "dummy", "vmnet", "vboxnet"];
/// macOS 虚拟网卡特征模式
pub const VIRTUAL_IF_PATTERNS_MACOS: &[&str] = &["bridge", "vboxnet", "vmnet"];

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

/// 排序：有默认网关的接口优先，其余按名称排序
pub fn sort_interfaces(mut interfaces: Vec<InterfaceInfo>) -> Vec<InterfaceInfo> {
    use std::cmp::Ordering;
    interfaces.sort_by(|a, b| match (a.gateway.is_some(), b.gateway.is_some()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    interfaces
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
    fn test_filter_excludes_loopback_and_virtual() {
        let interfaces = vec![
            InterfaceInfo {
                name: "lo".into(),
                ipv4: Ipv4Addr::LOCALHOST,
                gateway: None,
                is_wifi: false,
                ssid: None,
            },
            InterfaceInfo {
                name: "vEthernet (Default Switch)".into(),
                ipv4: Ipv4Addr::new(172, 16, 0, 1),
                gateway: None,
                is_wifi: false,
                ssid: None,
            },
        ];
        assert!(filter_interfaces(interfaces).is_empty());
    }
}
