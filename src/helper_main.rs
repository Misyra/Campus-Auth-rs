//! 更新 helper 独立入口：等待主进程退出 -> 替换 exe -> 启动新 exe -> 清理
//!
//! 由主进程的 `UpdaterService::spawn_helper()` spawn，接收 `--pid` 参数。
//! 从 `<base_path>/update/pending.json` 读取 staging / target 信息，
//! 等待主进程退出后完成替换并重启新版本。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use campus_auth::utils::io::file_sha256;
use campus_auth::utils::lock::is_process_alive;
use campus_auth::utils::paths::same_existing_path;
use chrono::Local;
use clap::Parser;

// MSRV 1.85 兼容：fs4::FileExt 为低版本 Rust 提供 try_lock（与 utils::lock 同模式）；
// Rust 1.96+ 内置方法优先级更高，不会冲突。
#[allow(unused_imports)]
use fs4::FileExt;

/// 等待主进程退出的轮询间隔（毫秒）
const PROCESS_EXIT_POLL_MS: u64 = 100;
/// 等待主进程退出的总超时（秒），超时报错退出并保留 staging/pending
const PROCESS_EXIT_TIMEOUT_SECS: u64 = 60;
/// 新进程存活二次探活前的延迟（秒）：等待潜在秒退窗口过去
const SECOND_ALIVE_PROBE_DELAY_SECS: u64 = 5;

/// campus-auth 更新助手进程
///
/// 两种模式，互斥：
/// - `--apply-update`：等待主进程退出 → 替换 exe → 重启新版本（见 `run_apply_update`）
/// - `--uninstall`：等待主进程退出 → 删除安装目录（+ 可选保留用户数据）→ 系统提示框
///
/// 合并进同一个 binary 而不是再加一个可执行文件：两者共用"等 PID 退出""路径守卫"
/// "best-effort 日志"三块逻辑，且发布包少一个文件就少一处需要同步的版本管理。
#[derive(Parser)]
#[command(name = "campus-auth-helper", version, about = "Campus-Auth 更新助手")]
struct HelperCli {
    /// 应用待处理更新（从 pending.json 读取配置）
    #[arg(long)]
    apply_update: bool,

    /// 卸载：等待主进程退出后删除安装目录（与 --apply-update 互斥）
    #[arg(long, conflicts_with = "apply_update")]
    uninstall: bool,

    /// 主进程 PID（等待其退出后执行替换）
    #[arg(long)]
    pid: u32,

    /// staging 目录路径（可选，默认从 pending.json 读取）
    #[arg(long)]
    staging: Option<PathBuf>,

    /// 目标 exe 路径（可选，默认从 pending.json 读取）
    #[arg(long)]
    target: Option<PathBuf>,

    /// 基础路径（可选，默认从 exe 所在目录推断）
    #[arg(long)]
    base_path: Option<PathBuf>,

    /// 卸载时保留用户数据（config / tasks / logs / environment / update）
    #[arg(long)]
    keep_user_data: bool,

    /// 卸载第二段（内部使用）：本实例已退到系统临时目录，等待第一段退出后执行删除
    #[arg(long)]
    uninstall_phase2: bool,

    /// 卸载第二段（内部使用）：被卸载的程序目录；第一段显式传入
    #[arg(long)]
    install_dir: Option<PathBuf>,
}

/// pending.json 数据结构（与 UpdaterService 的 PendingUpdate 对应）
#[derive(serde::Deserialize)]
struct PendingInfo {
    staging_dir: String,
    target_exe: String,
    /// 主程序已解析并固定的 Worker 更新目标；旧 pending 回退 base/python_worker
    #[serde(default)]
    worker_target_dir: String,
    original_args: Vec<String>,
    /// 暂存包预期 SHA256（G13：替换前复核；空值直接拒绝替换，不降级）
    #[serde(default)]
    sha256: String,
    /// 待应用版本号（见下方 2.5 版本闸门）
    version: String,
}

/// helper 的 best-effort 落盘日志
///
/// GUI 子系统（Windows release 双击启动）下 stdout/stderr 完全不可见，更新失败
/// 将不可诊断。每条消息同时写入 `<base_path>/logs/helper.log`（追加）与 stderr：
/// 目录不存在则尝试创建；文件打开/写入失败仅退回 stderr，绝不 panic、绝不阻断
/// 更新流程（日志是尽力而为的旁路）。
struct HelperLog {
    file: Option<std::fs::File>,
}

impl HelperLog {
    /// 打开日志文件（失败则退回仅 stderr 模式）
    fn open(base_path: &Path) -> Self {
        Self::open_at(&base_path.join("logs").join("helper.log"))
    }

    /// 打开指定路径的日志文件（卸载模式用：日志不能留在即将被删除的目录里）
    fn open_at(log_path: &Path) -> Self {
        let file = std::fs::create_dir_all(log_path.parent().unwrap_or_else(|| Path::new(".")))
            .ok()
            .and_then(|()| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path)
                    .ok()
            });
        Self { file }
    }

    /// 写一条日志：stderr（带 [helper] 前缀与时间戳）+ 文件追加
    fn write(&mut self, level: &str, msg: &str) {
        let line = format!(
            "[helper] {} [{level}] {msg}",
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        eprintln!("{line}");
        if let Some(f) = self.file.as_mut() {
            // 写日志失败（磁盘满/句柄失效）静默放弃，不影响主流程
            let _ = writeln!(f, "{line}");
        }
    }

    fn info(&mut self, msg: &str) {
        self.write("INFO", msg);
    }

    fn error(&mut self, msg: &str) {
        self.write("ERROR", msg);
    }

    fn debug(&mut self, msg: &str) {
        self.write("DEBUG", msg);
    }
}

fn main() {
    let cli = HelperCli::parse();

    if cli.uninstall {
        run_uninstall(&cli);
        return;
    }

    if !cli.apply_update {
        eprintln!("campus-auth-helper v{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    run_apply_update(&cli);
}

/// 更新模式主体：等待主进程退出 -> 替换 exe -> 启动新 exe -> 清理
fn run_apply_update(cli: &HelperCli) {
    // 0. 计算基准路径并初始化 best-effort 落盘日志
    // （GUI 子系统下 stdout/stderr 不可见，更新失败必须可从 helper.log 诊断）
    let base_path = cli.base_path.clone().unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let mut log = HelperLog::open(&base_path);

    // 0.6 helper 互斥：apply_update 的 spawn 与关机时 ensure_helper_for_shutdown
    // 的补唤醒可能同时存在两个 helper，并发执行备份/替换/回滚会产生竞态（最坏：
    // 后到者复制失败进入回滚，把先行者刚替换的新 exe 用旧备份覆盖回去）。
    // 对 <base>/update/helper.lock 取排他文件锁，抢不到说明同伴在执行，安静退出。
    let _helper_lock = acquire_helper_lock(&base_path.join("update").join("helper.lock"), &mut log);

    // 1. 等待主进程退出
    log.info(&format!("等待主进程 (PID {}) 退出...", cli.pid));
    if !wait_for_process_exit(cli.pid, &mut log) {
        // 主进程未退出：中止更新，保留 staging 与 pending.json，待主进程下次启动
        // 时由 apply_pending_on_startup 应用（不执行 cleanup，避免摧毁待应用更新）
        std::process::exit(1);
    }
    // 额外等待一小段时间，确保文件句柄完全释放
    sleep(Duration::from_millis(500));

    // 2. 从 pending.json 读取配置（--apply-update 模式的唯一事实源）
    //
    // pending 缺失 / 解析失败 / 读取失败一律中止并**保留现场**（不 cleanup）：
    // 降级"按 CLI 参数继续"必然过不了 SHA 复核闸门（期望值就在 pending 里），
    // 而被拒绝的路径会调 cleanup 把 staging 与 pending 一并销毁——瞬时 IO 错误
    // 会永久丢弃待应用更新，日志还自称"已保留"，自相矛盾。
    let pending_path = base_path.join("update").join("pending.json");
    let pending: PendingInfo = match std::fs::read_to_string(&pending_path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(p) => p,
            Err(e) => {
                log.error(&format!(
                    "无法解析待应用更新记录（pending.json 损坏: {e}），已保留现场，更新中止"
                ));
                std::process::exit(1);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log.error("无法读取待应用更新记录（update/pending.json 不存在），已保留现场，更新中止");
            std::process::exit(1);
        }
        Err(e) => {
            log.error(&format!(
                "读取待应用更新记录失败（{e}），已保留现场，更新中止"
            ));
            std::process::exit(1);
        }
    };

    let staging_dir = match cli.staging.clone() {
        // CLI 参数优先（非空时）；空参数视同未提供
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            if pending.staging_dir.is_empty() {
                log.error("无法确定 staging 目录路径（--staging 与 pending.json 均为空）");
                std::process::exit(1);
            }
            PathBuf::from(&pending.staging_dir)
        }
    };

    // 目标 exe 由 helper 自身位置推导：helper 与主程序同目录、主程序文件名固定，
    // 因此无需信任 pending.json / CLI 提供的 target。
    // 比"位于 base_path 之内"更强——后者在 --base-path 与 exe 目录分离时恒不成立，
    // 会导致 helper 拒绝替换、staging 被下次启动的 Rust 侧清理，更新静默作废。
    let derived_target = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe_name())));
    let provided_target = match cli.target.clone() {
        Some(t) => Some(t),
        // 旧 pending 缺字段时回退 None（校验交给 resolve_target_exe 的推导分支）
        None if pending.target_exe.is_empty() => None,
        None => Some(PathBuf::from(&pending.target_exe)),
    };
    let target_exe = match resolve_target_exe(derived_target, provided_target, &base_path) {
        Some(p) => p,
        None => {
            log.error("拒绝执行：无法确定目标 exe（与推导的主程序不一致，或不在 base_path 内）");
            std::process::exit(1);
        }
    };

    // Worker 目标必须由主程序显式传递，并且仍等同于内置目录。外置 Worker、
    // Docker bind mount 等布局由部署系统负责更新，helper 不猜测也不跨目录覆盖。
    let bundled_worker_dir = base_path.join("python_worker");
    let worker_target_dir = {
        let from_pending = pending.worker_target_dir.trim();
        if from_pending.is_empty() {
            // 旧 pending 缺字段时回退内置目录（校验在下方统一做）
            bundled_worker_dir.clone()
        } else {
            PathBuf::from(from_pending)
        }
    };
    if !worker_target_dir.is_dir()
        || !bundled_worker_dir.is_dir()
        || !same_existing_path(&worker_target_dir, &bundled_worker_dir)
    {
        log.error(&format!(
            "拒绝执行：Worker 更新目标不是内置目录（目标 {}，要求 {}）",
            worker_target_dir.display(),
            bundled_worker_dir.display()
        ));
        std::process::exit(1);
    }

    // 2.5 版本闸门（纵深防御）：pending 版本须严格高于本 helper 版本（helper 与
    // 被替换主程序同版本发布）。不高于或无法解析均拒绝替换并保留现场——
    // 主进程侧（pin 路径 / apply_pending_on_startup / 上传包提取版本闸门）已有
    // 闸门，此处补齐 helper 执行端的最后一环，堵住"篡改 pending 降级替换"的通道。
    if !pending_version_allowed(&pending.version, env!("CARGO_PKG_VERSION")) {
        log.error(&format!(
            "待应用版本 {} 不高于当前 {} 或无法解析，拒绝替换",
            pending.version,
            env!("CARGO_PKG_VERSION")
        ));
        std::process::exit(1);
    }

    // G13：staging 是 remove_dir_all 的目标，取值可能来自 pending.json——
    // 被篡改时会把任意系统目录变成清理对象，必须锁在 base_path 之内。
    if !is_within_base(&staging_dir, &base_path) {
        log.error(&format!(
            "拒绝执行：staging 路径不在 base_path 之内: {}",
            staging_dir.display()
        ));
        std::process::exit(1);
    }

    let extracted_exe = staging_dir.join("extracted").join(exe_name());

    // 3. 校验 staging 文件存在
    if !extracted_exe.exists() {
        log.error(&format!("staging 文件不存在: {}", extracted_exe.display()));
        cleanup(&base_path, &staging_dir, &mut log);
        std::process::exit(1);
    }

    // 3.5 G13：复制前复核 staging exe 的 SHA256（期望值从 pending.json 传入）
    // 校验失败（staging 损坏/被篡改）时中止替换并清理不可信的 staging
    if !verify_staging_sha256(&extracted_exe, &pending.sha256, &mut log) {
        log.error("staging exe SHA256 复核失败，中止替换");
        cleanup(&base_path, &staging_dir, &mut log);
        std::process::exit(1);
    }

    // 3.6 幂等跳过：目标 exe 已与 staging 内容一致，说明此前有 helper 完成
    // 复制后未及启动/清理即死亡（锁随进程释放）。跳过备份/替换/回滚，直接
    // 进入分发同步与启动——从根上消除"回滚分支把新 exe 覆盖回旧版"的风险。
    let already_replaced = files_identical(&extracted_exe, &target_exe);
    if already_replaced {
        log.info("目标 exe 已是新版内容（此前 helper 已完成替换），跳过复制步骤");
    }

    // 4. 备份旧 exe（统一 "<原名>.bak"：unix 上 with_extension("exe.bak") 会产出
    // campus-auth.exe.bak 的怪名——无扩展名文件被凭空拼出 .exe）
    let backup_path = target_exe
        .file_name()
        .map(|n| target_exe.with_file_name(format!("{}.bak", n.to_string_lossy())))
        .unwrap_or_else(|| target_exe.with_extension("exe.bak"));
    if !already_replaced && target_exe.exists() {
        log.info(&format!("备份旧版本 -> {}", backup_path.display()));
        if let Err(e) = std::fs::copy(&target_exe, &backup_path) {
            log.error(&format!(
                "备份失败: {e}，将在无备份情况下继续替换，失败后无法回滚"
            ));
            // 备份失败不阻断替换流程
        }
    }

    // 5. 替换 exe（helper 复制新文件覆盖旧 exe，而非替换自身）
    if !already_replaced {
        log.info(&format!(
            "替换 {} -> {}",
            extracted_exe.display(),
            target_exe.display()
        ));
        if let Err(e) = std::fs::copy(&extracted_exe, &target_exe) {
            log.error(&format!("替换失败: {e}"));
            // 尝试回退：从备份恢复
            if backup_path.exists() {
                match std::fs::copy(&backup_path, &target_exe) {
                    Ok(_) => log.error("已回退到备份版本"),
                    Err(e) => log.error(&format!(
                        "回退失败（{e}），exe 处于未知状态，请手动用备份 {} 恢复",
                        backup_path.display()
                    )),
                }
            }
            // 替换失败几乎必然是目标 exe 被占用（如重启场景的后继进程、杀软
            // 扫描），此时目标内容并未被修改（复制在打开目标阶段即失败），
            // 保留 pending.json 与 staging 供下次启动走 apply_pending_on_startup
            // → self_replace 重试（rename 语义不受目标占用影响）；仅当包本身
            // 损坏时才会到不了这里（前序 SHA256 复核已拒绝并清理）。
            log.info("已保留待应用更新记录，下次启动将自动重试应用更新");
            std::process::exit(1);
        }
    }
    // unix：fs::copy 只复制源文件权限，为防解压链路丢 +x，替换后显式确保
    // 新二进制可执行——否则重启必然 Exec format/permission 失败
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&target_exe, std::fs::Permissions::from_mode(0o755))
        {
            log.debug(&format!("设置新 exe 可执行权限失败: {e}"));
        }
    }

    // 5.5 同步全量分发内容：更新包不只有 exe——python_worker/（Python 代码）、
    // resources/、docs/ 同为新版本的一部分，只换 exe 会让 Python 侧修复
    // （如反馈资源快照）永远到不了走应用内更新的用户。overlay 语义：覆盖同名
    // 文件、新增缺失文件、绝不删除目标侧多余内容（python_worker/.venv 是
    // 用户运行态，config/tasks/logs 等用户数据不在 staging 内天然不受影响）。
    // resources/docs 跟随 base_path；Worker 使用 pending 中已经校验的明确目标。
    let extracted_dir = staging_dir.join("extracted");
    sync_distribution_files(&extracted_dir, &base_path, &worker_target_dir);
    // helper 自更新落点必须是 exe 所在目录，而非 base_path：spawn_helper 从主程序
    // 同级目录查找 helper，--base-path 与 exe 目录分离时，写进 base_path 的 helper
    // 永远不会被调用（同时在数据目录留下一份无用副本）。
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| base_path.clone());
    replace_helper(&extracted_dir, &install_dir, &mut log);

    // 6. 启动新 exe（传递原始启动参数）
    let original_args = pending.original_args.clone();
    log.info("启动新版本...");
    match std::process::Command::new(&target_exe)
        .args(&original_args)
        .spawn()
    {
        Ok(mut child) => {
            log.info("新版本已启动");
            // G13：延迟删 .bak——spawn 成功不代表新 exe 能正常运行（依赖缺失、
            // 版本不兼容时会秒退），立即删备份会让用户失去回退手段。
            // 权衡：多等 5 秒 + try_wait 两次探活，仅在确认新进程持续存活后才
            // 删除 .bak；否则保留备份并输出回退提示。
            let first_probe = probe_alive(&mut child);
            let second_probe = if matches!(first_probe, Ok(None)) {
                sleep(Duration::from_secs(SECOND_ALIVE_PROBE_DELAY_SECS));
                probe_alive(&mut child)
            } else {
                // 首查已退出/探测失败：无需二查
                first_probe
            };
            if decide_backup_deletion(first_probe, second_probe) {
                if let Err(e) = std::fs::remove_file(&backup_path) {
                    log.debug(&format!("清理备份文件失败: {e}"));
                }
                log.info("新版本持续运行，已清理备份");
            } else {
                log.error(&format!(
                    "新版本启动后疑似异常退出，保留备份 {} 供手动回退",
                    backup_path.display()
                ));
            }
        }
        Err(e) => {
            log.error(&format!("启动新版本失败: {e}"));
            log.error(&format!("保留备份 {} 供手动回退", backup_path.display()));
        }
    }

    // 7. 清理
    cleanup(&base_path, &staging_dir, &mut log);

    log.info("更新完成");
}

// ═══ 卸载模式 ═══

/// 卸载第二段所在的临时目录名前缀
///
/// 只被 `#[cfg(windows)]` 的 [`spawn_uninstall_phase2`] 使用——**必须一并加 `cfg`**：
/// unix 走单段直删，常量在那里没有任何引用，不加会让 `clippy -D warnings` 在
/// macOS / Linux 上以 `dead-code` 失败（Windows 本地看不出来，只有 CI 的跨平台矩阵会红）。
#[cfg(windows)]
const UNINSTALL_TEMP_PREFIX: &str = "campus-auth-uninst-";

/// 卸载模式：等待主进程退出后删除安装目录，并把结果写进系统提示框
///
/// ## 为什么分两段（仅 Windows）
///
/// 执行删除的进程不能住在被删的目录里：Windows 不允许删除运行中的 exe，而本进程默认
/// 就跑在安装目录下，`remove_dir_all(install_dir)` 必然撞上自己。故第一段先把自己
/// 复制到系统临时目录、spawn 第二段执行删除，第一段随即退出。unix 没有这个限制
/// （运行中的可执行文件可以 unlink），单段直删。
///
/// ## 为什么不用 `helper.lock`
///
/// 更新模式的互斥锁落在 `<base>/update/helper.lock`——正是要被删掉的东西之一，
/// 持锁删除锁文件在 Windows 上必然失败。且卸载期间主进程已退出、界面上不可能再发起
/// 第二次卸载，互斥在此处没有对象。改为：Web 路由在 spawn 之前**取消待应用更新**
/// （删 pending.json + staging），使新更新不会在卸载途中开始。
///
/// ## 删除清单/守卫
///
/// 一律来自 `campus_auth::uninstall`（**单一事实源**）：界面上列的清单与这里删的东西
/// 必须逐字对应，两处各写一份必然漂移。
fn run_uninstall(cli: &HelperCli) {
    // 日志必须落在系统临时目录：安装目录（含 logs/）正在被删
    let mut log = HelperLog::open_at(&std::env::temp_dir().join("campus-auth-uninstall.log"));

    let install_dir = match uninstall_install_dir(cli) {
        Ok(d) => d,
        Err(e) => uninstall_abort(&mut log, "卸载失败", &e),
    };
    let base_path = cli.base_path.clone().unwrap_or_else(|| install_dir.clone());
    log.info(&format!(
        "卸载模式启动（第二段={}，安装目录 {}，基础路径 {}，保留用户数据={}）",
        cli.uninstall_phase2,
        install_dir.display(),
        base_path.display(),
        cli.keep_user_data
    ));

    // 守卫：纵深防御。Web 路由已校验过一次（用户能当场看到拒绝原因），
    // 此处再校验一次，堵住"直接用 CLI 参数指定任意目录"的通道。
    if let Err(e) = campus_auth::uninstall::validate_install_dir(&install_dir) {
        uninstall_abort(&mut log, "卸载已中止", &e);
    }
    if let Err(e) = campus_auth::uninstall::validate_base_path(&base_path) {
        uninstall_abort(&mut log, "卸载已中止", &e);
    }

    // 1. 等待主进程退出
    log.info(&format!("等待主进程 (PID {}) 退出...", cli.pid));
    if !wait_for_process_exit(cli.pid, &mut log) {
        uninstall_abort(
            &mut log,
            "卸载已中止",
            &format!(
                "等待主进程退出超时（{} 秒）。\n\n请先退出「认证喵」再重新执行卸载。",
                PROCESS_EXIT_TIMEOUT_SECS
            ),
        );
    }
    // 额外等待一小段时间，确保文件句柄完全释放（与更新流程同口径）
    sleep(Duration::from_millis(500));

    // 2. 仅 Windows：第一段退位，把删除交给临时目录里的第二段
    #[cfg(windows)]
    {
        if !cli.uninstall_phase2 {
            match spawn_uninstall_phase2(&install_dir, &base_path, cli.keep_user_data, &mut log) {
                Ok(()) => {
                    log.info("已交给临时目录中的第二段执行删除，本进程退出");
                    std::process::exit(0);
                }
                Err(e) => {
                    // 不硬着头皮在原地删：删不掉自己所在的目录，只会留下半删的现场
                    uninstall_abort(
                        &mut log,
                        "卸载失败",
                        &format!("无法启动卸载第二段：{e}\n\n程序文件未被删除，可重试或手动删除。"),
                    );
                }
            }
        }
    }

    // 3. 执行删除
    let plan =
        match campus_auth::uninstall::build_plan(&install_dir, &base_path, cli.keep_user_data) {
            Ok(p) => p,
            Err(e) => uninstall_abort(&mut log, "卸载已中止", &e),
        };
    let report = campus_auth::uninstall::execute(&plan);
    log.info(&format!(
        "删除完成：成功 {} 项，失败 {} 项",
        report.steps.iter().filter(|s| s.success).count(),
        report.failures().count()
    ));

    // 4. 结果提示框：主进程已退出，这是唯一还能传达信息的出口
    let ok = report.all_ok();
    let text = campus_auth::uninstall::report_text(&report, &plan);
    notify(
        ok,
        if ok {
            "认证喵 · 卸载完成"
        } else {
            "认证喵 · 卸载未完全成功"
        },
        &text,
    );

    // 5. 最后处理自己（Windows：登记重启后删除；unix：已被一并删除，无需动作）
    schedule_self_delete(&mut log);
}

/// 解析被卸载的程序目录
///
/// 第二段跑在系统临时目录里，无法从自身位置推导出安装目录，必须由第一段用
/// `--install-dir` 显式传入——这也是"删除目标不接受外部任意路径"的一部分：
/// 第一段的值来自它自己的 `current_exe()`，不是用户输入。
fn uninstall_install_dir(cli: &HelperCli) -> Result<PathBuf, String> {
    if cli.uninstall_phase2 {
        return cli
            .install_dir
            .clone()
            .ok_or_else(|| "内部错误：卸载第二段缺少 --install-dir".to_string());
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| "无法确定程序所在目录".to_string())
}

/// 中止卸载：记日志 + 系统提示框 + 以失败码退出
///
/// 卸载失败必须**出声**——主进程已经退出，静默退出会让用户既没拿到结果、也不知道
/// 现场还剩什么。
fn uninstall_abort(log: &mut HelperLog, title: &str, message: &str) -> ! {
    log.error(&format!("{title}：{message}"));
    notify(false, &format!("认证喵 · {title}"), message);
    std::process::exit(1);
}

/// （Windows）把自身复制到系统临时目录并以 `--uninstall-phase2` 重启，由它执行删除
///
/// 复制而非移动：移动（rename）跨卷会失败，而系统临时目录与安装目录不一定同卷。
#[cfg(windows)]
fn spawn_uninstall_phase2(
    install_dir: &Path,
    base_path: &Path,
    keep_user_data: bool,
    log: &mut HelperLog,
) -> Result<(), String> {
    let self_exe = std::env::current_exe().map_err(|e| format!("无法确定自身路径: {e}"))?;
    let stage = std::env::temp_dir().join(format!("{UNINSTALL_TEMP_PREFIX}{}", std::process::id()));
    std::fs::create_dir_all(&stage).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let staged_exe = stage.join(campus_auth::uninstall::helper_exe_name());
    std::fs::copy(&self_exe, &staged_exe).map_err(|e| format!("复制自身到临时目录失败: {e}"))?;

    let mut cmd = std::process::Command::new(&staged_exe);
    cmd.arg("--uninstall")
        .arg("--uninstall-phase2")
        .arg("--pid")
        .arg(std::process::id().to_string())
        .arg("--install-dir")
        .arg(install_dir)
        .arg("--base-path")
        .arg(base_path);
    if keep_user_data {
        cmd.arg("--keep-user-data");
    }
    // helper 内有多行输出，Windows 上隐藏控制台窗口避免闪黑窗（与 spawn_helper 同口径）
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| format!("spawn 第二段失败: {e}"))?;
    log.info(&format!("第二段已从 {} 启动", staged_exe.display()));
    Ok(())
}

/// 系统提示框（Windows）/ stderr（其它平台）
#[cfg(windows)]
fn notify(success: bool, title: &str, text: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MessageBoxW,
    };

    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let text_w = wide(text);
    let title_w = wide(title);
    let icon = if success {
        MB_ICONINFORMATION
    } else {
        MB_ICONWARNING
    };
    // SAFETY: 两个宽字符串缓冲在调用期间存活且以 NUL 结尾；hwnd 传 null 表示无属主窗口
    // （无属主窗口仍会显示在任务栏并置前，符合"卸载完成"的提示需求）。
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | icon,
        );
    }
}

/// 非 Windows 平台：主进程已退出，stderr 可能无人接收，但至少留下痕迹
#[cfg(not(windows))]
fn notify(_success: bool, title: &str, text: &str) {
    eprintln!("[helper] {title}\n{text}");
}

/// （Windows）把自身与所在临时目录删掉
///
/// **主路径是 shell 兜底删除，不是 `MoveFileExW`**——真机演练实测：非管理员账户下
/// `MOVEFILE_DELAY_UNTIL_REBOOT` 会以 `ERROR_ACCESS_DENIED` 失败（该标志要求调用者
/// 属于 Administrators 组或 LocalSystem）。即"重启后删除"对普通用户**等于没做**，
/// 每次卸载都会在 `%TEMP%` 留下一个几 MB 的副本。故先 spawn `cmd`（等本进程退出后
/// 删除自身与已空的临时目录），只在派发失败时才退回 `MoveFileExW` 登记。
///
/// 两条路径的目标都先确认**确实位于系统临时目录内**：若有人直接在安装目录里以
/// `--uninstall-phase2` 手工运行，绝不能把安装目录里的助手当成本次卸载的残留删掉。
#[cfg(windows)]
fn schedule_self_delete(log: &mut HelperLog) {
    let Ok(self_exe) = std::env::current_exe() else {
        return;
    };
    let (Ok(exe_canonical), Ok(temp_canonical)) =
        (self_exe.canonicalize(), std::env::temp_dir().canonicalize())
    else {
        log.debug("跳过自身清理：路径无法规范化");
        return;
    };
    if !exe_canonical.starts_with(&temp_canonical) {
        log.debug(&format!(
            "跳过自身清理：{} 不在系统临时目录内",
            self_exe.display()
        ));
        return;
    }

    if spawn_delayed_delete(std::slice::from_ref(&self_exe), self_exe.parent(), log) {
        return;
    }
    register_delete_on_reboot(&self_exe, log);
}

/// 派发"本进程退出后删除这些路径（含可选的空目录）"的系统命令；成功派发返回 `true`
///
/// 组合是 `ping -n 3 127.0.0.1 >nul`（约 2 秒延迟，够本进程退出并释放文件锁）加
/// `del` / `rmdir`。用 `ping` 而非 `timeout`：后者在无控制台的进程里会因"输入重定向
/// 不受支持"立刻返回，起不到等待作用。目标目录此刻应已只装着我们这份副本，
/// `rmdir` 不带 `/s` 即可（非空时直接失败，比递归删除安全）。
///
/// **抽成独立函数是为了能被单测真的跑一次**：cmd 的引号规则与 `Command` 的默认转义
/// 语义不同（后者是 `CommandLineToArgvW` 那一套），靠推理写不对——本函数第一版就是
/// 这么写错的：命令派发成功、文件却还在。`test_spawn_delayed_delete_removes_file`
/// 会真的起一次 cmd 并断言文件消失。
///
/// 路径含 cmd 会二次解析的字符时**放弃派发**（返回 `false`）：宁可留一个临时文件，
/// 也不冒"命令被拆成两截、删到别的东西"的风险。
#[cfg(windows)]
fn spawn_delayed_delete(
    targets: &[PathBuf],
    dir_to_remove: Option<&Path>,
    log: &mut HelperLog,
) -> bool {
    use std::os::windows::process::CommandExt;

    let unsafe_path = targets.iter().any(|p| !cmd_safe_path(p))
        || dir_to_remove.is_some_and(|d| !cmd_safe_path(d));
    if unsafe_path {
        log.debug("路径含 cmd 特殊字符，跳过 shell 兜底删除");
        return false;
    }
    let quoted = |p: &Path| format!("\"{}\"", p.display());
    let mut line = String::from("ping -n 3 127.0.0.1 >nul");
    for t in targets {
        line.push_str(&format!(" & del /f /q {}", quoted(t)));
    }
    if let Some(dir) = dir_to_remove {
        line.push_str(&format!(" & rmdir /q {}", quoted(dir)));
    }

    let mut cmd = std::process::Command::new("cmd");
    // 命令以 `ping` 开头（首字符不是引号），故**不加** `""…""` 外层包裹：
    // cmd 的 `/c` 只在"首个字符是引号"时才做首尾引号剥离，而剥掉的恰好会是内层
    // 路径的引号，把整行拆坏（第一版就是这么错的：派发成功、文件还在）。
    // 现在的形态等价于在控制台里手敲这一整行。
    cmd.raw_arg(format!("/c {line}"));
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
    match cmd.spawn() {
        Ok(child) => {
            log.info(&format!(
                "已派发系统命令（PID {}）在本进程退出后删除自身与临时目录",
                child.id()
            ));
            true
        }
        Err(e) => {
            log.debug(&format!("派发 shell 兜底删除失败: {e}"));
            false
        }
    }
}

/// 路径是否可以安全地拼进 cmd 命令行（下列字符会被 cmd 二次解析）
#[cfg(windows)]
fn cmd_safe_path(p: &Path) -> bool {
    const UNSAFE: [char; 8] = ['%', '&', '^', '|', '<', '>', '"', '!'];
    let s = p.to_string_lossy();
    !s.chars()
        .any(|c| UNSAFE.contains(&c) || c == '\n' || c == '\r')
}

/// 备用：登记"下次重启时删除"（`MoveFileExW` + `MOVEFILE_DELAY_UNTIL_REBOOT`）
///
/// 仅在前一条路径派发失败时使用。**非管理员账户下这里通常也会失败**（真机演练实测），
/// 故它只是最后一道兜底，不能当主路径。
#[cfg(windows)]
fn register_delete_on_reboot(self_exe: &Path, log: &mut HelperLog) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};

    let wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let stage_dir = self_exe.parent().map(Path::to_path_buf);
    for (target, what) in [
        (Some(self_exe.to_path_buf()), "自身可执行文件"),
        (stage_dir, "临时目录"),
    ] {
        let Some(target) = target else { continue };
        let target_w = wide(&target);
        // SAFETY: 路径缓冲以 NUL 结尾且在调用期间存活；目标为 null 表示"只登记删除"。
        // 两次调用按可执行文件、目录的顺序注册，系统的待重命名队列按序执行，
        // 故目录必然在文件被删空之后才轮到自己。
        let ok = unsafe {
            MoveFileExW(
                target_w.as_ptr(),
                std::ptr::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            )
        };
        if ok == 0 {
            log.debug(&format!(
                "登记{what}重启后删除失败（非管理员账户下属预期）: {}",
                target.display()
            ));
        } else {
            log.info(&format!(
                "已登记{what}在下次重启时删除: {}",
                target.display()
            ));
        }
    }
}

/// 非 Windows：运行中的可执行文件允许 unlink，安装目录整体删除时本进程文件已被一并
/// 删除，无需任何后续动作。
#[cfg(not(windows))]
fn schedule_self_delete(_log: &mut HelperLog) {}

/// 对 helper 锁文件取排他锁（双 helper 互斥）
///
/// 锁文件打开失败按 fail-closed 处理（exit 1，更新留给下次尝试）；
/// 锁被同伴持有则安静退出（exit 0，先行者负责完成替换与清理）。
/// 返回的 `File` 须保持存活至进程退出（进程退出即释放）。
fn acquire_helper_lock(lock_path: &Path, log: &mut HelperLog) -> std::fs::File {
    if let Some(parent) = lock_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log.error(&format!("创建 update 目录失败: {e}"));
            std::process::exit(1);
        }
    }
    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            log.error(&format!("打开 helper 锁文件失败: {e}"));
            std::process::exit(1);
        }
    };
    match <std::fs::File as fs4::FileExt>::try_lock(&file) {
        Ok(()) => {}
        Err(fs4::TryLockError::WouldBlock) => {
            let e = "锁已被占用";
            log.info(&format!("另一 helper 持有互斥锁（{e}），本实例安静退出"));
            std::process::exit(0);
        }
        Err(fs4::TryLockError::Error(e)) => {
            log.error(&format!("获取 helper 互斥锁失败: {e}"));
            std::process::exit(1);
        }
    }
    file
}

/// pending 版本是否允许应用：须严格高于 `current`，解析失败按拒绝处理（fail-closed）
fn pending_version_allowed(pending_version: &str, current: &str) -> bool {
    let Ok(pending) = semver::Version::parse(pending_version) else {
        return false;
    };
    let current = semver::Version::parse(current).unwrap_or_else(|_| semver::Version::new(0, 0, 0));
    pending > current
}

/// 两文件内容是否一致（任一读取/缺失视为不同）
fn files_identical(a: &Path, b: &Path) -> bool {
    match (file_sha256(a), file_sha256(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 轮询等待指定 PID 的进程退出（最多等待 [`PROCESS_EXIT_TIMEOUT_SECS`] 秒）
///
/// 返回 `true` 表示主进程已退出；超时返回 `false`。5.3：超时后**不再强制继续**——
/// 主进程仍存活时覆盖运行中 exe 的替换必然失败，且强制继续会走 cleanup 摧毁 staging
/// 与 pending.json，导致更新彻底丢失。改为报错退出并保留 staging/pending，把应用机会
/// 留给主进程下次启动的 `apply_pending_on_startup`。
///
/// 超时信息写入 HelperLog（GUI 子系统下 stderr 不可见，更新失败必须可从 helper.log 诊断）。
fn wait_for_process_exit(pid: u32, log: &mut HelperLog) -> bool {
    for _ in 0..(PROCESS_EXIT_TIMEOUT_SECS * 1000 / PROCESS_EXIT_POLL_MS) {
        if !is_process_alive(pid) {
            return true;
        }
        sleep(Duration::from_millis(PROCESS_EXIT_POLL_MS));
    }
    log.error(&format!(
        "等待进程退出超时（{} 秒），中止更新（staging 与 pending 已保留）",
        PROCESS_EXIT_TIMEOUT_SECS
    ));
    false
}

/// overlay 同步更新包内的分发目录到 base_path（步骤 5.5）
///
/// 覆盖 `resources/`、`docs/`、`python_worker/`（Python 源码与 pyproject/uv.lock）。
/// `skip_names` 命中的目录名整棵子树跳过——发布包本就不含这些（release.yml 已排除），
/// 此处是防御性双保险：`.venv` 是用户引导出的运行态，`__pycache__` 运行时自动再生。
/// best-effort：单文件失败仅告警继续，不回滚（exe 已替换，半新半旧由下次更新收敛）。
fn sync_distribution_files(extracted_dir: &Path, base_path: &Path, worker_target_dir: &Path) {
    // 必须在 overlay 前比较并写标记：成功覆盖后源/目标必然相同；先写标记还可
    // 覆盖“清单已替换、helper 随后异常退出”的崩溃窗口。标记只会在主程序
    // 完成 uv sync + Worker 探针 + 指纹记录后清除。
    let manifests_changed = dependency_manifests_changed(extracted_dir, worker_target_dir);
    if manifests_changed {
        let marker = worker_target_dir.join(campus_auth::environment::RESYNC_MARKER);
        let marker_result = std::fs::create_dir_all(worker_target_dir)
            .and_then(|()| std::fs::write(&marker, Local::now().to_rfc3339()));
        match marker_result {
            Ok(()) => println!("[helper] Python 依赖清单变更，已预写重同步标记"),
            Err(e) => eprintln!("[helper] 写重同步标记失败: {e}"),
        }
    }

    for dir in ["resources", "docs", "python_worker"] {
        let src = extracted_dir.join(dir);
        if !src.exists() {
            continue;
        }
        let dst = if dir == "python_worker" {
            worker_target_dir.to_path_buf()
        } else {
            base_path.join(dir)
        };
        println!("[helper] 同步 {dir}/ -> {}", dst.display());
        if let Err(e) = copy_dir_overlay(&src, &dst, &[".venv", "__pycache__"]) {
            eprintln!("[helper] 同步 {dir}/ 失败（继续）: {e}");
        }
    }
}

/// 在 overlay 前判断 Python 依赖清单是否变化。
fn dependency_manifests_changed(extracted_dir: &Path, worker_target_dir: &Path) -> bool {
    let py_src = extracted_dir.join("python_worker").join("pyproject.toml");
    let lock_src = extracted_dir.join("python_worker").join("uv.lock");
    let py_dst = worker_target_dir.join("pyproject.toml");
    let lock_dst = worker_target_dir.join("uv.lock");
    (py_src.exists() && file_differs(&py_src, &py_dst))
        || (lock_src.exists() && file_differs(&lock_src, &lock_dst))
}

/// 两个文件内容是否不同（任一侧读取失败/缺失视为不同）
fn file_differs(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x != y,
        _ => true,
    }
}

/// 递归 overlay 复制目录：目标侧不存在的路径创建，已存在的文件覆盖
fn copy_dir_overlay(src: &Path, dst: &Path, skip_names: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut first_error = None;
    for entry in std::fs::read_dir(src)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                first_error.get_or_insert(e);
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if skip_names.contains(&name_str) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let result = if src_path.is_dir() {
            copy_dir_overlay(&src_path, &dst_path, skip_names)
        } else {
            std::fs::copy(&src_path, &dst_path).map(|_| ())
        };
        if let Err(e) = result {
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// 替换 helper 自身（步骤 5.5，best-effort）
///
/// Windows 不允许覆盖写运行中的 exe，但允许 rename：先把旧 helper 改名
/// `<原名>.old` 让位，再复制新版本；最后尝试删 .old（运行中删除会失败，
/// 残留一个无害文件，下次更新覆盖重试）。任一步失败仅告警——旧 helper
/// 依然能完成未来的 exe 替换（接口仅依赖 pending.json 文件，保持稳定）。
fn replace_helper(extracted_dir: &Path, base_path: &Path, log: &mut HelperLog) {
    // 名字取自 `uninstall`（与卸载路径共用，避免两处各写一份平台分支）
    let helper_name = campus_auth::uninstall::helper_exe_name();
    let new_helper = extracted_dir.join(helper_name);
    if !new_helper.exists() {
        return;
    }
    let target = base_path.join(helper_name);
    let old = base_path.join(format!("{helper_name}.old"));
    if let Err(e) = std::fs::remove_file(&old) {
        log.debug(&format!("清理残留的 {} 失败: {e}", old.display()));
    }
    if target.exists() {
        if let Err(e) = std::fs::rename(&target, &old) {
            eprintln!("[helper] 旧 helper 改名失败，跳过 helper 自更新: {e}");
            return;
        }
    }
    if let Err(e) = std::fs::copy(&new_helper, &target) {
        eprintln!("[helper] helper 替换失败: {e}，恢复旧版本");
        if let Err(e) = std::fs::rename(&old, &target) {
            log.debug(&format!(
                "恢复旧 helper 失败（{} -> {}）: {e}",
                old.display(),
                target.display()
            ));
        }
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)) {
            log.debug(&format!("设置新 helper 可执行权限失败: {e}"));
        }
    }
    println!("[helper] helper 已更新");
    if let Err(e) = std::fs::remove_file(&old) {
        log.debug(&format!("清理 {} 失败: {e}", old.display()));
    }
}

/// 清理 pending.json 标记与 staging 目录（staging 目录用 CLI --staging 传入的实际路径）
///
/// G13：staging 取值可能来自被篡改的 pending.json，remove_dir_all 前复核其
/// 确实位于 base_path 之内，避免 cleanup 变成任意目录删除。
fn cleanup(base_path: &Path, staging_dir: &Path, log: &mut HelperLog) {
    let pending_path = base_path.join("update").join("pending.json");
    if let Err(e) = std::fs::remove_file(&pending_path) {
        log.debug(&format!("清理 pending.json 失败: {e}"));
    }
    if is_within_base(staging_dir, base_path) {
        if let Err(e) = std::fs::remove_dir_all(staging_dir) {
            log.debug(&format!("清理 staging 目录失败: {e}"));
        }
    } else {
        eprintln!(
            "[helper] 拒绝清理 base_path 之外的 staging 目录: {}",
            staging_dir.display()
        );
    }
}

/// 获取当前平台的可执行文件名
fn exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "campus-auth.exe"
    } else {
        "campus-auth"
    }
}

/// 解析待替换的目标 exe（纯函数，便于单测）
///
/// `derived` 为 helper 按自身位置推导出的主程序路径，`provided` 为 `--target` /
/// pending.json 提供的值，`base_path` 用于推导值缺失时的兜底约束。
///
/// 分支：
/// - 推导值存在 → 以它为准；提供值必须与其一致，否则判定 pending 被篡改（返回 `None`）；
/// - 推导值缺失（主程序被重命名/删除）→ 退回提供值，但要求其确实是文件且位于
///   base_path 之内（保留 G13 旧约束，不退化为"任意已存在文件"）；
/// - 其余 → `None`，调用方拒绝执行。
fn resolve_target_exe(
    derived: Option<PathBuf>,
    provided: Option<PathBuf>,
    base_path: &Path,
) -> Option<PathBuf> {
    match (derived, provided) {
        (Some(derived), provided) if derived.is_file() => {
            if let Some(p) = provided {
                if !same_existing_path(&p, &derived) {
                    return None;
                }
            }
            Some(derived)
        }
        (_, Some(p)) if p.is_file() && is_within_base(&p, base_path) => Some(p),
        _ => None,
    }
}

/// 校验路径位于 base_path 之内（G13，防 pending.json 篡改的路径逃逸）
///
/// 双方 canonicalize（解析为绝对真实路径，含符号链接折叠与 Windows `\\?\`
/// 前缀归一）后做 `starts_with` 前缀比较；任一路径不存在（canonicalize 失败）
/// 均视为不合法。
///
/// 现用于 staging（remove_dir_all 的目标）与 target 的兜底分支；
/// target 的主校验已改为与推导值比对（见 [`resolve_target_exe`]）。
fn is_within_base(path: &Path, base_path: &Path) -> bool {
    let (Ok(canonical), Ok(base_canonical)) = (path.canonicalize(), base_path.canonicalize())
    else {
        return false;
    };
    canonical.starts_with(&base_canonical)
}

/// G13：替换前复核 staging exe 的 SHA256
///
/// `expected` 为空直接拒绝（不再降级信任 HTTPS）；非空但与实际不符时返回
/// false——staging 损坏或被篡改，必须中止替换（调用方随后清理不可信 staging）。
/// 失败原因写入 HelperLog（GUI 子系统下 stderr 不可见，见模块头说明）。
fn verify_staging_sha256(extracted_exe: &Path, expected: &str, log: &mut HelperLog) -> bool {
    if expected.is_empty() {
        log.error("pending.json 未携带 SHA256，已拒绝替换（需补校验值）");
        return false;
    }
    match file_sha256(extracted_exe) {
        Ok(actual) if actual.eq_ignore_ascii_case(expected) => true,
        Ok(actual) => {
            log.error(&format!("SHA256 不匹配: expected={expected}, got={actual}"));
            false
        }
        Err(e) => {
            log.error(&format!("计算 staging SHA256 失败: {e}"));
            false
        }
    }
}

/// 探测子进程存活状态（G13）
///
/// 返回值：`Ok(None)` = 仍在运行；`Ok(Some(code))` = 已退出（退出码，信号
/// 终止等无退出码场景以 -1 表示）；`Err(())` = try_wait 系统调用失败。
fn probe_alive(child: &mut std::process::Child) -> Result<Option<i32>, ()> {
    child
        .try_wait()
        .map(|status| status.map(|st| st.code().unwrap_or(-1)))
        .map_err(|_| ())
}

/// G13：根据新进程的两次探测结果决定是否删除备份
///
/// 仅当首查存活且 5 秒后复查仍存活（两次均为 `Ok(None)`）时才删除 .bak；
/// 其余组合（任一次已退出 / 探测失败 / 首查失败未二查）一律保留备份。
fn decide_backup_deletion(
    first_probe: Result<Option<i32>, ()>,
    second_probe: Result<Option<i32>, ()>,
) -> bool {
    matches!((first_probe, second_probe), (Ok(None), Ok(None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G13：路径逃逸防护——base_path 内的路径放行，外部路径与不存在路径拒绝
    #[test]
    fn test_is_within_base() {
        let base = tempfile::tempdir().unwrap();
        let inside = base.path().join("update").join("staging");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(is_within_base(&inside, base.path()));

        // 存在但位于 base 之外的路径必须拒绝
        let outside = tempfile::tempdir().unwrap();
        assert!(!is_within_base(outside.path(), base.path()));

        // 不存在的路径 canonicalize 失败 → 拒绝
        assert!(!is_within_base(
            &base.path().join("does-not-exist"),
            base.path()
        ));
    }

    /// target 解析三分支：推导命中且一致放行 / 不一致拒绝 / 推导缺失按 base 内约束兜底
    #[test]
    fn test_resolve_target_exe() {
        let base = tempfile::tempdir().unwrap();
        let derived = base.path().join("campus-auth.exe");
        std::fs::write(&derived, b"exe").unwrap();

        // 推导命中 + 提供值一致（路径写法不同）→ 放行，返回推导值
        assert_eq!(
            resolve_target_exe(
                Some(derived.clone()),
                Some(base.path().join("./campus-auth.exe")),
                base.path()
            ),
            Some(derived.clone())
        );
        // 推导命中 + 未提供值 → 放行
        assert_eq!(
            resolve_target_exe(Some(derived.clone()), None, base.path()),
            Some(derived.clone())
        );
        // 推导命中 + 提供值不一致（pending 被篡改）→ 拒绝
        let evil = base.path().join("evil.exe");
        std::fs::write(&evil, b"x").unwrap();
        assert_eq!(
            resolve_target_exe(Some(derived.clone()), Some(evil), base.path()),
            None
        );

        // 推导缺失（主程序被重命名）+ 提供值在 base 内 → 兜底放行
        let renamed = base.path().join("auth-renamed.exe");
        std::fs::write(&renamed, b"exe").unwrap();
        assert_eq!(
            resolve_target_exe(None, Some(renamed.clone()), base.path()),
            Some(renamed)
        );
        // 推导缺失 + 提供值在 base 之外 → 拒绝（不得退化为任意文件覆写）
        let outside = tempfile::tempdir().unwrap();
        let outside_exe = outside.path().join("target.exe");
        std::fs::write(&outside_exe, b"x").unwrap();
        assert_eq!(
            resolve_target_exe(None, Some(outside_exe), base.path()),
            None
        );
        // 两者皆无 → 拒绝
        assert_eq!(resolve_target_exe(None, None, base.path()), None);
    }

    /// G13：SHA 复核——正确值通过、错误值拒绝、空值拒绝（P1-3：缺失拒绝，不降级）、文件缺失拒绝
    #[test]
    fn test_verify_staging_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("campus-auth.exe");
        std::fs::write(&exe, b"staged-binary-content").unwrap();
        let mut log = HelperLog::open_at(&dir.path().join("helper.log"));

        use sha2::{Digest, Sha256};
        let correct = hex::encode(Sha256::digest(b"staged-binary-content"));

        // 空期望：拒绝（缺失拒绝，不降级跳过）
        assert!(!verify_staging_sha256(&exe, "", &mut log));
        // 正确摘要：通过（大小写不敏感）
        assert!(verify_staging_sha256(&exe, &correct, &mut log));
        assert!(verify_staging_sha256(
            &exe,
            &correct.to_uppercase(),
            &mut log
        ));
        // 错误摘要：拒绝
        assert!(!verify_staging_sha256(&exe, "deadbeef", &mut log));
        // 文件缺失：拒绝
        assert!(!verify_staging_sha256(
            &dir.path().join("missing.exe"),
            &correct,
            &mut log
        ));
    }

    /// G13：延迟删备份决策——仅"两次探测均存活"才允许删除
    #[test]
    fn test_decide_backup_deletion() {
        // 首查存活 + 5 秒后仍存活 → 删除
        assert!(decide_backup_deletion(Ok(None), Ok(None)));
        // 首查已退出（无需二查，重复传入首查值）→ 保留
        assert!(!decide_backup_deletion(Ok(Some(0)), Ok(Some(0))));
        // 首查存活、复查已退出（运行数秒后崩溃）→ 保留
        assert!(!decide_backup_deletion(Ok(None), Ok(Some(1))));
        // 任一次探测失败 → 保留（保守）
        assert!(!decide_backup_deletion(Ok(None), Err(())));
        assert!(!decide_backup_deletion(Err(()), Err(())));
    }

    /// overlay 同步：覆盖同名文件、新增缺失文件、跳过 .venv/__pycache__、
    /// 不删除目标侧独有内容
    #[test]
    fn test_copy_dir_overlay() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");

        // 源：worker 新版文件 + 意外混入的 .venv（防御性跳过）
        std::fs::create_dir_all(src.join("tests")).unwrap();
        std::fs::write(src.join("playwright_worker.py"), b"NEW").unwrap();
        std::fs::write(src.join("tests").join("t.py"), b"new-test").unwrap();
        std::fs::create_dir_all(src.join(".venv").join("Scripts")).unwrap();
        std::fs::write(src.join(".venv").join("Scripts").join("python.exe"), b"x").unwrap();

        // 目标：旧版同名文件 + 用户运行态 .venv + 目标独有文件
        std::fs::create_dir_all(dst.join(".venv").join("Scripts")).unwrap();
        std::fs::write(dst.join("playwright_worker.py"), b"OLD").unwrap();
        std::fs::write(
            dst.join(".venv").join("Scripts").join("python.exe"),
            b"USER-VENV",
        )
        .unwrap();
        std::fs::write(dst.join("user-config-only.txt"), b"KEEP").unwrap();

        copy_dir_overlay(&src, &dst, &[".venv", "__pycache__"]).unwrap();

        // 同名覆盖
        assert_eq!(
            std::fs::read(dst.join("playwright_worker.py")).unwrap(),
            b"NEW"
        );
        // 新增缺失
        assert_eq!(
            std::fs::read(dst.join("tests").join("t.py")).unwrap(),
            b"new-test"
        );
        // 目标侧 .venv 保留（用户运行态），src 侧 .venv 不入侵
        assert_eq!(
            std::fs::read(dst.join(".venv").join("Scripts").join("python.exe")).unwrap(),
            b"USER-VENV"
        );
        // 目标独有内容不删除
        assert!(dst.join("user-config-only.txt").exists());
    }

    #[test]
    fn test_sync_distribution_marks_changed_python_manifests_before_overlay() {
        let extracted = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let src_worker = extracted.path().join("python_worker");
        let dst_worker = base.path().join("python_worker");
        std::fs::create_dir_all(&src_worker).unwrap();
        std::fs::create_dir_all(&dst_worker).unwrap();
        std::fs::write(src_worker.join("pyproject.toml"), b"new-project").unwrap();
        std::fs::write(src_worker.join("uv.lock"), b"new-lock").unwrap();
        std::fs::write(dst_worker.join("pyproject.toml"), b"old-project").unwrap();
        std::fs::write(dst_worker.join("uv.lock"), b"old-lock").unwrap();

        sync_distribution_files(extracted.path(), base.path(), &dst_worker);

        assert_eq!(
            std::fs::read(dst_worker.join("pyproject.toml")).unwrap(),
            b"new-project"
        );
        assert!(
            dst_worker
                .join(campus_auth::environment::RESYNC_MARKER)
                .is_file(),
            "成功 overlay 后仍应保留预写的重同步标记"
        );
    }

    #[test]
    fn test_sync_distribution_does_not_mark_identical_python_manifests() {
        let extracted = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let dst_worker = base.path().join("python_worker");
        for root in [extracted.path(), base.path()] {
            let worker = root.join("python_worker");
            std::fs::create_dir_all(&worker).unwrap();
            std::fs::write(worker.join("pyproject.toml"), b"same-project").unwrap();
            std::fs::write(worker.join("uv.lock"), b"same-lock").unwrap();
        }

        sync_distribution_files(extracted.path(), base.path(), &dst_worker);

        assert!(
            !base
                .path()
                .join("python_worker")
                .join(campus_auth::environment::RESYNC_MARKER)
                .exists()
        );
    }

    /// 版本闸门：高于当前放行；等于/低于/无法解析拒绝；当前版本无法解析回退 0.0.0
    #[test]
    fn test_pending_version_allowed() {
        assert!(pending_version_allowed("5.0.1", "5.0.0"));
        assert!(pending_version_allowed("6.0.0-alpha.1", "5.0.0"));
        assert!(!pending_version_allowed("5.0.0", "5.0.0"));
        assert!(!pending_version_allowed("4.9.9", "5.0.0"));
        assert!(!pending_version_allowed("not-a-version", "5.0.0"));
        // 当前版本无法解析时回退 0.0.0（与主进程 UpdaterService 同语义）
        assert!(pending_version_allowed("0.0.1", "bad"));
    }

    /// 幂等跳过判定：内容一致 / 不一致 / 缺失
    #[test]
    fn test_files_identical() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.exe");
        let b = dir.path().join("b.exe");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(files_identical(&a, &b));
        std::fs::write(&b, b"different").unwrap();
        assert!(!files_identical(&a, &b));
        assert!(!files_identical(&a, &dir.path().join("missing.exe")));
    }

    /// file_sha256 与已知摘要一致
    #[test]
    fn test_file_sha256_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("data.bin");
        std::fs::write(&f, b"hello campus-auth").unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(
            file_sha256(&f).unwrap(),
            hex::encode(Sha256::digest(b"hello campus-auth"))
        );
    }

    /// 真的起一次 cmd，验证"退出后删除"的命令行能被 cmd 正确解析并执行
    ///
    /// 为什么要用真进程测：cmd 的引号规则不是 `CommandLineToArgvW` 那一套，靠推理
    /// 写不对——本函数第一版（`Command::arg` 直接拼、且外层引号对没配平）派发成功却
    /// 什么都没删，单测与人工审阅都看不出问题，是端到端演练才暴露的。
    ///
    /// 路径刻意带空格：用户的 `%TEMP%`（`C:\Users\John Doe\…`）与中文用户名下的临时
    /// 目录都带空格或非 ASCII，正是引号最容易出错的地方。
    #[cfg(windows)]
    #[test]
    fn test_spawn_delayed_delete_removes_file() {
        let base = tempfile::tempdir().unwrap();
        let stage = base
            .path()
            .join("with space")
            .join("campus-auth-uninst-1234");
        std::fs::create_dir_all(&stage).unwrap();
        let victim = stage.join("campus-auth-helper.exe");
        std::fs::write(&victim, b"x").unwrap();

        let mut log = HelperLog::open_at(&base.path().join("helper.log"));
        assert!(
            spawn_delayed_delete(std::slice::from_ref(&victim), Some(&stage), &mut log),
            "命令应派发成功"
        );

        // `ping -n 3` 约 2 秒延迟：轮询等它动手
        for _ in 0..60 {
            if !victim.exists() && !stage.exists() {
                break;
            }
            sleep(Duration::from_millis(250));
        }
        assert!(!victim.exists(), "cmd 没有删掉目标文件（引号规则有误）");
        assert!(!stage.exists(), "cmd 没有删掉空目录");
    }

    /// 含 cmd 特殊字符的路径直接放弃派发（宁可留临时文件，也不冒误删风险）
    #[cfg(windows)]
    #[test]
    fn test_spawn_delayed_delete_skips_unsafe_path() {
        let base = tempfile::tempdir().unwrap();
        let mut log = HelperLog::open_at(&base.path().join("helper.log"));
        // 只用 Windows 合法文件名字符中"对 cmd 有特殊含义"的那些
        // （`|` `<` `>` `"` 本身就是非法文件名，造不出这种路径来测）
        for name in ["a%b.exe", "a&b.exe", "a^b.exe", "a!b.exe"] {
            let p = base.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            assert!(
                !spawn_delayed_delete(std::slice::from_ref(&p), None, &mut log),
                "{name} 不该被拼进命令行"
            );
            assert!(p.exists(), "{name} 不该被删");
        }
    }
}
