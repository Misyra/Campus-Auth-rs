//! bridge 单测共享辅助：测试用 Supervisor 构造、断言与标准成功响应

use std::sync::Arc;

use serde_json::Value;

use super::session::check_session_compat;
use super::{BridgeError, BridgeSupervisor, IpcResponse, SessionType};

/// 断言 `check_session_compat` 在给定会话与方法下返回兼容（Ok）
pub(super) fn assert_ok(current: Option<SessionType>, method: &str) {
    assert!(
        check_session_compat(current, method).is_ok(),
        "{current:?} + {method} 应兼容",
    );
}

/// 断言 `check_session_compat` 在给定会话与方法下返回 [`BridgeError::WorkerBusy`]
pub(super) fn assert_busy(current: Option<SessionType>, method: &str) {
    assert!(
        matches!(
            check_session_compat(current, method),
            Err(BridgeError::WorkerBusy)
        ),
        "{current:?} + {method} 应忙碌",
    );
}

/// 构造测试用 BridgeSupervisor（临时目录，不 spawn 后台 task）。
/// 同时返回 TempDir 供调用方保活（避免 Windows 下目录提前删除影响配置读取）。
pub(super) async fn make_bridge() -> (Arc<BridgeSupervisor>, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let (reload_tx, _reload_rx) = tokio::sync::mpsc::channel(4);
    let config = crate::config::ConfigService::new(dir.path().to_path_buf(), reload_tx)
        .await
        .expect("ConfigService 构造失败");
    let status = Arc::new(crate::status::StatusManager::new());
    (
        BridgeSupervisor::new(dir.path().to_path_buf(), config, status, None),
        dir,
    )
}

/// 标准成功响应（id=1，success=true，data 为 Null）
pub(super) fn ok_resp() -> Result<IpcResponse, BridgeError> {
    Ok(IpcResponse {
        id: 1,
        result: crate::bridge::IpcResult {
            success: true,
            data: Value::Null,
            error: None,
        },
    })
}
