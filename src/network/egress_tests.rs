//! 统一出口的 DNS、鉴权、转发与租约回收回归

use super::*;

fn answer(query: &[u8], ip: Ipv4Addr) -> Vec<u8> {
    let mut data = query.to_vec();
    data[2] = 0x81;
    data[3] = 0x80;
    data[7] = 1;
    data.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4]);
    data.extend_from_slice(&ip.octets());
    data
}

#[test]
fn dns_checks_question_id_and_record_boundaries() {
    let query = dns_query("portal.example.edu").unwrap();
    let ip = Ipv4Addr::new(10, 2, 3, 4);
    let valid = answer(&query, ip);
    assert_eq!(
        parse_dns_answer(&query, &valid, 8080).unwrap(),
        [SocketAddr::from((ip, 8080))]
    );
    let mut wrong = valid.clone();
    wrong[0] ^= 1;
    assert!(parse_dns_answer(&query, &wrong, 80).is_err());
    wrong = valid.clone();
    wrong[13] ^= 1;
    assert!(parse_dns_answer(&query, &wrong, 80).is_err());
    for end in 0..valid.len() {
        assert!(parse_dns_answer(&query, &valid[..end], 80).is_err());
    }
}

#[tokio::test]
async fn dns_uses_selected_server_without_system_resolver_fallback() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut buffer = [0; 512];
        let (n, peer) = server.recv_from(&mut buffer).await.unwrap();
        let response = answer(&buffer[..n], Ipv4Addr::new(10, 9, 8, 7));
        server.send_to(&response, peer).await.unwrap();
    });
    let binding = Binding {
        adapter: None,
        dns: vec![addr],
    };
    let resolved = binding
        .resolve("does-not-exist.example.invalid", 80)
        .await
        .unwrap();
    assert_eq!(resolved, [SocketAddr::from(([10, 9, 8, 7], 80))]);
    task.await.unwrap();
    assert!(
        binding
            .resolve("does-not-exist.example.invalid", 80)
            .await
            .is_err()
    );
}

#[test]
fn invalid_interface_never_produces_an_unbound_socket() {
    let binding = Binding {
        adapter: Some(EgressAdapter {
            id: "missing".into(),
            name: "missing".into(),
            index: 0,
            ipv4: Ipv4Addr::LOCALHOST,
            mac: String::new(),
            dns_servers: Vec::new(),
        }),
        dns: Vec::new(),
    };
    assert!(binding.socket(Type::STREAM, Protocol::TCP).is_err());
    assert!(binding.socket(Type::DGRAM, Protocol::UDP).is_err());
    assert!(blocked_destination(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(blocked_destination(IpAddr::V4(Ipv4Addr::new(
        169, 254, 1, 1
    ))));
    assert!(!blocked_destination(IpAddr::V4(Ipv4Addr::new(10, 2, 3, 4))));
}

#[tokio::test]
async fn local_proxy_requires_auth_and_forwards_origin_form_without_proxy_secret() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = target.local_addr().unwrap();
    let proxy = LocalProxy::start(Arc::new(Binding {
        adapter: None,
        dns: Vec::new(),
    }))
    .await
    .unwrap();
    let proxy_addr = (proxy.url.host_str().unwrap(), proxy.url.port().unwrap());
    let mut unauthorized = TcpStream::connect(proxy_addr).await.unwrap();
    unauthorized
        .write_all(b"GET http://127.0.0.1/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let head = read_http_head(&mut unauthorized).await.unwrap();
    assert!(head.starts_with(b"HTTP/1.1 407"));
    let served = tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.unwrap();
        let head = read_http_head(&mut stream).await.unwrap();
        let text = String::from_utf8(head).unwrap();
        assert!(text.starts_with("GET /portal?step=1 HTTP/1.1\r\n"));
        assert!(!text.to_ascii_lowercase().contains("proxy-authorization"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(proxy.url.as_str()).unwrap())
        .build()
        .unwrap();
    let response = client
        .get(format!("http://{addr}/portal?step=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    served.await.unwrap();
}

#[tokio::test]
async fn connect_tunnel_relays_bytes_and_lease_drop_closes_connections() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = target.local_addr().unwrap();
    let proxy = LocalProxy::start(Arc::new(Binding {
        adapter: None,
        dns: Vec::new(),
    }))
    .await
    .unwrap();
    let url = proxy.url.clone();
    let echo = tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.unwrap();
        let mut data = [0; 4];
        stream.read_exact(&mut data).await.unwrap();
        stream.write_all(&data).await.unwrap();
        let mut tail = [0; 1];
        let _ = stream.read(&mut tail).await;
    });
    let mut tunnel = connect_http_proxy(&url, "127.0.0.1", addr.port())
        .await
        .unwrap();
    tunnel.write_all(b"ping").await.unwrap();
    let mut response = [0; 4];
    tunnel.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"ping");
    drop(proxy);
    let mut byte = [0; 1];
    let end = tokio::time::timeout(Duration::from_secs(2), tunnel.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(end, Ok(0) | Err(_)));
    echo.await.unwrap();
}

#[tokio::test]
async fn http_keep_alive_preserves_one_connection_across_token_and_login() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = target.local_addr().unwrap();
    let proxy = LocalProxy::start(Arc::new(Binding {
        adapter: None,
        dns: Vec::new(),
    }))
    .await
    .unwrap();
    let served = tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.unwrap();
        let first = read_http_head(&mut stream).await.unwrap();
        assert!(first.starts_with(b"GET /token HTTP/1.1"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n")
            .await
            .unwrap();
        let second = read_http_head(&mut stream).await.unwrap();
        let text = String::from_utf8(second).unwrap();
        assert!(text.starts_with("POST /login HTTP/1.1"));
        assert!(!text.to_ascii_lowercase().contains("proxy-authorization"));
        let mut body = [0; 3];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"pwd");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(proxy.url.as_str()).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(format!("http://{addr}/token"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    assert_eq!(
        client
            .post(format!("http://{addr}/login"))
            .body("pwd")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    served.await.unwrap();
}

#[tokio::test]
async fn socks5h_tcp_probe_sends_domain_to_the_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).await.unwrap();
        let mut head = [0; 5];
        stream.read_exact(&mut head).await.unwrap();
        assert_eq!(&head[..4], &[5, 1, 0, 3]);
        let mut name = vec![0; head[4] as usize];
        stream.read_exact(&mut name).await.unwrap();
        assert_eq!(name, b"portal.example.invalid");
        assert_eq!(stream.read_u16().await.unwrap(), 8080);
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        stream.write_all(b"ok").await.unwrap();
    });
    let route = PreparedEgress::prepare(&NetworkSettings {
        mode: NetworkMode::Proxy,
        proxy_url: format!("socks5h://{addr}"),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut stream = route.connect("portal.example.invalid", 8080).await.unwrap();
    let mut data = [0; 2];
    stream.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"ok");
    task.await.unwrap();
}

#[tokio::test]
async fn browser_proxy_credentials_are_decoded_and_kept_out_of_server_url() {
    let route = PreparedEgress::prepare(&NetworkSettings {
        mode: NetworkMode::Proxy,
        proxy_url: "http://user:p%40ss@127.0.0.1:7890".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut browser = serde_json::json!({"bind_proxy":"http://obsolete"});
    route.apply_browser(&mut browser);
    assert_eq!(browser["network_policy"]["proxy"]["password"], "p@ss");
    assert!(
        !browser["network_policy"]["proxy"]["server"]
            .as_str()
            .unwrap()
            .contains("user")
    );
    assert!(browser.get("bind_proxy").is_none());
}

#[test]
fn policy_validation_and_legacy_resolution_share_one_source() {
    let profile = crate::config::ProfileData::default();
    let mut global = crate::config::GlobalConfig::default();
    assert_eq!(
        NetworkSettings::effective(&profile, &global).mode,
        NetworkMode::Direct
    );
    global.browser.bind_proxy = "http://127.0.0.1:7890".into();
    assert_eq!(
        NetworkSettings::effective(&profile, &global).mode,
        NetworkMode::Proxy
    );
    let profile = crate::config::ProfileData {
        network: Some(NetworkSettings::default()),
        ..profile
    };
    assert_eq!(
        NetworkSettings::effective(&profile, &global).mode,
        NetworkMode::Direct
    );
    let invalid = NetworkSettings {
        mode: NetworkMode::Interface,
        ..Default::default()
    };
    assert!(invalid.validate().is_err());
    assert!(parse_dns_server("https://dns.example/dns-query").is_err());
    assert!(parse_dns_server("0.0.0.0").is_err());
    assert!(parse_dns_server("10.0.0.53").is_ok());
    let secret = NetworkSettings {
        proxy_url: "http://user:secret@proxy.example".into(),
        ..Default::default()
    };
    assert!(!format!("{secret:?}").contains("secret"));
}

/// 真起 Edge，经指定物理网卡查询 mock DNS、访问 mock 门户；不改系统网络配置。
#[cfg(windows)]
#[tokio::test]
#[ignore = "依赖真实 Windows 网卡、Edge 与本地 Python 测试环境，仅手动冒烟"]
async fn windows_bound_browser_and_dns_smoke() {
    let adapter = list_adapters()
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("需要已连接的物理网卡");
    let dns = UdpSocket::bind("0.0.0.0:0").await.unwrap();
    let dns_port = dns.local_addr().unwrap().port();
    let portal = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let portal_port = portal.local_addr().unwrap().port();
    let source = adapter.ipv4;
    let queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let received = queries.clone();
    let dns_task = tokio::spawn(async move {
        loop {
            let mut bytes = [0; 512];
            let (n, peer) = dns.recv_from(&mut bytes).await.unwrap();
            assert_eq!(peer.ip(), source, "DNS 必须使用所选网卡源地址");
            received.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dns.send_to(&answer(&bytes[..n], source), peer)
                .await
                .unwrap();
        }
    });
    let visits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = visits.clone();
    let portal_task = tokio::spawn(async move {
        loop {
            let (mut stream, peer) = portal.accept().await.unwrap();
            assert_eq!(peer.ip(), source, "门户连接必须使用同一网卡源地址");
            let _ = read_http_head(&mut stream).await.unwrap();
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = b"<html><title>campus-egress-ok</title><body>campus-egress-ok</body></html>";
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        }
    });
    let route = PreparedEgress::prepare(&NetworkSettings {
        mode: NetworkMode::Interface,
        interface_id: adapter.id,
        dns_servers: vec![format!("{source}:{dns_port}")],
        ..Default::default()
    })
    .await
    .unwrap();
    let mut browser =
        serde_json::json!({"browser_channel":"msedge", "headless":true, "pure_mode":true});
    // .invalid 域名只能由 mock 学校 DNS 解析，预检也必须保留本地绑定代理。
    let preflight = crate::monitor::portal::detect_portal_via(
        &[format!("http://campus-egress-smoke.invalid:{portal_port}/")],
        Duration::from_secs(5),
        &[],
        &std::collections::HashMap::new(),
        Duration::from_secs(5),
        true,
        Some(route.clone()),
    )
    .await;
    assert_eq!(
        preflight.status,
        crate::monitor::portal::PortalDetectStatus::CaptiveNoRedirect
    );
    route.apply_browser(&mut browser);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let python = std::env::var_os("CAMPUS_EGRESS_SMOKE_PYTHON")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join(".local/proxy-bypass-venv/Scripts/python.exe"));
    let script = r#"
import asyncio, json, os
from playwright_worker import WorkerCore
async def main():
    core = WorkerCore()
    try:
        await core._start_browser({'browser_settings': json.loads(os.environ['CAMPUS_EGRESS_SMOKE_SETTINGS'])})
        await core._page.goto(os.environ['CAMPUS_EGRESS_SMOKE_URL'], wait_until='domcontentloaded', timeout=15000)
        assert await core._page.title() == 'campus-egress-ok'
    finally:
        await core.close_browser()
        if core._playwright is not None:
            await core._playwright.stop()
asyncio.run(main())
"#;
    let mut command = tokio::process::Command::new(python);
    command
        .args(["-c", script])
        .current_dir(root.join("python_worker"))
        .env("CAMPUS_EGRESS_SMOKE_SETTINGS", browser.to_string())
        .env(
            "CAMPUS_EGRESS_SMOKE_URL",
            format!("http://campus-egress-smoke.invalid:{portal_port}/"),
        )
        .kill_on_drop(true)
        .creation_flags(0x08000000);
    let result = tokio::time::timeout(Duration::from_secs(30), command.output()).await;
    dns_task.abort();
    portal_task.abort();
    let output = result.unwrap().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(queries.load(std::sync::atomic::Ordering::Relaxed) > 0);
    assert!(visits.load(std::sync::atomic::Ordering::Relaxed) > 0);
}
