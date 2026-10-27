//! 统一出口：网卡绑定、学校 DNS、受鉴权的回环 HTTP 转发与客户端配置

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::adapters::{EgressAdapter, list_adapters};
use super::settings::{NetworkMode, NetworkSettings};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const DNS_TIMEOUT: Duration = Duration::from_secs(2);

/// 网络出口准备失败，不允许静默降级到系统默认出口。
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// 配置无效或所选网卡已离线。
    #[error("{0}")]
    Unavailable(String),
    /// 系统网络操作失败。
    #[error("网络出口准备失败: {0}")]
    Io(#[from] io::Error),
}

/// 一次操作使用的出口租约；浏览器租约由 Bridge 持有至切换或关闭。
pub struct PreparedEgress {
    /// 产生此出口的方案配置，供兼容入口比较。
    pub settings: NetworkSettings,
    binding: Option<Arc<Binding>>,
    proxy: Option<url::Url>,
    local: Option<LocalProxy>,
}

impl std::fmt::Debug for PreparedEgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedEgress")
            .field("mode", &self.settings.mode)
            .finish_non_exhaustive()
    }
}

impl PreparedEgress {
    /// 重新读取接口当前 IP 与 DNS；指定接口缺失时返回错误。
    pub async fn prepare(settings: &NetworkSettings) -> Result<Arc<Self>, EgressError> {
        settings.validate().map_err(EgressError::Unavailable)?;
        let mut route = Self {
            settings: settings.clone(),
            binding: None,
            proxy: None,
            local: None,
        };
        match settings.mode {
            NetworkMode::Interface => {
                let adapter = list_adapters()
                    .await?
                    .into_iter()
                    .find(|a| a.id.eq_ignore_ascii_case(settings.interface_id.trim()))
                    .ok_or_else(|| {
                        EgressError::Unavailable(
                            "校园网出口网卡已断开或不存在，未回退其他网卡".into(),
                        )
                    })?;
                let dns = if settings.dns_servers.is_empty() {
                    &adapter.dns_servers
                } else {
                    &settings.dns_servers
                };
                if dns.is_empty() {
                    return Err(EgressError::Unavailable(
                        "所选网卡没有可用 IPv4 DNS，请填写学校 DNS".into(),
                    ));
                }
                let dns = dns
                    .iter()
                    .map(|s| parse_dns_server(s))
                    .collect::<io::Result<Vec<_>>>()?;
                let binding = Arc::new(Binding {
                    adapter: Some(adapter),
                    dns,
                });
                // 提前校验绑定能力，权限不足或索引失效不能退回未绑定的套接字。
                binding.socket(Type::STREAM, Protocol::TCP)?;
                let local = LocalProxy::start(binding.clone()).await?;
                route.proxy = Some(local.url.clone());
                route.local = Some(local);
                route.binding = Some(binding);
            }
            NetworkMode::Proxy => {
                route.proxy = Some(
                    url::Url::parse(settings.proxy_url.trim())
                        .map_err(|_| EgressError::Unavailable("代理地址格式无效".into()))?,
                );
            }
            NetworkMode::Direct | NetworkMode::System => {}
        }
        Ok(Arc::new(route))
    }

    /// 在统一策略上叠加每次业务所需的 Cookie、超时和证书选项。
    pub fn client_builder(&self) -> Result<reqwest::ClientBuilder, reqwest::Error> {
        let mut builder = reqwest::Client::builder();
        if self.settings.mode != NetworkMode::System {
            builder = builder.no_proxy();
        }
        if let Some(proxy) = &self.proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy.as_str())?);
        }
        Ok(builder)
    }

    /// TCP 探测和入口预检使用同一出口，代理模式通过 HTTP CONNECT 或 SOCKS5 连接。
    pub async fn connect(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            if let Some(binding) = &self.binding {
                binding.connect(host, port).await
            } else if let Some(proxy) = &self.proxy {
                if proxy.scheme() == "http" {
                    connect_http_proxy(proxy, host, port).await
                } else {
                    connect_socks_proxy(proxy, host, port).await
                }
            } else {
                TcpStream::connect((host, port)).await
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "网络出口连接超时"))?
    }

    /// 域名检查使用所选网卡的 DNS，避免校验与连接走两套解析路径。
    pub async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        if let Some(binding) = &self.binding {
            binding.resolve(host, port).await
        } else {
            Ok(tokio::net::lookup_host((host, port)).await?.collect())
        }
    }

    /// 将所有浏览器入口所需的策略写入 IPC，不修改系统代理或 DNS。
    pub fn apply_browser(&self, browser: &mut serde_json::Value) {
        let proxy = self.proxy.as_ref().map(|url| {
            let mut server = url.clone();
            let _ = server.set_username("");
            let _ = server.set_password(None);
            if server.scheme() == "socks5h" {
                let _ = server.set_scheme("socks5");
            }
            let mut options = serde_json::json!({"server": server.as_str()});
            if !url.username().is_empty() || url.password().is_some() {
                options["username"] = decode_proxy_part(url.username()).into();
                options["password"] = decode_proxy_part(url.password().unwrap_or("")).into();
            }
            options
        });
        browser["network_policy"] = serde_json::json!({
            "mode": if proxy.is_some() { "proxy" } else if self.settings.mode == NetworkMode::System { "system" } else { "direct" },
            "proxy": proxy,
        });
        if let Some(map) = browser.as_object_mut() {
            map.remove("bind_proxy");
        }
    }

    /// 指定接口的当前本机地址，供凭据变换使用。
    pub fn local_address(&self) -> Option<super::LocalAddress> {
        self.binding
            .as_ref()?
            .adapter
            .as_ref()
            .map(|a| super::LocalAddress {
                ipv4: a.ipv4.to_string(),
                mac: a.mac.clone(),
            })
    }

    /// 比对 DHCP / DNS 变化；旧接口离线时不得继续使用其代理。
    pub async fn still_current(&self) -> bool {
        match &self.binding {
            Some(binding) => list_adapters()
                .await
                .ok()
                .is_some_and(|all| all.iter().any(|a| Some(a) == binding.adapter.as_ref())),
            None => true,
        }
    }
}

/// 校园网 DNS 仅接受实际 IPv4 端点，避免解析 DNS 本身形成循环。
pub fn parse_dns_server(value: &str) -> io::Result<SocketAddr> {
    let addr = value
        .trim()
        .parse::<Ipv4Addr>()
        .map(|ip| SocketAddr::from((ip, 53)))
        .or_else(|_| value.trim().parse::<SocketAddr>())
        .map_err(io::Error::other)?;
    if !addr.is_ipv4() || addr.port() == 0 || addr.ip().is_unspecified() || addr.ip().is_multicast()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DNS 必须是有效 IPv4 端点",
        ));
    }
    Ok(addr)
}

struct Binding {
    adapter: Option<EgressAdapter>,
    dns: Vec<SocketAddr>,
}

impl Binding {
    fn socket(&self, kind: Type, protocol: Protocol) -> io::Result<Socket> {
        let socket = Socket::new(Domain::IPV4, kind, Some(protocol))?;
        socket.set_nonblocking(true)?;
        if let Some(adapter) = &self.adapter {
            bind_interface(&socket, adapter)?;
            socket.bind(&SocketAddr::from((adapter.ipv4, 0)).into())?;
        } else {
            socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
        }
        Ok(socket)
    }

    async fn connect_addr(&self, addr: SocketAddr) -> io::Result<TcpStream> {
        if !addr.is_ipv4() {
            return Err(io::Error::other("指定网卡出口当前仅支持 IPv4"));
        }
        let socket = self.socket(Type::STREAM, Protocol::TCP)?;
        match socket.connect(&addr.into()) {
            Ok(()) => {}
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(115)
                    || e.raw_os_error() == Some(36) => {}
            Err(e) => return Err(e),
        }
        let stream = TcpStream::from_std(socket.into())?;
        stream.writable().await?;
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
        Ok(stream)
    }

    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        if self.adapter.is_none() && self.dns.is_empty() {
            return Ok(tokio::net::lookup_host((host, port)).await?.collect());
        }
        let query = dns_query(host)?;
        let mut last = io::Error::other("学校 DNS 无解析结果");
        for server in &self.dns {
            let response = tokio::time::timeout(DNS_TIMEOUT, async {
                let socket = UdpSocket::from_std(self.socket(Type::DGRAM, Protocol::UDP)?.into())?;
                socket.connect(server).await?;
                socket.send(&query).await?;
                let mut data = vec![0; 4096];
                let length = socket.recv(&mut data).await?;
                data.truncate(length);
                if data.get(2).is_some_and(|flags| flags & 2 != 0) {
                    let mut stream = self.connect_addr(*server).await?;
                    stream.write_u16(query.len() as u16).await?;
                    stream.write_all(&query).await?;
                    let length = stream.read_u16().await? as usize;
                    if length > 16 * 1024 {
                        return Err(io::Error::other("DNS 响应过大"));
                    }
                    data.resize(length, 0);
                    stream.read_exact(&mut data).await?;
                }
                parse_dns_answer(&query, &data, port)
            })
            .await;
            match response {
                Ok(Ok(addrs)) => return Ok(addrs),
                Ok(Err(error)) => last = error,
                Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, "学校 DNS 查询超时"),
            }
        }
        Err(last)
    }

    async fn connect(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        let mut last = io::Error::other("目标没有可用 IPv4 地址");
        for addr in self.resolve(host, port).await? {
            if self.adapter.is_some() && blocked_destination(addr.ip()) {
                return Err(io::Error::other("校园网转发拒绝回环、链路本地或通配目标"));
            }
            match self.connect_addr(addr).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last = error,
            }
        }
        Err(last)
    }
}

fn blocked_destination(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_broadcast()
        }
        IpAddr::V6(_) => true,
    }
}

fn bind_interface(socket: &Socket, adapter: &EgressAdapter) -> io::Result<()> {
    if adapter.index == 0 {
        return Err(io::Error::other("网卡索引无效"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            IP_UNICAST_IF, IPPROTO_IP, WSAGetLastError, setsockopt,
        };
        let index = adapter.index.to_be();
        // Winsock 要求以网络字节序传入接口索引；指针长度与 DWORD 一致。
        let result = unsafe {
            setsockopt(
                socket.as_raw_socket() as _,
                IPPROTO_IP,
                IP_UNICAST_IF,
                (&index as *const u32).cast(),
                std::mem::size_of::<u32>() as i32,
            )
        };
        if result != 0 {
            // 仅在 Winsock 调用失败后立即读取线程错误码。
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        socket.bind_device(Some(adapter.name.as_bytes()))
    }
    #[cfg(target_os = "macos")]
    {
        socket.bind_device_by_index_v4(std::num::NonZeroU32::new(adapter.index))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = socket;
        Err(io::Error::other("当前平台不支持指定出口网卡"))
    }
}

fn dns_query(host: &str) -> io::Result<Vec<u8>> {
    let host = match url::Host::parse(host).map_err(io::Error::other)? {
        url::Host::Domain(value) => value,
        _ => return Err(io::Error::other("DNS 域名无效")),
    };
    if host.len() > 253 {
        return Err(io::Error::other("DNS 域名过长"));
    }
    let mut query = vec![0; 12];
    query[..2].copy_from_slice(&rand::random::<u16>().to_be_bytes());
    query[2] = 1;
    query[5] = 1;
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(io::Error::other("DNS 标签无效"));
        }
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    Ok(query)
}

fn skip_dns_name(data: &[u8], offset: &mut usize) -> io::Result<()> {
    for _ in 0..128 {
        let length = *data
            .get(*offset)
            .ok_or_else(|| io::Error::other("DNS 名称截断"))?;
        *offset += 1;
        if length == 0 {
            return Ok(());
        }
        if length & 0xc0 == 0xc0 {
            if data.get(*offset).is_none() {
                return Err(io::Error::other("DNS 指针截断"));
            }
            *offset += 1;
            return Ok(());
        }
        if length > 63 {
            return Err(io::Error::other("DNS 名称无效"));
        }
        *offset += length as usize;
        if *offset > data.len() {
            return Err(io::Error::other("DNS 标签截断"));
        }
    }
    Err(io::Error::other("DNS 名称过长"))
}

fn parse_dns_answer(query: &[u8], data: &[u8], port: u16) -> io::Result<Vec<SocketAddr>> {
    if data.len() < query.len()
        || data[..2] != query[..2]
        || data[2] & 0x80 == 0
        || data[3] & 15 != 0
        || data[4..6] != [0, 1]
        || data[12..query.len()] != query[12..]
    {
        return Err(io::Error::other("学校 DNS 响应不匹配或解析失败"));
    }
    let answers = u16::from_be_bytes([data[6], data[7]]);
    let mut offset = query.len();
    let mut addrs = Vec::new();
    for _ in 0..answers {
        skip_dns_name(data, &mut offset)?;
        let header = data
            .get(offset..offset + 10)
            .ok_or_else(|| io::Error::other("DNS 记录截断"))?;
        let length = u16::from_be_bytes([header[8], header[9]]) as usize;
        let value = data
            .get(offset + 10..offset + 10 + length)
            .ok_or_else(|| io::Error::other("DNS 地址截断"))?;
        if header[..4] == [0, 1, 0, 1] && length == 4 {
            addrs.push(SocketAddr::from((
                Ipv4Addr::new(value[0], value[1], value[2], value[3]),
                port,
            )));
        }
        offset += 10 + length;
    }
    if addrs.is_empty() {
        Err(io::Error::other("学校 DNS 未返回 IPv4 地址"))
    } else {
        Ok(addrs)
    }
}

struct LocalProxy {
    url: url::Url,
    stop: CancellationToken,
}

impl Drop for LocalProxy {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl LocalProxy {
    async fn start(binding: Arc<Binding>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("campus:{token}"))
        );
        let url = url::Url::parse(&format!(
            "http://campus:{token}@{}/",
            listener.local_addr()?
        ))
        .map_err(io::Error::other)?;
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        tokio::spawn(async move {
            let limit = Arc::new(Semaphore::new(64));
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    result = listener.accept() => {
                        let Ok((stream, _)) = result else { break; };
                        let Ok(permit) = limit.clone().try_acquire_owned() else { continue; };
                        let binding = binding.clone(); let auth = auth.clone();
                        tasks.spawn(async move {
                            let _permit = permit;
                            if let Err(error) = serve_proxy(stream, binding, &auth).await { tracing::debug!(error = %error, "校园网回环转发结束"); }
                        });
                    }
                }
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self { url, stop })
    }
}

async fn read_http_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut head = Vec::new();
    loop {
        if head.len() >= 32 * 1024 {
            return Err(io::Error::other("代理请求头过大"));
        }
        head.push(stream.read_u8().await?);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head);
        }
    }
}

fn header_value<'a>(headers: &'a [(&str, &str)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

async fn copy_fixed(input: &mut TcpStream, output: &mut TcpStream, length: u64) -> io::Result<()> {
    let copied = tokio::io::copy(&mut (&mut *input).take(length), output).await?;
    if copied != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "HTTP 消息体截断",
        ));
    }
    Ok(())
}

async fn copy_chunked(input: &mut TcpStream, output: &mut TcpStream) -> io::Result<()> {
    loop {
        let mut line = Vec::new();
        while !line.ends_with(b"\r\n") {
            if line.len() >= 8192 {
                return Err(io::Error::other("HTTP 分块头过长"));
            }
            line.push(input.read_u8().await?);
        }
        let text = std::str::from_utf8(&line).map_err(io::Error::other)?;
        let size = u64::from_str_radix(text.trim().split(';').next().unwrap_or(""), 16)
            .map_err(io::Error::other)?;
        output.write_all(&line).await?;
        if size == 0 {
            let mut trailer = Vec::new();
            loop {
                if trailer.len() >= 32 * 1024 {
                    return Err(io::Error::other("HTTP 尾部过长"));
                }
                trailer.push(input.read_u8().await?);
                if trailer == b"\r\n" || trailer.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            output.write_all(&trailer).await?;
            return Ok(());
        }
        copy_fixed(input, output, size).await?;
        let mut ending = [0; 2];
        input.read_exact(&mut ending).await?;
        if ending != *b"\r\n" {
            return Err(io::Error::other("HTTP 分块边界无效"));
        }
        output.write_all(&ending).await?;
    }
}

fn body_length(headers: &[(&str, &str)]) -> io::Result<Option<u64>> {
    let lengths: Vec<_> = headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .collect();
    if lengths.len() > 1
        || (!lengths.is_empty() && header_value(headers, "transfer-encoding").is_some())
    {
        return Err(io::Error::other("HTTP 消息长度冲突"));
    }
    lengths
        .first()
        .map(|(_, value)| value.trim().parse::<u64>().map_err(io::Error::other))
        .transpose()
}

async fn copy_message_body(
    input: &mut TcpStream,
    output: &mut TcpStream,
    headers: &[(&str, &str)],
    response: bool,
) -> io::Result<bool> {
    let length = body_length(headers)?;
    if let Some(encoding) = header_value(headers, "transfer-encoding") {
        if !encoding.eq_ignore_ascii_case("chunked") {
            return Err(io::Error::other("不支持的 HTTP 传输编码"));
        }
        copy_chunked(input, output).await?;
    } else if let Some(length) = length {
        copy_fixed(input, output, length).await?;
    } else if response {
        tokio::io::copy(input, output).await?;
        return Ok(false);
    }
    Ok(true)
}

async fn serve_proxy(mut incoming: TcpStream, binding: Arc<Binding>, auth: &str) -> io::Result<()> {
    let mut connection: Option<(String, TcpStream)> = None;
    loop {
        let head = tokio::time::timeout(
            Duration::from_secs(if connection.is_some() { 90 } else { 8 }),
            read_http_head(&mut incoming),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "代理请求头超时"))??;
        let text = std::str::from_utf8(&head).map_err(io::Error::other)?;
        let mut lines = text.split("\r\n");
        let first = lines
            .next()
            .ok_or_else(|| io::Error::other("代理请求为空"))?;
        let headers: Vec<_> = lines
            .filter(|line| !line.is_empty())
            .map(|line| {
                line.split_once(':')
                    .ok_or_else(|| io::Error::other("HTTP 请求头无效"))
            })
            .collect::<io::Result<_>>()?;
        if header_value(&headers, "proxy-authorization") != Some(auth) {
            incoming.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"campus-egress\"\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?;
            return Ok(());
        }
        body_length(&headers)?;
        let parts: Vec<_> = first.split_whitespace().collect();
        if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
            return Err(io::Error::other("代理请求格式无效"));
        }
        let target = if parts[0] == "CONNECT" {
            format!("http://{}/", parts[1])
        } else {
            parts[1].to_string()
        };
        let url = url::Url::parse(&target).map_err(io::Error::other)?;
        if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
            return Err(io::Error::other("代理目标无效"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| io::Error::other("代理目标缺少主机"))?;
        let port = url.port_or_known_default().unwrap_or(80);
        let key = format!("{host}:{port}");
        if connection.as_ref().is_none_or(|(old, _)| old != &key) {
            let connected =
                tokio::time::timeout(CONNECT_TIMEOUT, binding.connect(host, port)).await;
            match connected {
                Ok(Ok(stream)) => connection = Some((key, stream)),
                failure => {
                    let message = match failure {
                        Ok(Err(error)) => error.to_string(),
                        Err(_) => "指定网卡连接超时".into(),
                        _ => "出口不可用".into(),
                    };
                    tracing::debug!(error = %message, "校园网出口连接失败，未回退系统路由");
                    incoming.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?;
                    return Ok(());
                }
            }
        }
        let upstream = &mut connection
            .as_mut()
            .ok_or_else(|| io::Error::other("出口连接缺失"))?
            .1;
        if parts[0] == "CONNECT" {
            incoming
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            tokio::io::copy_bidirectional(&mut incoming, upstream).await?;
            return Ok(());
        }
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        let close = parts[2] == "HTTP/1.0"
            || header_value(&headers, "connection")
                .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        let upgrade = header_value(&headers, "upgrade").is_some();
        upstream
            .write_all(format!("{} {} HTTP/1.1\r\n", parts[0], path).as_bytes())
            .await?;
        for (key, value) in &headers {
            if [
                "proxy-authorization",
                "proxy-connection",
                "connection",
                "expect",
            ]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
            {
                continue;
            }
            upstream
                .write_all(format!("{key}:{value}\r\n").as_bytes())
                .await?;
        }
        upstream
            .write_all(if upgrade {
                b"Connection: Upgrade\r\n\r\n"
            } else if close {
                b"Connection: close\r\n\r\n"
            } else {
                b"Connection: keep-alive\r\n\r\n"
            })
            .await?;
        if header_value(&headers, "expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
        {
            incoming.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        }
        copy_message_body(&mut incoming, upstream, &headers, false).await?;
        loop {
            let response = read_http_head(upstream).await?;
            let text = String::from_utf8_lossy(&response);
            let mut lines = text.split("\r\n");
            let status_line = lines
                .next()
                .ok_or_else(|| io::Error::other("HTTP 响应为空"))?;
            let status: u16 = status_line
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| io::Error::other("HTTP 状态缺失"))?
                .parse()
                .map_err(io::Error::other)?;
            let response_headers: Vec<_> = lines.filter_map(|line| line.split_once(':')).collect();
            body_length(&response_headers)?;
            incoming.write_all(&response).await?;
            if status == 101 {
                tokio::io::copy_bidirectional(&mut incoming, upstream).await?;
                return Ok(());
            }
            if status < 200 {
                continue;
            }
            let response_close = status_line.starts_with("HTTP/1.0")
                || header_value(&response_headers, "connection")
                    .is_some_and(|v| v.eq_ignore_ascii_case("close"));
            let framed = if parts[0] == "HEAD" || matches!(status, 204 | 304) {
                true
            } else {
                copy_message_body(upstream, &mut incoming, &response_headers, true).await?
            };
            if close || response_close || !framed {
                return Ok(());
            }
            break;
        }
    }
}

fn decode_proxy_part(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

async fn connect_socks_proxy(proxy: &url::Url, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect((
        proxy
            .host_str()
            .ok_or_else(|| io::Error::other("代理主机无效"))?,
        proxy
            .port()
            .ok_or_else(|| io::Error::other("SOCKS5 代理需要端口"))?,
    ))
    .await?;
    let has_auth = !proxy.username().is_empty();
    stream
        .write_all(if has_auth { &[5, 2, 0, 2] } else { &[5, 1, 0] })
        .await?;
    let mut selection = [0; 2];
    stream.read_exact(&mut selection).await?;
    if selection[0] != 5 {
        return Err(io::Error::other("SOCKS5 代理握手无效"));
    }
    if selection[1] == 2 && has_auth {
        let user = decode_proxy_part(proxy.username());
        let password = decode_proxy_part(proxy.password().unwrap_or(""));
        if user.len() > 255 || password.len() > 255 {
            return Err(io::Error::other("SOCKS5 凭据过长"));
        }
        let mut auth = vec![1, user.len() as u8];
        auth.extend_from_slice(user.as_bytes());
        auth.push(password.len() as u8);
        auth.extend_from_slice(password.as_bytes());
        stream.write_all(&auth).await?;
        stream.read_exact(&mut selection).await?;
        if selection != [1, 0] {
            return Err(io::Error::other("SOCKS5 代理认证失败"));
        }
    } else if selection[1] != 0 {
        return Err(io::Error::other("SOCKS5 代理不支持当前认证方式"));
    }
    let ip = match host.parse::<IpAddr>() {
        Ok(ip) => Some(ip),
        Err(_) if proxy.scheme() == "socks5" => Some(
            tokio::net::lookup_host((host, port))
                .await?
                .next()
                .ok_or_else(|| io::Error::other("目标解析无结果"))?
                .ip(),
        ),
        Err(_) => None,
    };
    let mut request = vec![5, 1, 0];
    match ip {
        Some(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Some(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        None => {
            if host.len() > 255 {
                return Err(io::Error::other("SOCKS5 目标域名过长"));
            }
            request.extend_from_slice(&[3, host.len() as u8]);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut response = [0; 4];
    stream.read_exact(&mut response).await?;
    if response[..3] != [5, 0, 0] {
        return Err(io::Error::other("SOCKS5 代理拒绝连接"));
    }
    let length = match response[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        _ => return Err(io::Error::other("SOCKS5 响应地址无效")),
    };
    let mut remaining = vec![0; length + 2];
    stream.read_exact(&mut remaining).await?;
    Ok(stream)
}

async fn connect_http_proxy(proxy: &url::Url, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect((
        proxy
            .host_str()
            .ok_or_else(|| io::Error::other("代理主机无效"))?,
        proxy.port_or_known_default().unwrap_or(80),
    ))
    .await?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let auth = if proxy.username().is_empty() {
        String::new()
    } else {
        format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                decode_proxy_part(proxy.username()),
                decode_proxy_part(proxy.password().unwrap_or(""))
            ))
        )
    };
    stream
        .write_all(
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth}\r\n").as_bytes(),
        )
        .await?;
    let response = read_http_head(&mut stream).await?;
    let first = std::str::from_utf8(&response)
        .map_err(io::Error::other)?
        .split("\r\n")
        .next()
        .unwrap_or("");
    if first.split_whitespace().nth(1) != Some("200") {
        return Err(io::Error::other("代理拒绝 TCP CONNECT"));
    }
    Ok(stream)
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
