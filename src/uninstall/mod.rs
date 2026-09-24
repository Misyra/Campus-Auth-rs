//! 卸载计划：删什么、拒什么、怎么删（**单一事实源**）
//!
//! 为什么单独成模块：卸载弹窗要**明确列出**将被删除的内容（用户拍板的口径），
//! 而真正执行删除的是另一个进程——`campus-auth-helper --uninstall`。Windows 不允许
//! 删除运行中的 exe，主程序必须先退出，删除只能交给助手。两处若各写一份清单，必然
//! 出现"界面说删 A、助手把 A+B 一起删了"。故清单与守卫都在此，两个 binary 共用。
//!
//! ## 为什么整目录删，而不是枚举包内文件
//!
//! 发布包（`.github/workflows/release.yml`）除 exe 外还带 `resources/`、`docs/`、
//! `python_worker/`，**以及 `src/` 与 `frontend/` 两份源码**。枚举清单只要漏一项，
//! 用户就会拿到一个"卸载完了但目录还在、里面躺着不认识的文件"的结果——而漏项几乎
//! 是必然的（包结构一改就得同步改清单，没有任何机制强制）。故：
//!
//! - **全删**（默认）：`install_dir` 整体 `remove_dir_all`；
//! - **保留用户数据**：逐项删 `install_dir` 的子项，只跳过数据目录名（见
//!   [`DATA_DIR_NAMES`]）——未知文件一样删掉，口径仍是"整个目录消失，只保数据"。
//!
//! ## 守卫（误删比残留严重得多）
//!
//! 整目录删的代价是爆炸半径大，故 [`validate_install_dir`] 必须硬：
//! 文件系统根目录、用户主目录、系统临时目录、**源码仓库**、**cargo 构建输出**五类一律
//! 拒绝。后两条都是真实踩点：仓库根放着一个早期 `campus-auth.exe`，它的 base_path 就是
//! 仓库根——天真实现会把 `E:\Campus-Auth-rs\src\` 与 `frontend\` 一起删掉；而开发实例住在
//! `target/debug/`（没有 `.git`/`Cargo.toml`，守卫第一版会放行），删掉的是整个构建产物
//! 目录外加那份实例自己的数据目录。守卫在**两处**执行：Web 路由（用户还在界面上，能当场
//! 看到拒绝原因）与助手（纵深防御，防止 CLI 参数被绕过）。

use std::path::{Path, PathBuf};

/// 用户数据目录名（`base_path` 下）：勾选「保留配置与任务」时这些整棵保留
///
/// 名字**取自** `utils::paths` 的目录常量而不是另写一份字面量：这五个名字是"保留数据"
/// 与"删除其余一切"之间的分界线，两边各写一份时改一处漏一处，后果是用户勾了保留却丢掉
/// 那一项（`remove_children_except` 把不认识的子项照删）。`test_data_dir_names_match_paths`
/// 把这条关系钉住。
pub const DATA_DIR_NAMES: [&str; 5] = [
    crate::utils::paths::CONFIG_DIR,
    crate::utils::paths::TASKS_DIR,
    crate::utils::paths::LOGS_DIR,
    crate::utils::paths::ENV_DIR,
    crate::utils::paths::UPDATE_DIR,
];

/// 用户数据目录的中文名（界面展示用，与 [`DATA_DIR_NAMES`] 同序）
pub const DATA_DIR_LABELS: [(&str, &str); 5] = [
    ("config", "配置与方案"),
    ("tasks", "任务与脚本"),
    ("logs", "日志"),
    ("environment", "Python 环境"),
    ("update", "更新缓存"),
];

/// 主程序可执行文件名（当前平台）
pub fn main_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "campus-auth.exe"
    } else {
        "campus-auth"
    }
}

/// 助手可执行文件名（当前平台）
pub fn helper_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "campus-auth-helper.exe"
    } else {
        "campus-auth-helper"
    }
}

/// 该目录是否为源码仓库 / 构建树（而非解压即用的安装目录）
///
/// 判据取 `.git` 与 `Cargo.toml`：发布包两者都不含（`build.ps1` / release.yml 均不打包），
/// 而仓库根与 `cargo` 工程根两者必居其一。命中即拒绝——把用户的项目源码当成安装目录
/// 删掉是不可逆的事故。
pub fn is_source_checkout(dir: &Path) -> bool {
    dir.join(".git").exists() || dir.join("Cargo.toml").is_file()
}

/// 该目录是否位于 cargo 构建输出（`target/`）之下
///
/// 与"源码仓库"分开判，因为这一类比它更隐蔽：`target/debug/` 里既没有 `.git` 也没有
/// `Cargo.toml`（因此 `is_source_checkout` 放行），却有主程序与助手——于是守卫放行，
/// 而整目录删除会连**开发实例自己的** `config/`、`tasks/`、`logs/` 与全部构建产物一起删掉。
///
/// 判据不只看组件名：要求路径里出现过 `target` 组件**且**祖先里能找到 `Cargo.toml`。
/// 只按组件名判会误伤用户自建的 `D:\target\` 这种普通安装目录，那种情形必须照常放行。
pub fn is_cargo_target_dir(dir: &Path) -> bool {
    let in_target = dir
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"));
    if !in_target {
        return false;
    }
    dir.ancestors()
        .skip(1)
        .any(|ancestor| ancestor.join("Cargo.toml").is_file())
}

/// 校验目录可以作为**被卸载的安装目录**（不通过即拒绝执行）
///
/// 顺序有意从"最容易误伤"排到"最具体的特征"：根目录 / 主目录 / 临时目录都是
/// 一旦删错无法挽回的位置，先拦；源码仓库与 cargo 构建输出次之；最后才要求它确实像
/// 安装目录（主程序或助手与本模块同目录存在）。
pub fn validate_install_dir(install_dir: &Path) -> Result<(), String> {
    if install_dir.parent().is_none() {
        return Err(format!(
            "拒绝执行：{} 是文件系统根目录",
            install_dir.display()
        ));
    }
    if let Some(home) = dirs::home_dir() {
        if same_dir(install_dir, &home) {
            return Err("拒绝执行：该目录是用户主目录".to_string());
        }
    }
    if same_dir(install_dir, &std::env::temp_dir()) {
        return Err("拒绝执行：该目录是系统临时目录".to_string());
    }
    if is_source_checkout(install_dir) {
        return Err(format!(
            "拒绝执行：{} 看起来是源码仓库（含 .git 或 Cargo.toml），不是解压即用的安装目录。\
             若是开发环境，请手动清理 target/ 或改用安装包目录。",
            install_dir.display()
        ));
    }
    if is_cargo_target_dir(install_dir) {
        return Err(format!(
            "拒绝执行：{} 位于 cargo 构建输出（target/）之下，不是安装目录。\
             开发实例的数据目录就在旁边，删掉会连构建产物一起没了；\
             请把发布包解压到独立目录后再卸载。",
            install_dir.display()
        ));
    }
    if !install_dir.join(main_exe_name()).is_file()
        && !install_dir.join(helper_exe_name()).is_file()
    {
        return Err(format!(
            "拒绝执行：{} 下既没有 {} 也没有 {}，不像是安装目录",
            install_dir.display(),
            main_exe_name(),
            helper_exe_name()
        ));
    }
    Ok(())
}

/// 校验 `base_path` 可作为用户数据的删除范围
///
/// 比 [`validate_install_dir`] 宽松一档：`base_path` 允许是任意普通目录（`--base-path`
/// 可以把数据放到别处），但仍拒绝根目录与主目录——数据目录是它的**子目录**，
/// 父目录一旦是根/主目录，删错的后果与整目录删同级。
pub fn validate_base_path(base_path: &Path) -> Result<(), String> {
    if base_path.parent().is_none() {
        return Err(format!(
            "拒绝执行：{} 是文件系统根目录",
            base_path.display()
        ));
    }
    if let Some(home) = dirs::home_dir() {
        if same_dir(base_path, &home) {
            return Err("拒绝执行：数据目录是用户主目录".to_string());
        }
    }
    Ok(())
}

/// 两条路径是否指向同一个已存在的位置（canonicalize 后比较）
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 待删除的用户数据目录（相对 `base_path`，只列**实际存在**的）
pub fn existing_data_dirs(base_path: &Path) -> Vec<(String, PathBuf)> {
    DATA_DIR_NAMES
        .iter()
        .map(|name| ((*name).to_string(), base_path.join(name)))
        .filter(|(_, p)| p.exists())
        .collect()
}

/// 卸载计划（Web 路由据此渲染清单，助手据此执行）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallPlan {
    /// 程序文件所在目录（exe 与助手所在处），整体删除
    pub install_dir: PathBuf,
    /// 数据根目录（数据目录的父目录）
    pub base_path: PathBuf,
    /// 是否保留用户数据（`config/` `tasks/` `logs/` `environment/` `update/`）
    pub keep_user_data: bool,
    /// `install_dir` 与 `base_path` 是否同一目录（便携版常态）
    pub same_dir: bool,
    /// 将要删除的用户数据目录（仅 `keep_user_data == false` 时有意义）
    pub data_dirs: Vec<(String, PathBuf)>,
}

/// 构造卸载计划（含守卫校验）
///
/// 返回 `Err` 即拒绝卸载，错误文案直接可展示给用户（含具体路径与原因）。
pub fn build_plan(
    install_dir: &Path,
    base_path: &Path,
    keep_user_data: bool,
) -> Result<UninstallPlan, String> {
    validate_install_dir(install_dir)?;
    validate_base_path(base_path)?;
    let same_dir = same_dir(install_dir, base_path);
    let data_dirs = if keep_user_data {
        Vec::new()
    } else {
        existing_data_dirs(base_path)
    };
    Ok(UninstallPlan {
        install_dir: install_dir.to_path_buf(),
        base_path: base_path.to_path_buf(),
        keep_user_data,
        same_dir,
        data_dirs,
    })
}

/// 单步删除结果
#[derive(Debug, Clone)]
pub struct UninstallStep {
    /// 展示名（「程序目录」「配置与方案」…）
    pub label: String,
    pub path: PathBuf,
    pub success: bool,
    /// 成功时为结果说明（如「已删除」「已保留」），失败时为原因
    pub message: String,
}

/// 卸载执行报告（助手的系统提示框据此渲染）
#[derive(Debug, Clone, Default)]
pub struct UninstallReport {
    pub steps: Vec<UninstallStep>,
    /// 因「保留用户数据」而跳过的目录名（提示框要明确说没删什么）
    pub kept: Vec<String>,
}

impl UninstallReport {
    /// 是否全部成功
    pub fn all_ok(&self) -> bool {
        self.steps.iter().all(|s| s.success)
    }

    /// 失败项
    pub fn failures(&self) -> impl Iterator<Item = &UninstallStep> {
        self.steps.iter().filter(|s| !s.success)
    }
}

/// 按计划执行删除
///
/// 全部 best-effort、单步失败不中断：部分删掉总比一条失败就整个中止好（用户已确认
/// 卸载，半途而废只会留下更难收拾的现场）。结果逐项回报，由调用方展示。
///
/// 调用前提：主进程已退出、且**除本进程外没有其它进程占用 `install_dir`**
/// （Windows 由二段式助手保证：执行删除的那一份跑在系统临时目录里）。
pub fn execute(plan: &UninstallPlan) -> UninstallReport {
    let mut report = UninstallReport::default();

    // 1. 与安装目录**不同处**的数据目录：先行删除（安装目录还没被删，失败信息更具体）
    if !plan.keep_user_data {
        for (name, path) in &plan.data_dirs {
            if plan.same_dir {
                // 同目录：随安装目录一起删，此处不重复删（重复删的失败信息会误导）
                continue;
            }
            report.push_removal(dir_label(name), path, remove_dir_all(path));
        }
    } else {
        report.kept = DATA_DIR_NAMES
            .iter()
            .filter(|name| plan.base_path.join(name).exists())
            .map(|name| dir_label(name).to_string())
            .collect();
    }

    // 2. 程序目录
    if plan.keep_user_data {
        // 保留数据：逐项删安装目录的子项，只跳过数据目录名
        let (removed, failed, kept) = remove_children_except(&plan.install_dir, &DATA_DIR_NAMES);
        for name in removed {
            let path = plan.install_dir.join(&name);
            report.push_removal(name, &path, Ok(()));
        }
        for (name, err) in failed {
            let path = plan.install_dir.join(&name);
            report.push_removal(name, &path, Err(err));
        }
        for name in kept {
            let label = dir_label(&name);
            if !report.kept.contains(&label) {
                report.kept.push(label);
            }
        }
        // 删空后目录本身若已空，一并删掉：用户选的是"保留数据"，不是"保留一个空文件夹"。
        // 用**非递归** remove_dir——只有真的空了才会成功；数据留在里面时它必然失败，
        // 那正是要保留的情形，不记作失败项。
        if std::fs::read_dir(&plan.install_dir)
            .map(|it| it.count())
            .unwrap_or(0)
            == 0
        {
            let result =
                std::fs::remove_dir(&plan.install_dir).map_err(|e| format!("删除失败: {e}"));
            report.push_removal("程序目录（已清空）".to_string(), &plan.install_dir, result);
        }
    } else {
        report.push_removal(
            "程序目录".to_string(),
            &plan.install_dir,
            remove_dir_all(&plan.install_dir),
        );
    }

    report
}

impl UninstallReport {
    fn push_removal(&mut self, label: String, path: &Path, result: Result<(), String>) {
        let (success, message) = match result {
            Ok(()) => (true, "已删除".to_string()),
            Err(e) => (false, e),
        };
        self.steps.push(UninstallStep {
            label,
            path: path.to_path_buf(),
            success,
            message,
        });
    }
}

/// 目录名 → 展示名（数据目录用中文名，其余用目录名本身）
fn dir_label(name: &str) -> String {
    data_dir_label(name).to_string()
}

/// 数据目录名 → 中文展示名（界面、助手提示框、日志共用一套称呼）
pub fn data_dir_label(name: &str) -> &str {
    DATA_DIR_LABELS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, label)| *label)
        .unwrap_or(name)
}

/// 主程序侧：spawn 卸载助手（**不在此处退出主进程**）
///
/// 助手会等待 `--pid` 指定的主进程退出后才动手，故调用方 spawn 成功后应立刻触发
/// 优雅关闭，否则卸载会一直卡在"等主进程退出"上直到超时。
///
/// 不传安装目录：助手第一段按自身所在位置推导（它就在安装目录里），并自行做守卫
/// 校验——删除目标不接受来自外部的任意路径。
pub fn spawn_helper(
    install_dir: &Path,
    base_path: &Path,
    keep_user_data: bool,
) -> Result<(), String> {
    let helper_path = install_dir.join(helper_exe_name());
    if !helper_path.is_file() {
        return Err(format!("卸载助手缺失：{}", helper_path.display()));
    }
    let mut cmd = std::process::Command::new(&helper_path);
    cmd.args(helper_args(std::process::id(), base_path, keep_user_data));
    // helper 内有多行 println，Windows 上隐藏控制台窗口避免闪黑窗
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map_err(|e| format!("启动卸载助手失败: {e}"))?;
    Ok(())
}

/// 组装卸载助手的命令行参数（抽出来便于单测，不必真的 spawn 一个进程）
pub fn helper_args(pid: u32, base_path: &Path, keep_user_data: bool) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "--uninstall".into(),
        "--pid".into(),
        pid.to_string().into(),
        "--base-path".into(),
        base_path.into(),
    ];
    if keep_user_data {
        args.push("--keep-user-data".into());
    }
    args
}

/// 递归删除目录；不存在视为成功（幂等）
///
/// 带重试：首次失败最常见的原因是"文件仍被占用"的竞态——待删的 exe 可能刚被另一个
/// 进程（并发唤醒的更新助手、杀软扫描）打开。用户已经点过卸载，重试几次比立刻报错
/// 更符合预期；重试仍失败则如实回报。
fn remove_dir_all(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let mut last = String::new();
    for attempt in 0..REMOVE_ATTEMPTS {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        if attempt + 1 < REMOVE_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(REMOVE_RETRY_DELAY_MS));
        }
    }
    Err(format!("删除失败: {last}"))
}

/// 目录删除的重试次数与间隔
const REMOVE_ATTEMPTS: u32 = 5;
const REMOVE_RETRY_DELAY_MS: u64 = 400;

/// 渲染卸载结果文案（助手系统提示框 / 非 Windows 的 stderr 共用）
///
/// 用户要的是"明确知道删了什么、没删什么"——故成功项按**计划口径**概括（逐个子项
/// 铺开会淹没重点），失败项逐条列出（每一项都意味着现场还留着东西），保留项单独成段
/// （否则"卸载完成"与目录还在的事实自相矛盾）。
pub fn report_text(report: &UninstallReport, plan: &UninstallPlan) -> String {
    let mut lines: Vec<String> = Vec::new();

    if plan.keep_user_data {
        let removed = report
            .steps
            .iter()
            .filter(|s| s.success && s.path != plan.install_dir)
            .count();
        lines.push(format!("已删除：程序目录下的程序文件（{removed} 项）"));
        lines.push(format!("　　{}", plan.install_dir.display()));
        // 数据不在安装目录里时，安装目录删空后会被一并删掉——文案要跟上，
        // 否则用户按提示去找那个已经不存在的目录
        if !report
            .steps
            .iter()
            .any(|s| s.success && s.path == plan.install_dir)
        {
            lines.push("　　（目录本身已保留）".to_string());
        }
        if !report.kept.is_empty() {
            lines.push(String::new());
            lines.push("已保留（你勾选了「保留配置与任务」）：".to_string());
            lines.push(format!("　　{}", report.kept.join("、")));
            lines.push("　　下次运行仍可使用这些数据。".to_string());
        }
    } else {
        lines.push("已删除：".to_string());
        lines.push(format!("　　程序目录　{}", plan.install_dir.display()));
        for step in &report.steps {
            // 按路径字面比较（不能用 same_dir：安装目录此刻已被删掉，canonicalize
            // 必然失败，会把程序目录自己也当成数据目录再列一遍）
            if step.success && step.path != plan.install_dir {
                lines.push(format!("　　{}　{}", step.label, step.path.display()));
            }
        }
    }

    let failures: Vec<&UninstallStep> = report.failures().collect();
    if !failures.is_empty() {
        lines.push(String::new());
        lines.push(format!("未能删除（{} 项）：", failures.len()));
        for step in failures {
            lines.push(format!(
                "　　{}　{}：{}",
                step.label,
                step.path.display(),
                step.message
            ));
        }
        lines.push("　　可关闭可能占用这些文件的程序后手动删除。".to_string());
    }

    lines.join("\n")
}

/// 删除目录下除 `except` 之外的全部子项
///
/// 返回 (已删除项名, 失败项及其原因, 实际跳过的项名)。跳过项回报给调用方，让用户知道
/// **没删什么**——"保留配置与任务"这个选项的价值全在于此，静默跳过等于撒谎。
fn remove_children_except(
    dir: &Path,
    except: &[&str],
) -> (Vec<String>, Vec<(String, String)>, Vec<String>) {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    let mut kept = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            failed.push(("读取目录".to_string(), format!("删除失败: {e}")));
            return (removed, failed, kept);
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if except.contains(&name_str) {
            kept.push(name_str.to_string());
            continue;
        }
        let path = entry.path();
        // 符号链接/文件用 remove_file，目录用 remove_dir_all：对符号链接用
        // remove_dir_all 在部分平台会跟随链接删掉目标内容
        let result = if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed.push(name_str.to_string()),
            Err(e) => failed.push((name_str.to_string(), format!("删除失败: {e}"))),
        }
    }
    (removed, failed, kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个"像安装目录"的临时目录：含主程序与助手
    fn make_install_dir(base: &Path) -> PathBuf {
        let dir = base.join("install");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(main_exe_name()), b"exe").unwrap();
        std::fs::write(dir.join(helper_exe_name()), b"helper").unwrap();
        dir
    }

    /// 五个数据目录名必须与 `utils::paths` 的常量逐字对应。
    ///
    /// 失败模式是不可逆的：`DATA_DIR_NAMES` 少一项（或 `paths` 里改了名）时，那个目录会被
    /// 当成"不认识的子项"照删——用户勾了「保留配置与任务」，丢的却是其中一项。
    #[test]
    fn test_data_dir_names_match_paths() {
        use crate::utils::paths;
        assert_eq!(
            DATA_DIR_NAMES,
            [
                paths::CONFIG_DIR,
                paths::TASKS_DIR,
                paths::LOGS_DIR,
                paths::ENV_DIR,
                paths::UPDATE_DIR
            ]
        );
        // 展示名与目录名按序一一对应（错位会把「任务与脚本」的标签贴到别的目录上）
        assert_eq!(DATA_DIR_LABELS.len(), DATA_DIR_NAMES.len());
        for (i, name) in DATA_DIR_NAMES.iter().enumerate() {
            assert_eq!(DATA_DIR_LABELS[i].0, *name, "第 {i} 项标签名与目录名不一致");
            assert_eq!(data_dir_label(name), DATA_DIR_LABELS[i].1);
        }
    }

    /// 源码仓库识别：.git 与 Cargo.toml 各自足矣（发布包两者都没有）
    #[test]
    fn test_is_source_checkout() {
        let tmp = tempfile::tempdir().unwrap();

        let git_dir = tmp.path().join("repo-git");
        std::fs::create_dir_all(git_dir.join(".git")).unwrap();
        assert!(is_source_checkout(&git_dir));

        let cargo_dir = tmp.path().join("repo-cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(cargo_dir.join("Cargo.toml"), b"[package]").unwrap();
        assert!(is_source_checkout(&cargo_dir));

        // 解压即用的安装目录：既无 .git 也无 Cargo.toml
        let install = make_install_dir(tmp.path());
        assert!(!is_source_checkout(&install));
    }

    /// 守卫必须拦住真实的误删风险：仓库根放着主程序时不得当成安装目录
    #[test]
    fn test_validate_install_dir_rejects_source_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), b"[package]").unwrap();
        std::fs::write(repo.join(main_exe_name()), b"exe").unwrap();
        std::fs::write(repo.join(helper_exe_name()), b"helper").unwrap();

        let err = validate_install_dir(&repo).unwrap_err();
        assert!(err.contains("源码仓库"), "应说明拒绝原因: {err}");

        // 移除源码特征后放行——证明拒绝的是源码特征而不是路径本身
        std::fs::remove_dir_all(repo.join(".git")).unwrap();
        std::fs::remove_file(repo.join("Cargo.toml")).unwrap();
        assert!(validate_install_dir(&repo).is_ok());
    }

    /// 守卫：文件系统根目录一律拒绝
    #[test]
    fn test_validate_install_dir_rejects_root() {
        let root = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        let err = validate_install_dir(&root).unwrap_err();
        assert!(err.contains("根目录"), "{err}");
    }

    /// 守卫：不像安装目录的普通目录拒绝（避免把任意目录交给 remove_dir_all）
    #[test]
    fn test_validate_install_dir_rejects_non_install() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("just-a-folder");
        std::fs::create_dir_all(&plain).unwrap();
        let err = validate_install_dir(&plain).unwrap_err();
        assert!(err.contains("不像是安装目录"), "{err}");
    }

    /// cargo 构建输出识别：路径里有 `target` **且** 祖先是 cargo 工程才算
    #[test]
    fn test_is_cargo_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let debug = repo.join("target").join("debug");
        std::fs::create_dir_all(&debug).unwrap();
        assert!(
            !is_cargo_target_dir(&debug),
            "祖先里没有 Cargo.toml 时只是恰好叫 target 的普通目录"
        );

        std::fs::write(repo.join("Cargo.toml"), b"[package]").unwrap();
        assert!(is_cargo_target_dir(&debug));
        assert!(
            !is_cargo_target_dir(&repo),
            "仓库根由「源码仓库」那条判据拦下"
        );

        // 用户自建的 target 目录（不在 cargo 工程下）必须照常放行——只按组件名判会误伤
        let plain = tmp.path().join("target");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!is_cargo_target_dir(&plain));
    }

    /// 守卫：开发实例住的 `target/debug/` 必须拦下（那里没有 .git / Cargo.toml）
    #[test]
    fn test_validate_install_dir_rejects_cargo_target() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let debug = repo.join("target").join("debug");
        std::fs::create_dir_all(&debug).unwrap();
        std::fs::write(repo.join("Cargo.toml"), b"[package]").unwrap();
        std::fs::write(debug.join(main_exe_name()), b"exe").unwrap();
        std::fs::write(debug.join(helper_exe_name()), b"helper").unwrap();

        let err = validate_install_dir(&debug).unwrap_err();
        assert!(err.contains("cargo"), "应说明拒绝原因是构建输出目录: {err}");

        // 把发布包解压到独立目录（同样叫 target、但不在 cargo 工程里）照常可用
        let standalone = tmp.path().join("target");
        std::fs::create_dir_all(&standalone).unwrap();
        std::fs::write(standalone.join(main_exe_name()), b"exe").unwrap();
        assert!(validate_install_dir(&standalone).is_ok());
    }

    /// 计划：同目录 + 全删 → 数据目录归入安装目录一起删，不在 data_dirs 里重复列
    #[test]
    fn test_build_plan_same_dir_all_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        std::fs::create_dir_all(install.join("config")).unwrap();

        let plan = build_plan(&install, &install, false).unwrap();
        assert!(plan.same_dir);
        assert!(!plan.keep_user_data);
        // 同目录时由 remove_dir_all 一次搞定，data_dirs 只用于展示之外的判断
        let report = execute(&plan);
        assert!(!install.exists(), "整体删除后目录应消失");
        assert!(
            report.all_ok(),
            "{:?}",
            report.failures().collect::<Vec<_>>()
        );
    }

    /// 计划：保留用户数据 → 程序文件删掉、数据目录原名保留、unknown 文件照删
    #[test]
    fn test_execute_keep_user_data() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        std::fs::create_dir_all(install.join("config").join("profiles")).unwrap();
        std::fs::write(install.join("config").join("settings.json"), b"{}").unwrap();
        std::fs::create_dir_all(install.join("tasks").join("scripts")).unwrap();
        std::fs::write(
            install.join("tasks").join("scripts").join("a.py"),
            b"print(1)",
        )
        .unwrap();
        std::fs::create_dir_all(install.join("logs")).unwrap();
        std::fs::create_dir_all(install.join("resources").join("icons")).unwrap();
        std::fs::write(install.join("README.md"), b"readme").unwrap();

        let plan = build_plan(&install, &install, true).unwrap();
        let report = execute(&plan);

        assert!(
            report.all_ok(),
            "{:?}",
            report.failures().collect::<Vec<_>>()
        );
        // 程序产物（含未知文件）已删除
        assert!(!install.join(main_exe_name()).exists());
        assert!(!install.join(helper_exe_name()).exists());
        assert!(!install.join("resources").exists());
        assert!(
            !install.join("README.md").exists(),
            "未知文件也应删除（整目录口径）"
        );
        // 数据目录整棵保留
        assert!(install.join("config").join("settings.json").is_file());
        assert!(install.join("tasks").join("scripts").join("a.py").is_file());
        assert!(install.join("logs").is_dir());
        assert!(install.is_dir(), "数据还在目录里，目录本身必须保留");
        // 报告必须说清"没删什么"——静默跳过等于撒谎
        assert!(
            report.kept.iter().any(|k| k.contains("配置与方案")),
            "{:?}",
            report.kept
        );
        assert!(
            report.kept.iter().any(|k| k.contains("任务与脚本")),
            "{:?}",
            report.kept
        );
        assert!(
            report.kept.iter().any(|k| k.contains("日志")),
            "{:?}",
            report.kept
        );
    }

    /// 数据目录与安装目录分离（`--base-path` 指到别处）：两边各自删除
    #[test]
    fn test_execute_split_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("config")).unwrap();
        std::fs::write(data.join("config").join("settings.json"), b"{}").unwrap();
        std::fs::create_dir_all(data.join("logs")).unwrap();

        let plan = build_plan(&install, &data, false).unwrap();
        assert!(!plan.same_dir);
        let report = execute(&plan);

        assert!(
            report.all_ok(),
            "{:?}",
            report.failures().collect::<Vec<_>>()
        );
        assert!(!install.exists(), "程序目录应整体删除");
        assert!(!data.join("config").exists(), "数据目录应删除");
        assert!(!data.join("logs").exists());
        assert!(data.exists(), "数据根目录本身不属于程序，不删");
    }

    /// 保留数据但数据在别处：程序目录整体删除，数据目录原封不动
    #[test]
    fn test_execute_keep_data_with_split_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("config")).unwrap();
        std::fs::write(data.join("config").join("settings.json"), b"{}").unwrap();

        let plan = build_plan(&install, &data, true).unwrap();
        let report = execute(&plan);

        assert!(
            report.all_ok(),
            "{:?}",
            report.failures().collect::<Vec<_>>()
        );
        // 数据在别处：安装目录删空后连目录一起删掉，不留空文件夹
        assert!(!install.exists(), "程序目录既已删空，不应留下空目录");
        assert!(
            data.join("config").join("settings.json").is_file(),
            "异地数据必须保留"
        );
        assert!(report.kept.iter().any(|k| k.contains("配置与方案")));
    }

    /// 幂等：目录已不存在时重复执行不报错
    #[test]
    fn test_execute_idempotent_on_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        let plan = build_plan(&install, &install, false).unwrap();
        assert!(execute(&plan).all_ok());
        assert!(!install.exists());
        // 现场已不存在：重新构造计划会被守卫拦下（像安装目录的特征没了），
        // 这正是想要的——第二次卸载不该再动任何东西
        assert!(build_plan(&install, &install, false).is_err());
    }

    /// 保留数据时 unknown 子项删除失败要如实回报（不吞错）
    #[test]
    fn test_execute_reports_failures() {
        let tmp = tempfile::tempdir().unwrap();
        // 直接构造一个"删除必然失败"的计划：目标是文件系统根目录是拦不下来的，
        // 故改用一个不存在的路径 + 保留数据分支下的读取失败路径
        let missing = tmp.path().join("nope");
        let plan = UninstallPlan {
            install_dir: missing.clone(),
            base_path: missing,
            keep_user_data: true,
            same_dir: true,
            data_dirs: Vec::new(),
        };
        let report = execute(&plan);
        assert!(!report.all_ok(), "读取失败必须回报");
        assert!(
            report
                .failures()
                .next()
                .unwrap()
                .message
                .contains("删除失败")
        );
    }

    /// 报告文案：全删时列出程序目录与各数据目录，且不逐条铺开子项
    #[test]
    fn test_report_text_full_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        std::fs::create_dir_all(install.join("config")).unwrap();
        std::fs::create_dir_all(install.join("logs")).unwrap();

        let plan = build_plan(&install, &install, false).unwrap();
        let report = execute(&plan);
        let text = report_text(&report, &plan);

        assert!(text.contains("已删除"), "{text}");
        assert!(text.contains("程序目录"), "{text}");
        assert!(text.contains(&install.display().to_string()), "{text}");
        assert!(!text.contains("未能删除"), "全成功时不该有失败段: {text}");
    }

    /// 报告文案：保留数据时必须点名"保留了什么"，否则「卸载完成」与目录仍在自相矛盾
    #[test]
    fn test_report_text_keep_user_data_names_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let install = make_install_dir(tmp.path());
        std::fs::create_dir_all(install.join("config")).unwrap();
        std::fs::create_dir_all(install.join("tasks")).unwrap();

        let plan = build_plan(&install, &install, true).unwrap();
        let report = execute(&plan);
        let text = report_text(&report, &plan);

        assert!(text.contains("程序文件"), "{text}");
        assert!(text.contains("已保留"), "{text}");
        assert!(text.contains("配置与方案"), "{text}");
        assert!(text.contains("任务与脚本"), "{text}");
        assert!(text.contains("保留配置与任务"), "{text}");
    }

    /// 报告文案：失败项逐条列出（每一项都意味着现场还留着东西）
    #[test]
    fn test_report_text_lists_failures() {
        let plan = UninstallPlan {
            install_dir: PathBuf::from("/install"),
            base_path: PathBuf::from("/install"),
            keep_user_data: false,
            same_dir: true,
            data_dirs: Vec::new(),
        };
        let report = UninstallReport {
            steps: vec![UninstallStep {
                label: "程序目录".to_string(),
                path: PathBuf::from("/install"),
                success: false,
                message: "删除失败: 拒绝访问".to_string(),
            }],
            kept: Vec::new(),
        };
        let text = report_text(&report, &plan);
        assert!(text.contains("未能删除（1 项）"), "{text}");
        assert!(text.contains("拒绝访问"), "{text}");
    }

    /// 助手参数：`--keep-user-data` 只在勾选时出现（多一个参数就等于多删一块数据）
    #[test]
    fn test_helper_args() {
        let with_keep = helper_args(4242, Path::new("/data"), true);
        let as_str: Vec<String> = with_keep
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(as_str[0], "--uninstall");
        assert!(
            as_str.windows(2).any(|w| w == ["--pid", "4242"]),
            "{as_str:?}"
        );
        assert!(
            as_str.windows(2).any(|w| w == ["--base-path", "/data"]),
            "{as_str:?}"
        );
        assert!(as_str.iter().any(|a| a == "--keep-user-data"), "{as_str:?}");

        let without = helper_args(1, Path::new("/d"), false);
        assert!(
            !without.iter().any(|a| a == "--keep-user-data"),
            "未勾选时不得携带保留参数"
        );
    }

    /// 助手缺失时如实报错：静默失败会让用户以为卸载在进行，实际什么都没发生
    #[test]
    fn test_spawn_helper_reports_missing_helper() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("install");
        std::fs::create_dir_all(&dir).unwrap();
        let err = spawn_helper(&dir, &dir, false).unwrap_err();
        assert!(err.contains("卸载助手缺失"), "{err}");
    }
}
