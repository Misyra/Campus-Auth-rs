//! 平台特定代码：自启动、WiFi SSID、默认网关、Shell 检测、平台信息
//!
//! 各平台原生实现，通过 `#[cfg]` 条件编译隔离，避免引入抽象层：
//! - Windows：schtasks / netsh / route / where
//! - macOS：LaunchAgent plist / networksetup / route / which
//! - Linux：XDG autostart desktop / iwgetid / ip route / which
//! - 其它平台：返回 `Unsupported` 错误

use std::path::PathBuf;

/// Shell 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// Windows cmd.exe
    Cmd,
    /// Windows PowerShell (5.x)
    PowerShell,
    /// PowerShell Core (7+)
    Pwsh,
    /// Bourne Again Shell
    Bash,
    /// POSIX Shell
    Sh,
    /// Z Shell
    Zsh,
}

/// Shell 信息
#[derive(Debug, Clone)]
pub struct ShellInfo {
    /// Shell 名称（如 "cmd"、"powershell"）
    pub name: String,
    /// Shell 可执行文件路径
    pub path: PathBuf,
    /// Shell 类型
    pub kind: ShellKind,
}

/// 平台信息
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    /// 操作系统标识（如 "windows"、"linux"）
    pub os: String,
    /// CPU 架构标识（如 "x86_64"、"aarch64"）
    pub arch: String,
    /// 系统默认 Shell
    pub default_shell: ShellInfo,
}

#[cfg(target_os = "windows")]
mod imp {
    use anyhow::{bail, Result};
    use std::process::Command;

    /// 注册/取消系统自启动（Windows：schtasks 计划任务）
    pub fn set_self_start(enabled: bool) -> Result<()> {
        if enabled {
            let exe = std::env::current_exe()?;
            let status = Command::new("schtasks")
                .args([
                    "/create",
                    "/tn",
                    "Campus-Auth",
                    "/sc",
                    "ONLOGON",
                    "/rl",
                    "LIMITED",
                    "/tr",
                    &format!("\"{}\"", exe.display()),
                    "/f",
                ])
                .status()?;
            if !status.success() {
                bail!("注册自启动失败（schtasks 返回非零退出码）");
            }
        } else {
            let status = Command::new("schtasks")
                .args(["/delete", "/tn", "Campus-Auth", "/f"])
                .status()?;
            if !status.success() {
                bail!("取消自启动失败（schtasks 返回非零退出码）");
            }
        }
        Ok(())
    }

    /// 获取当前 WiFi SSID（netsh wlan show interfaces 解析）
    pub fn get_wifi_ssid() -> Result<String> {
        let out = Command::new("netsh")
            .args(["wlan", "show", "interfaces"])
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("SSID") {
                // 确保匹配的是 SSID 字段，而非 "SSID 广播" 等行
                if rest.starts_with(':') || rest.starts_with(' ') || rest.starts_with('\t') {
                    if let Some(idx) = rest.find(':') {
                        let ssid = rest[idx + 1..].trim();
                        if !ssid.is_empty() {
                            return Ok(ssid.to_string());
                        }
                    }
                }
            }
        }
        bail!("无法获取 WiFi SSID");
    }

    /// 获取默认网关 IP（route print 解析 0.0.0.0 行）
    pub fn get_default_gateway() -> Result<String> {
        let out = Command::new("route").args(["print"]).output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 3 && cols[0] == "0.0.0.0" && !cols[0].contains('/') {
                return Ok(cols[2].to_string());
            }
        }
        bail!("无法获取默认网关");
    }

    /// 通过 `where` 命令查找可执行文件路径
    fn find_executable(name: &str) -> Option<std::path::PathBuf> {
        let out = Command::new("where").arg(name).output().ok()?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let first = text.lines().next()?.trim();
            if first.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(first))
            }
        } else {
            None
        }
    }

    /// 检测系统可用的 shell 列表
    ///
    /// Windows 上检测 cmd、powershell、pwsh 三种 shell。
    pub fn detect_shells() -> Vec<super::ShellInfo> {
        use super::{ShellInfo, ShellKind};
        let mut shells = Vec::new();

        // cmd 总是可用
        shells.push(ShellInfo {
            name: "cmd".to_string(),
            path: std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            kind: ShellKind::Cmd,
        });

        // 检测 Windows PowerShell
        if let Some(path) = find_executable("powershell") {
            shells.push(ShellInfo {
                name: "powershell".to_string(),
                path,
                kind: ShellKind::PowerShell,
            });
        }

        // 检测 PowerShell Core
        if let Some(path) = find_executable("pwsh") {
            shells.push(ShellInfo {
                name: "pwsh".to_string(),
                path,
                kind: ShellKind::Pwsh,
            });
        }

        shells
    }

    /// 返回当前平台信息
    pub fn current_platform() -> super::PlatformInfo {
        use super::PlatformInfo;
        let default_shell = detect_shells()
            .into_iter()
            .next()
            .expect("至少应有 cmd 可用");
        PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            default_shell,
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{bail, Result};
    use std::path::PathBuf;
    use std::process::Command;

    /// LaunchAgent 标识（同时也是 plist 文件名前缀）
    const LAUNCH_AGENT_LABEL: &str = "com.misyra.Campus-Auth";

    /// 注册/取消系统自启动（macOS：~/Library/LaunchAgents/ 下的 LaunchAgent plist）
    ///
    /// 写入 plist 后由 launchd 在用户登录时自动加载，语义等价于 Windows schtasks 的 ONLOGON。
    pub fn set_self_start(enabled: bool) -> Result<()> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法确定用户主目录"))?;
        let dir = home.join("Library/LaunchAgents");
        let plist_path = dir.join(format!("{}.plist", LAUNCH_AGENT_LABEL));

        if enabled {
            let exe = std::env::current_exe()?;
            // plist 字符串做最小 XML 转义，避免路径中的特殊字符破坏文档
            let exe_escaped = exe
                .display()
                .to_string()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let plist = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\">\n\
                 <dict>\n\
                 \t<key>Label</key>\n\
                 \t<string>{label}</string>\n\
                 \t<key>ProgramArguments</key>\n\
                 \t<array>\n\
                 \t\t<string>{exe}</string>\n\
                 \t</array>\n\
                 \t<key>RunAtLoad</key>\n\
                 \t<true/>\n\
                 </dict>\n\
                 </plist>\n",
                label = LAUNCH_AGENT_LABEL,
                exe = exe_escaped,
            );
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&plist_path, plist)?;
        } else if plist_path.exists() {
            std::fs::remove_file(&plist_path)?;
        }
        Ok(())
    }

    /// 获取当前 WiFi SSID（networksetup -getairportnetwork en0 解析）
    pub fn get_wifi_ssid() -> Result<String> {
        let out = Command::new("networksetup")
            .args(["-getairportnetwork", "en0"])
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.trim();
        // 输出形如 "Current Wi-Fi Network: <SSID>"，旧版 macOS 可能写作 "Current AirPort Network:"
        let ssid = line
            .strip_prefix("Current Wi-Fi Network:")
            .or_else(|| line.strip_prefix("Current AirPort Network:"))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match ssid {
            Some(s) => Ok(s.to_string()),
            None => bail!("无法获取 WiFi SSID"),
        }
    }

    /// 获取默认网关 IP（route -n get default 解析 gateway 行）
    pub fn get_default_gateway() -> Result<String> {
        let out = Command::new("route")
            .args(["-n", "get", "default"])
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("gateway:") {
                let gw = rest.trim();
                if !gw.is_empty() {
                    return Ok(gw.to_string());
                }
            }
        }
        bail!("无法获取默认网关");
    }

    /// 通过 `which` crate 查找可执行文件路径
    fn find_executable(name: &str) -> Option<PathBuf> {
        which::which(name).ok()
    }

    /// 检测系统可用的 shell 列表（macOS：zsh 默认，其次 bash、sh）
    pub fn detect_shells() -> Vec<super::ShellInfo> {
        use super::{ShellInfo, ShellKind};
        let mut shells = Vec::new();

        if let Some(path) = find_executable("zsh") {
            shells.push(ShellInfo {
                name: "zsh".to_string(),
                path,
                kind: ShellKind::Zsh,
            });
        }

        if let Some(path) = find_executable("bash") {
            shells.push(ShellInfo {
                name: "bash".to_string(),
                path,
                kind: ShellKind::Bash,
            });
        }

        // sh 总是可用
        shells.push(ShellInfo {
            name: "sh".to_string(),
            path: PathBuf::from("/bin/sh"),
            kind: ShellKind::Sh,
        });

        shells
    }

    /// 返回当前平台信息
    pub fn current_platform() -> super::PlatformInfo {
        use super::PlatformInfo;
        let default_shell = detect_shells()
            .into_iter()
            .next()
            .expect("至少应有 sh 可用");
        PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            default_shell,
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{bail, Result};
    use std::path::PathBuf;
    use std::process::Command;

    /// XDG autostart desktop 文件名
    const AUTOSTART_DESKTOP_FILE: &str = "campus-auth.desktop";
    /// 桌面入口显示名称
    const DESKTOP_ENTRY_NAME: &str = "Campus-Auth";

    /// 注册/取消系统自启动（Linux：~/.config/autostart/*.desktop）
    ///
    /// 写入 XDG autostart desktop 文件后由桌面环境在登录时自动启动，
    /// 语义等价于 Windows schtasks 的 ONLOGON。无桌面环境时该文件不生效。
    pub fn set_self_start(enabled: bool) -> Result<()> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("无法确定用户配置目录（XDG_CONFIG_HOME）"))?;
        let autostart_dir = config_dir.join("autostart");
        let desktop_path = autostart_dir.join(AUTOSTART_DESKTOP_FILE);

        if enabled {
            let exe = std::env::current_exe()?;
            // desktop 文件 Exec 行需转义空格，否则会被当作参数分隔符
            let exec_value = exe.display().to_string().replace(' ', "\\ ");
            let content = format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name={name}\n\
                 Exec={exec}\n\
                 Terminal=false\n\
                 X-GNOME-Autostart-enabled=true\n",
                name = DESKTOP_ENTRY_NAME,
                exec = exec_value,
            );
            std::fs::create_dir_all(&autostart_dir)?;
            std::fs::write(&desktop_path, content)?;
        } else if desktop_path.exists() {
            std::fs::remove_file(&desktop_path)?;
        }
        Ok(())
    }

    /// 获取当前 WiFi SSID（iwgetid -r 返回当前无线网络 ESSID）
    pub fn get_wifi_ssid() -> Result<String> {
        let out = Command::new("iwgetid").arg("-r").output()?;
        if !out.status.success() {
            bail!("无法获取 WiFi SSID（iwgetid 返回非零退出码）");
        }
        let ssid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if ssid.is_empty() {
            bail!("无法获取 WiFi SSID");
        }
        Ok(ssid)
    }

    /// 获取默认网关 IP（ip route 解析 default 行，回退 route -n）
    pub fn get_default_gateway() -> Result<String> {
        let out = Command::new("ip").args(["route"]).output()?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let cols: Vec<&str> = line.split_whitespace().collect();
                // 形如 "default via 192.168.1.1 dev wlan0 ..."
                if cols.len() >= 3 && cols[0] == "default" && cols[1] == "via" {
                    return Ok(cols[2].to_string());
                }
            }
        }

        // 回退到旧版 route -n（iproute2 缺失时）
        let out = Command::new("route").args(["-n"]).output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Linux route -n: Destination Gateway Genmask Flags Metric Ref Use Iface
            // 默认路由标志含 G（网关）
            if cols.len() >= 4 && cols[0] == "0.0.0.0" && cols[3].contains('G') {
                return Ok(cols[1].to_string());
            }
        }
        bail!("无法获取默认网关");
    }

    /// 通过 `which` crate 查找可执行文件路径
    fn find_executable(name: &str) -> Option<PathBuf> {
        which::which(name).ok()
    }

    /// 检测系统可用的 shell 列表（Linux：bash 默认，其次 zsh、sh）
    pub fn detect_shells() -> Vec<super::ShellInfo> {
        use super::{ShellInfo, ShellKind};
        let mut shells = Vec::new();

        if let Some(path) = find_executable("bash") {
            shells.push(ShellInfo {
                name: "bash".to_string(),
                path,
                kind: ShellKind::Bash,
            });
        }

        if let Some(path) = find_executable("zsh") {
            shells.push(ShellInfo {
                name: "zsh".to_string(),
                path,
                kind: ShellKind::Zsh,
            });
        }

        // sh 总是可用
        shells.push(ShellInfo {
            name: "sh".to_string(),
            path: PathBuf::from("/bin/sh"),
            kind: ShellKind::Sh,
        });

        shells
    }

    /// 返回当前平台信息
    pub fn current_platform() -> super::PlatformInfo {
        use super::PlatformInfo;
        let default_shell = detect_shells()
            .into_iter()
            .next()
            .expect("至少应有 sh 可用");
        PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            default_shell,
        }
    }
}

/// 其它平台（BSD 等）fallback：返回 Unsupported 错误，仅 shell 检测返回 /bin/sh。
#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux"
)))]
mod imp {
    use anyhow::{bail, Result};
    use std::path::PathBuf;

    use super::{PlatformInfo, ShellInfo, ShellKind};

    /// 注册/取消系统自启动（当前平台不支持）
    pub fn set_self_start(_enabled: bool) -> Result<()> {
        bail!("当前平台不支持自启动注册");
    }

    /// 获取当前 WiFi SSID（当前平台不支持）
    pub fn get_wifi_ssid() -> Result<String> {
        bail!("当前平台不支持 WiFi SSID 检测");
    }

    /// 获取默认网关 IP（当前平台不支持）
    pub fn get_default_gateway() -> Result<String> {
        bail!("当前平台不支持默认网关检测");
    }

    /// 检测系统可用的 shell 列表（当前平台仅返回 /bin/sh）
    pub fn detect_shells() -> Vec<ShellInfo> {
        vec![ShellInfo {
            name: "sh".to_string(),
            path: PathBuf::from("/bin/sh"),
            kind: ShellKind::Sh,
        }]
    }

    /// 返回当前平台信息
    pub fn current_platform() -> PlatformInfo {
        PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            default_shell: ShellInfo {
                name: "sh".to_string(),
                path: PathBuf::from("/bin/sh"),
                kind: ShellKind::Sh,
            },
        }
    }
}

pub use imp::*;
