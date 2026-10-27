//! 指定方案的出口解析、脚本限制及 HTTP 登录到网络验证的双出口回归

use super::*;
use crate::network::settings::{NetworkMode, NetworkSettings};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn explicit_profile_keeps_credentials_and_browser_network_together() {
    let dir = tempfile::TempDir::new().unwrap();
    let orch = tests::make_orchestrator_at(dir.path()).await;
    let active = orch.config.runtime().load_full();
    let network = NetworkSettings {
        mode: NetworkMode::Interface,
        interface_id: "school-adapter-b".into(),
        dns_servers: vec!["10.0.0.53".into()],
        ..Default::default()
    };
    let profile = crate::config::ProfileData {
        id: "school-b".into(),
        username: "account-b".into(),
        password: orch.config.encrypt_password("password-b").unwrap(),
        network: Some(network.clone()),
        ..Default::default()
    };
    orch.config.save_profile(&profile).await.unwrap();
    let selected = orch
        .resolve_runtime(LoginSource::Manual, &active, &Some(profile.id.clone()))
        .await
        .unwrap_or_else(|_| panic!("指定方案应可加载"));
    let payload = orch
        .build_worker_config(&selected, &selected.profile, "", None)
        .await;
    assert_eq!(payload["username"], "account-b");
    assert_eq!(payload["password"], "password-b");
    assert_eq!(payload["network"], serde_json::to_value(network).unwrap());
    assert_eq!(orch.config.runtime().load().profile.id, active.profile.id);
    for id in [None, Some(String::new())] {
        let current = orch
            .resolve_runtime(LoginSource::Manual, &active, &id)
            .await
            .unwrap_or_else(|_| panic!("未指定方案时应使用当前配置"));
        assert_eq!(current.network, active.network);
        assert_eq!(current.profile.id, active.profile.id);
    }
    let missing = orch
        .resolve_runtime(LoginSource::Manual, &active, &Some("missing".into()))
        .await;
    assert!(missing.is_err(), "指定方案不存在时不能回退到活动方案");
}

#[tokio::test]
async fn explicit_script_profile_cannot_bypass_interface_guard() {
    let dir = tempfile::TempDir::new().unwrap();
    let orch = tests::make_orchestrator_at(dir.path()).await;
    let profile = crate::config::ProfileData {
        id: "legacy-script".into(),
        login_channel: crate::config::LoginChannel::Script,
        network: Some(NetworkSettings {
            mode: NetworkMode::Interface,
            interface_id: "school-adapter-b".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    // 直接保存模拟旧配置或磁盘编辑，验证运行时防线独立于 Web 保存校验。
    orch.config.save_profile(&profile).await.unwrap();
    let result = orch
        .submit(LoginSource::Manual, None, Some(profile.id))
        .await
        .await_result()
        .await;
    assert_eq!(result.terminal, LoginTerminal::Failed);
    assert!(
        result.message.contains("脚本渠道无法强制绑定网卡"),
        "{}",
        result.message
    );
}

/// 代理替身按目标 URL 返回登录结果或联网证据，保留实际收到的请求行。
async fn mock_egress(
    online: bool,
) -> (
    NetworkSettings,
    Arc<StdMutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read_u8().await {
                    Ok(byte) => head.push(byte),
                    Err(_) => break,
                }
                assert!(head.len() < 8192);
            }
            let request = String::from_utf8(head).unwrap();
            let first = request.lines().next().unwrap_or("").to_string();
            let probe = first.contains("/generate_204");
            captured.lock().unwrap().push(first);
            let response = if probe && online {
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
            } else if probe {
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlogin"
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            };
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (
        NetworkSettings {
            mode: NetworkMode::Proxy,
            proxy_url: format!("http://{address}"),
            ..Default::default()
        },
        requests,
        task,
    )
}

#[tokio::test]
async fn explicit_http_profile_uses_one_egress_for_login_and_verification() {
    for selected_online in [true, false] {
        let dir = tempfile::TempDir::new().unwrap();
        let orch = tests::make_orchestrator_at(dir.path()).await;
        let (active_network, active_requests, active_server) = mock_egress(true).await;
        let (selected_network, selected_requests, selected_server) =
            mock_egress(selected_online).await;
        let mut settings = orch.config.load_settings();
        settings.global.monitor.tcp_enabled = false;
        settings.global.monitor.url_enabled = false;
        settings.global.monitor.http_enabled = true;
        settings.global.monitor.http_targets =
            vec!["http://probe.example.invalid/generate_204".into()];
        settings.global.monitor.http_timeout = 1;
        settings.global.monitor.post_login_delay = 0;
        settings.global.retry_settings.max_retries = 0;
        orch.config.save_settings(&settings).await.unwrap();
        let mut active = orch.config.load_profile("default").unwrap();
        active.network = Some(active_network);
        orch.config.save_profile(&active).await.unwrap();
        orch.config.reload().await.unwrap();
        let profile = crate::config::ProfileData {
            id: "school-b".into(),
            username: "account-b".into(),
            password: orch.config.encrypt_password("password-b").unwrap(),
            login_channel: crate::config::LoginChannel::Http,
            active_http_task: "mock-login".into(),
            network: Some(selected_network),
            ..Default::default()
        };
        orch.config.save_profile(&profile).await.unwrap();
        let task = crate::tasks::HttpTaskConfig {
            method: crate::tasks::HttpRequestMethod::Get,
            url: "http://portal.example.invalid/login?account={username}".into(),
            ..Default::default()
        };
        orch.tasks
            .save_task("mock-login", &crate::tasks::TaskKind::Http(Box::new(task)))
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            orch.submit(LoginSource::Manual, None, Some(profile.id))
                .await
                .await_result()
                .await
        })
        .await
        .expect("本地 mock 登录不得挂起");
        active_server.abort();
        selected_server.abort();
        assert_eq!(result.is_success(), selected_online, "{}", result.message);
        assert!(
            active_requests.lock().unwrap().is_empty(),
            "活动方案出口不得参与指定方案登录或验证"
        );
        let requests = selected_requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "应只发送一次登录和一次验证：{requests:?}"
        );
        assert!(
            requests[0].contains("/login?account=account-b"),
            "{requests:?}"
        );
        assert!(requests[1].contains("/generate_204"), "{requests:?}");
        assert_eq!(orch.config.runtime().load().profile.id, "default");
    }
}
