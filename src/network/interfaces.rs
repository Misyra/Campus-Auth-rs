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

/// 本机地址快照：直连脚本 `ctx.local_ip` / `ctx.local_mac` 的数据源。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalAddress {
    /// 主用接口的 IPv4 地址（字符串形式，无可用接口时为空）
    pub ipv4: String,
    /// 主用接口的 MAC 地址（规范化小写冒号分隔；接口无物理地址时为空）
    pub mac: String,
}

/// 从网卡列表中挑选「主用接口」，作为本机地址的来源。
///
/// 优先级：有默认网关的接口（即真正承载默认路由的那块）→ 非 WiFi 有线接口
/// → 列表首个。校园网登录场景下必须优先取带网关的接口：多网卡机器（虚拟机、
/// Docker、VPN 并存）里 `list_interfaces` 的首个元素常是内部虚拟网段，取它会让
/// 脚本算出的密钥与门户看到的来源 IP 不一致，且症状是「认证失败」而非报错，
/// 极难排查。
pub fn select_primary_interface(interfaces: &[InterfaceInfo]) -> Option<&InterfaceInfo> {
    interfaces
        .iter()
        .find(|i| i.gateway.is_some())
        .or_else(|| interfaces.iter().find(|i| !i.is_wifi))
        .or_else(|| interfaces.first())
}

/// 从网卡列表生成本机地址快照（无可选接口时返回默认空值）。
pub fn local_address_from(interfaces: &[InterfaceInfo]) -> LocalAddress {
    match select_primary_interface(interfaces) {
        Some(i) => LocalAddress {
            ipv4: i.ipv4.to_string(),
            mac: i.mac.clone().unwrap_or_default(),
        },
        None => LocalAddress::default(),
    }
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
                mac: Some("00:1a:2b:3c:4d:5e".into()),
            },
            InterfaceInfo {
                name: "未指定".into(),
                ipv4: Ipv4Addr::UNSPECIFIED,
                gateway: None,
                is_wifi: false,
                ssid: None,
                mac: None,
            },
            InterfaceInfo {
                name: "链路本地".into(),
                ipv4: Ipv4Addr::new(169, 254, 1, 2),
                gateway: None,
                is_wifi: false,
                ssid: None,
                mac: None,
            },
        ];
        let filtered = filter_interfaces(interfaces);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "以太网");
        assert_eq!(filtered[0].mac.as_deref(), Some("00:1a:2b:3c:4d:5e"));
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
            mac: None,
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
            mac: None,
        }];
        assert!(filter_interfaces(interfaces).is_empty());
    }

    // ============ select_primary_interface / local_address_from ============

    fn iface(
        name: &str,
        ip: [u8; 4],
        gateway: Option<[u8; 4]>,
        mac: Option<&str>,
    ) -> InterfaceInfo {
        InterfaceInfo {
            name: name.into(),
            ipv4: Ipv4Addr::from(ip),
            gateway: gateway.map(Ipv4Addr::from),
            is_wifi: name.starts_with("wl"),
            ssid: None,
            mac: mac.map(str::to_string),
        }
    }

    #[test]
    fn test_select_primary_prefers_gateway_over_first() {
        // 多网卡：首个是无网关的虚拟段，第二个才有默认网关 → 必须选后者。
        // 选错会让脚本用错来源 IP 算密钥，症状是「认证失败」而非报错。
        let list = vec![
            iface("veth0", [172, 17, 0, 1], None, Some("02:42:ac:11:00:02")),
            iface(
                "以太网",
                [192, 168, 1, 100],
                Some([192, 168, 1, 1]),
                Some("00:1a:2b:3c:4d:5e"),
            ),
        ];
        let picked = select_primary_interface(&list).unwrap();
        assert_eq!(picked.name, "以太网");
    }

    #[test]
    fn test_select_primary_falls_back_to_wired_when_no_gateway() {
        // 全部无网关时优先有线（非 WiFi），避免选到无线
        let list = vec![
            iface("wlan0", [192, 168, 5, 20], None, None),
            iface("eth0", [10, 0, 0, 5], None, None),
        ];
        assert_eq!(select_primary_interface(&list).unwrap().name, "eth0");
    }

    #[test]
    fn test_select_primary_uses_first_as_last_resort() {
        // 只剩 WiFi 时退化为取首个，而非返回 None（有网卡就该给出地址）
        let list = vec![iface("wlan0", [192, 168, 5, 20], None, None)];
        assert_eq!(select_primary_interface(&list).unwrap().name, "wlan0");
    }

    #[test]
    fn test_local_address_from_empty_is_blank() {
        let addr = local_address_from(&[]);
        assert_eq!(addr, LocalAddress::default());
        assert!(addr.ipv4.is_empty() && addr.mac.is_empty());
    }

    #[test]
    fn test_local_address_from_takes_mac_of_primary() {
        let list = vec![
            iface("veth0", [172, 17, 0, 1], None, Some("02:42:ac:11:00:02")),
            iface(
                "以太网",
                [192, 168, 1, 100],
                Some([192, 168, 1, 1]),
                Some("00:1a:2b:3c:4d:5e"),
            ),
        ];
        let addr = local_address_from(&list);
        assert_eq!(addr.ipv4, "192.168.1.100");
        assert_eq!(addr.mac, "00:1a:2b:3c:4d:5e");
    }

    #[test]
    fn test_local_address_from_mac_missing_is_blank_not_panic() {
        // 接口无 MAC（虚拟/隧道适配器）时该字段为空串，IP 仍应正常给出
        let list = vec![iface("eth0", [10, 0, 0, 5], Some([10, 0, 0, 1]), None)];
        let addr = local_address_from(&list);
        assert_eq!(addr.ipv4, "10.0.0.5");
        assert!(addr.mac.is_empty());
    }
}
