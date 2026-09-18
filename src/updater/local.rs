//! 手动更新的本地安装包复用：扫描 `update/` 根目录并校验摘要
//!
//! 用途：用户自行把发布包放进 `<base_path>/update/`，应用内检查更新发现新版本后
//! 复用该文件，省去下载。**本地包不构成独立信任源**——它必须先与远程清单声明的
//! SHA256 一致才被认作"已下载好的缓存"，因此无法用于完全离线更新（离线时拿不到
//! 远程摘要），也无法绕过现有校验链。
//!
//! 两个入口分工：
//! - [`find_local_package`]：检查阶段，仅探测并回报给前端（不移动文件）；
//! - [`stage_local_package`]：应用阶段，边复制边哈希暂存到 `update/staging/`，
//!   后续解压与 `pending.json` 写入复用网络下载的同一路径。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::updater::apply::STAGING_DIR_NAME;
use crate::updater::download::MAX_UPDATE_ARCHIVE_BYTES;
use crate::updater::error::UpdaterError;

/// `update/` 根目录内由程序自身维护、不可被当作安装包的固定文件名
///
/// 用户选择把压缩包直接放在 `update/` 根目录（而非独立子目录），故扫描必须显式
/// 排除这些文件；`staging/` 是目录，非递归扫描天然不会进入。
const NON_PACKAGE_NAMES: [&str; 3] = ["pending.json", "helper.lock", "last_check.json"];

/// 本地安装包（`update/` 根目录中与远程摘要一致的文件）
#[derive(Clone, Debug, serde::Serialize)]
pub struct LocalPackage {
    /// 文件名（不含目录，仅用于展示）
    pub file_name: String,
    /// 文件大小（字节）
    pub size: u64,
    /// 文件 SHA256（hex 小写，与远程清单声明值一致）
    pub sha256: String,
}

/// 文件名是否可能是一个安装包（纯函数，便于单测）
///
/// 只接受发布资产的归档扩展名：解压按扩展名分派（[`crate::utils::io::extract_archive`]），
/// 无扩展名或不认识的扩展名无法进入后续流程，故在扫描阶段就排除，避免无谓地
/// 对无关大文件做哈希。
fn is_package_candidate(name: &str) -> bool {
    if name.starts_with('.') || NON_PACKAGE_NAMES.contains(&name) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    // 浏览器/下载器的半成品后缀：内容不完整，不参与匹配
    if lower.ends_with(".tmp") || lower.ends_with(".part") || lower.ends_with(".crdownload") {
        return false;
    }
    lower.ends_with(".zip") || lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
}

/// 计算文件 SHA256（失败返回 `None`，调用方跳过该候选）
fn sha256_of(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hex::encode(hasher.finalize()))
}

/// 同步扫描 `update/` 根目录，返回摘要匹配的候选（实现体，见 [`find_local_package`]）
///
/// 摘要不一致的文件按未命中处理，不报错——用户可能只是放了旧版本的包。
///
/// **大小不参与筛选**：UPD-7 已明确清单声明的 `size` 是咨询性字段（与
/// Content-Length 不符时下载路径仅告警、以 SHA256 为准）。此处若因大小不符就跳过，
/// 等于把咨询性字段升级为硬门槛：远程清单误标长度时，一个内容完全正确的本地包会被
/// 静默忽略、用户被迫重新下载。大小只在命中后做一次交叉核对并告警，与下载路径同口径。
fn find_local_package_blocking(
    base_path: &Path,
    expected_sha256: &str,
    expected_size: Option<u64>,
) -> Option<LocalPackage> {
    let update_dir = base_path.join(crate::utils::paths::UPDATE_DIR);
    let entries = std::fs::read_dir(&update_dir).ok()?;
    for entry in entries.flatten() {
        // 只认普通文件：目录（含 staging/）不递归进入
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if !is_package_candidate(&name) {
            continue;
        }
        let size = meta.len();
        if size > MAX_UPDATE_ARCHIVE_BYTES {
            tracing::debug!(file = %name, size, "本地安装包超过大小上限，跳过");
            continue;
        }
        let Some(actual) = sha256_of(&entry.path()) else {
            tracing::debug!(file = %name, "本地文件读取失败，跳过");
            continue;
        };
        if actual.eq_ignore_ascii_case(expected_sha256) {
            // 与下载路径同口径：大小不符仅告警，完整性以摘要为准
            if let Some(expected) = expected_size
                && expected != size
            {
                tracing::warn!(
                    file = %name,
                    expected,
                    actual = size,
                    "本地安装包大小与清单声明不一致（以 SHA256 校验为准）"
                );
            }
            return Some(LocalPackage {
                file_name: name,
                size,
                sha256: actual,
            });
        }
    }
    None
}

/// 扫描 `update/` 根目录，找摘要与远程清单一致的本地安装包
///
/// `expected_sha256` 为空时直接返回 `None`（fail-closed）：本地包的可信度完全来自
/// 远程声明的摘要，无摘要即无从校验，不做"信任本地文件"的降级。
///
/// 目录列举与哈希为阻塞 I/O（大包可达数十 MB），统一在阻塞线程池执行，
/// 避免长时间占用 tokio worker 线程。
pub(crate) async fn find_local_package(
    base_path: &Path,
    expected_sha256: &str,
    expected_size: Option<u64>,
) -> Option<LocalPackage> {
    if expected_sha256.is_empty() {
        return None;
    }
    let base = base_path.to_path_buf();
    let expected = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        find_local_package_blocking(&base, &expected, expected_size)
    })
    .await
    .unwrap_or(None)
}

/// 把本地安装包暂存到 `update/staging/`，边复制边校验摘要
///
/// 返回暂存后的压缩包路径（与网络下载成功后的返回值同语义，可直接交给
/// [`crate::updater::download::extract_to_staging`]）。
///
/// 校验方式是"边拷贝边哈希"而非"先校验再解压原文件"：检查阶段与点击更新之间
/// 文件可能被替换，若直接以 `update/` 根目录下的原文件作为解压源，被替换的
/// 内容会进入 staging 并被写入 `pending.json` 的 `sha256`，成为 helper 认可的
/// 合法更新——信任锚就此断裂。拷贝副本 + 摘要比对由同一次读取完成，不存在
/// 该窗口。
///
/// `archive_name` 取远程资产名（而非本地文件名）：摘要一致意味着内容与远程
/// 资产相同，格式也随之确定，按远程名分派解压才不会因用户改过本地扩展名而选错。
pub(crate) async fn stage_local_package(
    src: &Path,
    base_path: &Path,
    archive_name: &str,
    expected_sha256: &str,
) -> Result<PathBuf, UpdaterError> {
    let src = src.to_path_buf();
    let staging_dir = base_path.join(STAGING_DIR_NAME);
    let archive_name = archive_name.to_string();
    let expected = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        stage_local_package_blocking(&src, &staging_dir, &archive_name, &expected)
    })
    .await
    .map_err(|e| UpdaterError::ExtractFailed(format!("本地包暂存任务执行失败: {e}")))?
}

/// 同步实现体：复制 + 增量哈希 + 校验 + 原子改名
fn stage_local_package_blocking(
    src: &Path,
    staging_dir: &Path,
    archive_name: &str,
    expected_sha256: &str,
) -> Result<PathBuf, UpdaterError> {
    std::fs::create_dir_all(staging_dir).map_err(UpdaterError::StagingDirCreateFailed)?;
    let tmp_path = staging_dir.join(format!("{archive_name}.local.tmp"));
    // 上次中断可能残留同名临时文件；覆盖写前先清掉，避免尾部残留字节
    if tmp_path.exists() {
        let _ = std::fs::remove_file(&tmp_path);
    }

    let mut reader = std::fs::File::open(src).map_err(UpdaterError::PendingWriteFailed)?;
    let mut writer = std::fs::File::create(&tmp_path).map_err(UpdaterError::PendingWriteFailed)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut written = 0u64;
    loop {
        let n =
            std::io::Read::read(&mut reader, &mut buf).map_err(UpdaterError::PendingWriteFailed)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut writer, &buf[..n])
            .map_err(UpdaterError::PendingWriteFailed)?;
        written += n as u64;
        if written > MAX_UPDATE_ARCHIVE_BYTES {
            drop(writer);
            let _ = std::fs::remove_file(&tmp_path);
            return Err(UpdaterError::DownloadTooLarge {
                limit: MAX_UPDATE_ARCHIVE_BYTES,
            });
        }
    }
    std::io::Write::flush(&mut writer).map_err(UpdaterError::PendingWriteFailed)?;
    drop(writer);

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(UpdaterError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }

    let archive_path = staging_dir.join(archive_name);
    std::fs::rename(&tmp_path, &archive_path).map_err(UpdaterError::PendingWriteFailed)?;
    Ok(archive_path)
}

/// 净化上传方提供的文件名（纯函数，便于单测）
///
/// 浏览器的 `File.name` 只是末段文件名，但 multipart 字段由客户端构造，不能假定
/// 其形态：取末段、剥离路径分隔符与控制字符、去掉前导点，超长或净化后为空时回退
/// 固定名（与 [`crate::updater::download::archive_name_from_url`] 同一防御口径）。
pub(crate) fn sanitize_upload_name(raw: &str) -> String {
    let last_segment = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last_segment
        .chars()
        .filter(|c| !matches!(c, '/' | '\\') && !c.is_control())
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() || trimmed.len() > 128 {
        "campus-auth-upload.zip".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 把外部文件流式复制到 `update/staging/<archive_name>`（实现体）
///
/// 复制而非移动/引用：源文件在系统临时目录，且 Web 层的临时文件句柄在请求结束后
/// 即被删除，必须先把内容固定到 staging；同时按上限拒绝超限内容（分块检查，
/// 不在内存里整包持有）。
fn copy_into_staging_blocking(
    src: &Path,
    staging_dir: &Path,
    archive_name: &str,
) -> Result<PathBuf, UpdaterError> {
    std::fs::create_dir_all(staging_dir).map_err(UpdaterError::StagingDirCreateFailed)?;
    let path = staging_dir.join(archive_name);
    let mut reader = std::fs::File::open(src).map_err(UpdaterError::PendingWriteFailed)?;
    let mut writer = std::fs::File::create(&path).map_err(UpdaterError::PendingWriteFailed)?;
    let mut buf = [0u8; 65536];
    let mut written = 0u64;
    loop {
        let n =
            std::io::Read::read(&mut reader, &mut buf).map_err(UpdaterError::PendingWriteFailed)?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > MAX_UPDATE_ARCHIVE_BYTES {
            // 半写入的目标文件必须清掉：否则 staging 里留一个超限的残包
            drop(writer);
            let _ = std::fs::remove_file(&path);
            return Err(UpdaterError::DownloadTooLarge {
                limit: MAX_UPDATE_ARCHIVE_BYTES,
            });
        }
        std::io::Write::write_all(&mut writer, &buf[..n])
            .map_err(UpdaterError::PendingWriteFailed)?;
    }
    std::io::Write::flush(&mut writer).map_err(UpdaterError::PendingWriteFailed)?;
    Ok(path)
}

/// 把外部压缩包流式复制到 `update/staging/<archive_name>`，返回落盘路径
///
/// 复制与解压都是 CPU+IO 密集操作，统一在阻塞线程池执行。
pub(crate) async fn copy_into_staging(
    src: &Path,
    staging_dir: &Path,
    archive_name: &str,
) -> Result<PathBuf, UpdaterError> {
    let src = src.to_path_buf();
    let staging_dir = staging_dir.to_path_buf();
    let archive_name = archive_name.to_string();
    tokio::task::spawn_blocking(move || {
        copy_into_staging_blocking(&src, &staging_dir, &archive_name)
    })
    .await
    .map_err(|e| UpdaterError::ExtractFailed(format!("安装包复制任务执行失败: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sha256_hex(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    /// 候选过滤：归档扩展名放行，程序自身文件 / 半成品 / 隐藏文件 / 无扩展名排除
    #[test]
    fn test_is_package_candidate() {
        assert!(is_package_candidate("campus-auth-5.0.1-windows-x64.zip"));
        assert!(is_package_candidate("campus-auth-5.0.1-macos-arm64.tar.gz"));
        assert!(is_package_candidate("campus-auth-5.0.1-linux-x64.tgz"));
        // 大写扩展名同样识别（正规化在函数内完成）
        assert!(is_package_candidate("Campus-Auth-5.0.1-Windows-X64.ZIP"));

        assert!(!is_package_candidate("pending.json"));
        assert!(!is_package_candidate("helper.lock"));
        assert!(!is_package_candidate("last_check.json"));
        assert!(!is_package_candidate(".hidden.zip"));
        assert!(!is_package_candidate("pkg.zip.tmp"));
        assert!(!is_package_candidate("pkg.zip.part"));
        assert!(!is_package_candidate("pkg.zip.crdownload"));
        assert!(!is_package_candidate("campus-auth-5.0.1"));
        assert!(!is_package_candidate("README.md"));
    }

    /// 命中：目录下的包摘要与远程一致时返回元信息
    #[test]
    fn test_find_local_package_hit() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update");
        std::fs::create_dir_all(&update).unwrap();
        let payload = b"fake-update-payload-12345";
        std::fs::write(update.join("campus-auth-9.9.9-windows-x64.zip"), payload).unwrap();

        let found = find_local_package_blocking(
            dir.path(),
            &sha256_hex(payload),
            Some(payload.len() as u64),
        )
        .expect("摘要一致应命中");
        assert_eq!(found.file_name, "campus-auth-9.9.9-windows-x64.zip");
        assert_eq!(found.size, payload.len() as u64);
    }

    /// 未命中：摘要不符 / 空摘要（fail-closed）/ 目录不存在均返回 None
    #[test]
    fn test_find_local_package_miss_cases() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update");
        std::fs::create_dir_all(&update).unwrap();
        let payload = b"fake-update-payload-12345";
        std::fs::write(update.join("pkg.zip"), payload).unwrap();
        let sha = sha256_hex(payload);

        // 摘要不符（旧版本的包）
        assert!(find_local_package_blocking(dir.path(), &"0".repeat(64), None).is_none());
        // 空摘要：不信任本地文件
        assert!(find_local_package_blocking(dir.path(), "", None).is_none());
        // 目录不存在
        let empty = tempfile::tempdir().unwrap();
        assert!(find_local_package_blocking(empty.path(), &sha, None).is_none());
    }

    /// 大小是咨询性字段（UPD-7 同口径）：清单声明大小与实际不符时仍按摘要命中，
    /// 不把误标长度升级成"内容正确的本地包被静默忽略"
    #[test]
    fn test_find_local_package_ignores_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update");
        std::fs::create_dir_all(&update).unwrap();
        let payload = b"fake-update-payload-12345";
        std::fs::write(update.join("pkg.zip"), payload).unwrap();

        let found = find_local_package_blocking(
            dir.path(),
            &sha256_hex(payload),
            Some(payload.len() as u64 + 4096),
        )
        .expect("大小不符但摘要一致时应命中");
        // 回报的是真实文件大小，而非清单声明值
        assert_eq!(found.size, payload.len() as u64);
    }

    /// 扫描不递归、不误认程序自身文件与 staging 产物
    #[test]
    fn test_find_local_package_skips_own_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update");
        std::fs::create_dir_all(update.join("staging").join("extracted")).unwrap();
        let payload = b"fake-update-payload-12345";
        let sha = sha256_hex(payload);

        // 同名内容分别落在：程序自身文件、staging 子目录、隐藏文件
        std::fs::write(update.join("pending.json"), payload).unwrap();
        std::fs::write(update.join("last_check.json"), payload).unwrap();
        std::fs::write(update.join("helper.lock"), payload).unwrap();
        std::fs::write(update.join("staging").join("pkg.zip"), payload).unwrap();
        std::fs::write(update.join(".hidden.zip"), payload).unwrap();

        assert!(
            find_local_package_blocking(dir.path(), &sha, None).is_none(),
            "程序自身文件 / 子目录 / 隐藏文件都不得被当作本地安装包"
        );
    }

    /// 暂存成功：副本落盘、临时文件不残留
    #[test]
    fn test_stage_local_package_success() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update");
        std::fs::create_dir_all(&update).unwrap();
        let payload = b"fake-update-payload-12345";
        let src = update.join("campus-auth-9.9.9-windows-x64.zip");
        std::fs::write(&src, payload).unwrap();

        let staged = stage_local_package_blocking(
            &src,
            &dir.path().join("update").join("staging"),
            "remote-asset-name.zip",
            &sha256_hex(payload),
        )
        .expect("摘要一致应暂存成功");
        assert_eq!(std::fs::read(&staged).unwrap(), payload);
        // 命名取远程资产名，而非本地文件名
        assert!(staged.ends_with("remote-asset-name.zip"));
        // 原文件保留（用户可能希望继续留着）
        assert!(src.exists());
        // 无 .tmp 残留
        let leftovers: Vec<_> = std::fs::read_dir(staged.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应残留临时文件: {leftovers:?}");
    }

    /// 暂存失败（摘要不符，模拟 check 与 apply 之间文件被替换）：报错且不留残留
    #[test]
    fn test_stage_local_package_rejects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update");
        std::fs::create_dir_all(&update).unwrap();
        let src = update.join("pkg.zip");
        let mut f = std::fs::File::create(&src).unwrap();
        f.write_all(b"tampered").unwrap();
        drop(f);

        let staging = update.join("staging");
        let err = stage_local_package_blocking(&src, &staging, "pkg.zip", &"0".repeat(64))
            .expect_err("摘要不符应拒绝");
        assert!(
            matches!(err, UpdaterError::ChecksumMismatch { .. }),
            "{err}"
        );
        // 临时文件被清理，未落盘任何可信产物
        let entries: Vec<_> = std::fs::read_dir(&staging)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(entries.is_empty(), "失败路径不应留下产物: {entries:?}");
    }

    /// 上传文件名净化：只保留末段、剥离分隔符与控制字符、去前导点、回退固定名
    #[test]
    fn test_sanitize_upload_name() {
        // 正常名透传（扩展名决定解压分派，必须保留）
        assert_eq!(
            sanitize_upload_name("campus-auth-9.9.9-windows-x64.zip"),
            "campus-auth-9.9.9-windows-x64.zip"
        );
        assert_eq!(
            sanitize_upload_name("campus-auth-9.9.9-macos-arm64.tar.gz"),
            "campus-auth-9.9.9-macos-arm64.tar.gz"
        );
        // 路径片段剥离：只取末段（防 `../` 逃逸成落盘名）
        assert_eq!(
            sanitize_upload_name("../../evil/payload.zip"),
            "payload.zip"
        );
        assert_eq!(sanitize_upload_name("C:\\temp\\pkg.zip"), "pkg.zip");
        // 反斜杠即分隔符：Windows 形态混入时同样按末段取值（不残留 `a` 前缀）
        assert_eq!(sanitize_upload_name("a\\pkg.zip"), "pkg.zip");
        // 前导点去掉（隐藏文件形态）
        assert_eq!(sanitize_upload_name("...pkg.zip"), "pkg.zip");
        // 空/纯点/超长 → 回退固定名
        assert_eq!(sanitize_upload_name(""), "campus-auth-upload.zip");
        assert_eq!(sanitize_upload_name("..."), "campus-auth-upload.zip");
        assert_eq!(
            sanitize_upload_name(&"a".repeat(200)),
            "campus-auth-upload.zip"
        );
    }

    /// 上传包复制到 staging：内容一致、内存不整包持有（分块复制即可满足）
    #[test]
    fn test_copy_into_staging_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("upload.tmp");
        // 跨多个分块（> 65536）以便经过循环的多轮复制
        let payload = vec![7u8; 65536 * 2 + 123];
        std::fs::write(&src, &payload).unwrap();

        let staging = dir.path().join("update").join("staging");
        let dest = copy_into_staging_blocking(&src, &staging, "pkg.zip").unwrap();
        assert_eq!(dest, staging.join("pkg.zip"));
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }

    /// 上传包超限：拒绝且不留半写入的目标文件
    #[test]
    fn test_copy_into_staging_rejects_oversize() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("huge.tmp");
        // 稀疏写入超过上限：用 set_len 造一个 >512MB 的文件（不实际占盘）
        let f = std::fs::File::create(&src).unwrap();
        f.set_len(MAX_UPDATE_ARCHIVE_BYTES + 1).unwrap();
        drop(f);

        let staging = dir.path().join("update").join("staging");
        let err = copy_into_staging_blocking(&src, &staging, "pkg.zip").expect_err("超限应拒绝");
        assert!(
            matches!(err, UpdaterError::DownloadTooLarge { .. }),
            "{err}"
        );
        assert!(
            !staging.join("pkg.zip").exists(),
            "超限时半写入的目标文件必须清掉"
        );
    }
}
