//! 可绑定出口的物理网卡清单与稳定标识

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// 可用于校园网出口的 IPv4 网卡。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct EgressAdapter {
    /// 稳定标识：Windows 使用接口 GUID，Unix 优先使用 MAC。
    pub id: String,
    /// 用户可识别的接口名称。
    pub name: String,
    /// 当前接口索引。
    pub index: u32,
    /// 当前 IPv4 地址。
    pub ipv4: Ipv4Addr,
    /// 当前物理 MAC，供门户凭据变换使用。
    #[serde(default)]
    pub mac: String,
    /// 当前接口配置的 IPv4 DNS。
    #[serde(default)]
    pub dns_servers: Vec<String>,
}

/// 枚举网卡；超时或读取失败明确返回错误，不回退到其他接口。
pub async fn list_adapters() -> std::io::Result<Vec<EgressAdapter>> {
    #[cfg(windows)]
    {
        let script = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = 'Stop'
$rows = @(Get-NetIPConfiguration -All | Where-Object {
    $_.NetAdapter.Status -eq 'Up' -and $_.NetAdapter.HardwareInterface
} | ForEach-Object {
    $config = $_
    foreach ($address in @($config.IPv4Address)) {
        if ($address.IPAddress) {
            [pscustomobject]@{
                id = $config.NetAdapter.InterfaceGuid.ToString()
                name = $config.InterfaceAlias
                index = [uint32]$config.InterfaceIndex
                ipv4 = $address.IPAddress
                mac = ($config.NetAdapter.MacAddress -replace '-', ':').ToLowerInvariant()
                dns_servers = @($config.DNSServer.ServerAddresses | Where-Object { $_ -match '^\d+\.\d+\.\d+\.\d+$' })
            }
        }
    }
})
ConvertTo-Json -InputObject $rows -Depth 4 -Compress
"#;
        let mut command = tokio::process::Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ]);
        command.creation_flags(0x08000000);
        command.kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(8), command.output())
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "网卡枚举超时"))??;
        if !output.status.success() {
            return Err(std::io::Error::other("无法读取物理网卡信息"));
        }
        let bytes = output
            .stdout
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&output.stdout);
        let mut list: Vec<EgressAdapter> =
            serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
        list.retain(|a| {
            !a.ipv4.is_unspecified() && !a.ipv4.is_loopback() && !a.ipv4.is_link_local()
        });
        Ok(list)
    }
    #[cfg(not(windows))]
    {
        let interfaces = super::create_detector()
            .list_interfaces()
            .await
            .map_err(std::io::Error::other)?;
        let mut adapters = Vec::new();
        for interface in interfaces {
            let name =
                std::ffi::CString::new(interface.name.clone()).map_err(std::io::Error::other)?;
            // 接口名来自系统枚举，CString 保证传入的字符串以 NUL 结束。
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            if index == 0 {
                continue;
            }
            adapters.push(EgressAdapter {
                id: interface
                    .mac
                    .clone()
                    .unwrap_or_else(|| interface.name.clone()),
                name: interface.name,
                index,
                ipv4: interface.ipv4,
                mac: interface.mac.unwrap_or_default(),
                dns_servers: Vec::new(),
            });
        }
        Ok(adapters)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// 只读系统网卡，需在真实 Windows 主机手动执行。
    #[tokio::test]
    #[ignore = "依赖宿主物理网卡与 PowerShell，仅手动冒烟"]
    async fn windows_adapter_inventory_smoke() {
        let adapters = list_adapters().await.unwrap();
        assert!(!adapters.is_empty(), "当前主机需要至少一块已连接的物理网卡");
        for adapter in adapters {
            assert!(!adapter.id.is_empty());
            assert!(adapter.index > 0);
            assert!(!adapter.name.is_empty());
        }
    }
}
