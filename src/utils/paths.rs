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
/// http 直连任务子目录名（位于 tasks/ 下）
pub const HTTP_TASKS_DIR: &str = "http";
/// 定时任务目录名（位于 tasks/ 下）
pub const SCHEDULED_DIR_NAME: &str = "scheduled";
/// 定时任务执行历史子目录名（位于 tasks/scheduled/ 下）
pub const HISTORY_DIR_NAME: &str = "history";
/// 日志根目录名
pub const LOGS_DIR: &str = "logs";
/// 事件流根目录名（AppEvent JSONL 落盘，按日分文件）
pub const EVENTS_DIR: &str = "events";
/// 持久浏览器会话根目录名（门户「记住密码」方案的 user_data_dir 容器）
///
/// 其下按方案 id 分目录（见 [`browser_profile_dir`]）。卸载口径与 `config/`
/// 同进退（勾选保留时整棵保留），见 `uninstall::DATA_DIR_NAMES`。
pub const BROWSER_PROFILES_DIR: &str = "browser_profiles";
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

/// 解析运行时 base 目录：`CAMPUS_AUTH_BASE_DIR` 环境变量覆盖优先（最高优先级），
/// 未设置/空串时回退调用方给定的默认解析值
///
/// 用途：安卓端由 JNI 启动层把应用私有目录（`Context.getFilesDir()`）写入
/// `CAMPUS_AUTH_BASE_DIR`，主程序无需感知 APK 安装布局即可把 `config/` `logs/`
/// 等运行时目录整体落到应用私有存储；桌面/路由器构建不设置该变量，行为与
/// 既有解析完全一致。与 [`worker_project_dir`] 的 `CAMPUS_AUTH_WORKER_DIR`
/// 同模式：环境变量是启动层注入的单一事实源。
pub fn resolve_base_dir(default: PathBuf) -> PathBuf {
    match std::env::var("CAMPUS_AUTH_BASE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
        _ => default,
    }
}

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

/// 读取运行时实际监听端口（`config/.runtime_port`，Axum 绑定成功后写入）
///
/// `--port` CLI 覆盖与端口不可用后的系统分配回退都只体现在该文件；配置快照的
/// `app.port` 仍是 settings.json 的值，不能代表实际监听端口。文件缺失 /
/// 内容非法时返回 `None`，调用方回退到配置值。
pub fn read_runtime_port(base_path: &Path) -> Option<u16> {
    std::fs::read_to_string(runtime_port_path(base_path))
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
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

/// http 直连任务目录 `<base>/tasks/http`
///
/// 与 `browser` / `scripts` 平级：任务类型决定存储桶，同名任务切换类型时由
/// TaskManager 负责清掉其他桶的残留，避免同一 id 出现两份定义。
pub fn http_tasks_dir(base: &Path) -> PathBuf {
    tasks_dir(base).join(HTTP_TASKS_DIR)
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

/// 事件流目录 `<base>/events`
pub fn events_dir(base: &Path) -> PathBuf {
    base.join(EVENTS_DIR)
}

/// 单个方案的持久浏览器会话目录 `<base>/browser_profiles/<profile_id>`
///
/// 目录内容是浏览器持久上下文的 `user_data_dir`（cookie/localStorage，即门户
/// 「记住密码」机制的全部载体），**按需创建**：只有启用持久会话的方案才会产生
/// 数据，故不进 [`ensure_runtime_dirs`] 的启动预建清单（对比 `events/` 的启动建）。
/// 创建时机在配置会话/持久登录启动之前（登录编排负责，见 `src/login/`）。
///
/// `profile_id` 直接用作目录名，合法性由 `config::service::is_valid_profile_id`
/// 保障（非空且 ≤64 字节、仅 ASCII 字母/数字/下划线/连字符，天然排除路径分隔符
/// 与点号；Windows 保留设备名一并拒绝）——Profile 的所有 save/load 路径都会过
/// 该校验，本函数不再做二次防御。删除方案时对应目录的清理时机归登录编排批
/// （本模块只负责拼接，不做生命周期联动）。
pub fn browser_profile_dir(base: &Path, profile_id: &str) -> PathBuf {
    base.join(BROWSER_PROFILES_DIR).join(profile_id)
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
    if candidate.is_dir() {
        return candidate;
    }
    // Docker 镜像将 python_worker 置于 /app/python_worker（见 Dockerfile）
    let docker_path = Path::new("/app").join(WORKER_PROJECT_DIR);
    if docker_path.is_dir() {
        return docker_path;
    }
    // 环境变量覆盖（便于自定义挂载路径）
    if let Ok(env_path) = std::env::var("CAMPUS_AUTH_WORKER_DIR") {
        let pp = PathBuf::from(&env_path);
        if pp.is_dir() {
            return pp;
        }
        let p = pp.join(WORKER_PROJECT_DIR);
        if p.is_dir() {
            return p;
        }
    }
    if let Some(repo) = base_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join(WORKER_PROJECT_DIR))
        && repo.is_dir()
    {
        return repo;
    }
    let mf = Path::new(env!("CARGO_MANIFEST_DIR")).join(WORKER_PROJECT_DIR);
    if mf.is_dir() { mf } else { candidate }
}

/// 判定两条路径指向同一已存在的文件（canonicalize 后逐分量比较）
///
/// 更新替换路径的校验基元：`pending.json` 提供的 `target_exe` 必须与运行时
/// 推导出的真实目标一致，否则判定为被篡改。任一路径不存在即返回 `false`
/// （合法流程走到校验处时两者必然已存在）。
///
/// 放在 `utils::paths` 供主程序（`updater`）与助手进程（`helper_main`）共用，
/// 避免两个 binary 各写一份产生分歧。
pub fn same_existing_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// ---- 预建 + 权限探测 ----

/// 启动时一次性预建全部运行时目录（幂等）
///
/// 覆盖：`config/`、`config/profiles/`、`tasks/browser`、`tasks/scripts`、
/// `tasks/http`、`tasks/scheduled/history`、`logs/login_history`、`events/`、
/// `environment/`。各服务构造内的 `create_dir_all` 保留为防御性幂等调用（测试直构服务时仍可用），
/// 但启动权威路径只此一处。
///
/// `browser_profiles/` 刻意不在预建清单：持久会话目录按需创建（只有启用该功能
/// 的方案才产生数据，见 [`browser_profile_dir`]），给全部用户预建空目录只会
/// 制造"这个目录是干嘛的"困惑。
pub fn ensure_runtime_dirs(base: &Path) -> std::io::Result<()> {
    for dir in [
        profiles_dir(base),
        browser_tasks_dir(base),
        scripts_dir(base),
        http_tasks_dir(base),
        scheduled_history_dir(base),
        login_history_dir(base),
        events_dir(base),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// base 目录环境变量覆盖：设置后最高优先、空串视同未设置、未设置回退默认
    #[test]
    fn test_resolve_base_dir_env_override() {
        // 进程级环境状态，测试内自行清理（仓库内无其他用例读取该变量）
        let key = "CAMPUS_AUTH_BASE_DIR";
        unsafe { std::env::set_var(key, "/data/data/app/files") };
        assert_eq!(
            resolve_base_dir(PathBuf::from("/opt/campus")),
            PathBuf::from("/data/data/app/files")
        );
        unsafe { std::env::set_var(key, "   ") };
        assert_eq!(
            resolve_base_dir(PathBuf::from("/opt/campus")),
            PathBuf::from("/opt/campus")
        );
        unsafe { std::env::remove_var(key) };
        assert_eq!(
            resolve_base_dir(PathBuf::from("/opt/campus")),
            PathBuf::from("/opt/campus")
        );
    }

    #[test]
    fn test_read_runtime_port_roundtrip_and_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        // 文件缺失 → None（调用方回退配置端口）
        assert_eq!(read_runtime_port(dir.path()), None);
        // 正常写入（含空白） → 解析成功
        std::fs::create_dir_all(config_dir(dir.path())).unwrap();
        std::fs::write(runtime_port_path(dir.path()), "18080\n").unwrap();
        assert_eq!(read_runtime_port(dir.path()), Some(18080));
        // 非法内容 → None
        std::fs::write(runtime_port_path(dir.path()), "not-a-port").unwrap();
        assert_eq!(read_runtime_port(dir.path()), None);
    }

    /// 同一文件判定：存在且一致放行，不一致 / 任一不存在均拒绝
    #[test]
    fn test_same_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("campus-auth.exe");
        std::fs::write(&file, b"exe").unwrap();

        // 同一路径（含不同写法）→ true
        assert!(same_existing_path(&file, &file));
        assert!(same_existing_path(
            &file,
            &dir.path().join("./campus-auth.exe")
        ));
        // 不同文件 → false
        let other = dir.path().join("other.exe");
        std::fs::write(&other, b"exe").unwrap();
        assert!(!same_existing_path(&file, &other));
        // 任一不存在 → false
        assert!(!same_existing_path(&file, &dir.path().join("missing.exe")));
        assert!(!same_existing_path(&dir.path().join("missing.exe"), &file));
    }

    /// 持久会话目录拼接：`<base>/browser_profiles/<profile_id>` 两段齐全，且不进
    /// 启动预建清单（按需建，避免给未启用该功能的用户预留空目录）。
    #[test]
    fn test_browser_profile_dir_join_and_not_prebuilt() {
        let base = Path::new("/data");
        assert_eq!(
            browser_profile_dir(base, "dorm"),
            PathBuf::from("/data/browser_profiles/dorm")
        );
        assert_eq!(
            browser_profile_dir(Path::new("/data"), "default"),
            PathBuf::from("/data/browser_profiles/default")
        );

        // 启动预建不含持久会话目录（按需建的口径钉住）
        let tmp = tempfile::tempdir().unwrap();
        ensure_runtime_dirs(tmp.path()).unwrap();
        assert!(
            !tmp.path().join(BROWSER_PROFILES_DIR).exists(),
            "browser_profiles 按需创建，不得出现在启动预建清单"
        );
    }
}
