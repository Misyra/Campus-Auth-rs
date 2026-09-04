//! 运行时目录布局：单一事实源
//!
//! 历史上 `base_path.join("config" / "tasks" / "logs" / "environment" …)` 散落在
//! launcher / config / tasks / scheduler / login / app / lock 等 9 处，各自
//! `create_dir_all` 且错误策略不一（`?` 抛错 / `warn` 吞错 / `required` 分级）。
//! 本模块收敛全部相对布局：目录名常量 + 派生路径 + 一次性预建 + 写权限探测。
//!
//! 分层说明：`utils` 为最底层（仅依赖 `std`），`config` / `environment` /
//! `scheduler` / `app` 的同名常量改为由此 re-export，避免 `utils -> config` 的
//! 上层依赖环。字面量仅在此出现一处。

use std::path::{Path, PathBuf};

// ---- 顶层目录名（与历史值逐字一致） ----

/// 配置根目录名
pub const CONFIG_DIR: &str = "config";
/// 主配置文件名
pub const SETTINGS_FILE: &str = "settings.json";
/// Profile 子目录名（相对于 config/）
pub const PROFILES_DIR: &str = "profiles";
/// 任务根目录名
pub const TASKS_DIR: &str = "tasks";
/// 浏览器任务子目录名（位于 tasks/ 下）
pub const BROWSER_TASKS_DIR: &str = "browser";
/// 脚本任务子目录名（位于 tasks/ 下）
pub const SCRIPTS_DIR: &str = "scripts";
/// 定时任务目录名（位于 tasks/ 下）
pub const SCHEDULED_DIR_NAME: &str = "scheduled";
/// 定时任务执行历史子目录名（位于 tasks/scheduled/ 下）
pub const HISTORY_DIR_NAME: &str = "history";
/// 日志根目录名
pub const LOGS_DIR: &str = "logs";
/// 登录历史子目录名（位于 logs/ 下）
pub const LOGIN_HISTORY_DIR: &str = "login_history";
/// 环境根目录名
pub const ENV_DIR: &str = "environment";
/// Python Worker 工程目录名
pub const WORKER_PROJECT_DIR: &str = "python_worker";
/// 虚拟环境目录名（相对于 worker 工程目录）
pub const VENV_DIR: &str = ".venv";
/// 更新 staging 目录名
pub const UPDATE_DIR: &str = "update";
/// 待应用更新描述文件名（位于 update/ 下）
pub const PENDING_FILE: &str = "pending.json";
/// 运行端口记录文件名（位于 config/ 下）
pub const RUNTIME_PORT_FILE: &str = ".runtime_port";
/// 实例文件锁名（位于 config/ 下）
pub const INSTANCE_LOCK_FILE: &str = ".lock";
/// 实例信息文件名（位于 config/ 下，PID + 端口）
pub const INSTANCE_INFO_FILE: &str = ".instance";

// ---- 派生路径 ----

/// 配置根目录 `<base>/config`
pub fn config_dir(base: &Path) -> PathBuf {
    base.join(CONFIG_DIR)
}

/// 主配置文件 `<base>/config/settings.json`
pub fn settings_path(base: &Path) -> PathBuf {
    config_dir(base).join(SETTINGS_FILE)
}

/// Profile 目录 `<base>/config/profiles`
pub fn profiles_dir(base: &Path) -> PathBuf {
    config_dir(base).join(PROFILES_DIR)
}

/// 实例锁文件 `<base>/config/.lock`
pub fn instance_lock_path(base: &Path) -> PathBuf {
    config_dir(base).join(INSTANCE_LOCK_FILE)
}

/// 实例信息文件 `<base>/config/.instance`
pub fn instance_info_path(base: &Path) -> PathBuf {
    config_dir(base).join(INSTANCE_INFO_FILE)
}

/// 运行端口文件 `<base>/config/.runtime_port`
pub fn runtime_port_path(base: &Path) -> PathBuf {
    config_dir(base).join(RUNTIME_PORT_FILE)
}

/// 任务根目录 `<base>/tasks`
pub fn tasks_dir(base: &Path) -> PathBuf {
    base.join(TASKS_DIR)
}

/// 浏览器任务目录 `<base>/tasks/browser`
pub fn browser_tasks_dir(base: &Path) -> PathBuf {
    tasks_dir(base).join(BROWSER_TASKS_DIR)
}

/// 脚本任务目录 `<base>/tasks/scripts`
pub fn scripts_dir(base: &Path) -> PathBuf {
    tasks_dir(base).join(SCRIPTS_DIR)
}

/// 定时任务目录 `<base>/tasks/scheduled`
pub fn scheduled_dir(base: &Path) -> PathBuf {
    tasks_dir(base).join(SCHEDULED_DIR_NAME)
}

/// 定时任务历史目录 `<base>/tasks/scheduled/history`
pub fn scheduled_history_dir(base: &Path) -> PathBuf {
    scheduled_dir(base).join(HISTORY_DIR_NAME)
}

/// 日志根目录 `<base>/logs`
pub fn logs_dir(base: &Path) -> PathBuf {
    base.join(LOGS_DIR)
}

/// 登录历史目录 `<base>/logs/login_history`
pub fn login_history_dir(base: &Path) -> PathBuf {
    logs_dir(base).join(LOGIN_HISTORY_DIR)
}

/// 环境根目录 `<base>/environment`
pub fn env_dir(base: &Path) -> PathBuf {
    base.join(ENV_DIR)
}

/// 更新目录 `<base>/update`
pub fn update_dir(base: &Path) -> PathBuf {
    base.join(UPDATE_DIR)
}

/// 待应用更新描述 `<base>/update/pending.json`
pub fn pending_path(base: &Path) -> PathBuf {
    update_dir(base).join(PENDING_FILE)
}

/// 解析 python_worker 工程目录（单一事实源）
///
/// 主路径为 `<base_path>/python_worker`；开发模式（如 cargo run 时
/// base_path=target/debug）该目录不存在，回退到仓库根 /
/// CARGO_MANIFEST_DIR 下的 python_worker。Bridge 的 spawn 前检查必须使用本函数
/// 结果，否则 dev 模式会误报"Worker 环境未安装"。
///
/// 历史位置 `environment::resolve_worker_project_path`，收敛至此以消除
/// `bridge` / `web` / `environment` 三方各自解析的分歧。
pub fn worker_project_dir(base_path: &Path) -> PathBuf {
    let candidate = base_path.join(WORKER_PROJECT_DIR);
    if candidate.exists() {
        return candidate;
    }
    // Docker 镜像将 python_worker 置于 /app/python_worker（见 Dockerfile）
    let docker_path = Path::new("/app").join(WORKER_PROJECT_DIR);
    if docker_path.exists() {
        return docker_path;
    }
    // 环境变量覆盖（便于自定义挂载路径）
    if let Ok(env_path) = std::env::var("CAMPUS_AUTH_WORKER_DIR") {
        let pp = PathBuf::from(&env_path);
        if pp.exists() {
            return pp;
        }
        let p = pp.join(WORKER_PROJECT_DIR);
        if p.exists() {
            return p;
        }
    }
    if let Some(repo) = base_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join(WORKER_PROJECT_DIR))
    {
        if repo.exists() {
            return repo;
        }
    }
    let mf = Path::new(env!("CARGO_MANIFEST_DIR")).join(WORKER_PROJECT_DIR);
    if mf.exists() { mf } else { candidate }
}

// ---- 预建 + 权限探测 ----

/// 启动时一次性预建全部运行时目录（幂等）
///
/// 覆盖：`config/`、`config/profiles/`、`tasks/browser`、`tasks/scripts`、
/// `tasks/scheduled/history`、`logs/login_history`、`environment/`。
/// 各服务构造内的 `create_dir_all` 保留为防御性幂等调用（测试直构服务时仍可用），
/// 但启动权威路径只此一处。
pub fn ensure_runtime_dirs(base: &Path) -> std::io::Result<()> {
    for dir in [
        profiles_dir(base),
        browser_tasks_dir(base),
        scripts_dir(base),
        scheduled_history_dir(base),
        login_history_dir(base),
        env_dir(base),
    ] {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(())
}

/// 检查单个目录可写性（幂等建目录 + 写探测文件）
///
/// `required=true` 时不可写即返回错误阻断启动；`false` 时仅告警并降级继续。
/// 历史位置 `launcher::check_dir_writable`，收敛至此供启动检查复用。
pub fn check_dir_writable(path: &Path, name: &str, required: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    let test_file = path.join(format!(".write_test_{}", std::process::id()));
    match std::fs::write(&test_file, b"test") {
        Ok(()) => {
            let _ = std::fs::remove_file(&test_file);
            Ok(())
        }
        Err(e) if required => {
            anyhow::bail!("{name} 目录不可写 ({}): {e}", path.display());
        }
        Err(e) => {
            tracing::warn!("{name} 目录不可写 ({}): {e}（功能降级）", path.display());
            Ok(())
        }
    }
}
