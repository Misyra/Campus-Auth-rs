// 每个 `tests/*.rs` 都是独立 crate，本模块按 crate 编译——单个 crate 未用到的
// helper 会触发 dead_code，属正常现象，整体允许。
#![allow(dead_code)]

//! Rust 集成测试共享工具
//!
//! 每个 `tests/*.rs` 都是独立 crate，需在文件头声明 `mod common;`
//! 即可使用本模块（`tests/common/mod.rs` 非独立测试目标）。
//!
//! 约定：
//! - 临时目录一律 `tempfile::TempDir`，禁止写入 `target/`（构建产物目录）；
//! - 需真实二进制的用例走 `CARGO_BIN_EXE_campus-auth` + `InstanceGuard` 保活；
//! - 找不到 Python/venv 时返回 `None`，调用方打印原因后直接 `return` 跳过。

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

/// 子进程守卫：无论测试从哪条路径失败（panic/断言），Drop 都会强杀实例，
/// 防止残留进程占住测试端口或锁文件。
///
/// stderr 重定向到 `NamedTempFile`（随机名，Drop 自动删除）：失败断言经
/// [`InstanceGuard::stderr_path`] 读取内容，成功/失败都不在 `%TEMP%` 留文件
/// （此前固定名 `campus-auth-it-{port}.log` 会无限堆积）。
pub struct InstanceGuard(pub Child, tempfile::NamedTempFile);

impl InstanceGuard {
    /// 用已启动的子进程与独立的临时 stderr 落盘构造守卫
    pub fn with_stderr_log(child: Child, log: tempfile::NamedTempFile) -> Self {
        Self(child, log)
    }

    /// stderr 落盘路径（断言失败信息用；守卫释放后文件即删除）
    pub fn stderr_path(&self) -> &std::path::Path {
        self.1.path()
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        // 字段按声明顺序析构：子进程已退出后 NamedTempFile 自动删文件
    }
}

/// 取一个当前空闲的高位端口（存在 TOCTOU 窗口，但测试环境可接受）
pub fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("绑定临时端口失败")
        .local_addr()
        .unwrap()
        .port()
}

/// 等待端口开始接受 TCP 连接（最多 15s）
pub fn wait_listening(port: u16) -> bool {
    for _ in 0..60 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// 在限期内等待子进程退出；超时则强杀并失败
///
/// `stderr_log` 用于失败诊断：panic 信息附带实例 stderr 落盘内容，
/// 否则 NamedTempFile 在守卫 Drop 时删除，超时场景的证据随之丢失。
pub fn wait_exit_or_kill(child: &mut Child, label: &str, stderr_log: Option<&std::path::Path>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let stderr = stderr_log
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let stderr_tail: String = {
        // 只保留尾部，避免超长日志淹没 panic 信息
        let lines: Vec<&str> = stderr.lines().collect();
        let start = lines.len().saturating_sub(20);
        lines[start..].join("\n")
    };
    panic!("{label} 未在期限内退出；实例 stderr 尾部:\n{stderr_tail}");
}

/// 预置测试实例的 OCR 用户偏好为启用。
///
/// 实例引导按偏好对齐 worker 项目的 ddddocr 声明（`reconcile_ocr_preference`）：
/// 偏好缺失（新实例的默认态，即大部分不使用验证码识别用户的状态）等价于
/// 「未启用」，引导对已声明 ddddocr 的项目执行 `uv remove`。登录链路用例依赖
/// OCR，需在 `spawn_instance` 前调用本函数——实例引导会自动 `uv add` 到自己的
/// worker 副本，等价于真实用户在设置页点「安装 OCR 依赖」的路径。
pub fn preset_ocr_preference(base: &Path) {
    let dir = base.join("environment");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let payload = serde_json::json!({ "schema_version": 1, "ocr_enabled": true });
    let _ = std::fs::write(dir.join("python-preferences.json"), payload.to_string());
}

/// 复制仓库 `python_worker/` 源码到 `<base>/python_worker`，排除本地虚拟环境
/// 与各类缓存（清单与 build.ps1 口径一致）。
///
/// 测试实例经 `worker_project_dir` 路径兜底会共用仓库 worker 项目：bootstrap 的
/// `uv sync` 与 OCR 偏好对齐会改写它的 pyproject/uv.lock 与 .venv——测试间互拆
/// 环境、还弄脏工作区。复制进 base 后 `worker_project_dir` 第一候选即命中副本，
/// 实例的全部依赖操作隔离在 TempDir 内，仓库不再被触碰；复制的少量小文件
/// （排除 .venv 后约几百 KB）相对实例启动成本可忽略。
fn copy_worker_project(base: &str) {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python_worker");
    let dst = Path::new(base).join("python_worker");
    // 源不存在（异常环境）或副本已就位（幂等）时不动；复制尽力而为，
    // 局部失败等价回退到旧行为（实例兜底共用仓库），不会更糟
    if !src.is_dir() || dst.is_dir() {
        return;
    }
    const EXCLUDED_DIRS: &[&str] = &[
        ".venv",
        "__pycache__",
        ".pytest_cache",
        ".tmp-uv-cache",
        ".mypy_cache",
        ".ruff_cache",
        "captures",
        "build",
        "dist",
        "node_modules",
    ];
    let mut stack = vec![(src, dst)];
    while let Some((from, to)) = stack.pop() {
        if std::fs::create_dir_all(&to).is_err() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&from) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            if EXCLUDED_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let target = to.join(&name);
            if file_type.is_dir() {
                stack.push((entry.path(), target));
            } else if file_type.is_file() {
                let _ = std::fs::copy(entry.path(), target);
            }
        }
    }
}

/// 启动一个完整模式实例（托盘与浏览器均禁用），stderr 进临时文件便于失败排查
///
/// 实例的 worker 项目使用 base 内的源码副本（见 [`copy_worker_project`]），
/// 不需要 OCR 的用例（默认态）不预置任何偏好，即覆盖大部分用户「未启用 OCR」
/// 的真实引导路径。
pub fn spawn_instance(base: &str, port: u16) -> InstanceGuard {
    copy_worker_project(base);
    let log = tempfile::NamedTempFile::new().expect("创建 stderr 临时文件失败");
    let err_file = log.reopen().expect("复用 stderr 临时文件失败");
    let child = StdCommand::new(env!("CARGO_BIN_EXE_campus-auth"))
        .args([
            "--base-path",
            base,
            "--port",
            &port.to_string(),
            "--no-tray",
            "--no-browser",
            "--mode",
            "full",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file))
        .spawn()
        .expect("启动实例进程失败");
    InstanceGuard::with_stderr_log(child, log)
}

/// 定位本地 Python 解释器（优先项目内 venv，其次 PATH）。
///
/// 仅判断文件存在不够：uv 重建/删除后 venv 的 python.exe 可能仍是
/// 指向已不存在解释器的启动器，启动会以非 0 退出——必须真实 `--version` 探活。
/// 找不到时返回 `None`，调用方跳过测试。
pub fn locate_python() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("python_worker/.venv/Scripts/python.exe"),
        manifest.join("environment/.venv/Scripts/python.exe"),
        manifest.join("python_worker/.venv/bin/python3"),
        manifest.join("environment/.venv/bin/python3"),
    ];
    for c in candidates {
        if c.exists()
            && std::process::Command::new(&c)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        {
            return Some(c);
        }
    }
    ["python3", "python"].into_iter().find_map(|name| {
        let path = which::which(name).ok()?;
        let output = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .ok()?;
        output.status.success().then_some(path)
    })
}

/// 定位本地 Python venv 目录（其下须含 `Scripts/python.exe` 或 `bin/python3`）。
/// 找不到时返回 `None`，调用方跳过测试。
pub fn locate_venv() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("python_worker/.venv"),
        manifest.join("environment/.venv"),
    ];
    candidates.into_iter().find(|v| {
        let python = if cfg!(windows) {
            v.join("Scripts/python.exe")
        } else {
            v.join("bin/python3")
        };
        python.exists()
            && std::process::Command::new(python)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
    })
}
