//! Axum 服务器构建与按需启停
//!
//! - `build_router()`：组装 CORS / gzip / 路由 / WebSocket / 静态文件
//! - `prepare_axum_listener()`：真实绑定首选端口，冲突或 Windows 保留端口时由系统分配回退端口
//! - `start_axum()`：组装 Router → serve → 记录运行端口
//! - `stop_axum()`：优雅关闭

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::container::ServiceContainer;
use crate::web::state::{AppState, LogEntry};

/// WebSocket 通用事件通道容量
pub const WS_EVENT_CAPACITY: usize = 1024;

/// 默认监听端口
pub const DEFAULT_PORT: u16 = 50721;
/// 默认绑定地址（本地回环，Docker 环境由 launcher 覆盖为 0.0.0.0）
pub const BIND_ADDR: [u8; 4] = [127, 0, 0, 1];
/// Docker 默认绑定地址
pub const DOCKER_BIND_ADDR: [u8; 4] = [0, 0, 0, 0];
// 运行端口记录文件名（相对于 config/）：单一事实源见 `utils::paths`，此处 re-export 保持调用路径稳定。
pub use crate::utils::paths::RUNTIME_PORT_FILE;

/// 解析绑定地址字符串为 `IpAddr`
///
/// 支持 `127.0.0.1` / `0.0.0.0` / `::` 等格式，解析失败则回退到
/// `BIND_ADDR`（127.0.0.1）。
pub fn parse_bind_addr(host: &str) -> std::net::IpAddr {
    use std::net::IpAddr;
    use std::str::FromStr;
    IpAddr::from_str(host).unwrap_or_else(|_| IpAddr::from(BIND_ADDR))
}

/// 判断是否运行在 Docker 容器内
///
/// 通过 `/.dockerenv` 文件或 `CAMPUS_AUTH_DOCKER` / `DOCKER_CONTAINER` 环境变量判断。
pub fn is_docker_env() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        || std::env::var("CAMPUS_AUTH_DOCKER").is_ok()
        || std::env::var("DOCKER_CONTAINER").is_ok()
}

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

/// 已成功绑定、等待装配 Router 的 Axum 监听器
///
/// 完整模式在初始化服务容器前先持有该监听器，避免端口不可用时启动整套后台服务；
/// 轻量模式则在用户首次打开控制台时按需创建。
pub struct PreparedAxumListener {
    listener: TcpListener,
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

    // CORS 与 gzip 均由内层 `web::build_router` 统一处理（历史遗留 #16）：
    // 此处不再叠加 CompressionLayer，避免双层 gzip 判定（外层因 `Content-Encoding`
    // 已存在而退化为 Identity，但仍多一次 `should_compress` 开销）。
    Ok(crate::web::build_router(state))
}

/// 判断绑定错误是否可通过改用其他端口恢复
///
/// Windows 的 WinNAT / Hyper-V excluded port range 常返回 WSAEACCES(10013)，
/// 此时端口可能没有进程监听，但仍不能绑定；与真正的 AddrInUse 一样应换端口。
fn is_recoverable_port_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AddrInUse
        || (cfg!(windows) && error.raw_os_error() == Some(10013))
}

/// 绑定 Axum 监听器
///
/// 首次直接绑定用户配置的端口。回环地址的端口若被占用，或 Windows 将其划入
/// WinNAT / Hyper-V 保留区（10013），则绑定端口 0，让内核原子选择可用端口；
/// 不做“先探测再绑定”，避免探测与使用之间被其他进程抢占的 TOCTOU 竞态。
/// 非回环地址（如 Docker/LAN 的 `0.0.0.0`）保持固定端口语义，失败时明确报错，
/// 避免容器端口映射或外部客户端在不知情时失配。
pub async fn prepare_axum_listener(
    port: u16,
    host: Option<&str>,
) -> anyhow::Result<PreparedAxumListener> {
    let bind_ip = match host {
        Some(h) if !h.is_empty() => parse_bind_addr(h),
        _ => {
            if is_docker_env() {
                std::net::IpAddr::from(DOCKER_BIND_ADDR)
            } else {
                std::net::IpAddr::from(BIND_ADDR)
            }
        }
    };
    info!(%bind_ip, requested_port = port, "准备 Axum 监听端口");

    let requested_addr = SocketAddr::new(bind_ip, port);
    let listener = match TcpListener::bind(requested_addr).await {
        Ok(listener) => listener,
        Err(primary_error)
            if port != 0 && bind_ip.is_loopback() && is_recoverable_port_error(&primary_error) =>
        {
            let fallback_addr = SocketAddr::new(bind_ip, 0);
            let listener = TcpListener::bind(fallback_addr).await.map_err(|fallback_error| {
                anyhow::anyhow!(
                    "Axum 请求端口 {requested_addr} 不可用（{primary_error}），系统自动分配端口也失败: {fallback_error}"
                )
            })?;
            let fallback_port = listener.local_addr()?.port();
            warn!(
                requested_port = port,
                actual_port = fallback_port,
                error = %primary_error,
                "当前端口不可用，已随机选择可用端口"
            );
            listener
        }
        Err(error) => anyhow::bail!("Axum 绑定 {requested_addr} 失败: {error}"),
    };
    let actual_port = listener.local_addr()?.port();
    debug!(%bind_ip, requested_port = port, actual_port, "Axum 监听器绑定成功");
    Ok(PreparedAxumListener {
        listener,
        port: actual_port,
    })
}

/// 使用已绑定监听器启动 Axum 服务器
///
/// 成功后将实际监听端口写入 `config/.runtime_port`。
pub fn start_axum_with_listener(
    container: Arc<ServiceContainer>,
    log_tx: broadcast::Sender<LogEntry>,
    prepared: PreparedAxumListener,
) -> anyhow::Result<AxumServeHandle> {
    let PreparedAxumListener { listener, port } = prepared;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
    let router = build_router(container.clone(), log_tx, shutdown_tx)?;

    // Router 构建成功后才发布运行端口，避免 auth token 等初始化失败时留下
    // “已有 Web 服务”的陈旧端口记录。
    let port_path = crate::utils::paths::runtime_port_path(&container.config.base_path());
    if let Err(e) = std::fs::write(&port_path, port.to_string()) {
        warn!(path = %port_path.display(), error = %e, "写入运行端口文件失败");
    }

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

    Ok(AxumServeHandle {
        handle,
        stop_tx,
        shutdown_rx,
        port,
    })
}

/// 绑定并启动 Axum 服务器
///
/// `host` 为绑定地址字符串（如 `127.0.0.1` / `0.0.0.0`），为空则根据环境自动选择。
/// 完整模式优先拆用 [`prepare_axum_listener`] / [`start_axum_with_listener`]，以便在
/// 初始化后台服务前确定端口；轻量模式按需启动可直接调用本函数。
pub async fn start_axum(
    container: Arc<ServiceContainer>,
    log_tx: broadcast::Sender<LogEntry>,
    port: u16,
    host: Option<&str>,
) -> anyhow::Result<AxumServeHandle> {
    let prepared = prepare_axum_listener(port, host).await?;
    start_axum_with_listener(container, log_tx, prepared)
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
#[cfg(test)]
mod tests {
    use super::*;

    /// 绑定地址解析：合法输入直通，非法/空回退回环（配置笔误不导致监听失败）
    #[test]
    fn test_parse_bind_addr_valid_and_fallback() {
        assert_eq!(
            parse_bind_addr("127.0.0.1"),
            std::net::IpAddr::from(BIND_ADDR)
        );
        assert_eq!(
            parse_bind_addr("0.0.0.0"),
            std::net::IpAddr::from(DOCKER_BIND_ADDR)
        );
        assert!(parse_bind_addr("::").is_unspecified());
        assert_eq!(
            parse_bind_addr("not-an-ip"),
            std::net::IpAddr::from(BIND_ADDR)
        );
        assert_eq!(parse_bind_addr(""), std::net::IpAddr::from(BIND_ADDR));
    }

    /// 端口占用时不扫描相邻端口，直接由内核原子选择可用端口
    #[tokio::test]
    async fn test_prepare_listener_falls_back_from_occupied_port() {
        let occupied = std::net::TcpListener::bind((std::net::Ipv4Addr::from(BIND_ADDR), 0))
            .expect("占用测试端口");
        let requested = occupied.local_addr().expect("读取测试端口").port();

        let prepared = prepare_axum_listener(requested, Some("127.0.0.1"))
            .await
            .expect("应回退到系统分配端口");

        assert_ne!(prepared.port, requested);
        assert!(prepared.port > 0);
    }

    /// Windows WinNAT / Hyper-V 保留端口错误属于可恢复绑定错误
    #[test]
    fn test_recoverable_port_error_classification() {
        let occupied = std::io::Error::new(std::io::ErrorKind::AddrInUse, "occupied");
        assert!(is_recoverable_port_error(&occupied));

        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert!(!is_recoverable_port_error(&denied));

        #[cfg(windows)]
        assert!(is_recoverable_port_error(
            &std::io::Error::from_raw_os_error(10013)
        ));

        assert_eq!(DEFAULT_PORT, 50721);
        assert_eq!(WS_EVENT_CAPACITY, 1024);
    }
}
