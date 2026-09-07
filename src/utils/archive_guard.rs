//! 未解压运行拦截：检测从压缩包预览 / 系统临时目录直接启动

use std::path::{Path, PathBuf};

/// 便携包必备同级目录（解压后应与 exe 同级）
const REQUIRED_SIBLINGS: &[&str] = &["resources", "python_worker"];
/// 环境变量：强制放行临时目录检查
const ENV_ALLOW_TEMP: &str = "CAMPUS_AUTH_ALLOW_TEMP";

/// 未解压拦截原因
#[derive(Debug, Clone, Copy)]
enum BlockReason {
    /// 可执行文件路径含 `.zip` 片段（资源管理器压缩文件夹虚拟路径）
    ZipSegment,
    /// 位于系统临时目录且缺失便携包同级目录（典型：双击 zip 内 exe 后仅单文件释放入 Temp）
    TempMissingSiblings,
}

/// 判定是否应拒绝启动
///
/// - `exe`：当前可执行文件路径（`current_exe`）
/// - `base`：解析后的基准路径（优先为 exe 所在目录，CLI `--base-path` 可覆盖）
/// - `allow_temp`：CLI `--allow-temp` 或环境变量已显式放行
///
/// 返回 `Some(拦截提示文案)` 表示应拒绝启动，`None` 为放行。
pub fn check_archive_block(exe: &Path, base: &Path, allow_temp: bool) -> Option<String> {
    if allow_temp || is_env_allow_temp() {
        return None;
    }
    // 容器 / 显式指定数据目录的场景不做临时目录误判的核心豁免：
    // Docker 镜像内 exe 位于 /usr/local/bin，base 为 /data，路径特征与便携包完全不同
    if crate::app::is_docker_env() {
        return None;
    }
    // base 被显式覆盖为与 exe 不同目录时（如 CAMPUS_AUTH_BASE_PATH=/data），
    // exe 在 Temp 但数据目录合法属可预期——仅保留"压缩包虚拟路径"强拦截，
    // 临时目录拦截降为仅在 base==exe_parent 时生效，避免云盘/自定义布局误伤
    let exe_parent = exe.parent();
    let base_is_exe_dir = exe_parent.is_some_and(|p| paths_equal(p, base));

    let exe_str = exe.to_string_lossy().to_string();
    // 1) 强特征：路径含 .zip 片段（大小写不敏感），必为压缩包内直接运行
    if contains_zip_segment(&exe_str) {
        return Some(format_block_message(
            BlockReason::ZipSegment,
            exe,
            base,
            base_is_exe_dir,
        ));
    }

    // 2) 次强特征：位于系统临时目录 + 缺失同级资源（仅 Windows 启用，避免 Linux /tmp 常见误伤）
    #[cfg(windows)]
    {
        if base_is_exe_dir {
            if let Some(parent) = exe_parent {
                if is_in_temp_dir(parent) && !has_required_siblings(parent) {
                    return Some(format_block_message(
                        BlockReason::TempMissingSiblings,
                        exe,
                        base,
                        base_is_exe_dir,
                    ));
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = base_is_exe_dir;
    }
    None
}

/// 路径字符串是否包含压缩包片段（大小写不敏感）
///
/// 匹配：`*.zip` / `*.7z` / `*.rar` 后接路径分隔或位于末尾，防止 `myzipfile` 类误判。
fn contains_zip_segment(s: &str) -> bool {
    let lower = s.to_lowercase();
    let norm = lower.replace('/', "\\");
    let candidates = [".zip", ".7z", ".rar"];
    for suf in candidates {
        if norm.contains(&format!("{suf}\\")) || norm.contains(&format!("{suf}/")) {
            return true;
        }
        if norm.ends_with(suf) || lower.ends_with(suf) {
            return true;
        }
        // 原分隔符形式也匹配（norm 已统一为 \，额外保留 lower 的 / 形式兜底）
        if lower.contains(&format!("{suf}\\")) || lower.contains(&format!("{suf}/")) {
            return true;
        }
    }
    false
}

/// 系统临时目录候选
fn temp_dir_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    out.push(std::env::temp_dir());
    for key in ["TEMP", "TMP"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                out.push(PathBuf::from(v));
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            out.push(PathBuf::from(local).join("Temp"));
        }
    }
    out
}

/// 判断路径是否位于任一系统临时目录之下（Windows 大小写不敏感）
fn is_in_temp_dir(path: &Path) -> bool {
    let path_norm = normalize_for_cmp(path);
    for cand in temp_dir_candidates() {
        let cand_norm = normalize_for_cmp(&cand);
        if cand_norm.is_empty() {
            continue;
        }
        // 确保以分隔符结尾再做前缀匹配，避免 C:\Temp2 误判为 C:\Temp
        let cand_prefix = format!("{}\\", cand_norm.trim_end_matches('\\'));
        if path_norm == cand_norm || path_norm.starts_with(&cand_prefix) {
            return true;
        }
    }
    false
}

/// 同级是否包含便携包必备目录（任一命中即视为已正确解压）
fn has_required_siblings(dir: &Path) -> bool {
    REQUIRED_SIBLINGS.iter().any(|name| dir.join(name).exists())
}

/// 大小写不敏感、分隔符归一的路径规范化（仅用于前缀比较，不做 canonicalize）
fn normalize_for_cmp(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    // 去掉末尾的 \?\\ 前缀与分隔符抖动
    let s = s.replace('/', "\\");
    let lower = s.to_lowercase();
    // 去除 Windows 扩展路径前缀 \\?\ 与末尾分隔符
    lower
        .strip_prefix(r"\\?\")
        .unwrap_or(&lower)
        .trim_end_matches('\\')
        .to_string()
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    normalize_for_cmp(a) == normalize_for_cmp(b)
}

fn is_env_allow_temp() -> bool {
    matches!(
        std::env::var(ENV_ALLOW_TEMP).map(|v| v.to_lowercase()),
        Ok(v) if v == "1" || v == "true" || v == "yes"
    )
}

fn format_block_message(
    reason: BlockReason,
    exe: &Path,
    base: &Path,
    _base_is_exe_dir: bool,
) -> String {
    let reason_text = match reason {
        BlockReason::ZipSegment => "看起来你是直接在压缩包里双击打开的",
        BlockReason::TempMissingSiblings => "程序在临时文件夹里运行，而且旁边缺少必要的文件",
    };
    format!(
        "请先解压后再运行\n\
        \n\
        程序检测到你可能没有解压就直接运行了，已停止启动。\n\
        原因：{reason_text}\n\
        程序位置：{exe}\n\
        运行目录：{base}\n\
        \n\
        请这样做：\n\
        1. 找到下载的 .zip 压缩包，右键选择“全部解压缩”或“解压到...”，解压到一个普通文件夹（比如 桌面上的 Campus-Auth 或 D:\\Campus-Auth）；\n\
        2. 打开解压后的文件夹，确认里面 campus-auth{ext} 和 resources、python_worker 是放在一起的；\n\
        3. 在解压后的文件夹里双击 campus-auth{ext} 启动，不要在压缩包的预览窗口里直接双击。\n\
        \n\
        如果你已经解压了还是看到这个提示：\n\
        1. 关掉压缩包的预览窗口，去解压后的文件夹里再打开；\n\
        2. 右键压缩包 → 属性，看看有没有“解除锁定”，有的话点一下再重新解压；\n\
        3. 把解压后的整个文件夹移到简单一点的路径，比如 D:\\Campus-Auth，尽量不要放在 OneDrive 或很深的路径里；\n\
        4. 解压时要点“解压全部文件”，不要只拖出来一个 exe，不然会丢文件；\n\
        5. 看看杀毒软件有没有把文件夹里的文件删掉。\n\
        \n\
        临时需要跳过检查：加参数 --allow-temp 启动，或设置环境变量 {env}=1（仅用于排查）。",
        reason_text = reason_text,
        exe = exe.display(),
        base = base.display(),
        ext = if cfg!(windows) { ".exe" } else { "" },
        env = ENV_ALLOW_TEMP,
    )
}

#[cfg(windows)]
/// Windows GUI 子系统下弹出 MessageBox（双击启动无控制台时的唯一可见通道）
pub fn show_block_dialog(message: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let title = "Campus-Auth - 请解压后运行";
    let to_wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let title_w = to_wide(title);
    let msg_w = to_wide(message);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
/// 非 Windows 的 no-op 桩：与 Windows 版（MessageBoxW 弹窗）保持同签名，调用点
/// （launcher 的未解压拦截）在类 Unix 平台仅依赖 stderr 输出，无需 GUI 弹窗
pub fn show_block_dialog(_message: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_contains_zip_segment() {
        assert!(contains_zip_segment(
            r"C:\Downloads\campus-auth.zip\campus-auth.exe"
        ));
        assert!(contains_zip_segment(
            r"C:\Users\a\AppData\Local\Temp\Temp1_campus-auth.zip\campus-auth.exe"
        ));
        assert!(contains_zip_segment("/tmp/campus-auth.ZIP/campus-auth"));
        assert!(contains_zip_segment("campus-auth.zip"));
        assert!(!contains_zip_segment(r"C:\Campus-Auth\campus-auth.exe"));
        assert!(!contains_zip_segment(r"C:\myzipfile\app.exe"));
        assert!(!contains_zip_segment(r"C:\zip\app.exe"));
    }

    #[test]
    fn test_has_required_siblings_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("resources")).unwrap();
        assert!(has_required_siblings(dir.path()));
        let dir2 = tempfile::tempdir().unwrap();
        fs::create_dir(dir2.path().join("python_worker")).unwrap();
        assert!(has_required_siblings(dir2.path()));
    }

    #[test]
    fn test_has_required_siblings_false_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_required_siblings(dir.path()));
    }

    #[test]
    fn test_check_zip_segment_blocks_even_with_base_override() {
        let exe = Path::new(r"C:\Downloads\campus-auth.zip\campus-auth.exe");
        let base = Path::new(r"D:\data");
        let msg = check_archive_block(exe, base, false);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("请先解压后再运行"));
    }

    #[test]
    fn test_check_allow_temp_bypasses() {
        let exe = Path::new(r"C:\Downloads\campus-auth.zip\campus-auth.exe");
        let base = Path::new(r"C:\Downloads\campus-auth.zip");
        assert!(check_archive_block(exe, base, true).is_none());
    }

    #[test]
    fn test_normal_path_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("resources")).unwrap();
        let exe = dir.path().join("campus-auth.exe");
        fs::write(&exe, b"x").unwrap();
        assert!(check_archive_block(&exe, dir.path(), false).is_none());
    }

    #[test]
    fn test_is_in_temp_dir_positive() {
        let tmp = std::env::temp_dir();
        let nested = tmp.join("Campus-Auth").join("campus-auth.exe");
        assert!(is_in_temp_dir(&nested));
        assert!(is_in_temp_dir(&tmp));
    }

    #[test]
    fn test_paths_equal_case_insensitive() {
        assert!(paths_equal(
            Path::new(r"C:\Temp\Campus-Auth"),
            Path::new(r"c:\temp\campus-auth")
        ));
        assert!(!paths_equal(
            Path::new(r"C:\Temp\Campus-Auth"),
            Path::new(r"D:\Campus-Auth")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn test_temp_missing_siblings_blocks() {
        let tmp = std::env::temp_dir().join(format!("ca-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let exe = tmp.join("campus-auth.exe");
        let _ = fs::write(&exe, b"x");
        // 无 siblings -> 拦截
        let msg = check_archive_block(&exe, &tmp, false);
        assert!(msg.is_some(), "临时目录且缺失同级目录应拦截");
        assert!(msg.unwrap().contains("临时文件夹"));
        // 补上 resources 后放行
        fs::create_dir(tmp.join("resources")).unwrap();
        assert!(check_archive_block(&exe, &tmp, false).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn test_temp_with_base_override_not_blocked() {
        // exe 在 Temp，但 base 被显式覆盖为非 exe 目录：不拦截（避免自定义布局误伤）
        let tmp = std::env::temp_dir().join(format!("ca-test-override-{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let exe = tmp.join("campus-auth.exe");
        let _ = fs::write(&exe, b"x");
        let other_base = tempfile::tempdir().unwrap();
        // 即使缺失 siblings，因 base != exe_parent，不触发 Temp 拦截
        assert!(check_archive_block(&exe, other_base.path(), false).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }
}
