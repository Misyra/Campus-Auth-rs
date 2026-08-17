//! OCR 路由：OCR 识别（通过 Bridge 执行）

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// POST /api/ocr/recognize — 执行 OCR 识别
pub async fn ocr_recognize(
    State(state): State<AppState>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 注入 cancel_id="ocr"，使 /api/ocr/uninstall 的 bridge.cancel("ocr") 能命中
    // execute_inner 注册的取消令牌（execute_inner 从 params["cancel_id"] 读取并注册）
    if let Some(obj) = body.as_object_mut() {
        obj.insert("cancel_id".into(), Value::String("ocr".into()));
    }
    let result = state.container.bridge.execute("ocr_recognize", body).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/ocr/status — 获取 OCR 状态
///
/// 返回 `installed`（OCR 运行环境是否就绪）与 `size_mb`（环境目录估算体积）。
pub async fn ocr_status(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let env = state.container.environment.status();
    let installed = env.python_ready && env.playwright_ready;
    // 统计 environment 目录大小（递归）
    let env_dir = state.config.base_path().join("environment");
    let size_bytes = if env_dir.exists() {
        // dir_size 递归遍历文件系统（同步阻塞），用 spawn_blocking 避免阻塞 async 运行时
        tokio::task::spawn_blocking(move || dir_size(&env_dir))
            .await
            .unwrap_or(0)
    } else {
        0
    };
    let status = serde_json::json!({
        "installed": installed,
        "size_mb": (size_bytes as f64 / (1024.0 * 1024.0)).round(),
    });
    Ok(data(status))
}

/// 递归统计目录大小
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_size(&p);
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// POST /api/ocr/uninstall — 卸载 OCR（取消在途任务并释放资源）
pub async fn ocr_uninstall(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    state.container.bridge.cancel("ocr");
    Ok(data(Value::String("ok".into())))
}

/// POST /api/ocr/install — 触发 OCR 环境安装
///
/// 后台执行环境能力安装（uv/Python/Playwright），进度通过 StatusManager 推送。
pub async fn ocr_install(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let env = state.container.environment.clone();
    tokio::spawn(async move {
        if let Err(e) = env.ensure_capability().await {
            tracing::error!("OCR 环境安装失败: {e}");
        }
    });
    Ok(data(serde_json::json!({
        "message": "OCR 环境安装已启动",
    })))
}
