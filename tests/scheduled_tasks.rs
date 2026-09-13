//! 定时任务 API 集成测试（无 Python 门槛，普通 test job 全平台跑）
//!
//! 锁 2026-09-13 便携版实测覆盖的服务端口径（前端 6 Tab 定时任务表单的 API 面）：
//! - create：cron / startup 两种触发方式的字段落盘（C1/FE1-2 启动三字段）
//! - update：merge 后经 normalize_for_save 按触发方式权威归一化——切回 cron
//!   时启动字段由服务端清空（互斥不靠调用方自觉），startup 时钳制到合法区间
//! - list：id 回填（此前缺失导致前端行级操作全失效的回归）、task_type、
//!   schedule_invalid、startup 任务的 startup_runs_today
//! - 错误分支：重复 id 409、非法 trigger 400、更新不存在 404

mod common;

use std::time::Duration;

use common::{free_port, spawn_instance, wait_listening};
use serde_json::{Value, json};

struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl Api {
    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let mut req = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if !self.token.is_empty() {
            req = req.header("X-Auth-Token", &self.token);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("API 请求发送失败");
        let status = resp.status();
        let v: Value = resp.json().await.expect("API 响应非 JSON");
        assert!(
            status.is_success(),
            "API {method} {path} 返回 {status}：{v}"
        );
        v["data"].clone()
    }

    /// 预期失败（4xx）的请求：返回 (状态码, 响应 JSON)
    async fn request_expect_fail(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if !self.token.is_empty() {
            req = req.header("X-Auth-Token", &self.token);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("API 请求发送失败");
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        (status, v)
    }
}

async fn wait_token(base_path: &std::path::Path) -> String {
    let path = base_path.join("config").join(".auth_token");
    for _ in 0..40 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("实例未在期限内写入 .auth_token");
}

/// 启动隔离实例 + 一个可关联的浏览器目标任务，返回 API 客户端
async fn setup_env() -> (tempfile::TempDir, common::InstanceGuard, Api) {
    let dir = tempfile::TempDir::new().expect("创建临时目录失败");
    let base = dir.path().to_str().unwrap().to_string();
    let port = free_port();
    let instance = spawn_instance(&base, port);
    assert!(wait_listening(port), "实例未在期限内监听端口 {port}");
    let token = wait_token(dir.path()).await;
    let api = Api {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .unwrap(),
        base: format!("http://127.0.0.1:{port}"),
        token,
    };
    // 定时任务的 target_id 关联目标任务（类型由任务权威推导）
    api.request(
        "PUT",
        "/api/tasks/browser-target",
        Some(json!({
            "type": "browser",
            "task_id": "browser-target",
            "name": "目标登录任务",
            "url": "http://127.0.0.1:1/",
            "steps": [
                {"id": "s1", "type": "sleep", "duration": 100, "description": "占位步骤"}
            ]
        })),
    )
    .await;
    (dir, instance, api)
}

fn find_job(list: &Value, id: &str) -> Value {
    list.as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|j| j["id"] == id)
        .unwrap_or_else(|| panic!("列表中未找到定时任务 {id}：{list}"))
}

/// create/update 全字段落盘 + 启动三字段 + 服务端"仅更新出现字段"口径
#[tokio::test]
async fn scheduled_job_create_update_persists_all_fields() {
    let (_dir, _instance, api) = setup_env().await;

    // 1) cron 模式创建
    api.request(
        "POST",
        "/api/scheduler/jobs",
        Some(json!({
            "id": "job-cron",
            "name": "每日登录维护",
            "description": "每天 08:00 执行",
            "target_id": "browser-target",
            "cron": "0 8 * * *",
            "timeout": 60,
            "enabled": true,
        })),
    )
    .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    let job = find_job(&list, "job-cron");
    assert_eq!(job["trigger"], "cron", "{job}");
    assert_eq!(job["cron"], "0 8 * * *", "{job}");
    assert_eq!(job["target_id"], "browser-target", "{job}");
    assert_eq!(job["timeout"], 60, "{job}");
    assert_eq!(job["enabled"], true, "{job}");
    assert_eq!(
        job["task_type"], "browser",
        "task_type 应由目标任务推导：{job}"
    );

    // 2) 切换 startup：调用方显式提交 cron=""（服务端仅更新出现的字段）
    api.request(
        "PUT",
        "/api/scheduler/jobs/job-cron",
        Some(json!({
            "trigger": "startup",
            "cron": "",
            "startup_delay_secs": 30,
            "max_retries": 2,
            "max_runs_per_day": 1,
        })),
    )
    .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    let job = find_job(&list, "job-cron");
    assert_eq!(job["trigger"], "startup", "{job}");
    assert_eq!(job["cron"], "", "{job}");
    assert_eq!(job["startup_delay_secs"], 30, "启动延迟应落盘：{job}");
    assert_eq!(job["max_retries"], 2, "失败重试应落盘：{job}");
    assert_eq!(job["max_runs_per_day"], 1, "每日成功上限应落盘：{job}");
    assert_eq!(
        job["startup_runs_today"], 0,
        "startup 任务列表应带当日成功计数：{job}"
    );

    // 3) 切回 cron：normalize_for_save 按触发方式权威归一化——启动触发专属
    // 字段应被服务端清空（不残留旧启动配置），cron 恢复
    api.request(
        "PUT",
        "/api/scheduler/jobs/job-cron",
        Some(json!({
            "trigger": "cron",
            "cron": "*/5 * * * *",
        })),
    )
    .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    let job = find_job(&list, "job-cron");
    assert_eq!(job["trigger"], "cron", "{job}");
    assert_eq!(job["cron"], "*/5 * * * *", "{job}");
    assert_eq!(
        job["startup_delay_secs"],
        Value::Null,
        "切回 cron 后启动延迟应被 normalize 清空：{job}"
    );
    assert_eq!(
        job["max_retries"],
        Value::Null,
        "切回 cron 后失败重试应被 normalize 清空：{job}"
    );
    assert_eq!(
        job["max_runs_per_day"],
        Value::Null,
        "切回 cron 后每日成功上限应被 normalize 清空：{job}"
    );
}

/// 错误分支：重复 id 409、非法 trigger 400、更新不存在 404、toggle 往返、删除
#[tokio::test]
async fn scheduled_job_rejects_invalid_input_and_lifecycle() {
    let (_dir, _instance, api) = setup_env().await;
    let body = json!({
        "id": "job-x",
        "target_id": "browser-target",
        "cron": "0 9 * * *",
    });

    api.request("POST", "/api/scheduler/jobs", Some(body.clone()))
        .await;

    // 重复 id：显式 409（save_task 为 upsert 语义，此处不得静默覆盖）
    let (status, _) = api
        .request_expect_fail("POST", "/api/scheduler/jobs", Some(body.clone()))
        .await;
    assert_eq!(status, 409, "重复创建应返回 409");

    // 非法 trigger：显式 400（缺省 cron / startup / 其他）
    let (status, _) = api
        .request_expect_fail(
            "POST",
            "/api/scheduler/jobs",
            Some(json!({
                "id": "job-bad",
                "target_id": "browser-target",
                "trigger": "hourly",
            })),
        )
        .await;
    assert_eq!(status, 400, "非法 trigger 应返回 400");

    // 更新不存在：404
    let (status, _) = api
        .request_expect_fail(
            "PUT",
            "/api/scheduler/jobs/no-such-job",
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(status, 404, "更新不存在的任务应返回 404");

    // toggle 往返 + 删除后列表消失
    api.request("POST", "/api/scheduler/jobs/job-x/toggle", None)
        .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    assert_eq!(find_job(&list, "job-x")["enabled"], false, "{list}");
    api.request("POST", "/api/scheduler/jobs/job-x/toggle", None)
        .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    assert_eq!(find_job(&list, "job-x")["enabled"], true, "{list}");

    api.request("DELETE", "/api/scheduler/jobs/job-x", None)
        .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    assert!(
        list.as_array()
            .is_none_or(|a| !a.iter().any(|j| j["id"] == "job-x")),
        "删除后列表不应再包含 job-x：{list}"
    );
}

/// cron 任务的列表项不得携带 startup_runs_today（仅 startup 任务补充该字段）
#[tokio::test]
async fn scheduled_job_list_scopes_startup_runs_today() {
    let (_dir, _instance, api) = setup_env().await;
    api.request(
        "POST",
        "/api/scheduler/jobs",
        Some(json!({
            "id": "job-cron-only",
            "target_id": "browser-target",
            "cron": "0 8 * * *",
        })),
    )
    .await;
    let list = api.request("GET", "/api/scheduler/jobs", None).await;
    let job = find_job(&list, "job-cron-only");
    assert!(
        job.get("startup_runs_today").is_none(),
        "cron 任务不应带 startup_runs_today：{job}"
    );
}
