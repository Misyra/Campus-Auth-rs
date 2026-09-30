//! 更新分发文件同步：正常助手与启动兜底共用，保留运行态并安排依赖重同步。

use chrono::Local;
use std::path::Path;

/// 同步随包助手；运行中的旧助手先改名，复制失败则恢复原路径。
pub fn replace_helper(extracted_dir: &Path, install_dir: &Path) -> std::io::Result<()> {
    let name = crate::uninstall::helper_exe_name();
    let source = extracted_dir.join(name);
    if !source.exists() {
        return Ok(());
    }
    let target = install_dir.join(name);
    let old = install_dir.join(format!("{name}.{}.old", uuid::Uuid::new_v4()));
    let had_target = target.exists();
    if had_target {
        std::fs::rename(&target, &old)?;
    }
    let result = crate::utils::io::atomic_copy_file(&source, &target);
    if let Err(error) = result {
        if had_target {
            std::fs::rename(&old, &target)?;
        }
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    }
    if had_target {
        // Windows 上正在执行的旧助手只能留待后续清理，不能因此撤销成功替换。
        if let Err(error) = std::fs::remove_file(&old) {
            tracing::debug!("旧助手暂不能删除（{}）: {error}", old.display());
        }
    }
    Ok(())
}

/// 同步便携标记，同时保留源码仓库及 Cargo 构建目录的卸载保护。
pub fn sync_portable_marker(extracted_dir: &Path, install_dir: &Path) -> std::io::Result<()> {
    if crate::uninstall::is_portable_distribution(extracted_dir)
        && !install_dir.join(".git").exists()
        && !crate::uninstall::is_cargo_target_dir(install_dir)
    {
        let marker = crate::uninstall::PORTABLE_MARKER_FILE;
        let bytes = std::fs::read(extracted_dir.join(marker))?;
        crate::utils::io::atomic_write_bytes(&install_dir.join(marker), &bytes)?;
    }
    Ok(())
}

/// overlay 同步更新包内的分发目录到 base_path（步骤 5.5）
///
/// 覆盖 `resources/`、`docs/`、`python_worker/`（Python 源码与 pyproject/uv.lock）。
/// `skip_names` 命中的目录名整棵子树跳过——发布包本就不含这些（release.yml 已排除），
/// 此处是防御性双保险：`.venv` 是用户引导出的运行态，`__pycache__` 运行时自动再生。
/// 失败返回错误，调用方保留 staging 供重试；依赖标记必须先于清单覆盖。
pub fn sync_distribution_files(
    extracted_dir: &Path,
    base_path: &Path,
    worker_target_dir: &Path,
) -> std::io::Result<()> {
    // 必须在 overlay 前比较并写标记：成功覆盖后源/目标必然相同；先写标记还可
    // 覆盖“清单已替换、helper 随后异常退出”的崩溃窗口。标记只会在主程序
    // 完成 uv sync + Worker 探针 + 指纹记录后清除。
    let manifests_changed = dependency_manifests_changed(extracted_dir, worker_target_dir);
    if manifests_changed {
        let marker = worker_target_dir.join(crate::environment::RESYNC_MARKER);
        std::fs::create_dir_all(worker_target_dir)?;
        crate::utils::io::atomic_write_bytes(&marker, Local::now().to_rfc3339().as_bytes())?;
    }

    for dir in ["resources", "docs", "python_worker"] {
        let src = extracted_dir.join(dir);
        if !src.exists() {
            continue;
        }
        let dst = if dir == "python_worker" {
            worker_target_dir.to_path_buf()
        } else {
            base_path.join(dir)
        };
        copy_dir_overlay(&src, &dst, &[".venv", "__pycache__"])?;
    }
    Ok(())
}

/// 在 overlay 前判断 Python 依赖清单是否变化。
fn dependency_manifests_changed(extracted_dir: &Path, worker_target_dir: &Path) -> bool {
    let py_src = extracted_dir.join("python_worker").join("pyproject.toml");
    let lock_src = extracted_dir.join("python_worker").join("uv.lock");
    let py_dst = worker_target_dir.join("pyproject.toml");
    let lock_dst = worker_target_dir.join("uv.lock");
    (py_src.exists() && file_differs(&py_src, &py_dst))
        || (lock_src.exists() && file_differs(&lock_src, &lock_dst))
}

/// 两个文件内容是否不同（任一侧读取失败/缺失视为不同）
fn file_differs(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x != y,
        _ => true,
    }
}

/// 递归 overlay 复制目录：目标侧不存在的路径创建，已存在的文件覆盖
pub fn copy_dir_overlay(src: &Path, dst: &Path, skip_names: &[&str]) -> std::io::Result<()> {
    if std::fs::symlink_metadata(src).is_ok_and(|m| m.file_type().is_symlink())
        || std::fs::symlink_metadata(dst).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "目标目录不能为符号链接",
        ));
    }
    std::fs::create_dir_all(dst)?;
    let mut first_error = None;
    for entry in std::fs::read_dir(src)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                first_error.get_or_insert(e);
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if skip_names.contains(&name_str) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let src_type = std::fs::symlink_metadata(&src_path)?.file_type();
        if src_type.is_symlink()
            || std::fs::symlink_metadata(&dst_path).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "拒绝同步符号链接",
            ));
        }
        let result = if src_type.is_dir() {
            copy_dir_overlay(&src_path, &dst_path, skip_names)
        } else {
            crate::utils::io::atomic_copy_file(&src_path, &dst_path)
        };
        if let Err(e) = result {
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
