//! 按方案保存的统一校园网出口与 DNS 策略

use serde::{Deserialize, Serialize};

/// 校园网登录、检测和 DNS 的出口模式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// 忽略系统代理，使用系统路由。
    #[default]
    Direct,
    /// 沿用系统代理及路由。
    System,
    /// 使用明确指定的 HTTP 或 SOCKS5 代理。
    Proxy,
    /// 使用指定物理网卡及该网卡的 DNS。
    Interface,
}

/// 校园网流量的唯一出口配置；不影响更新下载等公网业务。
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkSettings {
    /// 出口模式。
    pub mode: NetworkMode,
    /// 物理网卡稳定标识；不保存会随 DHCP 变化的源 IP。
    pub interface_id: String,
    /// HTTP / SOCKS5 代理 URL，HTTP 允许 Basic 认证。
    pub proxy_url: String,
    /// 指定网卡的 IPv4 DNS；为空时读取该网卡的 DNS，可用 IP:端口。
    pub dns_servers: Vec<String>,
}

impl std::fmt::Debug for NetworkSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkSettings")
            .field("mode", &self.mode)
            .field("interface_id", &self.interface_id)
            .field(
                "proxy_url",
                &if self.proxy_url.is_empty() {
                    ""
                } else {
                    "[REDACTED]"
                },
            )
            .field("dns_servers", &self.dns_servers)
            .finish()
    }
}

impl NetworkSettings {
    /// 在一个位置兼容旧的浏览器与检测代理字段。
    pub fn effective(
        profile: &crate::config::ProfileData,
        global: &crate::config::GlobalConfig,
    ) -> Self {
        profile.network.clone().unwrap_or_else(|| {
            if !global.browser.bind_proxy.trim().is_empty() {
                Self {
                    mode: NetworkMode::Proxy,
                    proxy_url: global.browser.bind_proxy.clone(),
                    ..Self::default()
                }
            } else if !global.monitor.disable_proxy {
                Self {
                    mode: NetworkMode::System,
                    ..Self::default()
                }
            } else {
                Self::default()
            }
        })
    }

    /// 保存和执行共用的配置校验；不可用的网卡在执行时明确报错。
    pub fn validate(&self) -> Result<(), String> {
        if self.interface_id.len() > 256 || self.proxy_url.len() > 2048 {
            return Err("网络出口配置过长".into());
        }
        if self.mode == NetworkMode::Interface && self.interface_id.trim().is_empty() {
            return Err("请选择校园网出口网卡".into());
        }
        if self.mode == NetworkMode::Proxy {
            let url = url::Url::parse(self.proxy_url.trim()).map_err(|_| "代理地址格式无效")?;
            if !matches!(url.scheme(), "http" | "socks5" | "socks5h")
                || url.host_str().is_none()
                || url.port_or_known_default().is_none()
            {
                return Err("校园网代理须为 http:// 或 socks5:// 或 socks5h://主机:端口".into());
            }
        }
        if self.dns_servers.len() > 4 {
            return Err("最多配置 4 个校园网 DNS".into());
        }
        for value in &self.dns_servers {
            super::egress::parse_dns_server(value)
                .map_err(|_| "校园网 DNS 须为 IPv4 地址或 IPv4:端口")?;
        }
        if self.mode != NetworkMode::Interface && !self.dns_servers.is_empty() {
            return Err("指定校园网 DNS 需同时选择出口网卡".into());
        }
        Ok(())
    }
}
