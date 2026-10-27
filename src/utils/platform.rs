//! 平台特定代码：自启动
//!
#[cfg(target_os = "windows")]
#[path = "platform_windows.rs"]
mod imp;
#[cfg(target_os = "macos")]
mod imp {
    use anyhow::Result;

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
}

#[cfg(all(target_os = "linux", not(feature = "openwrt")))]
mod imp {
    use anyhow::Result;

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
}
#[cfg(all(target_os = "linux", feature = "openwrt"))]
mod imp {
    use anyhow::{Result, bail};

    /// 查询 procd 服务是否已注册开机启动，不依赖可能被 SSH 操作绕过的配置标记。
    pub fn openwrt_self_start_enabled() -> Result<bool> {
        let script = std::path::Path::new("/etc/init.d/campus-auth");
        if !script.is_file() {
            return Ok(false);
        }
        let status = std::process::Command::new(script).arg("enabled").status()?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!("无法查询 procd 自启动状态: {status}"),
        }
    }

    /// 使用已安装的 procd 服务注册或取消开机启动。
    pub fn set_self_start(enabled: bool) -> Result<()> {
        let script = std::path::Path::new("/etc/init.d/campus-auth");
        if !script.is_file() {
            bail!("请先按 OpenWrt 部署指南安装 /etc/init.d/campus-auth 服务脚本");
        }
        let status = std::process::Command::new(script)
            .arg(if enabled { "enable" } else { "disable" })
            .status()?;
        if !status.success() {
            bail!("procd 自启动设置失败，退出状态: {status}");
        }
        Ok(())
    }
}
/// 其它平台（BSD 等）fallback：自启动不支持。
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod imp {
    use anyhow::{Result, bail};

    /// 注册/取消系统自启动（当前平台不支持）
    pub fn set_self_start(_enabled: bool) -> Result<()> {
        bail!("当前平台不支持自启动注册");
    }
}

pub use imp::*;
