//! 平台特定代码：自启动
//!
#[cfg(target_os = "windows")]
#[path = "platform_windows.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "platform_macos.rs"]
mod imp;

// ---- Desktop Entry 规范「The Exec Key」的转义/回读（P3-37/P3-38）----
//
// 仅 Linux（非 openwrt）注册路径与测试构建编译：生产非 Linux 目标无调用方，
// 不编译可避免 dead_code 告警

/// 按 Desktop Entry 规范构造 Exec 键的程序参数（P3-37）
///
/// 规范要求：参数含保留字符（空格/引号/反引号/`$`/`\` 等）时必须整体双引号
/// 包裹，串内 `"`、`` ` ``、`$`、`\` 前置反斜杠转义；字面 `%` 写作 `%%`
///（防被当作 `%f` 等字段代码展开——GLib `expand_macro` 的 `case '%'` 同口径）。
/// 无保留字符的路径也允许整体包裹（"may be quoted in whole"），统一包裹可让
/// 写入/回读两侧共用同一形态。消费方 GLib 以 `g_shell_parse_argv` 按壳层语义
/// 切分，本形态与其完全兼容；旧实现只转义空格，路径含 `$`/`%`/反引号/引号时
/// 登录自启失效
#[cfg(any(test, all(target_os = "linux", not(feature = "openwrt"))))]
pub(crate) fn quote_desktop_exec_arg(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            // 字面百分号：字段代码由「% + 字符」组成，`%%` 即转义后的字面 %
            '%' => out.push_str("%%"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// 从 desktop 文件内容中提取 `Exec=` 行的值（键大小写敏感，与写入侧一致）
#[cfg(any(test, all(target_os = "linux", not(feature = "openwrt"))))]
fn desktop_exec_value(content: &str) -> Option<&str> {
    content
        .lines()
        .find_map(|line| line.strip_prefix("Exec=").map(str::trim_end))
}

/// 解析 Exec 值的首个参数（程序路径，P3-38 路径校验用），按写入侧同口径反转义：
///
/// - 双引号/单引号包裹整体剥离（GLib `g_shell_parse_argv` 的壳层切分语义）
/// - `\x` → 字面 `x`（旧实现仅转义空格的文件同样可回读）
/// - `%%` → 字面 `%`；其余 `%X` 字段代码不是本程序写入的形态，跳过不回显
///   （回读后与当前 exe 比对不上即如实呈现「路径不匹配」）
///
/// 空值 / 引号未闭合返回 `None`（GLib 解析失败同样拒绝执行）
#[cfg(any(test, all(target_os = "linux", not(feature = "openwrt"))))]
fn parse_desktop_exec_program(exec: &str) -> Option<String> {
    let mut program = String::new();
    let mut chars = exec.chars();
    let mut in_double = false;
    let mut in_single = false;
    while let Some(ch) = chars.next() {
        match ch {
            // 引号内的空白属于参数本体；引号外空白即参数分隔
            ' ' | '\t' | '\n' | '\r' if !in_double && !in_single => break,
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '%' => match chars.next() {
                // 字面百分号（写入侧的 %% 转义）
                Some('%') => program.push('%'),
                // 字段代码（%f/%u 等）：非本程序写入形态，不回显
                Some(_) => {}
                // 行尾孤立 %：GLib 按字段代码残尾处理，丢弃
                None => break,
            },
            '\\' => {
                // 引号外与引号内统一按「下一字符字面化」回读——写入侧引号内
                // 只会产生 \" ` \$ \\ 四种，旧形态只在引号外产生 \ 空格
                program.push(chars.next()?);
            }
            _ => program.push(ch),
        }
    }
    if program.is_empty() || in_double || in_single {
        return None;
    }
    Some(program)
}

#[cfg(all(target_os = "linux", not(feature = "openwrt")))]
mod imp {
    use anyhow::Result;

    /// XDG autostart desktop 文件名（观测侧 P3-38 经 `super::` 复用同一常量）
    pub(super) const AUTOSTART_DESKTOP_FILE: &str = "campus-auth.desktop";
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
            // Exec 值按 Desktop Entry 规范整体引号包裹 + 保留字符转义
            // （见 super::quote_desktop_exec_arg，P3-37）
            let exec_value = super::quote_desktop_exec_arg(&exe.display().to_string());
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

/// Linux（XDG autostart）系统观测结论（P3-38）：对齐 Windows/macOS 的
/// 「系统事实优先」口径，字段命名与 `WindowsAutostartStatus` 逐一同义
#[cfg(all(target_os = "linux", not(feature = "openwrt")))]
#[derive(Debug, serde::Serialize)]
pub struct XdgAutostartStatus {
    /// autostart desktop 文件是否存在
    pub registered: bool,
    /// Exec 程序是否指向当前可执行文件
    pub path_match: bool,
    /// 登记的可执行文件是否仍存在
    pub target_exists: bool,
    /// 当前可执行文件路径
    pub expected_path: String,
    /// desktop 文件路径
    pub desktop_path: String,
    /// 文件 Exec 值的首参数（程序路径；缺失/不可解析为 null）
    pub desktop_exec: Option<String>,
    /// 系统观测推导的有效开关（enabled 状态机结论）
    pub enabled: bool,
    /// 状态机：not_registered / path_mismatch / enabled（命名对齐 Windows）
    pub state: &'static str,
}

/// 观测 XDG autostart 的系统事实（P3-38）：desktop 文件存在性 + Exec 指向校验
///
/// 对齐 Windows `path_mismatch` 语义——配置位不再直接充当「已启用」，
/// 用户手删 .desktop 文件后状态如实回落 not_registered；Exec 指向别处
/// （便携目录搬移后未重注册）如实呈现 path_mismatch。纯读取，不落盘不报错
#[cfg(all(target_os = "linux", not(feature = "openwrt")))]
pub fn xdg_self_start_status() -> XdgAutostartStatus {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let desktop_path = dirs::config_dir()
        .map(|c| c.join("autostart").join(imp::AUTOSTART_DESKTOP_FILE))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let content = std::fs::read_to_string(&desktop_path).ok();
    let program = content
        .as_deref()
        .and_then(desktop_exec_value)
        .and_then(parse_desktop_exec_program);
    let registered = std::path::Path::new(&desktop_path).is_file();
    let path_match = program.as_deref() == Some(exe.as_str());
    let target_exists = program
        .as_deref()
        .is_some_and(|p| std::path::Path::new(p).is_file());
    let state = if !registered {
        "not_registered"
    } else if path_match && target_exists {
        "enabled"
    } else {
        "path_mismatch"
    };
    XdgAutostartStatus {
        registered,
        path_match,
        target_exists,
        expected_path: exe,
        desktop_path,
        desktop_exec: program,
        enabled: state == "enabled",
        state,
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// P3-37：整体引号包裹 + 保留字符转义 + `%%`——含全部需转义字符的路径
    /// 逐字符钉住写入形态
    #[test]
    fn quote_desktop_exec_arg_escapes_reserved_characters() {
        assert_eq!(
            quote_desktop_exec_arg("/opt/My App/campus-auth"),
            "\"/opt/My App/campus-auth\"",
            "空格路径整体包裹即可，无需内部转义"
        );
        // 四种内部转义字符 + 字面百分号一次全含
        let raw = "/opt/my \"q\" $d `t` \\s 50%.exe";
        assert_eq!(
            quote_desktop_exec_arg(raw),
            "\"/opt/my \\\"q\\\" \\$d \\`t\\` \\\\s 50%%.exe\"",
            "引号/美元/反引号/反斜杠前置反斜杠，% 写作 %%"
        );
        // 无特殊字符的普通路径同样整体包裹（规范允许，统一写入形态）
        assert_eq!(
            quote_desktop_exec_arg("/usr/bin/campus-auth"),
            "\"/usr/bin/campus-auth\""
        );
    }

    /// P3-37：写入→回读往返——任意含保留字符的路径经 quote + parse 还原
    #[test]
    fn parse_desktop_exec_program_roundtrips_quoted_form() {
        for raw in [
            "/opt/My App/campus-auth",
            "/opt/my \"q\" $d `t` \\s 50%.exe",
            "/opt/100% 副本/campus-auth",
            "/usr/bin/campus-auth",
            "/opt/'单引号' 目录/app",
        ] {
            let quoted = quote_desktop_exec_arg(raw);
            assert_eq!(
                parse_desktop_exec_program(&quoted),
                Some(raw.to_string()),
                "往返失败: {raw} -> {quoted}"
            );
        }
    }

    /// P3-37 兼容：旧实现仅反斜杠转义空格的裸路径形态同样可回读
    #[test]
    fn parse_desktop_exec_program_reads_legacy_space_escape() {
        assert_eq!(
            parse_desktop_exec_program("/opt/My\\ App/campus-auth"),
            Some("/opt/My App/campus-auth".into())
        );
    }

    /// P3-38：回读边界——引号未闭合/空值拒绝，字段代码不回显，引号内空白
    /// 不切分
    #[test]
    fn parse_desktop_exec_program_rejects_malformed_and_drops_field_codes() {
        // 引号未闭合：GLib 解析失败拒绝执行，回读同样返回 None
        assert_eq!(parse_desktop_exec_program("\"/opt/My App"), None);
        assert_eq!(parse_desktop_exec_program(""), None);
        assert_eq!(parse_desktop_exec_program("   "), None);
        // 字段代码（%f/%u 等）非本程序写入形态：只回读程序名
        assert_eq!(
            parse_desktop_exec_program("\"/usr/bin/app\" %f"),
            Some("/usr/bin/app".into())
        );
        // 引号内空白属于参数本体，不切分
        assert_eq!(
            parse_desktop_exec_program("\"/opt/My App\" --arg"),
            Some("/opt/My App".into())
        );
    }

    /// P3-38：desktop 文件 Exec 行提取——键精确匹配、取首个、去行尾空白
    #[test]
    fn desktop_exec_value_extracts_exec_line() {
        let content = "[Desktop Entry]\nType=Application\nExec=\"/opt/My App/app\" --flag\n";
        assert_eq!(
            desktop_exec_value(content),
            Some("\"/opt/My App/app\" --flag")
        );
        // 无 Exec 行 / 键带前缀空白均不匹配（GKeyFile 键大小写敏感且不缩进）
        assert_eq!(desktop_exec_value("Type=Application\n"), None);
        assert_eq!(desktop_exec_value(" Exec=x\n"), None);
    }
}
