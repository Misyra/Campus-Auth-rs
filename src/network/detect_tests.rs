use super::*;
use crate::network::interfaces::is_excluded;

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

// ============ 虚拟网卡特征表（A3：统一走 interfaces::is_excluded） ============
//
// is_excluded 按平台返回各自的虚拟网卡特征表（interfaces::virtual_if_patterns），
// 以下用 Windows 专属适配器名（VMware/Npcap/Wintun 等）的用例仅在 Windows 编译；
// linux / macOS 的特征表断言见下方 test_is_linux_virtual_* / test_is_macos_virtual_*。

#[cfg(windows)]
#[test]
fn test_is_virtual_vmware() {
    assert!(is_excluded("VMware Network Adapter"));
    assert!(is_excluded("vmware8"));
}

#[cfg(windows)]
#[test]
fn test_is_virtual_vbox() {
    assert!(is_excluded("VirtualBox Host-Only Ethernet Adapter"));
}

#[cfg(windows)]
#[test]
fn test_is_virtual_docker() {
    assert!(is_excluded("DockerNAT"));
    assert!(is_excluded("docker0"));
}

#[cfg(windows)]
#[test]
fn test_is_virtual_tunnel() {
    assert!(is_excluded("隧道适配器 Tunnel"));
    assert!(is_excluded("Tunnel Adapter"));
}

#[cfg(windows)]
#[test]
fn test_is_virtual_npcap() {
    assert!(is_excluded("Npcap Loopback Adapter"));
}

#[cfg(windows)]
#[test]
fn test_is_virtual_vpn_tun_tap_wireguard_clash() {
    // A3 补充的常见 VPN / 隧道接口模式
    assert!(is_excluded("TAP-Windows Adapter V9"));
    assert!(is_excluded("Wintun Userspace Tunnel"));
    assert!(is_excluded("WireGuard Tunnel"));
    assert!(is_excluded("Clash Verge Virtual NIC"));
}

#[test]
fn test_is_not_virtual_normal_interface() {
    assert!(!is_excluded("以太网"));
    assert!(!is_excluded("WLAN"));
    assert!(!is_excluded("Wi-Fi"));
    assert!(!is_excluded("Ethernet"));
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

// 虚拟适配器过滤依赖 Windows 特征表（Npcap 等），仅 Windows 编译
#[cfg(windows)]
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
    assert!(
        decoded.contains("IPv4 地址"),
        "decode 应正确解码 'IPv4 地址'"
    );
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

// 实际调用 Windows 的 ipconfig，unix 上无此命令，仅 Windows 编译
#[cfg(windows)]
#[tokio::test]
async fn test_run_ipconfig_actual() {
    // 实际调用 ipconfig /all 验证 run_command 是否正常工作
    let result = run_command("ipconfig", &["/all"]).await;
    match result {
        Ok(out) => {
            eprintln!("ipconfig output length: {}", out.len());
            eprintln!(
                "First 200 chars: {:?}",
                out.chars().take(200).collect::<String>()
            );
            assert!(!out.is_empty(), "ipconfig 输出不应为空");
            // 中英文 Windows 均包含冒号（用于键值分隔，如 "IPv4 地址 : x.x.x.x"）
            assert!(out.contains(':'), "输出应包含冒号（键值分隔符）");
            let interfaces = parse_ipconfig(&out);
            eprintln!("Parsed {} interfaces", interfaces.len());
            assert!(
                !interfaces.is_empty(),
                "应至少解析出一个网络接口, 输出前500字符: {:?}",
                out.chars().take(500).collect::<String>()
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

#[test]
fn test_parse_ip_addr_multidigit_interface_index() {
    // G10：接口序号 ≥10（"10:" / "255:"）应正确开新块，inet 行不得并入上一接口。
    // 原 strip_prefix(is_ascii_digit) 只剥一个数字，"10: eth0:" 无法识别。
    let input = r#"1: eth1: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 state UP
inet 192.168.1.1/24 scope global eth1
9: eth9: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 state UP
inet 192.168.1.9/24 scope global eth9
10: eth10: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 state UP
inet 192.168.1.10/24 scope global eth10
255: eth255: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 state UP
inet 192.168.1.255/24 scope global eth255
"#;
    let interfaces = parse_ip_addr(input);
    assert_eq!(interfaces.len(), 4, "四个接口都应独立解析: {interfaces:?}");
    assert_eq!(interfaces[0].name, "eth1");
    assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(192, 168, 1, 1));
    assert_eq!(interfaces[1].name, "eth9");
    assert_eq!(interfaces[1].ipv4, Ipv4Addr::new(192, 168, 1, 9));
    assert_eq!(interfaces[2].name, "eth10");
    assert_eq!(interfaces[2].ipv4, Ipv4Addr::new(192, 168, 1, 10));
    assert_eq!(interfaces[3].name, "eth255");
    assert_eq!(interfaces[3].ipv4, Ipv4Addr::new(192, 168, 1, 255));
}

#[test]
fn test_parse_ip_addr_non_digit_prefix_not_header() {
    // 冒号前段非纯数字的行（如 IPv6 地址行）不应被误认为接口标题
    let input = r#"2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 state UP
inet 192.168.1.100/24 scope global eth0
inet6 2001:db8::1/64 scope global eth0
"#;
    let interfaces = parse_ip_addr(input);
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].name, "eth0");
    assert_eq!(interfaces[0].ipv4, Ipv4Addr::new(192, 168, 1, 100));
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

// ============ decode_netsh_ssid_hex 测试 ============

#[test]
fn test_decode_netsh_ssid_hex_utf8() {
    // "Ciallo～(∠・ω< )⌒★" 的 UTF-8 hex 转义（netsh 对非 ASCII SSID 的输出形态）
    let hex = "4369616C6C6FEFBD9E28E288A0E383BBCF893C2029E28C92E29885";
    assert_eq!(decode_netsh_ssid_hex(hex), "Ciallo～(∠・ω< )⌒★");
}

#[test]
fn test_decode_netsh_ssid_hex_plain_names_untouched() {
    // 解码结果全 ASCII（"ABCD"）→ 不满足"含非 ASCII"，保留原样
    assert_eq!(decode_netsh_ssid_hex("41424344"), "41424344");
    // 非 hex 字符 / 奇数长度 / 解码非法 UTF-8 → 保留原样
    assert_eq!(decode_netsh_ssid_hex("HomeWiFi"), "HomeWiFi");
    assert_eq!(decode_netsh_ssid_hex("abc"), "abc");
    assert_eq!(decode_netsh_ssid_hex("abcdef12"), "abcdef12");
    // 短于 8 的合法 hex 串不处理（避免误伤常见短名）
    assert_eq!(decode_netsh_ssid_hex("4142"), "4142");
}

#[test]
fn test_parse_netsh_ssid_applies_hex_decode() {
    let text = "    SSID                   : 4369616C6C6FEFBD9E28E288A0E383BBCF893C2029E28C92E29885\n    BSSID                 : aa:bb:cc:dd:ee:ff";
    assert_eq!(
        parse_netsh_ssid(text),
        Some("Ciallo～(∠・ω< )⌒★".to_string())
    );
}
