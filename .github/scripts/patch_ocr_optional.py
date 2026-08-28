from pathlib import Path
import re


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    source = path.read_text(encoding="utf-8")
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: 预期 1 个锚点，实际 {count}")
    path.write_text(source.replace(old, new, 1), encoding="utf-8")
    print(f"{label}: ok")


def regex_once(path: Path, pattern: str, replacement: str, label: str) -> None:
    source = path.read_text(encoding="utf-8")
    updated, count = re.subn(pattern, replacement, source, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: 预期 1 个锚点，实际 {count}")
    path.write_text(updated, encoding="utf-8")
    print(f"{label}: ok")


pyproject = Path("python_worker/pyproject.toml")
replace_once(
    pyproject,
    'dependencies = [\n    "ddddocr>=1.6.1",\n    "playwright>=1.40",\n]\n\n# 开发/测试依赖',
    'dependencies = [\n    "playwright>=1.40",\n]\n\n# OCR 为显式可选能力，默认环境不安装 ddddocr/ONNX/OpenCV。\n[project.optional-dependencies]\nocr = [\n    "ddddocr>=1.6.1",\n]\n\n# 开发/测试依赖',
    "pyproject optional ocr",
)

python_rs = Path("src/environment/python.rs")
replace_once(
    python_rs,
    '/// 检查 `.venv` 目录是否存在，不存在则执行 `uv sync` 创建（基础依赖，\n/// 不含 ddddocr）。OCR 依赖（ddddocr）由前端的显式"安装/卸载"操作经\n/// `uv add/remove ddddocr` 管理（见 environment::uv::install_ocr_dep /\n/// remove_ocr_dep），此处不做自动补装，避免显式卸载后又被自动装回。',
    '/// 检查 `.venv` 目录是否存在，不存在则执行 `uv sync` 创建。OCR 依赖\n/// （ddddocr）属于 `ocr` extra；是否随环境修复安装由 environment/ocr.enabled\n/// 持久标记决定，显式安装/卸载不会再修改 pyproject.toml。',
    "ensure_venv docs",
)

ocr_declared_block = '''/// OCR 依赖（ddddocr）是否在 `python_worker/pyproject.toml` 中声明
///
/// 作为 OCR 可用性的权威来源：仅当项目声明了该依赖，前端才展示「安装/卸载」
/// 入口与识别能力。文件缺失或读取失败时返回 false，避免把损坏的环境误报为可用。
pub(crate) fn ocr_declared(mgr: &EnvironmentManager) -> bool {
    let pyproject = mgr.worker_project_path().join("pyproject.toml");
    let content = match std::fs::read_to_string(&pyproject) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // 简单稳健的判定：依赖列表中出现 ddddocr（>=1.6.1 之类约束），无需完整 TOML 解析
    let mut in_deps = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with("dependencies") {
            in_deps = true;
            continue;
        }
        // 进入其它顶层表头（如 [build-system] / [tool.*]）则退出依赖块
        if in_deps && line.starts_with('[') {
            in_deps = false;
        }
        if in_deps && line.contains("ddddocr") {
            return true;
        }
    }
    false
}
'''
ocr_declared_new = '''/// OCR extra 是否在 `python_worker/pyproject.toml` 中声明 ddddocr。
///
/// `declared` 表示该构建支持 OCR 可选能力，并不等于用户已安装 OCR。
/// 文件缺失或声明损坏时返回 false，避免前端误报能力。
pub(crate) fn ocr_declared(mgr: &EnvironmentManager) -> bool {
    let pyproject = mgr.worker_project_path().join("pyproject.toml");
    let content = match std::fs::read_to_string(&pyproject) {
        Ok(c) => c,
        Err(_) => return false,
    };
    ocr_declared_in_pyproject(&content)
}

fn ocr_declared_in_pyproject(content: &str) -> bool {
    let mut in_optional_dependencies = false;
    let mut in_ocr_extra = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_optional_dependencies = line == "[project.optional-dependencies]";
            in_ocr_extra = false;
            continue;
        }
        if !in_optional_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }

        if !in_ocr_extra {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if name.trim() != "ocr" {
                continue;
            }
            if value.contains("ddddocr") {
                return true;
            }
            in_ocr_extra = !value.contains(']');
            continue;
        }

        if line.contains("ddddocr") {
            return true;
        }
        if line.contains(']') {
            in_ocr_extra = false;
        }
    }

    false
}
'''
replace_once(python_rs, ocr_declared_block, ocr_declared_new, "ocr declaration parser")

old_test = '''    /// 不存在的解释器路径必须判定为不可用。
    #[tokio::test]
    async fn test_python_executable_works_rejects_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!python_executable_works(&dir.path().join("missing-python.exe")).await);
    }
'''
new_test = '''    /// 不存在的解释器路径必须判定为不可用。
    #[tokio::test]
    async fn test_python_executable_works_rejects_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!python_executable_works(&dir.path().join("missing-python.exe")).await);
    }

    #[test]
    fn test_ocr_declared_requires_ocr_optional_extra() {
        let optional = r#"
[project]
dependencies = ["playwright>=1.40"]

[project.optional-dependencies]
ocr = [
    "ddddocr>=1.6.1",
]
"#;
        assert!(ocr_declared_in_pyproject(optional));

        let legacy_main_dependency = r#"
[project]
dependencies = [
    "ddddocr>=1.6.1",
    "playwright>=1.40",
]
"#;
        assert!(!ocr_declared_in_pyproject(legacy_main_dependency));

        let unrelated_extra = r#"
[project.optional-dependencies]
devtools = ["ddddocr>=1.6.1"]
ocr = ["pillow"]
"#;
        assert!(!ocr_declared_in_pyproject(unrelated_extra));
    }
'''
replace_once(python_rs, old_test, new_test, "ocr declaration tests")

uv_rs = Path("src/environment/uv.rs")
uv_replacement = '''/// 用户显式启用 OCR 的持久标记。项目更新/venv 修复后仍按该偏好同步 `ocr` extra。
const OCR_ENABLED_MARKER: &str = "ocr.enabled";

fn ocr_marker_path(mgr: &EnvironmentManager) -> PathBuf {
    mgr.env_path().join(OCR_ENABLED_MARKER)
}

fn ocr_extra_enabled(mgr: &EnvironmentManager) -> bool {
    ocr_marker_path(mgr).is_file()
}

/// 执行 `uv sync` 安装 Python 虚拟环境。
///
/// 默认只同步基础依赖；用户显式安装过 OCR 时，根据 environment/ocr.enabled
/// 持久标记追加 `--extra ocr`，保证环境修复不会悄悄丢失用户选择。
pub async fn run_uv_sync(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    run_uv_sync_with_ocr(mgr, ocr_extra_enabled(mgr)).await
}

async fn run_uv_sync_with_ocr(
    mgr: &EnvironmentManager,
    include_ocr: bool,
) -> Result<(), EnvironmentError> {
    if !mgr.worker_project_path().exists() {
        return Err(EnvironmentError::WorkerProjectNotFound {
            path: mgr.worker_project_path().clone(),
        });
    }

    let uv_exe = uv_exe_path(mgr);
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;
    let venv_path = mgr.worker_project_path().join(crate::environment::VENV_DIR);

    let mut cmd = uv_command(&uv_exe);
    cmd.arg("sync")
        .arg("--project")
        .arg(&*mgr.worker_project_path().to_string_lossy());
    if include_ocr {
        cmd.arg("--extra").arg("ocr");
    }
    let cmd_future = cmd
        .env("UV_PROJECT_ENVIRONMENT", &venv_path)
        .current_dir(mgr.base_path())
        .output();

    let output = tokio::time::timeout(UV_SYNC_TIMEOUT, cmd_future)
        .await
        .map_err(|_| EnvironmentError::UvSyncTimeout {
            timeout_secs: UV_SYNC_TIMEOUT.as_secs(),
        })?
        .map_err(EnvironmentError::UvExtractFailed)?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(EnvironmentError::UvSyncFailed {
            exit_code: output.status.code(),
            stderr,
        })
    }
}

/// 安装 OCR 可选依赖，并持久记录用户启用偏好。
pub async fn install_ocr_dep(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;
    let marker = ocr_marker_path(mgr);
    tokio::fs::write(&marker, b"enabled")
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;

    if let Err(error) = run_uv_sync_with_ocr(mgr, true).await {
        let _ = tokio::fs::remove_file(&marker).await;
        return Err(error);
    }

    tracing::info!("OCR 可选依赖安装完成");
    Ok(())
}

/// 卸载 OCR 可选依赖，并清除用户启用偏好。
pub async fn remove_ocr_dep(mgr: &EnvironmentManager) -> Result<(), EnvironmentError> {
    let marker = ocr_marker_path(mgr);
    let had_marker = marker.is_file();
    if had_marker {
        tokio::fs::remove_file(&marker)
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;
    }

    if let Err(error) = run_uv_sync_with_ocr(mgr, false).await {
        if had_marker {
            let _ = tokio::fs::write(&marker, b"enabled").await;
        }
        return Err(error);
    }

    tracing::info!("OCR 可选依赖卸载完成");
    Ok(())
}

'''
regex_once(
    uv_rs,
    r'/// 执行 `uv sync` 安装 Python 虚拟环境与基础依赖（不含 OCR 可选依赖）。\n.*?(?=/// 构造 uv 下载 URL（zip）— 主站)',
    uv_replacement,
    "uv optional ocr sync",
)

uv_test_anchor = '    /// 5.4：uv --version 输出解析（含 Windows 可能的括号后缀）\n'
uv_test = '''    #[test]
    fn test_ocr_extra_enabled_uses_persistent_marker() {
        let dir = tempfile::tempdir().unwrap();
        let status = Arc::new(StatusManager::new());
        let mgr = EnvironmentManager::new(dir.path().to_path_buf(), status, false);
        assert!(!ocr_extra_enabled(&mgr));

        std::fs::create_dir_all(mgr.env_path()).unwrap();
        std::fs::write(ocr_marker_path(&mgr), b"enabled").unwrap();
        assert!(ocr_extra_enabled(&mgr));
    }

'''
replace_once(uv_rs, uv_test_anchor, uv_test + uv_test_anchor, "ocr marker test")

ocr_route = Path("src/web/routes/ocr.rs")
source = ocr_route.read_text(encoding="utf-8")
replacements = [
    (
        '- `declared`：项目是否在 `python_worker/pyproject.toml` 中声明了 ddddocr 依赖，\n///   作为「是否支持 OCR」的权威来源（用户要求依 pyproject.toml 判定）。',
        '- `declared`：项目是否在 `python_worker/pyproject.toml` 的 `ocr` optional extra\n///   中声明 ddddocr，表示当前构建支持 OCR 可选能力。',
        "ocr status docs",
    ),
    (
        '/// 取消在途 OCR 识别任务（bridge.cancel），并执行 `uv remove ddddocr`\n/// 移除 OCR 依赖（environment.remove_ocr_dep）。',
        '/// 取消在途 OCR 识别任务（bridge.cancel），并通过基础 `uv sync`\n/// 移除 OCR extra（environment.remove_ocr_dep）。',
        "ocr uninstall docs",
    ),
    (
        '/// 后台执行环境能力安装（uv/Python/Playwright）并显式 `uv add ddddocr`\n/// 补齐 OCR 依赖，进度通过 StatusManager 推送。',
        '/// 后台执行环境能力安装（uv/Python/Playwright）并显式同步 `ocr` extra，\n/// 补齐 OCR 依赖，进度通过 StatusManager 推送。',
        "ocr install docs",
    ),
    (
        '// 先确保核心能力就绪，再补装 OCR 依赖（uv add ddddocr）',
        '// 先确保核心能力就绪，再同步 OCR optional extra',
        "ocr install comment",
    ),
]
for old, new, label in replacements:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: 预期 1 个锚点，实际 {count}")
    source = source.replace(old, new, 1)
    print(f"{label}: ok")
ocr_route.write_text(source, encoding="utf-8")
