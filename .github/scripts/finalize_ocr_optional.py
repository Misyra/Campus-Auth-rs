from pathlib import Path


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    source = path.read_text(encoding="utf-8")
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: 预期 1 个锚点，实际 {count}")
    path.write_text(source.replace(old, new, 1), encoding="utf-8")
    print(f"{label}: ok")

uv = Path("src/environment/uv.rs")
replace_once(
    uv,
    '''    let marker = ocr_marker_path(mgr);
    tokio::fs::write(&marker, b"enabled")
        .await
        .map_err(EnvironmentError::UvExtractFailed)?;

    if let Err(error) = run_uv_sync_with_ocr(mgr, true).await {
        let _ = tokio::fs::remove_file(&marker).await;
        return Err(error);
    }
''',
    '''    let marker = ocr_marker_path(mgr);
    let had_marker = marker.is_file();
    if !had_marker {
        tokio::fs::write(&marker, b"enabled")
            .await
            .map_err(EnvironmentError::UvExtractFailed)?;
    }

    if let Err(error) = run_uv_sync_with_ocr(mgr, true).await {
        if !had_marker {
            let _ = tokio::fs::remove_file(&marker).await;
        }
        return Err(error);
    }
''',
    "preserve existing marker on install failure",
)

mod = Path("src/environment/mod.rs")
replace_once(
    mod,
    '''    /// OCR 依赖（ddddocr）不在此自动补装：由前端显式"安装/卸载"经
    /// `uv add/remove ddddocr` 管理，避免自动化与显式卸载互相冲突。
''',
    '''    /// OCR 依赖由前端显式安装/卸载；用户启用后会写入持久标记，后续环境
    /// 修复通过 `uv sync --extra ocr` 保留该选择，未启用时只同步基础依赖。
''',
    "environment manager docs",
)

bootstrap = Path("src/environment/bootstrap.rs")
replace_once(
    bootstrap,
    '''    // ── 阶段 2: 确保 Python 虚拟环境就绪 ──
    // 仅创建 venv（基础依赖）；OCR 依赖由前端显式管理，不在此自动补装。
''',
    '''    // ── 阶段 2: 确保 Python 虚拟环境就绪 ──
    // OCR 是否随 venv 同步由用户持久启用标记决定；未启用时仅安装基础依赖。
''',
    "bootstrap docs",
)
