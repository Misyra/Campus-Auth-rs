//! 网络接口：网卡检测
//!
//! 网卡枚举（[`detect`]）用于 Profile 自动切换与网关/SSID 检测；
//! 浏览器与探测流量一律走系统默认路由（不做网卡绑定）。

pub mod detect;
pub mod interfaces;

pub use detect::{
    InterfaceInfo, LinuxDetect, MacosDetect, NetworkDetect, NetworkError, UnsupportedDetect,
    WindowsDetect, create_detector,
};
pub use interfaces::{
    VIRTUAL_IF_PATTERNS_LINUX, VIRTUAL_IF_PATTERNS_MACOS, VIRTUAL_IF_PATTERNS_WINDOWS,
    filter_interfaces, is_excluded, virtual_if_patterns,
};
