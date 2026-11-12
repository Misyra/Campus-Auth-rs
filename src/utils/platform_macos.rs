//! macOS 自启动：LaunchAgent plist + launchd bootstrap/bootout 的登记、卸载与状态观测。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

/// LaunchAgent 标识（plist 文件名与 launchd 域内 id；与 .app bundle 无关的固定值）。
///
/// 沿用历史版本的 PascalCase 标识：改 id 会让旧版本登记的 plist 成为残留孤儿
/// （注销前仍会按 RunAtLoad 自启），因此保持不变。
const LAUNCH_AGENT_LABEL: &str = "com.misyra.Campus-Auth";

/// launchctl 语义状态：print 未命中（退出码 113）为未加载；运行中携带任务 PID。
#[derive(Debug, PartialEq, Eq)]
enum LaunchdState {
    /// gui 域内无此任务。
    NotFound,
    /// 已登记但未运行（RunAtLoad 触发前或已退出的任务）。
    Idle,
    /// 正在运行，携带 launchd 观测到的进程 PID。
    Running(u32),
}

/// 本进程对应的 launchd 自启动状态；配置标记不参与有效状态计算。
#[derive(Debug, Serialize)]
pub struct MacosAutostartStatus {
    /// LaunchAgent plist 是否存在。
    pub registered: bool,
    /// plist 登记命令是否指向当前可执行文件。
    pub path_match: bool,
    /// plist 登记参数是否与当前数据目录一致（含参数合法性）。
    pub base_path_match: bool,
    /// 登记目标可执行文件是否存在。
    pub target_exists: bool,
    /// plist 是否声明 RunAtLoad（无法解析时为 null）。
    pub run_at_load: Option<bool>,
    /// launchd 是否已登记该任务（`launchctl print` 命中）。
    pub loaded: bool,
    /// 加载任务的运行中 PID；已登记未运行为 null。
    pub job_pid: Option<u32>,
    /// 当前可执行文件路径。
    pub expected_path: String,
    /// 当前数据目录。
    pub expected_base_path: String,
    /// LaunchAgent plist 路径；未登记为 null。
    pub plist_path: Option<String>,
    /// plist 登记的完整参数表；无法解析（如二进制 plist）为 null。
    pub program_arguments: Option<Vec<String>>,
    /// 根据系统观测推导的有效开关。
    pub enabled: bool,
    /// 有效状态及不能启用的具体原因。
    pub state: &'static str,
}

/// plist XML 文本节点最小转义：仅 `& < >` 三个字符（引号在元素文本中合法）。
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 构造 LaunchAgent plist 内容；`run_at_load` 控制是否声明登录即启动。
///
/// ProgramArguments 每个元素是独立字符串节点，路径中的空格无需转义
/// （launchd 直接按数组取参，不存在 shell 分词）；仅 XML 文本节点需要转义。
fn build_plist(label: &str, exe: &Path, base: &Path, run_at_load: bool) -> String {
    let mut plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n",
        escape_xml(label),
    );
    // 与 Windows 注册口径一致：来源标记 + 数据目录，供启动状态复核比对
    for arg in [
        exe.to_string_lossy().into_owned(),
        "--startup-launch".to_owned(),
        "--base-path".to_owned(),
        base.to_string_lossy().into_owned(),
    ] {
        plist.push_str(&format!("\t\t<string>{}</string>\n", escape_xml(&arg)));
    }
    plist.push_str("\t</array>\n");
    if run_at_load {
        plist.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    }
    plist.push_str("</dict>\n</plist>\n");
    plist
}

/// 从 plist 文本提取 ProgramArguments 数组（仅按本项目生成形态做轻量解析；
/// 二进制 plist 或手写异形 plist 解析失败时返回 None，状态机按 path_mismatch 处理）。
fn parse_program_arguments(content: &str) -> Option<Vec<String>> {
    let rest = &content[content.find("<key>ProgramArguments</key>")?..];
    let array_start = rest.find("<array>")? + "<array>".len();
    let array = &rest[array_start..rest[array_start..].find("</array")? + array_start];
    let mut args = Vec::new();
    let mut search = array;
    while let Some(start) = search.find("<string>") {
        let after = &search[start + "<string>".len()..];
        let end = after.find("</string>")?;
        args.push(unescape_xml(&after[..end]));
        search = &after[end + "</string>".len()..];
    }
    Some(args)
}

/// 还原 [`escape_xml`] 的转义（先处理尖括号，最后处理 &，避免二次还原）。
fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// 从 plist 文本提取 RunAtLoad 声明；键缺失或值形态异常时返回 None。
fn parse_run_at_load(content: &str) -> Option<bool> {
    let rest = &content[content.find("<key>RunAtLoad</key>")? + "<key>RunAtLoad</key>".len()..];
    let value = rest.trim_start();
    if value.starts_with("<true/>") {
        Some(true)
    } else if value.starts_with("<false/>") {
        Some(false)
    } else {
        None
    }
}

/// 归一两个路径再比较：canonicalize 解析符号链接与大小写（macOS 默认卷大小写不敏感），
/// 不存在的路径按字面比较，与 Windows 侧行为一致。
fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// plist 登记参数与当前 exe/数据目录的状态判定（纯函数，便于单测覆盖状态机）。
///
/// 返回 (state, path_match, target_exists, base_path_match)；
/// `base_path_match` 与 Windows 口径一致，包含参数合法性（未知参数会判 false）。
fn classify(
    registered: bool,
    args: Option<&[String]>,
    run_at_load: Option<bool>,
    loaded: bool,
    exe: &Path,
    base: &Path,
) -> (&'static str, bool, bool, bool) {
    let path = args.and_then(|a| a.first());
    let path_match = path.is_some_and(|p| same_path(Path::new(p), exe));
    let target_exists = path.is_some_and(|p| Path::new(p).is_file());
    // 兼容旧版无参数 plist（仅裸 exe）：exe 所在目录即默认数据目录；
    // 新登记只允许来源标记和数据目录参数，未知参数可能导致 CLI 直接退出
    let mut base_match = exe.parent().is_some_and(|p| same_path(p, base));
    let mut valid_args = true;
    let mut seen_marker = false;
    let mut seen_base = false;
    if let Some(args) = args {
        let mut tail = args.iter().skip(1);
        while let Some(arg) = tail.next() {
            match arg.as_str() {
                "--startup-launch" => {
                    valid_args &= !seen_marker;
                    seen_marker = true;
                }
                "--base-path" => {
                    valid_args &= !seen_base;
                    seen_base = true;
                    base_match = tail.next().is_some_and(|p| same_path(Path::new(p), base));
                }
                _ => valid_args = false,
            }
        }
    }
    let state = if !registered {
        "not_registered"
    } else if !path_match || !target_exists {
        "path_mismatch"
    } else if !base_match || !valid_args || run_at_load != Some(true) {
        "command_mismatch"
    } else if !loaded {
        "not_loaded"
    } else {
        // 与 Windows 的 "enabled" 区分开：前端对该值的描述文案是 Windows 专属，
        // macOS 走前端 default 分支显示中性的「已启用」
        "launchd_enabled"
    };
    (state, path_match, target_exists, base_match && valid_args)
}

/// LaunchAgent 目录（~/Library/LaunchAgents）。
fn launch_agents_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("无法确定用户主目录"))?;
    Ok(home.join("Library/LaunchAgents"))
}

/// plist 落盘路径（`<dir>/<label>.plist`）。
fn plist_path_in(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!("{label}.plist"))
}

/// launchd gui 域内的任务引用（`gui/<uid>/<label>`）。
fn service_target(uid: u32, label: &str) -> String {
    format!("gui/{uid}/{label}")
}

/// 查询 launchd 域内任务状态。
///
/// Err 仅表示 launchctl 本身不可执行；print 未命中（退出码 113）返回 NotFound。
fn launchctl_state(target: &str) -> Result<LaunchdState> {
    let output = std::process::Command::new("launchctl")
        .arg("print")
        .arg(target)
        .output()
        .map_err(|error| anyhow!("执行 launchctl print 失败: {error}"))?;
    if !output.status.success() {
        return Ok(LaunchdState::NotFound);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 运行中任务的输出含 "pid = 503" 行（部分版本带分号结尾）；空闲任务无 pid 行
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("pid =")
            && let Ok(pid) = rest.trim().trim_end_matches(';').trim().parse::<u32>()
        {
            return Ok(LaunchdState::Running(pid));
        }
    }
    Ok(LaunchdState::Idle)
}

/// 在 gui 域登记任务（macOS 10.11+ 推荐口径，支持显式指定域，SSH 会话亦可操作）。
fn launchctl_bootstrap(uid: u32, plist: &Path) -> Result<()> {
    let output = std::process::Command::new("launchctl")
        .arg("bootstrap")
        .arg(format!("gui/{uid}"))
        .arg(plist)
        .output()
        .map_err(|error| anyhow!("执行 launchctl bootstrap 失败: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "launchctl bootstrap 失败（{}）: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// 卸载 gui 域内任务；退出码 3（No such process）视为成功（并发下可能已被卸载）。
fn launchctl_bootout(target: &str) -> Result<()> {
    let output = std::process::Command::new("launchctl")
        .arg("bootout")
        .arg(target)
        .output()
        .map_err(|error| anyhow!("执行 launchctl bootout 失败: {error}"))?;
    if output.status.success() || output.status.code() == Some(3) {
        return Ok(());
    }
    bail!(
        "launchctl bootout 失败（{}）: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// legacy `load` 口径（macOS 10.10 起废弃，仅作为 bootstrap 不可用时的回退）。
fn launchctl_load(plist: &Path) -> Result<()> {
    let output = std::process::Command::new("launchctl")
        .arg("load")
        .arg(plist)
        .output()
        .map_err(|error| anyhow!("执行 launchctl load 失败: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "launchctl load 失败（{}）: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// 原子写入 plist（复用全局 tmp+persist 原子写，避免半截 XML 被下次登录的 launchd 读取）。
/// 返回 plist 落盘路径。
fn write_plist(
    dir: &Path,
    label: &str,
    exe: &Path,
    base: &Path,
    run_at_load: bool,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = plist_path_in(dir, label);
    crate::utils::io::atomic_write_bytes(
        &path,
        build_plist(label, exe, base, run_at_load).as_bytes(),
    )
    .with_context(|| format!("写入 LaunchAgent plist 失败: {}", path.display()))?;
    Ok(path)
}

/// 以「无 RunAtLoad」临时形态登记，激活成功后立即恢复正式 plist。
///
/// 直接 bootstrap 含 RunAtLoad 的 plist 会让 launchd 立即拉起一个重复实例——
/// 实例锁冲突后它会替运行中的实例打开浏览器标签，用户会把这次弹窗误解为启用
/// 操作出了错。launchd 只在登记时读取一次 plist 文件，因此先以不带 RunAtLoad
/// 的形态登记（加载但不运行），成功后再把正式内容写回磁盘；下次登录时 launchd
/// 重新读取磁盘 plist，按 RunAtLoad 正常自启。
fn activate_and_restore(dir: &Path, label: &str, exe: &Path, base: &Path, uid: u32) -> Result<()> {
    let plist_path = write_plist(dir, label, exe, base, false)?;
    let activated = match launchctl_bootstrap(uid, &plist_path) {
        Ok(()) => Ok(()),
        Err(bootstrap_error) => match launchctl_load(&plist_path) {
            Ok(()) => {
                tracing::warn!("bootstrap 不可用，已回退 legacy load 登记: {bootstrap_error:#}");
                Ok(())
            }
            Err(load_error) => Err(anyhow!(
                "launchctl bootstrap 失败（{bootstrap_error:#}），legacy load 回退也失败（{load_error:#}）"
            )),
        },
    };
    // 无论激活成败都把磁盘恢复为正式形态（含 RunAtLoad），保持持久事实一致
    write_plist(dir, label, exe, base, true)?;
    activated
}

/// 写入正式 plist 并复核登记参数；launchd 加载态缺失只告警（见 verify_enable）。
fn verify_enable(dir: &Path, label: &str, exe: &Path, base: &Path) -> Result<()> {
    let status = inspect_in(dir, label, exe, base)?;
    if status.enabled {
        return Ok(());
    }
    if status.state == "not_loaded" {
        // bootstrap 退出码已证明 launchd 接受登记，print 未命中多见于受限会话；
        // 磁盘 plist 是下次登录的持久事实源，不判失败
        tracing::warn!("launchd 已接受登记但 print 未命中（受限会话？），下次登录自启动仍生效");
        return Ok(());
    }
    bail!("自启动写入后复核失败（{}），请重新启用修复", status.state)
}

/// 登记或卸载 LaunchAgent（`label` 独立成参供测试使用探针标签，生产传 [`LAUNCH_AGENT_LABEL`]）。
///
/// enable：先写正式 plist（持久事实源，launchctl 失败不回滚——下次登录仍生效），
/// 再观测加载态并按需卸载旧任务，最后以「无 RunAtLoad」形态激活后恢复正式 plist。
/// disable：先删磁盘 plist（用户意图的持久载体），再尽力卸载本会话加载态。
fn apply(dir: &Path, label: &str, enabled: bool, exe: &Path, base: &Path) -> Result<()> {
    // SAFETY：getuid 是无副作用的系统调用封装，仅读取当前用户 id
    let uid = unsafe { libc::getuid() };
    let target = service_target(uid, label);
    if enabled {
        write_plist(dir, label, exe, base, true)?;
        let state = match launchctl_state(&target) {
            Ok(state) => state,
            Err(error) => {
                return Err(
                    error.context("查询 launchd 任务状态失败（plist 已写入，下次登录生效）")
                );
            }
        };
        match state {
            LaunchdState::Running(pid) if pid == std::process::id() => {
                // 加载中的任务就是本进程（登录自启动场景）：bootout 会 SIGTERM 自杀，
                // 跳过 launchd 操作；磁盘 plist 已更新，下次登录按新参数加载
            }
            LaunchdState::Running(_) | LaunchdState::Idle => {
                // 已有旧加载（重复启用或参数变更后的刷新）：先卸载再重新登记
                launchctl_bootout(&target)?;
                activate_and_restore(dir, label, exe, base, uid)?;
            }
            LaunchdState::NotFound => {
                activate_and_restore(dir, label, exe, base, uid)?;
            }
        }
        return verify_enable(dir, label, exe, base);
    }

    // 先删磁盘 plist：即使后续 launchctl 步骤中断，「下次登录不再自启」也已达成；
    // bootout 只影响当前会话的加载态，不是持久事实源
    let plist_path = plist_path_in(dir, label);
    match std::fs::remove_file(&plist_path) {
        Ok(()) => {}
        // 未登记过视为幂等成功（与 Windows 删除缺失注册值的口径一致）
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match launchctl_state(&target) {
        Ok(LaunchdState::Running(pid)) if pid == std::process::id() => {
            // 本进程即加载任务：bootout 会 SIGTERM 自杀，跳过；RunAtLoad 任务退出后
            // launchd 不会重跑，磁盘已删保证下次登录不再加载
        }
        Ok(LaunchdState::Running(_) | LaunchdState::Idle) => {
            // 结束非本进程的残留任务；失败不阻断（磁盘已删，下次登录不会加载）
            if let Err(error) = launchctl_bootout(&target) {
                tracing::warn!(%error, "卸载 launchd 任务失败（plist 已删除，下次登录不会自启动）");
            }
        }
        Ok(LaunchdState::NotFound) => {}
        Err(error) => {
            tracing::warn!(%error, "查询 launchd 任务状态失败（plist 已删除，下次登录不会自启动）");
        }
    }
    // 复核：磁盘 plist 必须已消失（与 Windows 复核 registered 的口径对齐）
    if plist_path.exists() {
        bail!("launchd 自启动 plist 删除失败: {}", plist_path.display());
    }
    Ok(())
}

/// 读取 LaunchAgent 登记状态（磁盘 plist + launchd 加载态）。
fn inspect_in(dir: &Path, label: &str, exe: &Path, base: &Path) -> Result<MacosAutostartStatus> {
    let plist_path = plist_path_in(dir, label);
    let registered = plist_path.is_file();
    let content = std::fs::read_to_string(&plist_path).ok();
    let program_arguments = content.as_deref().and_then(parse_program_arguments);
    let run_at_load = content.as_deref().and_then(parse_run_at_load);
    // SAFETY：getuid 是无副作用的系统调用封装，仅读取当前用户 id
    let uid = unsafe { libc::getuid() };
    let (loaded, job_pid) = match launchctl_state(&service_target(uid, label))? {
        LaunchdState::Running(pid) => (true, Some(pid)),
        LaunchdState::Idle => (true, None),
        LaunchdState::NotFound => (false, None),
    };
    let (state, path_match, target_exists, base_path_match) = classify(
        registered,
        program_arguments.as_deref(),
        run_at_load,
        loaded,
        exe,
        base,
    );
    Ok(MacosAutostartStatus {
        registered,
        path_match,
        base_path_match,
        target_exists,
        run_at_load,
        loaded,
        job_pid,
        expected_path: exe.display().to_string(),
        expected_base_path: base.display().to_string(),
        plist_path: registered.then(|| plist_path.display().to_string()),
        program_arguments,
        enabled: state == "launchd_enabled",
        state,
    })
}

/// 查询本进程的 launchd 自启动状态，不修改登记或配置。
pub fn self_start_status(base: &Path) -> Result<MacosAutostartStatus> {
    let exe = std::env::current_exe()?;
    inspect_in(&launch_agents_dir()?, LAUNCH_AGENT_LABEL, &exe, base)
}

/// 显式设置自启动并登记本实例的数据目录（与 Windows 的 `set_self_start_for_base` 同口径）。
///
/// 注意：plist 的 ProgramArguments 指向可执行文件直启。`.app` 打包形态下
/// （current_exe 位于 `.app/Contents/MacOS`）数据目录会落在 bundle 内，
/// 托盘与浏览器自动化等能力不做承诺——便携目录 / 明文 exe 场景为主。
pub fn set_self_start_for_base(enabled: bool, base: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    apply(
        &launch_agents_dir()?,
        LAUNCH_AGENT_LABEL,
        enabled,
        &exe,
        base,
    )
}

/// 注册或取消当前可执行文件默认目录下的自启动（exe 所在目录即数据目录）。
pub fn set_self_start(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let base = exe
        .parent()
        .ok_or_else(|| anyhow!("无法确定可执行文件目录"))?;
    set_self_start_for_base(enabled, base)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机 launchd 试验的兜底清理：测试 panic 后也不残留登记。
    struct ProbeAgentGuard {
        dir: PathBuf,
        label: String,
        uid: u32,
    }
    impl Drop for ProbeAgentGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(plist_path_in(&self.dir, &self.label));
            let _ = std::process::Command::new("launchctl")
                .arg("bootout")
                .arg(service_target(self.uid, &self.label))
                .output();
        }
    }

    /// 构造带空格与 XML 特殊字符的 exe / 数据目录，验证 plist 转义与参数回读。
    ///
    /// ProgramArguments 每个元素是独立字符串节点，空格路径无需 shell 式转义；
    /// `& < >` 必须做 XML 转义，否则 launchd 解析 plist 直接失败。
    #[test]
    fn plist_escapes_and_roundtrips_program_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("Campus Auth & <App>");
        std::fs::create_dir_all(&app_dir).unwrap();
        let exe = app_dir.join("campus-auth");
        std::fs::write(&exe, b"fake").unwrap();
        let base = dir.path().join("Campus 数据目录");
        std::fs::create_dir_all(&base).unwrap();

        let content = build_plist(LAUNCH_AGENT_LABEL, &exe, &base, true);
        assert!(content.contains("Campus Auth &amp; &lt;App&gt;"));
        assert!(!content.contains("Campus Auth & <App>"));
        let args = parse_program_arguments(&content).expect("参数可解析");
        assert_eq!(args.len(), 4);
        assert_eq!(args[0], exe.to_string_lossy());
        assert_eq!(args[1], "--startup-launch");
        assert_eq!(args[2], "--base-path");
        assert_eq!(args[3], base.to_string_lossy());
        assert_eq!(parse_run_at_load(&content), Some(true));

        // 无 RunAtLoad 的临时登记形态：键缺失时解析为 None
        let without = build_plist(LAUNCH_AGENT_LABEL, &exe, &base, false);
        assert_eq!(parse_run_at_load(&without), None);
        assert_eq!(
            parse_program_arguments(&without).unwrap(),
            parse_program_arguments(&content).unwrap()
        );
    }

    /// 状态机：对齐 Windows 词汇（not_registered/path_mismatch/command_mismatch），
    /// macOS 专属的 not_loaded/launchd_enabled 覆盖加载态与 RunAtLoad 缺失。
    ///
    /// exe 与数据目录使用真实临时文件：target_exists 依赖文件系统存在性，
    /// 虚构路径会恒判 path_mismatch。
    #[test]
    fn classify_covers_registered_states() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("App");
        std::fs::create_dir_all(&app_dir).unwrap();
        let exe = app_dir.join("campus-auth");
        std::fs::write(&exe, b"fake").unwrap();
        let base = app_dir.as_path();
        let other_dir = dir.path().join("Elsewhere");
        std::fs::create_dir_all(&other_dir).unwrap();
        let exe_str = exe.to_string_lossy().to_string();
        let base_str = base.to_string_lossy().to_string();
        let other_str = other_dir.to_string_lossy().to_string();
        let own_args = |exe: &str, base: &str| {
            vec![
                exe.to_owned(),
                "--startup-launch".to_owned(),
                "--base-path".to_owned(),
                base.to_owned(),
            ]
        };
        // 未登记：plist 不存在
        assert_eq!(
            classify(false, None, None, false, &exe, base).0,
            "not_registered"
        );

        // 完整参数 + RunAtLoad + 已加载
        let args = own_args(&exe_str, &base_str);
        let (state, path_match, target_exists, base_match) =
            classify(true, Some(&args), Some(true), true, &exe, base);
        assert_eq!(state, "launchd_enabled");
        assert!(path_match && target_exists && base_match);

        // 磁盘登记有效但 launchd 未加载
        assert_eq!(
            classify(true, Some(&args), Some(true), false, &exe, base).0,
            "not_loaded"
        );

        // 旧版无参数 plist（仅裸 exe）：exe 所在目录与数据目录一致视为有效（Windows 同口径）
        let legacy = vec![exe_str.clone()];
        assert_eq!(
            classify(true, Some(&legacy), Some(true), true, &exe, base).0,
            "launchd_enabled"
        );

        // 指向别的可执行文件 / 文件不存在
        assert_eq!(
            classify(
                true,
                Some(&own_args(&other_str, &base_str)),
                Some(true),
                true,
                &exe,
                base
            )
            .0,
            "path_mismatch"
        );

        // 数据目录不符 / 未知参数 / 重复参数 / RunAtLoad 缺失或 false
        assert_eq!(
            classify(
                true,
                Some(&own_args(&exe_str, &other_str)),
                Some(true),
                true,
                &exe,
                base
            )
            .0,
            "command_mismatch"
        );
        let unknown = vec![exe_str.clone(), "--autostart".to_owned()];
        assert_eq!(
            classify(true, Some(&unknown), Some(true), true, &exe, base).0,
            "command_mismatch"
        );
        let duplicated = vec![
            exe_str.clone(),
            "--startup-launch".to_owned(),
            "--startup-launch".to_owned(),
            "--base-path".to_owned(),
            base_str.clone(),
        ];
        assert_eq!(
            classify(true, Some(&duplicated), Some(true), true, &exe, base).0,
            "command_mismatch"
        );
        assert_eq!(
            classify(true, Some(&args), Some(false), true, &exe, base).0,
            "command_mismatch"
        );
        assert_eq!(
            classify(true, Some(&args), None, true, &exe, base).0,
            "command_mismatch"
        );
    }

    /// 真机闭环：enable（launchctl bootstrap 真调用）→ 状态观测 → 幂等重复 enable
    /// → disable → plist 消失且 launchd 卸载 → 重复 disable 幂等。
    ///
    /// 使用探针标签 + 临时目录，不触碰真实 `com.misyra.Campus-Auth` 登记；
    /// launchctl 真调用需要用户会话的 gui 域（本机 mac 桌面会话满足）。
    /// 无 gui 域 / 受限会话中 launchctl 失败时降级：仅断言磁盘 plist 事实并跳过
    /// 加载态断言（enable 的错误路径约定为「plist 已写入正式形态，不回滚」）。
    #[test]
    fn launchd_enable_status_disable_roundtrip() {
        // SAFETY：getuid 是无副作用的系统调用封装，仅读取当前用户 id
        let uid = unsafe { libc::getuid() };
        let dir = tempfile::tempdir().unwrap();
        let label = format!("com.misyra.campus-auth-it-{}", std::process::id());
        let _guard = ProbeAgentGuard {
            dir: dir.path().to_path_buf(),
            label: label.clone(),
            uid,
        };
        // 带空格的 exe 与数据目录：launchd 按数组取参，不做 shell 分词
        let exe = dir.path().join("Campus Auth App/campus-auth");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"fake").unwrap();
        let base = dir.path().join("Campus Data Dir");
        std::fs::create_dir_all(&base).unwrap();

        let degrade = match apply(dir.path(), &label, true, &exe, &base) {
            Ok(()) => false,
            Err(error) => {
                eprintln!("launchctl 失败，降级为 plist 文件断言: {error:#}");
                true
            }
        };
        let plist = plist_path_in(dir.path(), &label);
        assert!(plist.is_file(), "enable 失败路径也必须留下正式 plist");
        let content = std::fs::read_to_string(&plist).unwrap();
        let args = parse_program_arguments(&content).expect("ProgramArguments 可解析");
        assert_eq!(args[0], exe.to_string_lossy());
        assert_eq!(args[1], "--startup-launch");
        assert_eq!(args[2], "--base-path");
        assert_eq!(args[3], base.to_string_lossy());
        assert_eq!(parse_run_at_load(&content), Some(true));

        let status = inspect_in(dir.path(), &label, &exe, &base).unwrap();
        assert!(status.registered);
        assert!(status.path_match);
        assert!(status.base_path_match);
        assert!(status.target_exists);
        if degrade {
            eprintln!(
                "（降级分支）加载态断言跳过：state={}, enabled={}",
                status.state, status.enabled
            );
            return; // _guard drop 兜底清理 plist 与 bootout
        }
        assert!(status.loaded);
        assert_eq!(status.state, "launchd_enabled");
        assert!(status.enabled);

        // 幂等：重复 enable 走 bootout → bootstrap 刷新路径，结果不变
        apply(dir.path(), &label, true, &exe, &base).unwrap();
        let status = inspect_in(dir.path(), &label, &exe, &base).unwrap();
        assert_eq!(status.state, "launchd_enabled");
        assert!(status.enabled);

        // disable：plist 消失且 launchd 卸载；重复 disable 幂等
        apply(dir.path(), &label, false, &exe, &base).unwrap();
        assert!(!plist.exists());
        assert_eq!(
            launchctl_state(&service_target(uid, &label)).unwrap(),
            LaunchdState::NotFound
        );
        apply(dir.path(), &label, false, &exe, &base).unwrap();
        assert!(!plist.exists());
    }
}
