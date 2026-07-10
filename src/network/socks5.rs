//! SOCKS5 转发器 + SocksGuard RAII

use std::net::Ipv4Addr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::network::detect::NetworkError;

/// SOCKS5 默认监听端口（复用配置模块的常量，避免重复来源）
pub use crate::config::DEFAULT_SOCKS5_PORT;
/// 端口被占用时 +1 重试次数上限
pub const SOCKS5_PORT_RETRY_MAX: u8 = 5;
/// SOCKS5 服务端绑定地址（仅本地）
pub const SOCKS5_BIND_ADDR: &str = "127.0.0.1";

/// SOCKS5 转发器 RAII 守卫，管理转发器 task 的生命周期
pub struct SocksGuard {
    stop_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
    port: u16,
    bind_addr: Ipv4Addr,
}

impl SocksGuard {
    /// 返回注入到 reqwest / 浏览器的代理地址
    pub fn addr(&self) -> String {
        format!("socks5://127.0.0.1:{}", self.port)
    }

    /// 转发器 task 是否仍在运行
    pub fn is_alive(&self) -> bool {
        !self.join_handle.is_finished()
    }

    /// 监听端口
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 绑定的本地网卡 IP
    pub fn bind_addr(&self) -> Ipv4Addr {
        self.bind_addr
    }
}

impl Drop for SocksGuard {
    fn drop(&mut self) {
        // 通知转发器 task 停止，并兜底终止
        let _ = self.stop_tx.send(true);
        self.join_handle.abort();
    }
}

/// SOCKS5 转发器核心，运行在独立 tokio task 中
pub struct SocksForwarder {
    listener: TcpListener,
    bind_addr: Ipv4Addr,
    stop_rx: watch::Receiver<bool>,
}

impl SocksForwarder {
    async fn run(mut self) {
        loop {
            tokio::select! {
                _ = self.stop_rx.changed() => {
                    if *self.stop_rx.borrow() {
                        break;
                    }
                }
                accepted = self.listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let bind = self.bind_addr;
                            tokio::spawn(async move {
                                let _ = handle_connection(stream, bind).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }
}

/// 启动 SOCKS5 转发器并返回 RAII 守卫
pub fn spawn_socks_guard(
    bind_addr: Ipv4Addr,
    preferred_port: u16,
) -> Result<SocksGuard, NetworkError> {
    let mut port = preferred_port;
    let mut retries: u8 = 0;
    let listener = loop {
        match std::net::TcpListener::bind((SOCKS5_BIND_ADDR, port)) {
            Ok(l) => break l,
            Err(_) if retries < SOCKS5_PORT_RETRY_MAX => {
                port += 1;
                retries += 1;
            }
            Err(_) => {
                return Err(NetworkError::Socks5PortBusy { port, retries });
            }
        }
    };
    let listener = TcpListener::from_std(listener).map_err(NetworkError::Io)?;
    let (stop_tx, stop_rx) = watch::channel(false);
    let forwarder = SocksForwarder {
        listener,
        bind_addr,
        stop_rx,
    };
    let join_handle = tokio::spawn(forwarder.run());
    Ok(SocksGuard {
        stop_tx,
        join_handle,
        port,
        bind_addr,
    })
}

/// 处理单个 SOCKS5 客户端连接（仅支持 CONNECT + IPv4/域名，无认证）
async fn handle_connection(mut client: TcpStream, bind_addr: Ipv4Addr) -> std::io::Result<()> {
    // 握手：VER NMETHODS METHODS
    let mut hdr = [0u8; 2];
    client.read_exact(&mut hdr).await?;
    if hdr[0] != 0x05 {
        return Ok(());
    }
    let nmethods = hdr[1] as usize;
    let mut methods = vec![0u8; nmethods];
    client.read_exact(&mut methods).await?;
    // 仅接受无认证（0x00）
    client.write_all(&[0x05, 0x00]).await?;

    // 请求：VER CMD RSV ATYP DST.ADDR DST.PORT
    let mut req = [0u8; 4];
    client.read_exact(&mut req).await?;
    if req[0] != 0x05 || req[1] != 0x01 {
        // 仅支持 CONNECT
        let _ = client
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        return Ok(());
    }

    // 解析目标地址并连接
    let mut server = match req[3] {
        0x01 => {
            // IPv4
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip).await?;
            let mut p = [0u8; 2];
            client.read_exact(&mut p).await?;
            let sock = std::net::SocketAddr::from((
                std::net::Ipv4Addr::from(ip),
                u16::from_be_bytes(p),
            ));
            match connect_bound(bind_addr, sock).await {
                Ok(s) => s,
                Err(_) => {
                    let _ = client
                        .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    return Ok(());
                }
            }
        }
        0x03 => {
            // 域名
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            client.read_exact(&mut domain).await?;
            let mut p = [0u8; 2];
            client.read_exact(&mut p).await?;
            let domain = String::from_utf8_lossy(&domain).into_owned();
            let addr = format!("{}:{}", domain, u16::from_be_bytes(p));
            match connect_bound_addr(bind_addr, &addr).await {
                Ok(s) => s,
                Err(_) => {
                    let _ = client
                        .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    return Ok(());
                }
            }
        }
        _ => {
            let _ = client
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
            return Ok(());
        }
    };

    // 回复成功
    let _ = client
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await;

    // 双向转发（两个 TcpStream 均实现 AsyncRead + AsyncWrite）
    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
    Ok(())
}

/// 绑定到指定本地地址后连接目标（SocketAddr 版）
async fn connect_bound(
    bind_addr: Ipv4Addr,
    target: std::net::SocketAddr,
) -> std::io::Result<TcpStream> {
    let socket = TcpSocket::new_v4()?;
    socket.bind(std::net::SocketAddr::new(bind_addr.into(), 0))?;
    socket.connect(target).await
}

/// 绑定到指定本地地址后连接目标（域名字符串版，需 DNS 解析）
async fn connect_bound_addr(bind_addr: Ipv4Addr, target: &str) -> std::io::Result<TcpStream> {
    let addr = tokio::net::lookup_host(target)
        .await?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "DNS 解析失败"))?;
    let socket = TcpSocket::new_v4()?;
    socket.bind(std::net::SocketAddr::new(bind_addr.into(), 0))?;
    socket.connect(addr).await
}
