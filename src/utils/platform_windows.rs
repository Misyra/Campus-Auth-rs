//! Windows 自启动：读取系统事实、显式注册与写后复核。

use std::path::Path;
use std::ptr::{null, null_mut};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, LocalFree,
};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE_NAME: &str = "Campus-Auth";

/// 当前进程对应的用户级自启动状态；配置标记不参与有效状态计算。
#[derive(Debug, Serialize)]
pub struct WindowsAutostartStatus {
    /// 是否存在 Run 注册值。
    pub registered: bool,
    /// 注册命令是否指向当前可执行文件。
    pub path_match: bool,
    /// 注册命令是否使用当前数据目录。
    pub base_path_match: bool,
    /// 注册目标文件是否存在。
    pub target_exists: bool,
    /// Windows 审批状态；未知格式为 null。
    pub blocked_by_windows: Option<bool>,
    /// 当前可执行文件路径。
    pub expected_path: String,
    /// 当前数据目录。
    pub expected_base_path: String,
    /// 注册命令中的可执行文件路径。
    pub registry_path: Option<String>,
    /// Run 注册命令原文。
    pub registry_command: Option<String>,
    /// 审批值原始十六进制，缺失为 null。
    pub startup_approved: Option<String>,
    /// 根据系统观测推导的有效开关。
    pub enabled: bool,
    /// 有效状态及不能启用的具体原因。
    pub state: &'static str,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn checked(code: u32) -> Result<()> {
    if code != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    Ok(())
}

fn missing(code: u32) -> bool {
    matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

/// 原生 API 能区分不存在和拒绝访问，避免把权限错误显示成已关闭。
fn read_value(key: &str) -> Result<Option<(u32, Vec<u8>)>> {
    let mut kind = 0;
    let mut bytes = vec![0u8; 65536];
    let mut len = bytes.len() as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(VALUE_NAME).as_ptr(),
            RRF_RT_ANY | RRF_NOEXPAND,
            &mut kind,
            bytes.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if missing(code) {
        return Ok(None);
    }
    checked(code).with_context(|| format!("读取 HKCU\\{key}\\{VALUE_NAME} 失败"))?;
    bytes.truncate(len as usize);
    Ok(Some((kind, bytes)))
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn write_value(key: &str, kind: u32, bytes: &[u8]) -> Result<()> {
    let mut handle = null_mut();
    checked(unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut handle,
            null_mut(),
        )
    })?;
    let handle = Key(handle);
    checked(unsafe {
        RegSetValueExW(
            handle.0,
            wide(VALUE_NAME).as_ptr(),
            0,
            kind,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    })
}

fn delete_value(key: &str) -> Result<()> {
    let mut handle = null_mut();
    let code = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut handle,
        )
    };
    if missing(code) {
        return Ok(());
    }
    checked(code)?;
    let handle = Key(handle);
    let code = unsafe { RegDeleteValueW(handle.0, wide(VALUE_NAME).as_ptr()) };
    if missing(code) {
        return Ok(());
    }
    checked(code)
}

fn command_args(command: &str) -> Result<Vec<String>> {
    if command.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut count = 0;
    let args = unsafe { CommandLineToArgvW(wide(command).as_ptr(), &mut count) };
    if args.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let result = unsafe {
        std::slice::from_raw_parts(args, count as usize)
            .iter()
            .map(|arg| {
                let mut len = 0;
                while *arg.add(len) != 0 {
                    len += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(*arg, len))
            })
            .collect()
    };
    unsafe {
        LocalFree(args.cast());
    }
    Ok(result)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn approval(value: Option<&(u32, Vec<u8>)>) -> Option<bool> {
    let Some((kind, bytes)) = value else {
        return Some(false);
    };
    if *kind != REG_BINARY || bytes.len() != 12 {
        return None;
    }
    match u32::from_le_bytes(bytes[..4].try_into().ok()?) {
        2 | 6 => Some(false),
        3 | 7 => Some(true),
        _ => None,
    }
}

fn inspect(
    run_key: &str,
    approved_key: &str,
    exe: &Path,
    base: &Path,
) -> Result<WindowsAutostartStatus> {
    let run = read_value(run_key)?;
    let approved = read_value(approved_key)?;
    let command = run
        .as_ref()
        .map(|(kind, bytes)| {
            if *kind != REG_SZ || !bytes.len().is_multiple_of(2) {
                bail!("自启动 Run 值不是有效的 REG_SZ");
            }
            let words: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
                .collect();
            let end = words.iter().position(|v| *v == 0).unwrap_or(words.len());
            Ok(String::from_utf16(&words[..end])?)
        })
        .transpose()?;
    let args = command
        .as_deref()
        .map(command_args)
        .transpose()?
        .unwrap_or_default();
    let path = args.first().cloned();
    let path_match = path.as_ref().is_some_and(|p| same_path(Path::new(p), exe));
    let target_exists = path.as_ref().is_some_and(|p| Path::new(p).is_file());
    // 兼容旧版无参数注册；新注册只允许来源标记和数据目录，未知参数可能导致 CLI 直接退出。
    let mut base_match = exe.parent().is_some_and(|p| same_path(p, base));
    let mut valid_args = command
        .as_ref()
        .is_none_or(|c| c.matches('"').count().is_multiple_of(2));
    let mut seen_marker = false;
    let mut seen_base = false;
    let mut tail = args.iter().skip(1);
    while let Some(arg) = tail.next() {
        match arg.as_str() {
            "--startup-launch" => {
                valid_args &= !seen_marker;
                seen_marker = true;
            }
            "--base-path" => {
                valid_args &= !seen_base;
                seen_base = true;
                base_match = tail.next().is_some_and(|p| same_path(Path::new(p), base));
            }
            _ => valid_args = false,
        }
    }
    let blocked = approval(approved.as_ref());
    let state = if command.is_none() {
        "not_registered"
    } else if blocked.is_none() {
        "unknown"
    } else if blocked == Some(true) {
        "blocked"
    } else if !path_match || !target_exists {
        "path_mismatch"
    } else if !base_match || !valid_args {
        "command_mismatch"
    } else {
        "enabled"
    };
    Ok(WindowsAutostartStatus {
        registered: command.is_some(),
        path_match,
        base_path_match: base_match && valid_args,
        target_exists,
        blocked_by_windows: blocked,
        expected_path: exe.display().to_string(),
        expected_base_path: base.display().to_string(),
        registry_path: path,
        registry_command: command,
        startup_approved: approved.map(|(_, v)| hex::encode(v)),
        enabled: state == "enabled",
        state,
    })
}

/// 查询本进程的系统自启动状态，不修改注册项或配置。
pub fn self_start_status(base: &Path) -> Result<WindowsAutostartStatus> {
    inspect(RUN_KEY, APPROVED_KEY, &std::env::current_exe()?, base)
}

/// Windows 命令行引号规则要求结束引号前的反斜杠加倍（数据目录可能为盘符根）。
fn quote(value: &Path) -> String {
    let value = value.to_string_lossy();
    let trailing = value.chars().rev().take_while(|c| *c == '\\').count();
    format!("\"{value}{}\"", "\\".repeat(trailing))
}

fn register(enabled: bool, run: &str, approved: &str, exe: &Path, base: &Path) -> Result<()> {
    if enabled {
        let command = format!(
            "{} --startup-launch --base-path {}",
            quote(exe),
            quote(base)
        );
        // Run 命令行有 260 字符限制，超限时不能宣称注册成功。
        if command.encode_utf16().count() > 260 {
            bail!("自启动命令超过 Windows Run 的 260 字符限制");
        }
        let bytes: Vec<u8> = wide(&command)
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        write_value(run, REG_SZ, &bytes)?;
        // 仅显式启用时清除系统禁用记录；启动诊断与轮询永远只读。
        delete_value(approved)?;
    } else {
        // 删除 Run 已足够取消注册，不伪造 Windows 的禁用时间戳或审批记录。
        delete_value(run)?;
    }
    verify(enabled, inspect(run, approved, exe, base)?)
}

fn verify(enabled: bool, status: WindowsAutostartStatus) -> Result<()> {
    if (enabled && !status.enabled) || (!enabled && status.registered) {
        bail!(
            "系统自启动状态复核失败（{}），请检查 Windows 启动应用设置或安全软件拦截",
            status.state
        );
    }
    Ok(())
}

/// 显式设置自启动并保留本实例的数据目录，写入后立即和短暂延迟回读。
pub fn set_self_start_for_base(enabled: bool, base: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    register(enabled, RUN_KEY, APPROVED_KEY, &exe, base)?;
    // 捕获常见的异步撤销；之后的外部改动由状态轮询重新观测，不能保证未来必然启动。
    std::thread::sleep(std::time::Duration::from_millis(350));
    verify(enabled, self_start_status(base)?)
}

/// 注册或取消当前可执行文件默认目录下的自启动。
pub fn set_self_start(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let base = exe.parent().context("无法确定可执行文件目录")?;
    set_self_start_for_base(enabled, base)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 所有写入测试均使用随机专用子键，绝不触碰真实 Run 或 StartupApproved。
    struct Fixture {
        root: String,
        run: String,
        approved: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = format!(r"Software\Campus-Auth-Tests\{}", uuid::Uuid::new_v4());
            Self {
                run: format!(r"{root}\Run"),
                approved: format!(r"{root}\Approved"),
                root,
            }
        }
        fn status(&self) -> WindowsAutostartStatus {
            let exe = std::env::current_exe().unwrap();
            inspect(&self.run, &self.approved, &exe, exe.parent().unwrap()).unwrap()
        }
        fn set(&self, enabled: bool) {
            let exe = std::env::current_exe().unwrap();
            register(
                enabled,
                &self.run,
                &self.approved,
                &exe,
                exe.parent().unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                RegDeleteTreeW(HKEY_CURRENT_USER, wide(&self.root).as_ptr());
            }
        }
    }

    #[test]
    fn autostart_native_roundtrip_external_disable_and_revoke() {
        let fixture = Fixture::new();
        assert!(!fixture.status().registered);
        fixture.set(true);
        assert!(fixture.status().enabled);
        let mut blocked = [0u8; 12];
        blocked[0] = 3;
        write_value(&fixture.approved, REG_BINARY, &blocked).unwrap();
        assert_eq!(fixture.status().state, "blocked");
        assert!(!fixture.status().enabled);
        // 反复查询不得替用户清除任务管理器的选择。
        assert_eq!(
            fixture.status().startup_approved,
            Some(hex::encode(blocked))
        );
        fixture.set(true);
        assert!(fixture.status().enabled);
        delete_value(&fixture.run).unwrap();
        assert!(verify(true, fixture.status()).is_err());
        fixture.set(false);
        fixture.set(false);
    }

    #[test]
    fn autostart_unknown_approval_and_stale_command_are_not_enabled() {
        let fixture = Fixture::new();
        fixture.set(true);
        write_value(&fixture.approved, REG_BINARY, &[9; 12]).unwrap();
        assert_eq!(fixture.status().state, "unknown");
        assert!(!fixture.status().enabled);
        delete_value(&fixture.approved).unwrap();
        let bytes: Vec<u8> = wide(r#""C:\missing\campus-auth.exe""#)
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        write_value(&fixture.run, REG_SZ, &bytes).unwrap();
        assert_eq!(fixture.status().state, "path_mismatch");
        assert!(!fixture.status().target_exists);
        assert!(!fixture.status().enabled);
    }

    #[test]
    fn autostart_approval_known_states_and_malformed_data() {
        assert_eq!(approval(None), Some(false));
        for (state, expected) in [(2, false), (3, true), (6, false), (7, true)] {
            let mut value = vec![0; 12];
            value[0] = state;
            assert_eq!(approval(Some(&(REG_BINARY, value))), Some(expected));
        }
        assert_eq!(approval(Some(&(REG_SZ, vec![0; 12]))), None);
        assert_eq!(approval(Some(&(REG_BINARY, vec![2]))), None);
    }

    #[test]
    fn autostart_disable_keeps_external_approval_and_rejects_bad_commands() {
        let fixture = Fixture::new();
        fixture.set(true);
        let blocked = [3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        write_value(&fixture.approved, REG_BINARY, &blocked).unwrap();
        fixture.set(false);
        assert!(!fixture.status().registered);
        assert_eq!(
            fixture.status().startup_approved,
            Some(hex::encode(blocked))
        );
        delete_value(&fixture.approved).unwrap();
        let exe = std::env::current_exe().unwrap();
        for args in [
            "--autostart",
            "--startup-launch --startup-launch",
            "--base-path",
        ] {
            let command = format!("{} {args}", quote(&exe));
            let bytes: Vec<u8> = wide(&command)
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect();
            write_value(&fixture.run, REG_SZ, &bytes).unwrap();
            assert_eq!(fixture.status().state, "command_mismatch");
        }
    }

    #[test]
    fn autostart_command_roundtrip_and_legacy_compatibility() {
        for base in [r"C:\", r"C:\认证喵 data", r"C:\data\"] {
            let command = format!(
                r#""C:\认证喵\campus-auth.exe" --startup-launch --base-path {}"#,
                quote(Path::new(base))
            );
            let args = command_args(&command).unwrap();
            assert_eq!(args[3], base);
            assert_eq!(args[1], "--startup-launch");
        }
        let fixture = Fixture::new();
        let exe = std::env::current_exe().unwrap();
        let bytes: Vec<u8> = wide(&quote(&exe))
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        write_value(&fixture.run, REG_SZ, &bytes).unwrap();
        assert!(fixture.status().enabled);
        assert!(read_value(&fixture.approved).unwrap().is_none());
        let status = inspect(
            &fixture.run,
            &fixture.approved,
            &exe,
            Path::new(r"C:\other-data"),
        )
        .unwrap();
        assert_eq!(status.state, "command_mismatch");
    }
}
