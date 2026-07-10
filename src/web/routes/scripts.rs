//! 脚本路由：脚本管理、可执行文件列表、Shell 列表

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/scripts — 列出全部脚本（复用任务列表）
pub async fn list_scripts(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let tasks = state.container.tasks.list_all_tasks().await;
    Ok(data(tasks))
}

#[derive(Deserialize)]
pub struct RunScriptBody {
    /// 脚本任务 ID
    pub task_id: Option<String>,
    /// 直接传入脚本内容（与 task_id 二选一）
    pub script: Option<String>,
}

/// POST /api/scripts/run — 运行脚本
///
/// 支持两种调用方式：
/// - `{"task_id": "<task_id>"}`：运行已保存的脚本任务
/// - `{"script": "<content>"}`：直接运行临时脚本内容
pub async fn run_script(
    State(state): State<AppState>,
    Json(body): Json<RunScriptBody>,
) -> Result<Json<Value>, ApiError> {
    let task = if let Some(id) = body.task_id.as_deref() {
        state.container.tasks.load_task(id).await?
    } else if let Some(content) = body.script {
        crate::tasks::TaskKind::Script(crate::tasks::ScriptTaskConfig {
            common: crate::tasks::CommonFields {
                task_id: "adhoc_script".into(),
                name: "临时脚本".into(),
                description: String::new(),
            },
            content: Some(content),
            ..Default::default()
        })
    } else {
        return Err(ApiError::BadRequest("缺少 task_id 或 script 字段".into()));
    };
    let result = state.container.executor.execute(&task).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/scripts/binaries — 可用可执行文件列表
///
/// 扫描系统 PATH 中的常用解释器/可执行文件。
pub async fn list_binaries(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let python_path = state.container.environment.python_path().to_string_lossy().to_string();
    let mut binaries = vec![serde_json::json!({ "name": "python", "path": python_path })];

    // 扫描常见系统可执行文件（Windows + Unix 兼容）
    #[cfg(target_os = "windows")]
    for (name, exe) in [
        ("node", "node.exe"),
        ("powershell", "powershell.exe"),
        ("pwsh", "pwsh.exe"),
        ("git", "git.exe"),
    ] {
        if let Some(path) = find_in_path(exe) {
            binaries.push(serde_json::json!({
                "name": name,
                "path": path,
            }));
        }
    }
    #[cfg(not(target_os = "windows"))]
    for (name, exe) in [
        ("node", "node"),
        ("bash", "bash"),
        ("git", "git"),
        ("curl", "curl"),
        ("wget", "wget"),
    ] {
        if let Some(path) = find_in_path(exe) {
            binaries.push(serde_json::json!({
                "name": name,
                "path": path,
            }));
        }
    }
    Ok(data(binaries))
}

/// 在系统 PATH 中查找可执行文件
fn find_in_path(exe_name: &str) -> Option<String> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(exe_name);
            if full.is_file() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// GET /api/scripts/{task_id} — 获取脚本内容
pub async fn get_script(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if let Some(path) = state.container.tasks.get_script_path(&task_id).await {
        let content = tokio::fs::read_to_string(&path).await?;
        return Ok(data(serde_json::json!({ "id": task_id, "content": content })));
    }
    Err(ApiError::NotFound(format!("脚本 {} 不存在", task_id)))
}

/// PUT /api/scripts/{task_id} — 更新脚本
pub async fn update_script(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let task: crate::tasks::TaskKind = serde_json::from_value(body)?;
    state.container.tasks.save_task(&task_id, &task).await?;
    Ok(data(task_id))
}

/// DELETE /api/scripts/{task_id} — 删除脚本
pub async fn delete_script(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.container.tasks.delete_task(&task_id).await?;
    Ok(data(Value::String("ok".into())))
}

/// GET /api/shells — Shell 列表
///
/// 返回系统可用 Shell（用于 Shell 任务执行）。
pub async fn list_shells(
    State(_state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    #[cfg(target_os = "windows")]
    {
        Ok(data(serde_json::json!({
            "shells": [
                { "name": "PowerShell", "path": "powershell.exe" },
                { "name": "CMD", "path": "cmd.exe" }
            ],
            "default": "powershell.exe"
        })))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(data(serde_json::json!({
            "shells": [
                { "name": "bash", "path": "/bin/bash" },
                { "name": "sh", "path": "/bin/sh" }
            ],
            "default": "/bin/bash"
        })))
    }
}
