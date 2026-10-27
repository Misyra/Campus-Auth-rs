//! 网络接口：网卡检测
//!
//! 网卡枚举（[`detect`]）用于 Profile 自动切换与网关/SSID 检测；
//! 登录、探测与 DNS 共用按方案选择的出口，指定网卡不可用时禁止回退。

pub mod adapters;
pub mod egress;
pub mod settings;

pub mod detect;
pub mod interfaces;

pub use detect::{
    InterfaceInfo, LinuxDetect, MacosDetect, NetworkDetect, NetworkError, UnsupportedDetect,
    WindowsDetect, create_detector,
};
pub use interfaces::{
    LocalAddress, VIRTUAL_IF_PATTERNS_LINUX, VIRTUAL_IF_PATTERNS_MACOS,
    VIRTUAL_IF_PATTERNS_WINDOWS, filter_interfaces, is_excluded, local_address_from,
    select_primary_interface, virtual_if_patterns,
};
