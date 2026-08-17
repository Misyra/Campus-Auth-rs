//! 脚本路由：脚本管理、可执行文件列表、Shell 列表
//!
//! M1 细粒度 state：脚本 CRUD handler 声明 `State<Arc<dyn TaskApi>>` /
//! `State<Arc<dyn TaskRunApi>>` 依赖，可执行文件列表经
//! `State<Arc<dyn EnvironmentApi>>` 提取，不再触达 `state.container`。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::environment::EnvironmentApi;
use crate::tasks::{TaskApi, TaskRunApi};
use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// 校验脚本执行程序：仅允许 shell / bat / python / exe 四类，拒绝 PowerShell 等。
fn check_supported_binary(binary_path: Option<&str>, script_path: Option<&str>) -> Result<(), ApiError> {
    if let Some(bp) = binary_path {
        let lower = bp.to_lowercase();
        if lower.contains("powershell") || lower.contains("pwsh") || lower.ends_with(".ps1") {
            return Err(ApiError::BadRequest("不支持 PowerShell，仅支持 shell / bat / python / exe 四类脚本".into()));
        }
    }
    if let Some(sp) = script_path {
        let ext = std::path::Path::new(sp)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "ps1" {
            return Err(ApiError::BadRequest("不支持 .ps1 脚本，仅支持 shell / bat / python / exe 四类".into()));
        }
    }
    Ok(())
}

/// GET /api/scripts — 列出全部脚本（复用任务列表）
pub async fn list_scripts(
    State(tasks): State<Arc<dyn TaskApi>>,
) -> Result<Json<Value>, ApiError> {
    let tasks = tasks.list_all_tasks().await;
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
    State(tasks): State<Arc<dyn TaskApi>>,
    State(runner): State<Arc<dyn TaskRunApi>>,
    Json(body): Json<RunScriptBody>,
) -> Result<Json<Value>, ApiError> {
    let task = if let Some(id) = body.task_id.as_deref() {
        tasks.load_task(id).await?
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
    let result = runner.execute(&task).await?;
    Ok(data(serde_json::to_value(result)?))
}

/// GET /api/scripts/binaries — 可用可执行文件列表
///
/// 扫描系统 PATH 中的常用解释器/可执行文件。
pub async fn list_binaries(
    State(environment): State<Arc<dyn EnvironmentApi>>,
) -> Result<Json<Value>, ApiError> {
    let python_path = environment.python_path().to_string_lossy().to_string();
    let mut binaries = vec![serde_json::json!({ "name": "python", "path": python_path })];

    // 扫描常见系统可执行文件（Windows + Unix 兼容）。
    // 仅列出受支持的脚本解释器：shell / bat / python；powershell 不在支持范围。
    #[cfg(target_os = "windows")]
    for (name, exe) in [
        ("cmd", "cmd.exe"),
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
        ("bash", "bash"),
        ("sh", "sh"),
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

/// GET /api/scripts/{task_id} — 获取脚本完整内容
///
/// 返回编辑器所需的全部字段（name/description/content/binary_path 等）。
/// 内容来源两种存储模式：内联 `content` 字段或 `script_path` 指向的磁盘文件。
pub async fn get_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let task = tasks.load_task(&task_id).await?;
    let crate::tasks::TaskKind::Script(cfg) = task else {
        return Err(ApiError::NotFound(format!("脚本 {} 不存在", task_id)));
    };
    // script_path 模式：读取磁盘文件；内联模式：直接取 content 字段
    let content = if let Some(path) = tasks.get_script_path(&task_id).await {
        tokio::fs::read_to_string(&path).await?
    } else {
        cfg.content.clone().unwrap_or_default()
    };
    Ok(data(serde_json::json!({
        "id": task_id,
        "name": cfg.common.name,
        "description": cfg.common.description,
        "content": content,
        "binary_path": cfg.binary_path.clone().unwrap_or_default(),
        "script_path": cfg.script_path,
        "args": cfg.args,
        "timeout": cfg.timeout,
    })))
}

/// PUT /api/scripts/{task_id} — 更新脚本
///
/// 显式要求 `type == "script"`：`TaskKind` 反序列化在 type 缺失时默认归为
/// browser 任务，会把脚本负载静默转存为空的浏览器任务（脚本内容丢失），
/// 故在此前置拦截，防止前端回归。
pub async fn update_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if body.get("type").and_then(Value::as_str) != Some("script") {
        return Err(ApiError::BadRequest(
            "脚本接口仅接受 type=script 的负载".into(),
        ));
    }
    // 校验二进制与脚本路径：仅允许 shell / bat / python / exe，拒绝 PowerShell
    check_supported_binary(
        body.get("binary_path").and_then(Value::as_str),
        body.get("script_path").and_then(Value::as_str),
    )?;
    let task: crate::tasks::TaskKind = serde_json::from_value(body)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    tasks.save_task(&task_id, &task).await?;
    Ok(data(task_id))
}

/// DELETE /api/scripts/{task_id} — 删除脚本
pub async fn delete_script(
    State(tasks): State<Arc<dyn TaskApi>>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    tasks.delete_task(&task_id).await?;
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
