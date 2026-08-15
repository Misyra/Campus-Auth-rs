//! 网络接口：网卡检测与出口绑定抽象
//!
//! 网卡枚举（[`detect`]）用于 Profile 自动切换与网关/SSID 检测；
//! 出口绑定（[`EgressBinder`]）为预留接口，当前版本不实现网卡绑定。

pub mod detect;
pub mod interfaces;

pub use detect::{
    create_detector, InterfaceInfo, LinuxDetect, MacosDetect, NetworkDetect, NetworkError,
    UnsupportedDetect, WindowsDetect,
};
pub use interfaces::{
    filter_interfaces, is_excluded, virtual_if_patterns, VIRTUAL_IF_PATTERNS_LINUX,
    VIRTUAL_IF_PATTERNS_MACOS, VIRTUAL_IF_PATTERNS_WINDOWS,
};

/// 出口网卡绑定抽象（预留接口）。
///
/// 当前版本不实现网卡绑定：浏览器与探测流量一律走系统默认路由。
/// 未来如需「绑定网卡」（让流量走指定网卡，例如多网卡分流），实现此 trait
/// 并在 Engine 中接入即可；配置字段 [`crate::config::MonitorSettings::bind_interface_name`]
/// 已预留，届时直接读取生效。
pub trait EgressBinder: Send + Sync {
    /// 返回绑定的出口 IP 地址；`None` 表示不绑定、走默认路由。
    fn bind_addr(&self) -> Option<std::net::IpAddr>;
}
