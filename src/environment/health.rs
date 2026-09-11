//! Python Worker 运行时探测、依赖指纹与可选能力偏好

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::environment::{EnvironmentError, EnvironmentManager, RESYNC_MARKER};

/// Rust 控制平面当前要求的 Worker 协议版本。
pub const EXPECTED_WORKER_VERSION: &str = "1.0.0";
const WORKER_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const RUNTIME_STATE_FILE: &str = "python-runtime-state.json";
const PREFERENCES_FILE: &str = "python-preferences.json";
const STATE_SCHEMA_VERSION: u32 = 1;

/// Worker import 探针结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimeProbe {
    /// Playwright 与 Worker 主模块是否均可导入，且版本符合控制平面要求。
    pub ready: bool,
    /// Worker 实际上报的版本；导入失败时为空。
    pub version: Option<String>,
    /// 不可用时的可诊断原因。
    pub error: Option<String>,
}

/// 已验证依赖清单与当前工程清单的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestState {
    /// 上次成功验证的依赖指纹与当前一致。
    Current,
    /// 尚无验证记录，可在 import 探针通过后直接认领。
    Missing,
    /// 清单已变化或状态文件损坏，必须重新同步并验证。
    Stale,
}

#[derive(Debug, Serialize, Deserialize)]
struct RuntimeState {
    schema_version: u32,
    fingerprint: String,
    worker_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PythonPreferences {
    schema_version: u32,
    ocr_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct ProbePayload {
    version: String,
}

/// 实际运行 venv Python，验证 Playwright 包与 Worker 模块都能导入。
pub async fn probe_worker_runtime(mgr: &EnvironmentManager) -> WorkerRuntimeProbe {
    let python_exe = mgr.python_path();
    if !python_exe.is_file() {
        return failed_probe("Python 解释器文件不存在");
    }

    let script = concat!(
        "import json\n",
        "import playwright\n",
        "import worker_main\n",
        "from playwright_worker import WORKER_VERSION\n",
        "print(json.dumps({'version': WORKER_VERSION}))\n"
    );
    let mut cmd = tokio::process::Command::new(&python_exe);
    cmd.kill_on_drop(true)
        .arg("-c")
        .arg(script)
        .current_dir(mgr.worker_project_path())
        .env("PYTHONUTF8", "1");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match tokio::time::timeout(WORKER_PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let payload = stdout
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .and_then(|line| serde_json::from_str::<ProbePayload>(line).ok());
            match payload {
                Some(payload) if payload.version == EXPECTED_WORKER_VERSION => WorkerRuntimeProbe {
                    ready: true,
                    version: Some(payload.version),
                    error: None,
                },
                Some(payload) => WorkerRuntimeProbe {
                    ready: false,
                    error: Some(format!(
                        "Worker 版本不匹配：需要 {EXPECTED_WORKER_VERSION}，实际 {}",
                        payload.version
                    )),
                    version: Some(payload.version),
                },
                None => failed_probe("Worker 探针没有返回有效版本"),
            }
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            failed_probe(format!(
                "Worker import 探针失败（退出码 {:?}）：{}",
                output.status.code(),
                crate::environment::python::tail_chars(stderr.trim(), 500)
            ))
        }
        Ok(Err(error)) => failed_probe(format!("Worker import 探针无法启动：{error}")),
        Err(_) => failed_probe(format!(
            "Worker import 探针超时（>{}s）",
            WORKER_PROBE_TIMEOUT.as_secs()
        )),
    }
}

fn failed_probe(reason: impl Into<String>) -> WorkerRuntimeProbe {
    WorkerRuntimeProbe {
        ready: false,
        version: None,
        error: Some(reason.into()),
    }
}

/// 读取上次已验证的依赖指纹状态。
pub fn runtime_manifest_state(mgr: &EnvironmentManager) -> Result<ManifestState, EnvironmentError> {
    let expected = runtime_fingerprint(mgr)?;
    let path = runtime_state_path(mgr);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ManifestState::Missing);
        }
        Err(source) => return Err(EnvironmentError::StateIo { path, source }),
    };
    let state: RuntimeState = match serde_json::from_slice(&bytes) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(path = %path.display(), "Python 运行时状态文件损坏，将重新同步: {error}");
            return Ok(ManifestState::Stale);
        }
    };
    if state.schema_version == STATE_SCHEMA_VERSION
        && state.worker_version == EXPECTED_WORKER_VERSION
        && state.fingerprint == expected
    {
        Ok(ManifestState::Current)
    } else {
        Ok(ManifestState::Stale)
    }
}

/// 记录当前清单已通过 Worker import 与版本验证。
pub async fn record_verified_runtime(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let state = RuntimeState {
        schema_version: STATE_SCHEMA_VERSION,
        fingerprint: runtime_fingerprint(mgr)?,
        worker_version: EXPECTED_WORKER_VERSION.to_string(),
    };
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(|source| EnvironmentError::StateIo {
            path: mgr.env_path().clone(),
            source,
        })?;
    let path = runtime_state_path(mgr);
    crate::utils::atomic_write_json(&path, &state)
        .map_err(|source| EnvironmentError::StateIo { path, source })
}

/// 确保 Worker 核心依赖可导入；清单变化、显式修复或探针失败时执行 uv sync。
pub async fn ensure_worker_runtime(
    mgr: &EnvironmentManager,
    cancel: &CancellationToken,
    force_sync: bool,
) -> Result<(), EnvironmentError> {
    if cancel.is_cancelled() {
        return Err(EnvironmentError::Cancelled);
    }
    let resync_pending = mgr.worker_project_path().join(RESYNC_MARKER).is_file();
    let manifest_state = runtime_manifest_state(mgr).unwrap_or_else(|error| {
        // uv.lock 缺失本身正是需要 sync 的状态，不能在修复动作之前先报错退出。
        tracing::info!("依赖指纹暂不可计算，将通过 uv sync 重建: {error}");
        ManifestState::Stale
    });
    let initial_probe = probe_worker_runtime(mgr).await;

    let can_adopt = !force_sync
        && !resync_pending
        && initial_probe.ready
        && manifest_state != ManifestState::Stale;
    if can_adopt {
        if manifest_state == ManifestState::Missing {
            record_verified_runtime(mgr).await?;
        }
        return Ok(());
    }

    if force_sync {
        tracing::info!("按用户请求强制重新同步 Python Worker 依赖");
    } else if resync_pending || manifest_state == ManifestState::Stale {
        tracing::info!("Python Worker 依赖清单已变化，执行 uv sync 对齐");
    } else if let Some(reason) = &initial_probe.error {
        tracing::warn!(reason = %reason, "Python 解释器可用，但 Worker 依赖不完整，执行 uv sync 自愈");
    }

    crate::environment::uv::run_uv_sync(mgr, cancel).await?;
    let verified = probe_worker_runtime(mgr).await;
    if !verified.ready {
        return Err(EnvironmentError::WorkerRuntimeInvalid {
            reason: verified
                .error
                .unwrap_or_else(|| "Worker import 探针未通过".to_string()),
        });
    }
    record_verified_runtime(mgr).await?;
    let marker = mgr.worker_project_path().join(RESYNC_MARKER);
    if let Err(error) = tokio::fs::remove_file(&marker).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %marker.display(), "依赖重同步标记清理失败: {error}");
    }
    Ok(())
}

/// 读取用户 OCR 启用偏好；首次升级时兼容旧 `ocr.enabled` 标记。
pub fn ocr_enabled(mgr: &EnvironmentManager) -> Result<bool, EnvironmentError> {
    let path = preferences_path(mgr);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(mgr.ocr_marker_path().is_file());
        }
        Err(source) => return Err(EnvironmentError::StateIo { path, source }),
    };
    let prefs: PythonPreferences =
        serde_json::from_slice(&bytes).map_err(|error| EnvironmentError::StateDataInvalid {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    Ok(prefs.schema_version == STATE_SCHEMA_VERSION && prefs.ocr_enabled)
}

/// 原子保存用户 OCR 启用偏好。
pub async fn set_ocr_enabled(
    mgr: &EnvironmentManager,
    enabled: bool,
) -> Result<(), EnvironmentError> {
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(|source| EnvironmentError::StateIo {
            path: mgr.env_path().clone(),
            source,
        })?;
    let path = preferences_path(mgr);
    let prefs = PythonPreferences {
        schema_version: STATE_SCHEMA_VERSION,
        ocr_enabled: enabled,
    };
    crate::utils::atomic_write_json(&path, &prefs)
        .map_err(|source| EnvironmentError::StateIo { path, source })
}

/// 判断部署副本的 pyproject 是否由 uv add 声明了 ddddocr。
pub fn ddddocr_declared(mgr: &EnvironmentManager) -> bool {
    let Ok(text) = std::fs::read_to_string(mgr.worker_project_path().join("pyproject.toml")) else {
        return false;
    };
    text.lines().any(|line| {
        let code = line
            .split('#')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        code.contains("\"ddddocr") || code.contains("'ddddocr")
    })
}

fn runtime_fingerprint(mgr: &EnvironmentManager) -> Result<String, EnvironmentError> {
    let mut hasher = Sha256::new();
    for name in ["pyproject.toml", "uv.lock"] {
        let path = mgr.worker_project_path().join(name);
        let bytes =
            std::fs::read(&path).map_err(|source| EnvironmentError::StateIo { path, source })?;
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    hasher.update(EXPECTED_WORKER_VERSION.as_bytes());
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(std::env::consts::ARCH.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

fn runtime_state_path(mgr: &EnvironmentManager) -> PathBuf {
    mgr.env_path().join(RUNTIME_STATE_FILE)
}

fn preferences_path(mgr: &EnvironmentManager) -> PathBuf {
    mgr.env_path().join(PREFERENCES_FILE)
}

/// 成功迁移旧标记后清理，失败时保留供下次重试。
pub async fn clear_legacy_ocr_marker(mgr: &EnvironmentManager) {
    let path = mgr.ocr_marker_path();
    if let Err(error) = tokio::fs::remove_file(&path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), "旧版 OCR 标记清理失败: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusManager;
    use std::sync::Arc;

    fn manager_with_project() -> (tempfile::TempDir, Arc<EnvironmentManager>) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("python_worker");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            "[project]\ndependencies = []\n",
        )
        .unwrap();
        std::fs::write(project.join("uv.lock"), "version = 1\n").unwrap();
        let mgr = EnvironmentManager::new(dir.path().to_path_buf(), Arc::new(StatusManager::new()));
        (dir, mgr)
    }

    #[tokio::test]
    async fn manifest_state_detects_change_after_verification() {
        let (_dir, mgr) = manager_with_project();
        assert_eq!(
            runtime_manifest_state(&mgr).unwrap(),
            ManifestState::Missing
        );
        record_verified_runtime(&mgr).await.unwrap();
        assert_eq!(
            runtime_manifest_state(&mgr).unwrap(),
            ManifestState::Current
        );
        std::fs::write(
            mgr.worker_project_path().join("pyproject.toml"),
            "[project]\ndependencies = [\"playwright\"]\n",
        )
        .unwrap();
        assert_eq!(runtime_manifest_state(&mgr).unwrap(), ManifestState::Stale);
    }

    #[tokio::test]
    async fn worker_probe_rejects_missing_interpreter() {
        let (_dir, mgr) = manager_with_project();
        let probe = probe_worker_runtime(&mgr).await;
        assert!(!probe.ready);
        assert!(
            probe
                .error
                .as_deref()
                .is_some_and(|reason| reason.contains("解释器文件不存在"))
        );
    }

    #[tokio::test]
    async fn ocr_preference_migrates_legacy_marker_and_persists_false() {
        let (_dir, mgr) = manager_with_project();
        std::fs::create_dir_all(mgr.env_path()).unwrap();
        std::fs::write(mgr.ocr_marker_path(), b"enabled").unwrap();
        assert!(ocr_enabled(&mgr).unwrap());
        set_ocr_enabled(&mgr, false).await.unwrap();
        assert!(!ocr_enabled(&mgr).unwrap());
    }

    #[test]
    fn ddddocr_declaration_ignores_comments() {
        let (_dir, mgr) = manager_with_project();
        let path = mgr.worker_project_path().join("pyproject.toml");
        std::fs::write(
            &path,
            "[project]\n# \"ddddocr>=1.6.1\"\ndependencies = []\n",
        )
        .unwrap();
        assert!(!ddddocr_declared(&mgr));
        std::fs::write(&path, "[project]\ndependencies = [\"ddddocr>=1.6.1\"]\n").unwrap();
        assert!(ddddocr_declared(&mgr));
    }
}
