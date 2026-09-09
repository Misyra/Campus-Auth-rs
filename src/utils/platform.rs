//! 平台特定代码：自启动
//!
#[cfg(target_os = "windows")]
mod imp {
    use anyhow::{Result, bail};
    use std::process::Command;

    /// 注册/取消系统自启动（Windows：HKCU Run 注册表，经 reg.exe）
    ///
    /// 曾用 `schtasks /sc ONLOGON`：非提升权限创建登录触发任务会被拒绝
    /// （"拒绝访问"），且任务不存在时 /delete 报"系统找不到指定的文件"。
    /// HKCU Run 键用户级可写、无需提权，登录时由 explorer 拉起；
    /// 取消时键值不存在视为幂等成功（不报错）。
    pub fn set_self_start(enabled: bool) -> Result<()> {
        const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        if enabled {
            let exe = std::env::current_exe()?;
            // /d 值需整体加引号，路径含空格时保持一个参数
            let quoted = format!("\"{}\"", exe.display());
            let status = Command::new("reg")
                .args([
                    "add",
                    RUN_KEY,
                    "/v",
                    "Campus-Auth",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &quoted,
                    "/f",
                ])
                .status()?;
            if !status.success() {
                bail!("注册自启动失败（reg add 返回非零退出码）");
            }
        } else {
            let status = Command::new("reg")
                .args(["delete", RUN_KEY, "/v", "Campus-Auth", "/f"])
                .status()?;
            // 键值本就不存在：视为已取消（幂等），不算失败
            if !status.success() && status.code() != Some(1) {
                bail!("取消自启动失败（reg delete 返回非零退出码）");
            }
        }
        Ok(())
    }
}

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

#[cfg(target_os = "linux")]
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
