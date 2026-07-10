//! 监控路由：系统状态快照、网络测试

use axum::extract::State;
use axum::Json;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::engine::{EngineCommand, TestNetworkResult};
use crate::web::error::{data, ApiError};
use crate::web::state::AppState;

/// GET /api/monitor/status — 获取当前系统状态快照
pub async fn get_status(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let snapshot = state.container.status.borrow();
    Ok(data(serde_json::to_value(&snapshot)?))
}

/// GET /api/tools/network-interfaces — 列出真实网络接口
///
/// 调用平台对应的 NetworkDetect 实现返回有效网卡列表。
pub async fn list_network_interfaces(
    State(_state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let detector = crate::network::detect::create_detector();
    let interfaces = detector
        .list_interfaces()
        .await
        .map_err(|e| ApiError::Internal(format!("网络接口检测失败: {e}")))?;
    Ok(data(interfaces))
}

/// POST /api/monitor/test — 网络连通性测试
///
/// 派发 `EngineCommand::TestNetwork` 并等待 oneshot 回复（30s 超时）。
pub async fn test_network(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .container
        .engine_handle
        .engine
        .try_dispatch(EngineCommand::TestNetwork { reply: reply_tx })?;
    let result: TestNetworkResult = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        reply_rx,
    )
    .await
    .map_err(|_| ApiError::Internal("网络测试超时".into()))?
    .map_err(|_| ApiError::Internal("网络测试通道关闭".into()))?
    .map_err(|e| ApiError::Internal(format!("网络探测失败: {e}")))?;
    Ok(data(serde_json::to_value(&result)?))
}

/// POST /api/monitor/start — 启动网络监测
pub async fn start_monitor(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let _ = state
        .container
        .engine_handle
        .engine
        .try_dispatch(EngineCommand::Start);
    Ok(data(Value::String("监测已启动".into())))
}

/// POST /api/monitor/stop — 停止网络监测
pub async fn stop_monitor(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let _ = state
        .container
        .engine_handle
        .engine
        .try_dispatch(EngineCommand::Stop);
    Ok(data(Value::String("监测已停止".into())))
}
