//! 网络接口：网卡检测与 SOCKS5 转发器

pub mod detect;
pub mod interfaces;
pub mod socks5;

pub use detect::{
    create_detector, GatewayInfo, InterfaceInfo, LinuxDetect, MacosDetect, NetworkDetect,
    NetworkError, UnsupportedDetect, WindowsDetect,
};
pub use interfaces::{
    filter_interfaces, is_excluded, sort_interfaces, virtual_if_patterns,
    VIRTUAL_IF_PATTERNS_LINUX, VIRTUAL_IF_PATTERNS_MACOS, VIRTUAL_IF_PATTERNS_WINDOWS,
};
pub use socks5::{
    spawn_socks_guard, SocksForwarder, SocksGuard, DEFAULT_SOCKS5_PORT, SOCKS5_BIND_ADDR,
    SOCKS5_PORT_RETRY_MAX,
};
