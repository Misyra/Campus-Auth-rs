//! Axum 服务器构建与按需启停
//!
//! - `build_router()`：组装 CORS / gzip / 路由 / WebSocket / 静态文件
//! - `start_axum()`：绑定端口（冲突 +1 重试，最多 5 次）→ serve → 记录运行端口
//! - `stop_axum()`：优雅关闭

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tower_http::compression::CompressionLayer;
use tracing::{debug, error, info, warn};

use crate::container::ServiceContainer;
use crate::web::state::{AppState, LogEntry};

/// WebSocket 通用事件通道容量
pub const WS_EVENT_CAPACITY: usize = 1024;

/// 默认监听端口
pub const DEFAULT_PORT: u16 = 50721;
/// 端口冲突重试上限
pub const PORT_RETRY_MAX: u16 = 5;
/// 绑定地址
pub const BIND_ADDR: [u8; 4] = [127, 0, 0, 1];
/// 运行端口记录文件名（相对于 config/）
pub const RUNTIME_PORT_FILE: &str = ".runtime_port";

/// Axum 服务器运行句柄
pub struct AxumServeHandle {
    /// tokio task 句柄
    pub handle: JoinHandle<()>,
    /// 停止信号发送端（drop 时触发优雅关闭）
    pub stop_tx: tokio::sync::watch::Sender<()>,
    /// 应用级关闭信号接收端（由 Web 路由 shutdown_app 触发，通知 launcher 优雅关闭流程）
    pub shutdown_rx: tokio::sync::watch::Receiver<()>,
    /// 实际监听端口
    pub port: u16,
}

/// 构建完整 Router（含中间件、State 注入、路由挂载）
pub fn build_router(
    container: Arc<ServiceContainer>,
    log_tx: broadcast::Sender<LogEntry>,
    shutdown_tx: tokio::sync::watch::Sender<()>,
) -> anyhow::Result<axum::Router> {
    // 通用 WebSocket 事件通道（screenshot / step_progress 等），供 Bridge 推送
    let (ws_tx, _) = broadcast::channel::<String>(WS_EVENT_CAPACITY);
    // 将事件通道注入 Bridge，由其转发 Worker 事件
    container.bridge.set_event_tx(ws_tx.clone());
    // 本地 API 鉴权 token：加载或生成并持久化到 config/.auth_token
    let auth_token = crate::web::auth::load_or_create_token(&container.config.base_path())?;
    let state = AppState::new(container, log_tx, ws_tx, shutdown_tx, auth_token.into());

    // CORS 由内层 `web::build_router` 统一处理（mirror_request 放行任意本地来源）。
    // 不再在此叠加白名单层，避免双层 CORS 挡住 vite dev / 局域网来源（历史遗留 #16）。
    let compression = CompressionLayer::new().gzip(true);

    Ok(crate::web::build_router(state).layer(compression))
}

/// 启动 Axum 服务器（端口冲突 +1 重试，最多 `PORT_RETRY_MAX` 次）
///
/// 成功后将实际监听端口写入 `config/.runtime_port`。
pub async fn start_axum(
    container: Arc<ServiceContainer>,
    log_tx: broadcast::Sender<LogEntry>,
    port: u16,
) -> anyhow::Result<AxumServeHandle> {
    let mut bind_port = port;

    for attempt in 0..=PORT_RETRY_MAX {
        let addr = SocketAddr::from((BIND_ADDR, bind_port));
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                let actual_port = listener.local_addr()?.port();
                // launcher 已有"服务已启动"类 info，绑定成功降为 debug 防重复播报
                debug!(port = actual_port, "Axum 服务绑定成功");

                // 写入运行端口记录
                let port_path = container
                    .config
                    .base_path()
                    .join("config")
                    .join(RUNTIME_PORT_FILE);
                if let Err(e) = std::fs::write(&port_path, actual_port.to_string()) {
                    warn!("写入运行端口文件失败: {e}");
                }

                let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
                let router = build_router(container, log_tx, shutdown_tx)?;
                let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(());

                let handle = tokio::spawn(async move {
                    let server = axum::serve(listener, router.into_make_service());
                    let result = server
                        .with_graceful_shutdown(async move {
                            let _ = stop_rx.changed().await;
                        })
                        .await;
                    if let Err(e) = result {
                        error!("Axum 服务异常退出: {e}");
                    }
                });

                return Ok(AxumServeHandle {
                    handle,
                    stop_tx,
                    shutdown_rx,
                    port: actual_port,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if attempt < PORT_RETRY_MAX {
                    warn!(
                        port = bind_port,
                        "端口被占用，尝试 {} ({} / {})",
                        bind_port + 1,
                        attempt + 1,
                        PORT_RETRY_MAX
                    );
                    if bind_port == u16::MAX {
                        anyhow::bail!("端口已到上限 65535");
                    }
                    bind_port += 1;
                } else {
                    anyhow::bail!(
                        "端口 {}~{} 均被占用（重试 {} 次后放弃）",
                        port,
                        bind_port,
                        PORT_RETRY_MAX
                    );
                }
            }
            Err(e) => {
                anyhow::bail!("Axum 绑定失败: {e}");
            }
        }
    }

    unreachable!("端口重试循环应已返回或 bail")
}

/// 优雅关闭 Axum 服务器
///
/// 发送停止信号并等待 serve task 退出；超时则真正 `abort()` 挂起的 task，
/// 避免 task 常驻泄漏（历史遗留 #18：原实现超时后仅记日志、未中止）。
pub async fn stop_axum(mut handle: AxumServeHandle) {
    let _ = handle.stop_tx.send(());
    match tokio::time::timeout(std::time::Duration::from_secs(5), &mut handle.handle).await {
        Ok(Ok(())) => info!("Axum 服务已关闭"),
        Ok(Err(e)) => warn!("Axum 服务关闭时 task 异常: {e}"),
        Err(_) => {
            warn!("Axum 关闭超时，强制 abort 挂起的 serve task");
            handle.handle.abort();
        }
    }
}
