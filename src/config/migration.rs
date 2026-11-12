//! 配置 schema 版本迁移 pipeline（v5 → v6 → v7 → v8 → v9 → v10）
//!
//! 启动时若 `settings.json` 的 `config_version` 低于当前版本，按 `MIGRATIONS` 顺序
//! 执行迁移函数，将旧结构转换为新结构并写回。迁移是幂等的：Profile 文件使用原子覆盖
//! 写入（tmp + rename，不产生截断文件），`settings.json` 的 `config_version` 更新是
//! commit point——任一迁移返回 Err 都不会提交新版本号，下次启动重跑迁移。
//!
//! v10 起迁移可能**跨文件**（`migrate_v9_to_v10` 把方案里的直连字段搬成
//! `<base>/tasks/http/<id>.json`），因此每个迁移函数都要能安全重跑：先判断
//! 「是否已迁过」，再决定是否落盘。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::Value;

use crate::config::ConfigError;
use crate::config::schema::SettingsData;
use crate::config::service::is_valid_profile_id;
use crate::utils::io::{atomic_copy_file, atomic_write_bytes};

/// 单个迁移函数的签名
///
/// - `config_dir`：配置目录（`config/`）
/// - `value`：已解析的 settings.json 可变 JSON 值（迁移函数就地修改）
type MigrationFn = fn(config_dir: &Path, value: &mut Value) -> Result<(), ConfigError>;

/// 迁移表：目标版本 -> 迁移函数
///
/// 新增版本时在末尾追加 `(新版本, 迁移函数)` 即可。
pub const MIGRATIONS: &[(u32, MigrationFn)] = &[
    (6, migrate_v5_to_v6),
    (7, migrate_v6_to_v7),
    (8, migrate_v7_to_v8),
    (9, migrate_v8_to_v9),
    (10, migrate_v9_to_v10),
];

/// 执行所有需要的迁移
///
/// 就地修改 `value`，并将拆分出的 Profile 文件写入 `config_dir/profiles/`。
/// 返回迁移后的 schema 版本号。
pub fn run_migrations(config_dir: &Path, value: &mut Value) -> Result<u32, ConfigError> {
    let current = crate::config::CURRENT_CONFIG_VERSION;
    let version = value
        .get("config_version")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;

    if version >= current {
        return Ok(version);
    }

    for (target, func) in MIGRATIONS {
        if version < *target {
            func(config_dir, value)?;
        }
    }

    // 写回迁移后的版本号：仅改返回值会导致落盘的 settings.json 永远停在
    // 迁移前版本，每次启动重跑迁移链并重写盘（CFG-1）；v5→v6 内硬编码的
    // 中间 checkpoint（=6）保留，供迁移中途失败的下次续跑定位。
    // 本处即版本 commit point：上方任一迁移返回 Err 都到不了这里，盘上版本号
    // 保持迁移前值，下次启动重跑迁移（如 v10 方案写回失败的注入重试）。
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "config_version".to_string(),
            serde_json::Value::Number(serde_json::Number::from(current)),
        );
    }

    // 迁移成功留痕：配置结构发生了不可逆的结构性变更，用户应能从日志确认
    tracing::info!(
        from = version,
        to = current,
        "配置已迁移 v{version} → v{current}"
    );

    Ok(current)
}

/// v5 → v6 迁移
///
/// 关键变更：
/// 1. 将 settings.json 内联的 `profiles` 字典拆分为 `config/profiles/{id}.json` 独立文件
/// 2. 字段重命名：`carrier`→`isp`、`match_gateway_ip`→`gateway_ip`、`match_ssid`→`wifi_ssid`
/// 3. 删除废弃字段：`carrier_custom` 等
/// 4. 全局字段重命名：`check_interval_seconds`→`check_interval` 等
/// 5. 从 settings 移除 `profiles` 字段，置 `config_version = 6`
fn migrate_v5_to_v6(config_dir: &Path, value: &mut Value) -> Result<(), ConfigError> {
    // 0. 迁移前备份整个 config 目录，防止迁移异常导致配置丢失（失败仅告警，不阻断迁移）
    let backup_dir = match backup_config_dir(config_dir) {
        Ok(dir) => Some(dir),
        Err(e) => {
            tracing::warn!("迁移前备份配置目录失败（已忽略，继续迁移）: {e}");
            None
        }
    };

    // 1. 拆分 profiles 到独立文件
    if let Some(profiles) = value.get_mut("profiles").and_then(Value::as_object_mut) {
        let profiles_dir = config_dir.join("profiles");
        std::fs::create_dir_all(&profiles_dir)?;
        // R4：被跳过的非法 id 集合——若其中包含 active_profile_id，
        // 迁移结束后需回退 default，避免活跃 Profile 指向不存在的文件
        let mut skipped_invalid_ids: HashSet<String> = HashSet::new();
        for (id, profile) in profiles.iter_mut() {
            // 写盘前校验 id：非法 id（路径分隔符/点号等）直接拼进文件名会造成
            // 路径穿越（如 `../evil` 写到 profiles 目录之外）。跳过该 Profile 并
            // 告警，保留原始数据由用户处置，不做 slugify（避免静默改名后无法对应）
            if !is_valid_profile_id(id) {
                tracing::warn!("迁移跳过非法 Profile ID（含不安全字符，已保留在原配置中）: {id}");
                skipped_invalid_ids.insert(id.clone());
                continue;
            }
            // 2. 字段重命名（仅当旧字段存在）
            rename_field(profile, "carrier", "isp");
            rename_field(profile, "match_gateway_ip", "gateway_ip");
            rename_field(profile, "match_ssid", "wifi_ssid");
            // 3. 删除废弃字段
            if let Some(obj) = profile.as_object_mut() {
                obj.remove("carrier_custom");
            }
            // 确保 id 字段与文件名一致
            if let Some(obj) = profile.as_object_mut() {
                obj.insert("id".to_string(), Value::String(id.clone()));
            }
            // 写入独立文件
            let path = profiles_dir.join(format!("{id}.json"));
            let json = serde_json::to_string_pretty(profile)?;
            std::fs::write(&path, json)?;
        }
        // 被跳过的 id 若是活跃 Profile：回退 default，防止活跃指向悬空文件
        let active = value
            .get("active_profile_id")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string();
        if skipped_invalid_ids.contains(&active) {
            tracing::warn!("活跃 Profile ID 「{active}」非法已被跳过，迁移后回退到 default");
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "active_profile_id".to_string(),
                    Value::String("default".to_string()),
                );
            }
        }
    }

    // 4. 全局字段重命名（必须在各子段内部重命名，v5 的值嵌套在
    //    monitor/logging/app 子对象中，在 global 顶层重命名会丢失自定义值）
    if let Some(global) = value.get_mut("global").and_then(Value::as_object_mut) {
        // monitor 子段字段重命名 + 废弃字段清理
        if let Some(monitor) = global.get_mut("monitor") {
            rename_field(monitor, "check_interval_seconds", "check_interval");
            rename_field(monitor, "enable_tcp_check", "tcp_enabled");
            rename_field(monitor, "enable_http_check", "http_enabled");
            // v5 的 `enable_local_check` 是登录前物理网卡连接检查开关（decision.py
            // `check_login_prerequisites`），对应 `local_check_enabled`；URL 内容检测
            // 在 v5 没有独立开关（`url_check_urls` 列表非空即生效），因此 `url_enabled`
            // 在下方按拆分出的目标数派生，而非由该字段改名而来。
            rename_field(monitor, "enable_local_check", "local_check_enabled");
            rename_field(monitor, "ping_targets", "tcp_targets");
            rename_field(monitor, "test_urls", "http_targets");
            // v5 把 URL 与期望正文编码成 `url|expected`；v6 拆成目标数组与映射。
            // 不能只重命名，否则带 `|` 的整串会被当成非法 URL。
            if let Some(m) = monitor.as_object_mut()
                && let Some(Value::Array(items)) = m.remove("url_check_urls")
            {
                let mut targets = Vec::with_capacity(items.len());
                let mut expected = serde_json::Map::new();
                for item in items {
                    let Some(raw) = item.as_str() else {
                        continue;
                    };
                    if let Some((url, response)) = raw.split_once('|') {
                        let url = url.trim().to_string();
                        if !url.is_empty() {
                            targets.push(Value::String(url.clone()));
                            expected.insert(url, Value::String(response.trim().to_string()));
                        }
                    } else {
                        let url = raw.trim();
                        if !url.is_empty() {
                            targets.push(Value::String(url.to_string()));
                        }
                    }
                }
                // v5 语义：列表非空即启用 URL 内容检测，与 Web 层旧客户端的
                // “非空即启用”派生口径一致
                let url_enabled = !targets.is_empty();
                m.insert("url_targets".into(), Value::Array(targets));
                m.insert("url_expected_responses".into(), Value::Object(expected));
                m.insert("url_enabled".into(), Value::Bool(url_enabled));
            }
            // 废弃字段清理
            if let Some(m) = monitor.as_object_mut() {
                for f in [
                    "access_log",
                    "block_proxy",
                    "network_check_timeout",
                    "check_auth_url",
                    "auth_url_targets",
                ] {
                    m.remove(f);
                }
            }
        }
        // logging 子段字段重命名
        if let Some(logging) = global.get_mut("logging") {
            rename_field(logging, "log_retention_days", "retention_days");
        }
        // app 子段字段重命名 + 废弃字段清理
        if let Some(app) = global.get_mut("app") {
            rename_field(app, "app_port", "port");
            rename_field(app, "auto_open_browser", "auto_start_browser");
            if let Some(a) = app.as_object_mut() {
                for f in [
                    "shell_path",
                    "lightweight_tray",
                    "minimize_to_tray",
                    "proxy",
                ] {
                    a.remove(f);
                }
            }
        }
    }

    // 5. 从 settings 移除 profiles 字段，更新版本号
    if let Some(obj) = value.as_object_mut() {
        obj.remove("profiles");
        obj.insert(
            "config_version".to_string(),
            Value::Number(serde_json::Number::from(6u32)),
        );
    }

    // 5.5 迁移结果自检（G4）：确保产物能被当前 schema 解析后才允许删备份。
    // 若解析失败（迁移函数产生了非法结构）立即返回 Err 并保留备份目录——
    // 此时 settings.json 尚未写入新版本号（commit point 在调用方），
    // 下次启动会重跑迁移，用户也可从备份目录手动回滚。
    if let Err(e) = serde_json::from_value::<SettingsData>(value.clone()) {
        tracing::error!("迁移产物无法解析为当前 schema，保留备份目录: {e}");
        return Err(ConfigError::ConfigParseError {
            path: config_dir.display().to_string(),
            reason: format!("迁移产物无法解析为当前 schema: {e}"),
            backup_path: backup_dir.as_ref().map(|d| d.display().to_string()),
        });
    }

    // 6. 迁移成功，清理备份目录
    if let Some(dir) = backup_dir
        && let Err(e) = std::fs::remove_dir_all(&dir)
    {
        tracing::warn!("清理迁移备份目录失败: {e}");
    }

    Ok(())
}

/// 若 `from` 字段存在则重命名为 `to`（值整体搬移）
///
/// 接收 `&mut Value`，内部按需取 object map，兼容 `profiles` 与 `global` 两种调用场景。
fn rename_field(obj: &mut Value, from: &str, to: &str) {
    if let Some(map) = obj.as_object_mut()
        && let Some(v) = map.remove(from)
    {
        if map.contains_key(to) {
            tracing::warn!("迁移重命名跳过：{to} 已存在，丢弃旧字段 {from}");
        } else {
            map.insert(to.to_string(), v);
        }
    }
}

/// 迁移前递归备份整个 config 目录到 `.backup.v5.{timestamp}`
///
/// 仅用于迁移失败时的手动回滚，备份目录以 [`crate::config::BACKUP_PREFIX`] 前缀命名，
/// 不会被 `load_all_profiles` 等逻辑误读。
fn backup_config_dir(config_dir: &Path) -> std::io::Result<PathBuf> {
    let stamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    let backup_dir = unique_backup_dir(config_dir, &stamp);
    copy_dir_recursive(config_dir, &backup_dir)?;
    Ok(backup_dir)
}

/// 生成不与现有目录冲突的备份目录路径（G24）
///
/// 同一秒内多次迁移（崩溃重试/测试）会出现时间戳冲突：直接复用旧目录会让
/// 「不含本次配置」的旧备份被误当成有效备份，且迁移成功后的
/// `remove_dir_all` 会把他人尚未回滚的备份连带误删。冲突时加 `-2`、`-3` …
/// 序号后缀重试，保证每次迁移都得到独立的新目录。
fn unique_backup_dir(config_dir: &Path, stamp: &str) -> PathBuf {
    unique_backup_dir_with_prefix(config_dir, stamp, crate::config::BACKUP_PREFIX)
}

/// [`unique_backup_dir`] 的前缀参数化版本
///
/// 供按版本命名的方案文件备份复用（`.backup.v8.{ts}` / `.backup.v9.{ts}`），
/// 冲突加后缀的口径与整目录备份完全一致。
fn unique_backup_dir_with_prefix(config_dir: &Path, stamp: &str, prefix: &str) -> PathBuf {
    let mut candidate = config_dir.join(format!("{prefix}{stamp}"));
    let mut n: u32 = 2;
    while candidate.exists() {
        candidate = config_dir.join(format!("{prefix}{stamp}-{n}"));
        n += 1;
    }
    candidate
}

/// 迁移写回前备份将被改写的方案文件到 `<config_dir>/.backup.v{version}.{ts}/`
///
/// 落点与命名口径同 v5→v6 的整目录备份（[`backup_config_dir`]），区别是只拷贝
/// 将被改写的单个方案文件。`backup_dir` 由调用方跨多次调用复用（同一轮迁移
/// 共享一个备份目录），首次调用时创建（同秒冲突加 `-2` 后缀，口径同
/// [`unique_backup_dir`]）。
///
/// 备份是 best-effort：失败仅告警，不阻断迁移——写回本身已是原子替换（不产生
/// 截断文件），备份只提供内容层面的回滚副本。备份必须先于写回执行，且单份
/// 备份经 [`atomic_copy_file`] 落盘，崩溃不会留下半截的备份原文误导用户回滚。
fn backup_profile_before_rewrite(
    config_dir: &Path,
    version: u32,
    backup_dir: &mut Option<PathBuf>,
    profile_path: &Path,
) {
    if backup_dir.is_none() {
        let stamp = Local::now().format("%Y%m%d%H%M%S").to_string();
        let dir =
            unique_backup_dir_with_prefix(config_dir, &stamp, &format!(".backup.v{version}."));
        match std::fs::create_dir_all(&dir) {
            Ok(()) => *backup_dir = Some(dir),
            Err(e) => {
                tracing::warn!(
                    path = %dir.display(),
                    error = %e,
                    "创建方案备份目录失败（迁移继续，仅失去本次写回的回滚副本）"
                );
                return;
            }
        }
    }
    let Some(dir) = backup_dir else {
        return;
    };
    let Some(name) = profile_path.file_name() else {
        return;
    };
    let dest = dir.join(name);
    if let Err(e) = atomic_copy_file(profile_path, &dest) {
        tracing::warn!(
            from = %profile_path.display(),
            to = %dest.display(),
            error = %e,
            "备份将被改写的方案文件失败（迁移继续，仅失去该文件的回滚副本）"
        );
    }
}

/// 递归拷贝目录内容（跳过备份目录自身，避免自我嵌套）
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            // 不递归进入已有的备份目录
            if src_path
                .file_name()
                .map(|n| {
                    n.to_string_lossy()
                        .starts_with(crate::config::BACKUP_PREFIX)
                })
                .unwrap_or(false)
            {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// v6 → v7 迁移
///
/// 修复历史遗留的更新源指向：`4ae78a7` 将默认更新源从旧仓库 `Misyra/Campus-Auth`
/// 改指 `Misyra/Campus-Auth-rs`，但仅改了代码默认值——此前创建的配置文件已把旧
/// 地址持久化进 `global.updater.release_source_url`，不会自动跟进。旧仓库 latest
/// 是无平台标识的单资产发布（如 `Campus-Auth-4.2.3.zip`），导致"检查更新"永远
/// 报"发布中未找到平台下载包"。此处将旧地址改写为新仓库。
fn migrate_v6_to_v7(_config_dir: &Path, value: &mut Value) -> Result<(), ConfigError> {
    const OLD_SOURCE_URL: &str = "https://api.github.com/repos/Misyra/Campus-Auth/releases/latest";
    const NEW_SOURCE_URL: &str =
        "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest";

    let Some(updater) = value.get_mut("global").and_then(|g| g.get_mut("updater")) else {
        return Ok(());
    };
    if updater.get("release_source_url").and_then(Value::as_str) == Some(OLD_SOURCE_URL)
        && let Some(obj) = updater.as_object_mut()
    {
        obj.insert(
            "release_source_url".to_string(),
            Value::String(NEW_SOURCE_URL.to_string()),
        );
    }
    Ok(())
}

/// v7 → v8 迁移
///
/// 旧默认 `browser_channel = "playwright"` 为历史别名，实际等价 `chromium`。
/// `list_browsers` 已无 `playwright` 项，遗留值会导致前端无卡高亮、后端虽能
/// 回退但语义不一致。此处归一到 `chromium`。
fn migrate_v7_to_v8(_config_dir: &Path, value: &mut Value) -> Result<(), ConfigError> {
    let Some(browser) = value.get_mut("global").and_then(|g| g.get_mut("browser")) else {
        return Ok(());
    };
    if browser.get("browser_channel").and_then(Value::as_str) == Some("playwright")
        && let Some(obj) = browser.as_object_mut()
    {
        obj.insert(
            "browser_channel".to_string(),
            Value::String("chromium".to_string()),
        );
    }
    Ok(())
}

/// v8 → v9 迁移
///
/// 「启用哪个浏览器任务」从**全局唯一**改为**按方案绑定**：旧实现把选择存在
/// `tasks/.order.json` 的 `active` 字段（全局一份），新实现存在各 Profile 的
/// `active_task`（切方案即切任务）。本迁移把旧的全局选择搬给当前活跃方案，
/// 避免升级后用户的既有选择被静默丢弃（随后回退到内置 default 任务——
/// 表现为"升级后登录用了别的任务"）。
///
/// `.order.json` 的 `active` 字段此后不再读写（`OrderData` 已移除该字段），
/// 残留值会被 serde 忽略，无需清理。
fn migrate_v8_to_v9(config_dir: &Path, _value: &mut Value) -> Result<(), ConfigError> {
    // `.order.json` 位于 `<base>/tasks/`，而 config_dir 为 `<base>/config/`
    let order_path = config_dir
        .parent()
        .unwrap_or(config_dir)
        .join("tasks")
        .join(".order.json");

    let legacy_active = std::fs::read_to_string(&order_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("active")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty());

    let Some(task_id) = legacy_active else {
        // 无旧选择（新装或本就未设置）：不动，由登录解析回退内置 default
        return Ok(());
    };

    // 搬给当前活跃方案；后续未绑定方案的仍回退 default，行为不失真
    let active_id = _value
        .get("active_profile_id")
        .and_then(Value::as_str)
        .filter(|s| is_valid_profile_id(s))
        .unwrap_or("default");

    let profile_path = config_dir
        .join("profiles")
        .join(format!("{active_id}.json"));
    let Some(mut profile) = std::fs::read_to_string(&profile_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        tracing::warn!(
            profile_id = %active_id,
            path = %profile_path.display(),
            "v9 迁移跳过：活跃方案文件不可读，任务绑定留空（将由登录兜底到 default）"
        );
        return Ok(());
    };

    // 已有显式绑定则不覆盖（幂等：重复迁移结果一致）
    let already_bound = profile
        .get("active_task")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if !already_bound {
        if let Some(obj) = profile.as_object_mut() {
            obj.insert("active_task".to_string(), Value::String(task_id.clone()));
        }
        // 写回前先备份原文（best-effort，口径见 backup_profile_before_rewrite）
        let mut backup_dir: Option<PathBuf> = None;
        backup_profile_before_rewrite(config_dir, 8, &mut backup_dir, &profile_path);
        // 原子写回：迁移中途断电/ENOSPC 不得留下截断的方案文件（截断会被
        // load_all_profiles 永久跳过，凭据再也找不回来）
        let json = serde_json::to_string_pretty(&profile)?;
        atomic_write_bytes(&profile_path, json.as_bytes())?;
        tracing::info!(
            profile_id = %active_id,
            task_id = %task_id,
            "v9 迁移：原全局启用任务已绑定到当前方案"
        );
    }

    Ok(())
}

/// v9 → v10 迁移：直连请求参数从方案内联字段搬成独立的「直连任务」
///
/// 背景：v10 起直连请求的全部参数都存在具名任务里（`<base>/tasks/http/<id>.json`，
/// `type: "http"`），方案只保留绑定 `active_http_task`——同一门户的多个账号因此能
/// 共用一份配置，仓库也能分享它。不搬的后果是升级后方案里的 `http_*` 被 serde
/// 静默忽略，用户表现为"直连配置没了"，故必须无损搬过去。
///
/// 判定「这份方案配过直连」的口径：`ProfileData` 带 `#[serde(default)]`，任何方案
/// 文件都会写出全部 `http_*` 键（值是默认值），因此不能按「键存在」判断，只能按
/// **有意义的取值**判断（见 [`legacy_http_config_present`]）。
///
/// 迁移产物刻意**不填**任务的 `auth_url`：方案的 `auth_url` 仍保留在原处，
/// 留空即回退用它，升级前后行为完全一致（用户改方案里的认证地址依然生效）。
///
/// 幂等：任务文件已存在且与方案里的值一致时直接复用（不重复建、不覆盖用户后来
/// 的改动）；方案已有 `active_http_task` 时沿用不改写。
fn migrate_v9_to_v10(config_dir: &Path, _value: &mut Value) -> Result<(), ConfigError> {
    // 目录布局：`config_dir` 为 `<base>/config/`，任务在 `<base>/tasks/`
    let tasks_dir = config_dir.parent().unwrap_or(config_dir).join("tasks");
    let http_dir = tasks_dir.join("http");
    let profiles_dir = config_dir.join(crate::config::PROFILES_DIR);

    let entries = match std::fs::read_dir(&profiles_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // 方案目录不存在：即零个方案，v10 迁移无事可做（幂等 no-op，
            // 生产路径 profiles 目录已在 ConfigService 构造期预建）
            return Ok(());
        }
        Err(e) => {
            // 方案目录不可读：中止迁移而非跳过——此刻提交版本号会让所有方案的
            // 内联直连键在下次保存时静默丢弃，必须留待下次启动重试
            tracing::error!(
                path = %profiles_dir.display(),
                error = %e,
                "v10 迁移中止：方案目录不可读（版本号不提交，下次启动重试）"
            );
            return Err(e.into());
        }
    };

    let mut created: Vec<String> = Vec::new();
    // 同一轮迁移共享一个备份目录，首个将被改写的方案文件触发创建（惰性）
    let mut backup_dir: Option<PathBuf> = None;
    let mut profile_paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    // 目录遍历顺序不保证稳定；迁移结果的命名（`<id>` / `<id>-2`）与之相关，排序后再处理
    profile_paths.sort();

    for path in profile_paths {
        let Some(profile_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // 与配置系统其它入口同口径：非法 id 不是本程序写的文件，一律不碰
        if !is_valid_profile_id(profile_id) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut profile) = serde_json::from_str::<Value>(&raw) else {
            tracing::warn!(
                profile_id = %profile_id,
                "v10 迁移跳过：方案文件解析失败"
            );
            continue;
        };
        if !legacy_http_config_present(&profile) {
            // 没配过直连（或已迁过）：方案里的空 `http_*` 键留着无害——v10 的
            // ProfileData 不再有这些字段，反序列化时会忽略，保存时自然消失
            continue;
        }
        let already_bound = profile
            .get("active_http_task")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());

        let task_id = match pick_http_task_id(&tasks_dir, &http_dir, profile_id, &profile) {
            Ok(id) => id,
            Err(reason) => {
                tracing::error!(
                    profile_id = %profile_id,
                    reason = %reason,
                    "v10 迁移跳过：无法为该方案分配直连任务 ID，方案里的直连配置保持原样（未删除）"
                );
                continue;
            }
        };

        if !path_has_task(&http_dir, &task_id) {
            let task = build_legacy_http_task(&task_id, &profile);
            if let Err(e) = std::fs::create_dir_all(&http_dir) {
                // 传播 Err 而非跳过：跳过会照常提交版本号，该方案的内联直连键在
                // 下次保存时被 v10 schema 静默丢弃——中断重跑比静默丢失安全
                tracing::error!(
                    path = %http_dir.display(),
                    error = %e,
                    "v10 迁移中止：创建直连任务目录失败（版本号不提交，下次启动重试）"
                );
                return Err(e.into());
            }
            // 原子写入直连任务：中途断电/ENOSPC 不得留下截断的任务文件
            let task_path = http_dir.join(format!("{task_id}.json"));
            let json = serde_json::to_string_pretty(&task)?;
            if let Err(e) = atomic_write_bytes(&task_path, json.as_bytes()) {
                tracing::error!(
                    path = %task_path.display(),
                    error = %e,
                    "v10 迁移中止：写入直连任务失败（版本号不提交，下次启动重试）"
                );
                return Err(e.into());
            }
            created.push(task_id.clone());
            append_to_order(&tasks_dir, &task_id);
        }

        // 绑定 + 清掉内联字段：清干净才算迁移完成，否则下次启动会重复搬
        if let Some(obj) = profile.as_object_mut() {
            if !already_bound {
                obj.insert(
                    "active_http_task".to_string(),
                    Value::String(task_id.clone()),
                );
            }
            for key in LEGACY_HTTP_KEYS {
                obj.remove(key);
            }
        }
        // 写回前先备份原文（best-effort：备份失败仅告警，不阻断迁移）
        backup_profile_before_rewrite(config_dir, 9, &mut backup_dir, &path);
        // 原子写回 + 失败传播：写回失败若吞掉告警继续走，版本号照常提交、
        // 方案内联键注定静默丢弃（原「下次启动重试」的承诺并不成立）——
        // 改为传播 Err 阻断版本提交，下次启动重跑迁移补上绑定与清理
        let json = serde_json::to_string_pretty(&profile)?;
        if let Err(e) = atomic_write_bytes(&path, json.as_bytes()) {
            tracing::error!(
                profile_id = %profile_id,
                error = %e,
                "v10 迁移中止：写回方案失败（版本号不提交，下次启动重试）"
            );
            return Err(e.into());
        }
    }

    if !created.is_empty() {
        tracing::info!(
            count = created.len(),
            tasks = ?created,
            "v10 迁移：方案里的直连配置已搬为直连任务并完成绑定（任务页 · 直连任务）"
        );
    }
    Ok(())
}

/// v9 及以前内联在方案里的直连字段（v10 起全部搬进直连任务）
const LEGACY_HTTP_KEYS: [&str; 8] = [
    "http_method",
    "http_url",
    "http_headers",
    "http_body",
    "http_success_pattern",
    "http_failure_pattern",
    "http_crypto_script",
    "http_ignore_https_errors",
];

/// 这份方案是否真的配过直连（键必然存在，故只能按取值判断，见迁移函数说明）
///
/// 口径是「任何一项非空即算配过」而非只看 `http_url`：配到一半就升级的方案
/// （例如先写好凭据变换脚本、地址还没填）如果判为"没配过"，那些字段会被 v10 的
/// 结构直接忽略、在下一次保存时静默消失。宁可多搬出一个待补地址的任务——用户在
/// 任务编辑器里补上地址即可，而静默丢脚本是无法挽回的（脚本是用户逆出来的算法）。
fn legacy_http_config_present(profile: &Value) -> bool {
    let text = |key: &str| {
        profile
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    text("http_url")
        || text("http_headers")
        || text("http_body")
        || text("http_success_pattern")
        || text("http_failure_pattern")
        || text("http_crypto_script")
        // 非 null 的证书策略是显式选择（默认是 null = 跟随全局）
        || profile
            .get("http_ignore_https_errors")
            .is_some_and(|v| v.is_boolean())
}

/// 目标任务文件是否已存在（`<http_dir>/<id>.json`）
fn path_has_task(http_dir: &Path, task_id: &str) -> bool {
    http_dir.join(format!("{task_id}.json")).exists()
}

/// 该任务 ID 是否已被**任一类型**的任务占用
///
/// 三类任务共用同一个 `task_id` 命名空间（`save_task` 保存同 ID 的另一类型时会清掉
/// 原桶文件），迁移挑 ID 因此必须避开全部三个桶，而不是只看 `tasks/http/`：
/// 方案 id 与浏览器任务 id 撞名（`default` 是最典型的一个——方案叫 default、内置
/// 浏览器任务也叫 default）时，直连任务占了 `default` 会让用户一保存它就把浏览器
/// 兜底任务清掉。撞名则换 `<id>-2` 后缀。
fn task_id_taken(tasks_dir: &Path, task_id: &str) -> bool {
    ["browser", "scripts", "http"].iter().any(|bucket| {
        tasks_dir
            .join(bucket)
            .join(format!("{task_id}.json"))
            .exists()
    })
}

/// 为该方案挑一个可用的直连任务 ID
///
/// 首选方案 id（可读、与方案一一对应）；已被占用时依次尝试 `<id>-2`…`<id>-20`：
/// 占位说明用户自己建过同名任务、别的类型占用了同名 id，或上一次迁移已写过，
/// **绝不覆盖**用户的文件。
fn pick_http_task_id(
    tasks_dir: &Path,
    http_dir: &Path,
    profile_id: &str,
    profile: &Value,
) -> Result<String, String> {
    if let Some(existing) = reusable_existing_task(http_dir, profile_id, profile) {
        return Ok(existing);
    }
    if !task_id_taken(tasks_dir, profile_id) {
        return Ok(profile_id.to_string());
    }
    for n in 2..=20 {
        let candidate = format!("{profile_id}-{n}");
        if !task_id_taken(tasks_dir, &candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("{profile_id} 及其 -2…-20 后缀的任务 ID 都已被占用"))
}

/// 已存在的同名任务是否**就是**这份方案的直连配置（上次迁移写到一半就退出时复用，
/// 避免重复建出 `<id>-2`）；内容不一致（用户自建的同名任务）返回 `None`，由调用方换名。
fn reusable_existing_task(http_dir: &Path, profile_id: &str, profile: &Value) -> Option<String> {
    let path = http_dir.join(format!("{profile_id}.json"));
    let existing: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let same = |task_key: &str, profile_key: &str| {
        existing.get(task_key).and_then(Value::as_str).unwrap_or("")
            == profile
                .get(profile_key)
                .and_then(Value::as_str)
                .unwrap_or("")
    };
    let matches = same("url", "http_url")
        && same("headers", "http_headers")
        && same("body", "http_body")
        && same("success_pattern", "http_success_pattern")
        && same("failure_pattern", "http_failure_pattern")
        && same("crypto_script", "http_crypto_script");
    matches.then(|| profile_id.to_string())
}

/// 由方案里的 v9 内联字段构造直连任务 JSON（字段名与 `HttpTaskConfig` 对齐）
fn build_legacy_http_task(task_id: &str, profile: &Value) -> Value {
    let name = profile
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(task_id);
    let method = profile
        .get("http_method")
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_uppercase())
        .filter(|s| s == "POST")
        .unwrap_or_else(|| "GET".to_string());
    let ignore = profile
        .get("http_ignore_https_errors")
        .filter(|v| v.is_boolean())
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "type": "http",
        "task_id": task_id,
        "name": format!("{name} 直连"),
        "description": format!("v10 迁移：由方案「{name}」的直连配置生成"),
        "method": method,
        "url": profile.get("http_url").and_then(Value::as_str).unwrap_or(""),
        // 刻意留空：方案的 auth_url 仍在原处，留空即回退用它，升级前后行为一致
        "auth_url": "",
        "headers": profile.get("http_headers").and_then(Value::as_str).unwrap_or(""),
        "body": profile.get("http_body").and_then(Value::as_str).unwrap_or(""),
        "success_pattern": profile.get("http_success_pattern").and_then(Value::as_str).unwrap_or(""),
        "failure_pattern": profile.get("http_failure_pattern").and_then(Value::as_str).unwrap_or(""),
        "crypto_script": profile.get("http_crypto_script").and_then(Value::as_str).unwrap_or(""),
        "ignore_https_errors": ignore,
    })
}

/// 把新任务 id 追加进 `<tasks>/.order.json` 的排序表（缺失/损坏时按空表处理）
///
/// 不追加也能用（列表按扫描顺序兜底），但顺序会随目录遍历漂移；迁移产生的任务
/// 往往一次多个，落进排序表才能给出稳定顺序。
fn append_to_order(tasks_dir: &Path, task_id: &str) {
    let path = tasks_dir.join(".order.json");
    let mut order = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("order").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    if order.iter().any(|v| v.as_str() == Some(task_id)) {
        return;
    }
    order.push(Value::String(task_id.to_string()));
    let json = serde_json::json!({ "order": order });
    if let Ok(text) = serde_json::to_string_pretty(&json)
        && let Err(e) = atomic_write_bytes(&path, text.as_bytes())
    {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "v10 迁移：写入任务排序表失败（不影响任务可用，仅顺序不固定）"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 构造一个 v5 结构的 settings.json JSON 值（含旧字段名与废弃字段）
    fn v5_settings() -> Value {
        serde_json::json!({
            "config_version": 1,
            "active_profile_id": "default",
            "auto_switch": true,
            "profiles": {
                "default": {
                    "name": "默认",
                    "username": "u",
                    "password": "ENC:abc",
                    "carrier": "移动",
                    "match_gateway_ip": "192.168.1.1",
                    "match_ssid": "campus",
                    "carrier_custom": "custom",
                }
            },
            "global": {
                "monitor": {
                    "check_interval_seconds": 30,
                    "enable_tcp_check": true,
                    "enable_http_check": false,
                    "enable_local_check": true,
                    "ping_targets": ["1.1.1.1"],
                    "test_urls": ["http://test"],
                    "url_check_urls": ["http://apple|Success"],
                    "access_log": true,
                    "block_proxy": false,
                    "network_check_timeout": 5,
                    "check_auth_url": true,
                    "auth_url_targets": []
                },
                "logging": {
                    "log_retention_days": 7
                },
                "app": {
                    "app_port": 50721,
                    "auto_open_browser": true,
                    "shell_path": "cmd",
                    "lightweight_tray": false,
                    "minimize_to_tray": false,
                    "proxy": null
                }
            }
        })
    }

    /// 读取拆分出的 profile 文件内容
    fn read_profile(config_dir: &Path, id: &str) -> Value {
        let path = config_dir.join("profiles").join(format!("{id}.json"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn test_migrate_v5_to_v6_renames_global_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let mut v = v5_settings();

        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();

        // monitor 子段重命名
        let monitor = &v["global"]["monitor"];
        assert_eq!(monitor["check_interval"], 30);
        assert_eq!(monitor["tcp_enabled"], true);
        assert_eq!(monitor["http_enabled"], false);
        // enable_local_check 是登录前物理网卡检查开关 → local_check_enabled
        assert_eq!(monitor["local_check_enabled"], true);
        // url_enabled 由非空 url_check_urls 派生（v5 无独立 URL 检测开关）
        assert_eq!(monitor["url_enabled"], true);
        assert_eq!(monitor["tcp_targets"][0], "1.1.1.1");
        assert_eq!(monitor["http_targets"][0], "http://test");
        assert_eq!(monitor["url_targets"][0], "http://apple");
        assert_eq!(monitor["url_expected_responses"]["http://apple"], "Success");
        // 废弃字段已删除
        for f in [
            "check_interval_seconds",
            "enable_tcp_check",
            "enable_local_check",
            "ping_targets",
            "access_log",
            "block_proxy",
            "network_check_timeout",
        ] {
            assert!(monitor.get(f).is_none(), "废弃字段 {f} 应被删除");
        }
        // logging / app 重命名
        assert_eq!(v["global"]["logging"]["retention_days"], 7);
        assert_eq!(v["global"]["app"]["port"], 50721);
        assert_eq!(v["global"]["app"]["auto_start_browser"], true);
        assert!(v["global"]["app"].get("app_port").is_none());
        assert!(v["global"]["app"].get("shell_path").is_none());
    }

    /// v5 迁移中 url_enabled 派生与 local_check_enabled 映射相互独立：
    /// 前者只看 url_check_urls 是否非空，后者只看 enable_local_check 原值
    #[test]
    fn test_migrate_v5_to_v6_url_enabled_derives_from_list_only() {
        let tmp = tempfile::tempdir().unwrap();
        // 关闭本地检查但保留非空 URL 列表：url_enabled 仍应为 true
        let mut v = v5_settings();
        v["global"]["monitor"]["enable_local_check"] = serde_json::json!(false);
        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();
        let monitor = &v["global"]["monitor"];
        assert_eq!(monitor["local_check_enabled"], false);
        assert_eq!(monitor["url_enabled"], true);

        // 开启本地检查但清空 URL 列表（v5 关闭 URL 检测的唯一方式）：url_enabled 应为 false
        let tmp = tempfile::tempdir().unwrap();
        let mut v = v5_settings();
        v["global"]["monitor"]["url_check_urls"] = serde_json::json!([]);
        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();
        let monitor = &v["global"]["monitor"];
        assert_eq!(monitor["local_check_enabled"], true);
        assert_eq!(monitor["url_enabled"], false);
    }

    #[test]
    fn test_migrate_v5_to_v6_splits_profiles_and_renames_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let mut v = v5_settings();

        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();

        // profiles 已拆分到独立文件
        let p = read_profile(tmp.path(), "default");
        assert_eq!(p["id"], "default");
        assert_eq!(p["name"], "默认");
        assert_eq!(p["password"], "ENC:abc");
        // profile 字段重命名
        assert_eq!(p["isp"], "移动");
        assert_eq!(p["gateway_ip"], "192.168.1.1");
        assert_eq!(p["wifi_ssid"], "campus");
        // 废弃字段删除
        assert!(p.get("carrier").is_none());
        assert!(p.get("carrier_custom").is_none());
        // settings 中不再包含 profiles
        assert!(v.get("profiles").is_none());
        // 版本号已提交
        assert_eq!(v["config_version"], 6);
    }

    #[test]
    fn test_run_migrations_skips_when_version_current() {
        let tmp = tempfile::tempdir().unwrap();
        // 已是 CURRENT：跳过迁移链（版本前置检查），值原样保留
        let mut v = serde_json::json!({
            "config_version": crate::config::CURRENT_CONFIG_VERSION,
            "active_profile_id": "default",
        });
        let result = run_migrations(tmp.path(), &mut v).unwrap();
        assert_eq!(result, crate::config::CURRENT_CONFIG_VERSION);
        assert_eq!(v["active_profile_id"], "default");
        // 未创建 profiles 目录（无迁移发生）
        assert!(!tmp.path().join("profiles").exists());
    }

    /// CFG-1 回归：迁移链跑完后 config_version 必须写回 CURRENT，
    /// 落盘值不再停留于迁移前版本（否则每次启动重跑迁移）
    #[test]
    fn test_run_migrations_writes_back_config_version() {
        let tmp = tempfile::tempdir().unwrap();
        let mut v = serde_json::json!({
            "config_version": 6,
            "active_profile_id": "default",
        });
        let result = run_migrations(tmp.path(), &mut v).unwrap();
        assert_eq!(result, crate::config::CURRENT_CONFIG_VERSION);
        assert_eq!(
            v["config_version"].as_u64().unwrap(),
            u64::from(crate::config::CURRENT_CONFIG_VERSION),
            "迁移后 config_version 必须落回 value"
        );
    }

    #[test]
    fn test_migrate_v5_to_v6_is_idempotent() {
        // 迁移后的值再次迁移不改变结构（版本前置检查保证不重复执行，此处验证值本身稳定）
        let tmp = tempfile::tempdir().unwrap();
        let mut v = v5_settings();
        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();
        let first = v.clone();

        // 手动把版本降回 1 再执行（模拟异常路径下的重入）
        v["config_version"] = serde_json::json!(1);
        v["profiles"] = serde_json::json!({}); // 无 profiles 时拆分循环跳过
        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();

        // 字段层面（monitor）与首次一致
        assert_eq!(
            v["global"]["monitor"]["check_interval"],
            first["global"]["monitor"]["check_interval"]
        );
        assert_eq!(v["config_version"], 6);
    }

    #[test]
    fn test_rename_field_moves_value() {
        let mut obj = serde_json::json!({ "old": "v", "keep": 1 });
        rename_field(&mut obj, "old", "new");
        assert_eq!(obj["new"], "v");
        assert!(obj.get("old").is_none());
        assert_eq!(obj["keep"], 1);
        // 不存在的字段：无操作
        rename_field(&mut obj, "missing", "also_missing");
        assert!(obj.get("also_missing").is_none());
    }

    // ============ G24：备份目录时间戳冲突 ============

    #[test]
    fn test_unique_backup_dir_appends_suffix_on_collision() {
        let tmp = tempfile::tempdir().unwrap();
        // 预占同时间戳目录（模拟同秒内的上一次迁移备份）
        let occupied = tmp
            .path()
            .join(format!("{}20250101000000", crate::config::BACKUP_PREFIX));
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(occupied.join("sentinel"), "old-backup").unwrap();

        let got = unique_backup_dir(tmp.path(), "20250101000000");
        assert_eq!(
            got,
            tmp.path()
                .join(format!("{}20250101000000-2", crate::config::BACKUP_PREFIX))
        );
        // 二级冲突继续递增
        std::fs::create_dir_all(&got).unwrap();
        let got2 = unique_backup_dir(tmp.path(), "20250101000000");
        assert!(got2.to_string_lossy().ends_with("-3"));
        // 原备份目录不被触碰
        assert!(occupied.join("sentinel").exists());
    }

    #[test]
    fn test_backup_config_dir_collision_does_not_reuse_old_dir() {
        // 同秒内连续两次备份：第二次不复用第一次的目录（改名重试），
        // 两个目录均存在且相互独立——防止 remove_dir_all 误删他人备份
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("settings.json"), "{}").unwrap();

        let first = backup_config_dir(tmp.path()).unwrap();
        let second = backup_config_dir(tmp.path()).unwrap();
        assert!(first.exists());
        assert!(second.exists());
        assert_ne!(first, second, "冲突时必须改名而不是复用旧目录");
        // 两次备份均包含源内容
        assert!(first.join("settings.json").exists());
        assert!(second.join("settings.json").exists());
    }

    // ============ R4：迁移 profile id 路径穿越 ============

    #[test]
    fn test_migrate_skips_path_traversal_profile_id() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().parent().unwrap().to_path_buf();
        let mut v = v5_settings();
        v["active_profile_id"] = serde_json::json!("../evil");
        v["profiles"]["../evil"] = serde_json::json!({
            "name": "恶意",
            "username": "x",
            "password": "ENC:abc",
        });

        migrate_v5_to_v6(tmp.path(), &mut v).unwrap();

        // 不产生穿越文件（profiles 目录之外无 evil.json）
        assert!(
            !outside.join("evil.json").exists(),
            "不得写出 profiles 目录"
        );
        assert!(!tmp.path().join("evil.json").exists());
        // profiles 目录只含合法 id 的文件
        let profiles_dir = tmp.path().join("profiles");
        let names: Vec<String> = std::fs::read_dir(&profiles_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["default.json".to_string()]);
        // 非法 id 是活跃 Profile 时回退 default
        assert_eq!(v["active_profile_id"], "default");
    }

    // ============ G4：迁移产物解析失败保留备份、不提交版本 ============

    #[test]
    fn test_migrate_invalid_result_keeps_backup_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"config_version": 5}"#).unwrap();
        let mut v = v5_settings();
        // check_interval_seconds 类型错误（字符串）：重命名后 check_interval 无法解析为 u32
        v["global"]["monitor"]["check_interval_seconds"] = serde_json::json!("not-a-number");

        let result = migrate_v5_to_v6(tmp.path(), &mut v);
        assert!(result.is_err(), "迁移产物不可解析应返回错误");

        // 备份目录保留（未因「迁移成功」路径被清理），可供手动回滚
        let backups: Vec<std::path::PathBuf> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .map(|n| {
                        n.to_string_lossy()
                            .starts_with(crate::config::BACKUP_PREFIX)
                    })
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(backups.len(), 1, "应保留恰好一个备份目录");
        assert!(
            backups[0].join("settings.json").exists(),
            "备份应包含原配置"
        );
    }

    // ============ v6 → v7：更新源旧仓库地址改写 ============

    #[test]
    fn test_migrate_v6_to_v7_rewrites_old_source_url() {
        let mut v = serde_json::json!({
            "config_version": 6,
            "global": {
                "updater": {
                    "check_on_startup": true,
                    "release_source_url": "https://api.github.com/repos/Misyra/Campus-Auth/releases/latest",
                    "use_proxy": true,
                    "proxy_port": 7890
                }
            }
        });

        migrate_v6_to_v7(tempfile::tempdir().unwrap().path(), &mut v).unwrap();
        assert_eq!(
            v["global"]["updater"]["release_source_url"],
            "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest",
            "旧仓库更新源应改写为新仓库"
        );
        // 同文件其他字段不受影响
        assert_eq!(v["global"]["updater"]["proxy_port"], 7890);
    }

    #[test]
    fn test_migrate_v6_to_v7_keeps_other_urls() {
        // 已是新地址 / 自定义地址 / updater 缺失：均不动
        let mut v = serde_json::json!({
            "global": {
                "updater": {
                    "release_source_url": "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest"
                }
            }
        });
        migrate_v6_to_v7(tempfile::tempdir().unwrap().path(), &mut v).unwrap();
        assert_eq!(
            v["global"]["updater"]["release_source_url"],
            "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest"
        );

        let mut v = serde_json::json!({ "global": {} });
        migrate_v6_to_v7(tempfile::tempdir().unwrap().path(), &mut v).unwrap();
        assert!(
            v["global"].get("updater").is_none(),
            "无 updater 不应凭空创建"
        );
    }

    // ============ v8 → v9：全局启用任务搬给当前方案 ============

    /// 搭出 `<base>/config/settings.json` + `<base>/tasks/.order.json` + 方案文件的目录结构
    fn v8_fixture(order_active: Option<&str>, profile_active: Option<&str>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::create_dir_all(base.join("config").join("profiles")).unwrap();
        std::fs::create_dir_all(base.join("tasks")).unwrap();

        if let Some(a) = order_active {
            std::fs::write(
                base.join("tasks").join(".order.json"),
                format!(r#"{{"order":["t1"],"active":"{a}"}}"#),
            )
            .unwrap();
        }
        let mut profile = serde_json::json!({"id": "default", "name": "默认"});
        if let Some(t) = profile_active {
            profile["active_task"] = Value::String(t.to_string());
        }
        std::fs::write(
            base.join("config").join("profiles").join("default.json"),
            serde_json::to_string_pretty(&profile).unwrap(),
        )
        .unwrap();
        tmp
    }

    #[test]
    fn test_migrate_v8_to_v9_binds_global_task_to_active_profile() {
        let tmp = v8_fixture(Some("mytask"), None);
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 8, "active_profile_id": "default"});

        migrate_v8_to_v9(&config_dir, &mut v).unwrap();

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(config_dir.join("profiles").join("default.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved["active_task"], "mytask",
            "原全局启用任务应绑定到当前方案，避免升级后选择被静默丢弃"
        );
    }

    #[test]
    fn test_migrate_v8_to_v9_does_not_override_existing_binding() {
        // 方案已有显式绑定：幂等，不覆盖（重复迁移结果一致）
        let tmp = v8_fixture(Some("globaltask"), Some("boundtask"));
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 8, "active_profile_id": "default"});

        migrate_v8_to_v9(&config_dir, &mut v).unwrap();

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(config_dir.join("profiles").join("default.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["active_task"], "boundtask");
    }

    #[test]
    fn test_migrate_v8_to_v9_without_legacy_active_is_noop() {
        // 无旧全局选择（新装/未设置）：不动方案文件，由登录解析兜底 default
        let tmp = v8_fixture(None, None);
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 8, "active_profile_id": "default"});

        migrate_v8_to_v9(&config_dir, &mut v).unwrap();

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(config_dir.join("profiles").join("default.json")).unwrap(),
        )
        .unwrap();
        assert!(
            saved.get("active_task").is_none(),
            "无旧选择时不应凭空写入绑定"
        );
    }

    #[test]
    fn test_migrate_v8_to_v9_missing_order_file_is_noop() {
        // .order.json 不存在（如任务目录被清空）不应报错，迁移必须能继续
        let tmp = v8_fixture(None, None);
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 8, "active_profile_id": "default"});
        assert!(migrate_v8_to_v9(&config_dir, &mut v).is_ok());
    }

    // ============ v9 → v10：直连配置搬成直连任务 ============

    /// v9 形态配置目录：一个方案（含全套内联 `http_*` 键，与 `#[serde(default)]`
    /// 的落盘形态一致——即便没配直连也写着这些键）+ 空 `tasks/` 目录
    ///
    /// `http_url` 为空表示"这个方案没配过直连"：此时其余可选字段也必须留空，
    /// 因为真实存量里没动过直连的方案，`http_*` 全是 serde 默认值（空串/null）。
    /// 反之只要传了地址，就把整套字段填满——这两态正是迁移要区分的。
    fn v9_fixture(profile_id: &str, http_url: &str, ignore_tls: Option<bool>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let profiles_dir = tmp.path().join("config").join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::create_dir_all(tmp.path().join("tasks")).unwrap();
        let configured = !http_url.trim().is_empty();
        let text = |value: &str| {
            Value::String(if configured {
                value.to_string()
            } else {
                String::new()
            })
        };
        let profile = serde_json::json!({
            "id": profile_id,
            "name": "宿舍",
            "username": "20230001",
            "password": "ENC:xxx",
            "auth_url": "http://10.0.0.1/",
            "trigger_url": "",
            "login_channel": "http",
            "http_method": "POST",
            "http_url": http_url,
            "http_headers": text("Content-Type: application/x-www-form-urlencoded"),
            "http_body": text("u={username}&p={password}"),
            "http_success_pattern": text("登录成功"),
            "http_failure_pattern": text("密码错误"),
            "http_crypto_script": text("function transform(ctx) { return {}; }"),
            "http_ignore_https_errors": ignore_tls,
        });
        std::fs::write(
            profiles_dir.join(format!("{profile_id}.json")),
            serde_json::to_string_pretty(&profile).unwrap(),
        )
        .unwrap();
        tmp
    }

    #[test]
    fn test_migrate_v9_to_v10_moves_inline_config_into_task() {
        let tmp = v9_fixture("default", "http://10.0.0.1/login", Some(false));
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        let task_path = tmp.path().join("tasks").join("http").join("default.json");
        let task: Value =
            serde_json::from_str(&std::fs::read_to_string(&task_path).unwrap()).unwrap();
        assert_eq!(task["type"], "http");
        assert_eq!(task["task_id"], "default");
        assert_eq!(task["name"], "宿舍 直连");
        assert_eq!(task["method"], "POST");
        assert_eq!(task["url"], "http://10.0.0.1/login");
        assert_eq!(task["body"], "u={username}&p={password}");
        assert_eq!(task["success_pattern"], "登录成功");
        assert_eq!(task["failure_pattern"], "密码错误");
        assert_eq!(task["ignore_https_errors"], false);
        assert_eq!(
            task["auth_url"], "",
            "刻意留空：方案的认证地址仍在原处，留空即回退用它（升级前后行为一致）"
        );

        let profile = read_profile(&config_dir, "default");
        assert_eq!(profile["active_http_task"], "default");
        assert_eq!(profile["auth_url"], "http://10.0.0.1/", "认证地址仍属方案");
        for key in LEGACY_HTTP_KEYS {
            assert!(
                profile.get(key).is_none(),
                "{key} 必须搬走并清掉，否则下次启动会重复搬"
            );
        }

        let order: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("tasks").join(".order.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            order["order"][0], "default",
            "新任务要落进排序表，顺序才稳定"
        );
    }

    #[test]
    fn test_migrate_v9_to_v10_skips_profile_without_http_config() {
        // 「没配过直连」的判定必须按取值而非键存在：所有方案文件都有这些键
        let tmp = v9_fixture("default", "", None);
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        assert!(
            !tmp.path()
                .join("tasks")
                .join("http")
                .join("default.json")
                .exists(),
            "没填过直连地址就不该凭空造任务"
        );
        let profile = read_profile(&config_dir, "default");
        assert!(profile.get("active_http_task").is_none());
        assert_eq!(
            profile["http_url"], "",
            "未迁移的方案保持原样（空键留着无害，保存时自然消失）"
        );
    }

    #[test]
    fn test_migrate_v9_to_v10_keeps_partial_config_without_url() {
        // 配到一半（有脚本没地址）也要搬：判为"没配过"会让脚本在下次保存时静默消失。
        // 搬出来的任务地址为空，用户在任务编辑器里补上即可（比丢算法好得多）。
        let tmp = v9_fixture("default", "", None);
        let config_dir = tmp.path().join("config");
        let mut partial = read_profile(&config_dir, "default");
        partial["http_crypto_script"] =
            Value::String("function transform(ctx) { return {}; }".into());
        std::fs::write(
            config_dir.join("profiles").join("default.json"),
            serde_json::to_string_pretty(&partial).unwrap(),
        )
        .unwrap();
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        let task: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("tasks").join("http").join("default.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            task["crypto_script"],
            "function transform(ctx) { return {}; }"
        );
        assert_eq!(task["url"], "", "地址留空，等用户补");
        assert_eq!(
            read_profile(&config_dir, "default")["active_http_task"],
            "default"
        );
    }

    #[test]
    fn test_migrate_v9_to_v10_is_idempotent() {
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();
        let first =
            std::fs::read_to_string(tmp.path().join("tasks").join("http").join("default.json"))
                .unwrap();
        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        assert_eq!(
            first,
            std::fs::read_to_string(tmp.path().join("tasks").join("http").join("default.json"))
                .unwrap(),
            "重跑不得改写已生成的任务"
        );
        assert!(
            !tmp.path()
                .join("tasks")
                .join("http")
                .join("default-2.json")
                .exists(),
            "重跑不得重复建任务"
        );
        assert_eq!(
            read_profile(&config_dir, "default")["active_http_task"],
            "default"
        );
    }

    #[test]
    fn test_migrate_v9_to_v10_does_not_clobber_user_task() {
        // 用户自己建过同名直连任务：迁移让位（换 `<id>-2`），绝不覆盖用户文件
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let http_dir = tmp.path().join("tasks").join("http");
        std::fs::create_dir_all(&http_dir).unwrap();
        let mine = http_dir.join("default.json");
        std::fs::write(
            &mine,
            r#"{"type":"http","task_id":"default","name":"我自己建的"}"#,
        )
        .unwrap();
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        assert_eq!(
            std::fs::read_to_string(&mine).unwrap(),
            r#"{"type":"http","task_id":"default","name":"我自己建的"}"#,
            "用户的同名任务必须原封不动"
        );
        assert!(
            http_dir.join("default-2.json").exists(),
            "迁移应换用 -2 后缀"
        );
        assert_eq!(
            read_profile(&config_dir, "default")["active_http_task"],
            "default-2"
        );
    }

    #[test]
    fn test_migrate_v9_to_v10_avoids_id_taken_by_other_kind() {
        // 方案 id 与**别的类型**的任务撞名（真实场景：方案 default + 内置浏览器任务
        // default）。直连任务若占了这个 id，用户一保存它就会把内置浏览器兜底任务
        // 清掉，故迁移必须换后缀。
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let browser_dir = tmp.path().join("tasks").join("browser");
        std::fs::create_dir_all(&browser_dir).unwrap();
        std::fs::write(
            browser_dir.join("default.json"),
            r#"{"type":"browser","task_id":"default","name":"通用登录","steps":[{"type":"click"}]}"#,
        )
        .unwrap();
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        assert!(
            browser_dir.join("default.json").exists(),
            "内置浏览器任务不得被迁移动过"
        );
        assert!(
            tmp.path()
                .join("tasks")
                .join("http")
                .join("default-2.json")
                .exists(),
            "撞名时直连任务应换 -2 后缀"
        );
        assert_eq!(
            read_profile(&config_dir, "default")["active_http_task"],
            "default-2"
        );
    }

    #[test]
    fn test_migrate_v9_to_v10_reuses_task_from_interrupted_run() {
        // 上次迁移写了任务但没能写回方案：重跑要复用它，而不是再建一个 default-2
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let http_dir = tmp.path().join("tasks").join("http");
        std::fs::create_dir_all(&http_dir).unwrap();
        let profile_before = read_profile(&config_dir, "default");
        std::fs::write(
            http_dir.join("default.json"),
            serde_json::to_string_pretty(&build_legacy_http_task("default", &profile_before))
                .unwrap(),
        )
        .unwrap();
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});

        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        assert!(
            !http_dir.join("default-2.json").exists(),
            "内容一致的同名任务应复用，不重复建"
        );
        assert_eq!(
            read_profile(&config_dir, "default")["active_http_task"],
            "default"
        );
    }

    // ============ 中断续跑：写回失败传播 + 写回前备份 + 幂等重跑（T0b-1）============

    /// 列出 config_dir 下以指定前缀命名的备份目录（排序保证稳定）
    fn list_backup_dirs(config_dir: &Path, prefix: &str) -> Vec<std::path::PathBuf> {
        let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(config_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with(prefix))
                    .unwrap_or(false)
            })
            .collect();
        dirs.sort();
        dirs
    }

    /// v9→v10 方案写回失败必须传播 Err 阻断版本提交：config_version 停在 9、
    /// 方案文件保持原文；修复后下次迁移重跑成功（中断轮已写出的任务被复用）
    #[test]
    fn test_migrate_v9_to_v10_write_back_failure_blocks_version_commit() {
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let profiles_dir = config_dir.join("profiles");
        let profile_path = profiles_dir.join("default.json");
        let original = std::fs::read_to_string(&profile_path).unwrap();

        // 注入写回失败：Unix 让方案目录只读（atomic_write_bytes 建临时文件即失败）；
        // Windows 让目标文件只读（rename 覆盖只读文件被拒绝）
        let dir_perm = std::fs::metadata(&profiles_dir).unwrap().permissions();
        #[cfg(windows)]
        let file_perm = std::fs::metadata(&profile_path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut readonly = dir_perm.clone();
            readonly.set_mode(0o555);
            std::fs::set_permissions(&profiles_dir, readonly).unwrap();
        }
        #[cfg(windows)]
        {
            let mut readonly = file_perm.clone();
            readonly.set_readonly(true);
            std::fs::set_permissions(&profile_path, readonly).unwrap();
        }

        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});
        let result = run_migrations(&config_dir, &mut v);

        // 立即恢复权限：后续重跑与临时目录清理都需要可写
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut writable = dir_perm.clone();
            writable.set_mode(0o755);
            std::fs::set_permissions(&profiles_dir, writable).unwrap();
        }
        #[cfg(windows)]
        {
            let mut writable = file_perm.clone();
            writable.set_readonly(false);
            std::fs::set_permissions(&profile_path, writable).unwrap();
        }

        assert!(result.is_err(), "写回失败必须传播 Err 阻断版本提交");
        assert_eq!(v["config_version"], 9, "版本号不得提交，下次启动重跑迁移");
        assert_eq!(
            std::fs::read_to_string(&profile_path).unwrap(),
            original,
            "原子写失败不得改动方案文件（无截断、无半截内容）"
        );
        // 原子写语义兜住：失败后无 .tmp_ 残留
        let residue: Vec<String> = std::fs::read_dir(&profiles_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(crate::config::TMP_PREFIX))
            .collect();
        assert!(residue.is_empty(), "写回失败不得残留临时文件: {residue:?}");

        // 修复权限后重跑：迁移成功，中断轮已写出的直连任务被复用（不重复建 -2）
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});
        let version = run_migrations(&config_dir, &mut v).unwrap();
        assert_eq!(version, crate::config::CURRENT_CONFIG_VERSION);
        assert_eq!(
            read_profile(&config_dir, "default")["active_http_task"],
            "default"
        );
        assert!(
            !tmp.path()
                .join("tasks")
                .join("http")
                .join("default-2.json")
                .exists(),
            "重跑应复用中断轮已写出的任务"
        );
    }

    /// 写回前备份：`.backup.v8.*` / `.backup.v9.*` 保存改写前原文
    #[test]
    fn test_migrate_backs_up_profiles_before_rewrite() {
        // v9→v10：备份内联直连键尚未搬走的方案原文
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let original =
            std::fs::read_to_string(config_dir.join("profiles").join("default.json")).unwrap();
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});
        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        let backups = list_backup_dirs(&config_dir, ".backup.v9.");
        assert_eq!(backups.len(), 1, "应产生恰好一个 v9 备份目录");
        assert_eq!(
            std::fs::read_to_string(backups[0].join("default.json")).unwrap(),
            original,
            "备份必须是改写前原文"
        );
        assert!(original.contains("\"http_url\""), "原文应含内联直连键");
        assert!(!original.contains("active_http_task"), "原文应无新绑定");

        // v8→v9：同口径备份
        let tmp8 = v8_fixture(Some("mytask"), None);
        let config_dir8 = tmp8.path().join("config");
        let original8 =
            std::fs::read_to_string(config_dir8.join("profiles").join("default.json")).unwrap();
        let mut v8 = serde_json::json!({"config_version": 8, "active_profile_id": "default"});
        migrate_v8_to_v9(&config_dir8, &mut v8).unwrap();

        let backups8 = list_backup_dirs(&config_dir8, ".backup.v8.");
        assert_eq!(backups8.len(), 1, "应产生恰好一个 v8 备份目录");
        assert_eq!(
            std::fs::read_to_string(backups8[0].join("default.json")).unwrap(),
            original8,
            "v8 备份必须是改写前原文"
        );
        assert!(!original8.contains("\"active_task\""), "原文应无任务绑定");
    }

    /// 迁移完成后幂等重跑为 no-op：不改写文件、不产生新备份、不重复建任务
    #[test]
    fn test_migrate_rerun_after_success_is_noop() {
        // v9→v10：首轮已绑定并清掉内联键，重跑不应再改写任何文件
        let tmp = v9_fixture("default", "http://10.0.0.1/login", None);
        let config_dir = tmp.path().join("config");
        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});
        migrate_v9_to_v10(&config_dir, &mut v).unwrap();
        let profile_after =
            std::fs::read_to_string(config_dir.join("profiles").join("default.json")).unwrap();
        let task_after =
            std::fs::read_to_string(tmp.path().join("tasks").join("http").join("default.json"))
                .unwrap();
        let backup_count = list_backup_dirs(&config_dir, ".backup.v9.").len();

        let mut v = serde_json::json!({"config_version": 9, "active_profile_id": "default"});
        migrate_v9_to_v10(&config_dir, &mut v).unwrap();

        assert_eq!(
            std::fs::read_to_string(config_dir.join("profiles").join("default.json")).unwrap(),
            profile_after,
            "重跑不得改写方案文件"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tasks").join("http").join("default.json"))
                .unwrap(),
            task_after,
            "重跑不得改写已生成的任务"
        );
        assert_eq!(
            list_backup_dirs(&config_dir, ".backup.v9.").len(),
            backup_count,
            "重跑未发生改写，不得产生新备份"
        );
        assert!(
            !tmp.path()
                .join("tasks")
                .join("http")
                .join("default-2.json")
                .exists()
        );

        // v8→v9：首轮已绑定，重跑不覆盖、不备份
        let tmp8 = v8_fixture(Some("mytask"), None);
        let config_dir8 = tmp8.path().join("config");
        let mut v8 = serde_json::json!({"config_version": 8, "active_profile_id": "default"});
        migrate_v8_to_v9(&config_dir8, &mut v8).unwrap();
        let profile8_after =
            std::fs::read_to_string(config_dir8.join("profiles").join("default.json")).unwrap();
        let backup8_count = list_backup_dirs(&config_dir8, ".backup.v8.").len();

        let mut v8 = serde_json::json!({"config_version": 8, "active_profile_id": "default"});
        migrate_v8_to_v9(&config_dir8, &mut v8).unwrap();

        assert_eq!(
            std::fs::read_to_string(config_dir8.join("profiles").join("default.json")).unwrap(),
            profile8_after,
            "重跑不得改写方案文件"
        );
        assert_eq!(
            list_backup_dirs(&config_dir8, ".backup.v8.").len(),
            backup8_count,
            "重跑未发生改写，不得产生新备份"
        );
    }

    // ============ 属性测试（proptest）：迁移链全输入域不变量（T4-2b） ============
    //
    // 范式与 src/login/session.rs / src/bridge/process.rs 的既有属性测试一致：
    // 512 cases、回归文件自动落 proptest-regressions/config/migration.txt、
    // 随机失败必可 shrink 复现。
    //
    // 生成器覆盖面（合法旧版本配置域）：
    // - 起始版本 1..=9（权重偏向迁移最密集的 v5 / v9）
    // - 方案 0..=3 条：合法 id 池（default/dorm/lab，去重防撞）+ start≤5 时偶发
    //   非法 id（con / we/ird / a b，覆盖 Windows 保留名与路径穿越的「跳过」分支）；
    //   字段含 v5 旧名（carrier / match_gateway_ip / match_ssid / carrier_custom）、
    //   新旧键并存的改名冲突（旧值被丢弃）、v9 内联直连的「全套 / 全空 / 仅证书
    //   策略」三态
    // - 全局段按起始版本渲染 v5 旧名或 v6+ 现名；已知键可选存在（缺省 / 全键齐）、
    //   v5 废弃键混入、未知键混入（settings 顶层与 monitor 段内）
    // - 磁盘夹具：tasks/.order.json active（驱动 v8→v9 绑定）、浏览器任务占名
    //   （驱动 v9→v10 换 -2 后缀）、用户自建直连任务占名（全套配置让位 -2，
    //   仅证书策略时内容一致复用）
    //
    // 产线口径下「截断 JSON」在 run_migrations 上游即被隔离（service.rs 先按
    // Value 解析 settings.json，解析失败走 corrupt 隔离备份，不会进入迁移），
    // 故截断类畸形由下方畸形输入属性以根形状 / 类型错位 / profiles 形状 /
    // 深嵌套 / 非法版本号形态覆盖。

    use proptest::prelude::*;
    use serde_json::json;

    /// 属性测试统一口径：512 cases（CI 亲和；随机失败必可 shrink 复现）
    fn prop_config() -> proptest::test_runner::Config {
        proptest::test_runner::Config::with_cases(512)
    }

    /// [`prop_assert_eq!`] 的借用版：serde_json 索引与构造表达式统一按引用比较。
    /// prop_assert_eq 宏按值绑定参数（`let left = $left;`），而 `Value` 非 Copy，
    /// 索引表达式（解引用 `Index::index` 的返回）不可按值移出。
    macro_rules! prop_json_eq {
        ($left:expr, $right:expr $(,)?) => {
            prop_assert_eq!(&$left, &$right)
        };
        ($left:expr, $right:expr, $fmt:tt $($args:tt)*) => {
            prop_assert_eq!(&$left, &$right, $fmt $($args)*)
        };
    }

    /// v6→v7 迁移的新旧更新源（与 migrate_v6_to_v7 内部常量同值，供断言对照）
    const PROP_OLD_SOURCE_URL: &str =
        "https://api.github.com/repos/Misyra/Campus-Auth/releases/latest";
    const PROP_NEW_SOURCE_URL: &str =
        "https://api.github.com/repos/Misyra/Campus-Auth-rs/releases/latest";

    /// carrier 字段形态：缺省 / 仅旧名 / 新旧并存（改名遇同名新键时旧值被丢弃）
    #[derive(Debug, Clone)]
    enum PropCarrierSeed {
        Absent,
        Old(String),
        Both(String, String),
    }

    /// v9 内联直连配置形态：未配置（全空键）/ 仅证书策略显式 / 全套
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum PropHttpMode {
        None,
        IgnoreOnly,
        Full,
    }

    /// v9 内联直连字段种子（迁移按「取值」而非「键存在」判定是否配过）
    #[derive(Debug, Clone)]
    struct PropHttpSeed {
        mode: PropHttpMode,
        method: String,
        url: String,
        headers: String,
        body: String,
        success: String,
        failure: String,
        crypto: String,
        ignore: Option<bool>,
    }

    impl PropHttpSeed {
        /// 渲染为方案 JSON 的 http_* 键值（键恒全量存在，与 `#[serde(default)]`
        /// 落盘形态一致——即便没配直连也写着这些键）
        fn render_map(&self) -> Vec<(&'static str, Value)> {
            vec![
                ("http_method", Value::String(self.method.clone())),
                ("http_url", Value::String(self.url.clone())),
                ("http_headers", Value::String(self.headers.clone())),
                ("http_body", Value::String(self.body.clone())),
                ("http_success_pattern", Value::String(self.success.clone())),
                ("http_failure_pattern", Value::String(self.failure.clone())),
                ("http_crypto_script", Value::String(self.crypto.clone())),
                (
                    "http_ignore_https_errors",
                    self.ignore.map(Value::Bool).unwrap_or(Value::Null),
                ),
            ]
        }

        /// 是否会被迁移判定为「配过直连」（与 [`legacy_http_config_present`] 同口径）
        fn legacy_present(&self) -> bool {
            match self.mode {
                PropHttpMode::Full | PropHttpMode::IgnoreOnly => true,
                PropHttpMode::None => false,
            }
        }
    }

    /// 方案种子（渲染为 start≤5 的内联 v5 形态或 start≥6 的磁盘 v6+ 形态）
    #[derive(Debug, Clone)]
    struct PropProfileSeed {
        name: String,
        username: String,
        password: String,
        carrier: PropCarrierSeed,
        gateway_ip: Option<String>,
        wifi_ssid: Option<String>,
        custom: bool,
        http: PropHttpSeed,
    }

    impl PropProfileSeed {
        /// start≤5：v5 旧名内联形态（拆分前 settings.profiles 字典的值）
        fn render_inline_v5(&self) -> Value {
            let mut obj = serde_json::Map::new();
            obj.insert("name".into(), json!(self.name));
            obj.insert("username".into(), json!(self.username));
            obj.insert("password".into(), json!(self.password));
            match &self.carrier {
                PropCarrierSeed::Absent => {}
                PropCarrierSeed::Old(v) => {
                    obj.insert("carrier".into(), json!(v));
                }
                PropCarrierSeed::Both(old, new) => {
                    obj.insert("carrier".into(), json!(old));
                    obj.insert("isp".into(), json!(new));
                }
            }
            if let Some(g) = &self.gateway_ip {
                obj.insert("match_gateway_ip".into(), json!(g));
            }
            if let Some(w) = &self.wifi_ssid {
                obj.insert("match_ssid".into(), json!(w));
            }
            if self.custom {
                obj.insert("carrier_custom".into(), json!("旧版自定义运营商文本"));
            }
            for (k, v) in self.http.render_map() {
                obj.insert(k.into(), v);
            }
            Value::Object(obj)
        }

        /// start≥6：v6+ 磁盘文件形态（新名 + id；改名类迁移早已完成）
        fn render_file_v6(&self, id: &str) -> Value {
            let mut obj = serde_json::Map::new();
            obj.insert("id".into(), json!(id));
            obj.insert("name".into(), json!(self.name));
            obj.insert("username".into(), json!(self.username));
            obj.insert("password".into(), json!(self.password));
            match &self.carrier {
                PropCarrierSeed::Absent => {}
                PropCarrierSeed::Old(v) => {
                    obj.insert("isp".into(), json!(v));
                }
                PropCarrierSeed::Both(_, new) => {
                    obj.insert("isp".into(), json!(new));
                }
            }
            if let Some(g) = &self.gateway_ip {
                obj.insert("gateway_ip".into(), json!(g));
            }
            if let Some(w) = &self.wifi_ssid {
                obj.insert("wifi_ssid".into(), json!(w));
            }
            for (k, v) in self.http.render_map() {
                obj.insert(k.into(), v);
            }
            Value::Object(obj)
        }
    }

    /// monitor 段种子（start≤5 渲染 v5 旧名交由迁移改名/拆分；start≥6 渲染现名恒等保留）
    #[derive(Debug, Clone)]
    struct PropMonitorSeed {
        check_interval: Option<u32>,
        enable_tcp: Option<bool>,
        enable_http: Option<bool>,
        enable_local: Option<bool>,
        ping_targets: Option<Vec<String>>,
        test_urls: Option<Vec<String>>,
        /// url_check_urls 条目：(url 部分, Option<期望响应>)；url 为空串渲染 "|e"
        /// （迁移按空 URL 整条丢弃）
        url_items: Option<Vec<(String, Option<String>)>>,
        deprecated: bool,
        junk: bool,
    }

    impl PropMonitorSeed {
        fn render(&self, start: u32) -> Value {
            let mut m = serde_json::Map::new();
            if start <= 5 {
                if let Some(v) = self.check_interval {
                    m.insert("check_interval_seconds".into(), json!(v));
                }
                if let Some(v) = self.enable_tcp {
                    m.insert("enable_tcp_check".into(), json!(v));
                }
                if let Some(v) = self.enable_http {
                    m.insert("enable_http_check".into(), json!(v));
                }
                if let Some(v) = self.enable_local {
                    m.insert("enable_local_check".into(), json!(v));
                }
                if let Some(v) = &self.ping_targets {
                    m.insert("ping_targets".into(), json!(v));
                }
                if let Some(v) = &self.test_urls {
                    m.insert("test_urls".into(), json!(v));
                }
                if let Some(items) = &self.url_items {
                    m.insert(
                        "url_check_urls".into(),
                        Value::Array(
                            items
                                .iter()
                                .map(|(u, e)| {
                                    Value::String(match e {
                                        Some(exp) => format!("{u}|{exp}"),
                                        None => u.clone(),
                                    })
                                })
                                .collect(),
                        ),
                    );
                }
                if self.deprecated {
                    m.insert("access_log".into(), json!(true));
                    m.insert("block_proxy".into(), json!(false));
                    m.insert("network_check_timeout".into(), json!(5));
                    m.insert("check_auth_url".into(), json!(true));
                    m.insert("auth_url_targets".into(), json!([]));
                }
            } else {
                // v6+ 现名直渲（url 拆分规则与迁移派生一致）
                if let Some(v) = self.check_interval {
                    m.insert("check_interval".into(), json!(v));
                }
                if let Some(v) = self.enable_tcp {
                    m.insert("tcp_enabled".into(), json!(v));
                }
                if let Some(v) = self.enable_http {
                    m.insert("http_enabled".into(), json!(v));
                }
                if let Some(v) = &self.ping_targets {
                    m.insert("tcp_targets".into(), json!(v));
                }
                if let Some(v) = &self.test_urls {
                    m.insert("http_targets".into(), json!(v));
                }
                if let Some(items) = &self.url_items {
                    let mut targets = Vec::new();
                    let mut expected = serde_json::Map::new();
                    for (u, e) in items {
                        let u = u.trim();
                        if u.is_empty() {
                            continue;
                        }
                        targets.push(Value::String(u.to_owned()));
                        if let Some(exp) = e {
                            expected.insert(u.to_owned(), Value::String(exp.trim().to_owned()));
                        }
                    }
                    // 与迁移派生同口径：目标列表非空即启用
                    let enabled = !targets.is_empty();
                    m.insert("url_targets".into(), Value::Array(targets));
                    m.insert("url_expected_responses".into(), Value::Object(expected));
                    m.insert("url_enabled".into(), json!(enabled));
                }
            }
            if self.junk {
                m.insert("future_probe".into(), json!(1));
            }
            Value::Object(m)
        }
    }

    #[derive(Debug, Clone)]
    struct PropLoggingSeed {
        retention: Option<u32>,
        junk: bool,
    }

    impl PropLoggingSeed {
        fn render(&self, start: u32) -> Value {
            let mut m = serde_json::Map::new();
            m.insert("level".into(), json!("INFO"));
            m.insert("file_enabled".into(), json!(true));
            if start <= 5 {
                if let Some(v) = self.retention {
                    m.insert("log_retention_days".into(), json!(v));
                }
            } else if let Some(v) = self.retention {
                m.insert("retention_days".into(), json!(v));
            }
            if self.junk {
                m.insert("future_log".into(), json!(true));
            }
            Value::Object(m)
        }
    }

    #[derive(Debug, Clone)]
    struct PropAppSeed {
        port: Option<u16>,
        auto_open_browser: Option<bool>,
        deprecated: bool,
        junk: bool,
    }

    impl PropAppSeed {
        fn render(&self, start: u32) -> Value {
            let mut m = serde_json::Map::new();
            m.insert("runtime_mode".into(), json!("full"));
            if start <= 5 {
                if let Some(v) = self.port {
                    m.insert("app_port".into(), json!(v));
                }
                if let Some(v) = self.auto_open_browser {
                    m.insert("auto_open_browser".into(), json!(v));
                }
                if self.deprecated {
                    m.insert("shell_path".into(), json!("cmd"));
                    m.insert("lightweight_tray".into(), json!(false));
                    m.insert("minimize_to_tray".into(), json!(true));
                    m.insert("proxy".into(), Value::Null);
                }
            } else {
                if let Some(v) = self.port {
                    m.insert("port".into(), json!(v));
                }
                if let Some(v) = self.auto_open_browser {
                    m.insert("auto_start_browser".into(), json!(v));
                }
            }
            if self.junk {
                m.insert("future_app".into(), json!(1));
            }
            Value::Object(m)
        }
    }

    /// v6→v7 的更新源形态
    #[derive(Debug, Clone, PartialEq)]
    enum PropSourceUrl {
        Old,
        New,
        Custom,
    }

    impl PropSourceUrl {
        fn render(&self) -> &'static str {
            match self {
                PropSourceUrl::Old => PROP_OLD_SOURCE_URL,
                PropSourceUrl::New => PROP_NEW_SOURCE_URL,
                PropSourceUrl::Custom => "https://mirror.example.com/releases/latest",
            }
        }
    }

    /// v7→v8 的浏览器渠道形态
    #[derive(Debug, Clone, PartialEq)]
    enum PropChannelSeed {
        Playwright,
        Chromium,
        Msedge,
    }

    impl PropChannelSeed {
        fn render(&self) -> &'static str {
            match self {
                PropChannelSeed::Playwright => "playwright",
                PropChannelSeed::Chromium => "chromium",
                PropChannelSeed::Msedge => "msedge",
            }
        }
    }

    /// 一个迁移属性用例：起始版本 + 渲染好的 settings.json + 语义种子与磁盘夹具
    #[derive(Debug, Clone)]
    struct PropMigrationCase {
        start_version: u32,
        settings: Value,
        /// 合法 id 的方案种子（断言用，首个即活跃方案）；start≥6 时同序预写磁盘
        profiles: Vec<(String, PropProfileSeed)>,
        /// start≤5 内联字典中的非法 id（迁移跳过、不写文件）
        inline_invalid_ids: Vec<String>,
        /// start≤5 是否携带 profiles 键（false = 键整体缺省）
        has_profiles_key: bool,
        /// settings 顶层是否混入未知键
        top_junk: bool,
        monitor: PropMonitorSeed,
        logging: PropLoggingSeed,
        app: PropAppSeed,
        source: PropSourceUrl,
        channel: PropChannelSeed,
        /// tasks/.order.json 的 active（Some 时驱动 v8→v9 绑定）
        order_active: Option<String>,
        /// 用浏览器任务占住首个方案 id（v9→v10 须让位 -2）
        browser_conflict: bool,
        /// 用用户自建直连任务占住首个方案 id（全套配置让位 -2；仅证书策略时复用）
        http_conflict: bool,
    }

    /// 合法方案 id 池 + start≤5 专属非法 id 池
    const PROP_VALID_IDS: [&str; 3] = ["default", "dorm", "lab"];
    const PROP_INVALID_IDS: [&str; 3] = ["con", "we/ird", "a b"];

    fn prop_arb_profile_seed() -> impl proptest::strategy::Strategy<Value = PropProfileSeed> {
        (
            prop_oneof![
                Just("宿舍".to_owned()),
                Just("lab".to_owned()),
                Just(String::new()),
            ],
            prop_oneof![Just("20230001".to_owned()), Just("stu_x".to_owned())],
            prop_oneof![
                Just("ENC:seed-pwd".to_owned()),
                Just("plain-pwd".to_owned())
            ],
            proptest::option::of(prop_oneof![
                Just("移动".to_owned()),
                Just("cmcc".to_owned()),
                Just(String::new()),
            ]),
            any::<bool>(),
            proptest::option::of(Just("192.168.1.1".to_owned())),
            proptest::option::of(Just("campus-ssid".to_owned())),
            any::<bool>(),
            proptest::option::of(any::<bool>()),
            0u8..=3,
            0u8..=2,
        )
            .prop_map(
                |(
                    name,
                    username,
                    password,
                    carrier_opt,
                    both,
                    gateway,
                    wifi,
                    custom,
                    ignore,
                    method_idx,
                    mode_idx,
                )| {
                    let method = ["POST", "GET", "post", "PUT"][method_idx as usize].to_owned();
                    let url_pool = [
                        "http://10.0.0.1/login",
                        "http://172.16.0.1/sportal",
                        "https://campus.edu.cn/auth",
                    ];
                    let mode = match mode_idx {
                        0 => PropHttpMode::None,
                        1 => PropHttpMode::IgnoreOnly,
                        _ => PropHttpMode::Full,
                    };
                    let http = match mode {
                        // 未配置：键恒存在但全为空值（真实存量形态）
                        PropHttpMode::None => PropHttpSeed {
                            mode,
                            method,
                            url: String::new(),
                            headers: String::new(),
                            body: String::new(),
                            success: String::new(),
                            failure: String::new(),
                            crypto: String::new(),
                            ignore: None,
                        },
                        // 配到一半（只显式选了证书策略）：按取值判定必须搬移
                        PropHttpMode::IgnoreOnly => PropHttpSeed {
                            mode,
                            method,
                            url: String::new(),
                            headers: String::new(),
                            body: String::new(),
                            success: String::new(),
                            failure: String::new(),
                            crypto: String::new(),
                            ignore: Some(ignore.unwrap_or(true)),
                        },
                        PropHttpMode::Full => PropHttpSeed {
                            mode,
                            method,
                            url: url_pool[method_idx as usize % url_pool.len()].to_owned(),
                            headers: "Content-Type: application/x-www-form-urlencoded".to_owned(),
                            body: "u={username}&p={password}".to_owned(),
                            success: "登录成功".to_owned(),
                            failure: "密码错误".to_owned(),
                            crypto: "function transform(ctx) { return {}; }".to_owned(),
                            ignore,
                        },
                    };
                    let carrier = match carrier_opt {
                        None => PropCarrierSeed::Absent,
                        Some(v) if both => PropCarrierSeed::Both("旧 carrier 值".to_owned(), v),
                        Some(v) => PropCarrierSeed::Old(v),
                    };
                    PropProfileSeed {
                        name,
                        username,
                        password,
                        carrier,
                        gateway_ip: gateway,
                        wifi_ssid: wifi,
                        custom,
                        http,
                    }
                },
            )
    }

    fn prop_arb_monitor_seed() -> impl proptest::strategy::Strategy<Value = PropMonitorSeed> {
        (
            proptest::option::of(20u32..=1200),
            proptest::option::of(any::<bool>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(proptest::collection::vec(
                prop_oneof![Just("10.0.0.53:53".to_owned()), Just("8.8.8.8".to_owned())],
                0..=2,
            )),
            proptest::option::of(proptest::collection::vec(
                Just("http://probe.example.com".to_owned()),
                0..=2,
            )),
            proptest::option::of(proptest::collection::vec(
                (
                    prop_oneof![
                        Just("http://a.com".to_owned()),
                        Just("http://b.com/x".to_owned()),
                        Just(String::new()),
                    ],
                    proptest::option::of(prop_oneof![
                        Just("OK".to_owned()),
                        Just("成功|备用".to_owned()),
                    ]),
                ),
                0..=3,
            )),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(
                |(
                    check_interval,
                    enable_tcp,
                    enable_http,
                    enable_local,
                    ping,
                    test,
                    urls,
                    deprecated,
                    junk,
                )| {
                    PropMonitorSeed {
                        check_interval,
                        enable_tcp,
                        enable_http,
                        enable_local,
                        ping_targets: ping,
                        test_urls: test,
                        url_items: urls,
                        deprecated,
                        junk,
                    }
                },
            )
    }

    fn prop_arb_migration_case() -> impl proptest::strategy::Strategy<Value = PropMigrationCase> {
        (
            (
                prop_oneof![
                    3 => Just(5u32),
                    3 => Just(9u32),
                    1 => Just(1u32),
                    1 => Just(6u32),
                    1 => Just(7u32),
                    1 => Just(8u32),
                    1 => 2u32..=4,
                ],
                proptest::collection::vec((0u8..=2, prop_arb_profile_seed()), 0..=3),
            ),
            (
                (
                    prop_arb_monitor_seed(),
                    proptest::option::of(1u32..=90),
                    any::<bool>(),
                    proptest::option::of(1u16..=65535),
                    proptest::option::of(any::<bool>()),
                    any::<bool>(),
                    any::<bool>(),
                ),
                (
                    prop_oneof![
                        Just(PropSourceUrl::Old),
                        Just(PropSourceUrl::New),
                        Just(PropSourceUrl::Custom),
                    ],
                    prop_oneof![
                        Just(PropChannelSeed::Playwright),
                        Just(PropChannelSeed::Chromium),
                        Just(PropChannelSeed::Msedge),
                    ],
                    proptest::option::of(prop_oneof![
                        Just("t1".to_owned()),
                        Just("mytask".to_owned()),
                    ]),
                    any::<bool>(),
                    any::<bool>(),
                    any::<bool>(),
                    any::<bool>(),
                ),
            ),
        )
            .prop_map(
                |(
                    (start, profile_specs),
                    (
                        (
                            monitor,
                            retention,
                            logging_junk,
                            port,
                            auto_open,
                            app_deprecated,
                            app_junk,
                        ),
                        (
                            source,
                            channel,
                            order_active,
                            browser_conflict,
                            http_conflict,
                            has_profiles_key,
                            top_junk,
                        ),
                    ),
                )| {
                    // 组装去重后的方案 id + 种子（id 撞名加序号后缀，保证唯一）
                    let mut profiles: Vec<(String, PropProfileSeed)> = Vec::new();
                    let mut used = std::collections::HashSet::new();
                    for (idx, seed) in profile_specs {
                        let base = PROP_VALID_IDS[idx as usize];
                        let mut id = base.to_owned();
                        let mut n = 2;
                        while !used.insert(id.clone()) {
                            id = format!("{base}-{n}");
                            n += 1;
                        }
                        profiles.push((id, seed));
                    }
                    // start≥6 不写非法 id 文件；start≤5 以内联字典混入非法 id
                    let inline_invalid_ids = if start <= 5 && profiles.len() < 3 {
                        vec![PROP_INVALID_IDS[profiles.len() % PROP_INVALID_IDS.len()].to_owned()]
                    } else {
                        Vec::new()
                    };
                    // 活跃方案恒取首个合法 id（无方案时 default）
                    let active = profiles
                        .first()
                        .map(|(id, _)| id.clone())
                        .unwrap_or_else(|| "default".to_owned());

                    let mut root = serde_json::Map::new();
                    root.insert("config_version".into(), json!(start));
                    root.insert("active_profile_id".into(), json!(active));
                    root.insert("auto_switch".into(), json!(true));
                    let mut global = serde_json::Map::new();
                    global.insert("monitor".into(), monitor.render(start));
                    let logging = PropLoggingSeed {
                        retention,
                        junk: logging_junk,
                    };
                    global.insert("logging".into(), logging.render(start));
                    let app = PropAppSeed {
                        port,
                        auto_open_browser: auto_open,
                        deprecated: app_deprecated,
                        junk: app_junk,
                    };
                    global.insert("app".into(), app.render(start));
                    global.insert(
                        "updater".into(),
                        json!({"release_source_url": source.render(), "auto_check_enabled": true}),
                    );
                    global.insert(
                        "browser".into(),
                        json!({"browser_channel": channel.render(), "headless": true}),
                    );
                    root.insert("global".into(), Value::Object(global));
                    if start <= 5 && has_profiles_key {
                        let mut dict = serde_json::Map::new();
                        for (id, seed) in &profiles {
                            let rendered = seed.render_inline_v5();
                            // 生成器漂移防线：渲染产物必须与迁移「配过直连」判定一致
                            assert_eq!(
                                legacy_http_config_present(&rendered),
                                seed.http.legacy_present(),
                                "种子直连形态与 legacy_http_config_present 口径漂移（生成器缺陷）"
                            );
                            dict.insert(id.clone(), rendered);
                        }
                        for invalid in &inline_invalid_ids {
                            dict.insert(
                                invalid.clone(),
                                json!({"name": "非法 id 占位", "password": "ENC:x"}),
                            );
                        }
                        root.insert("profiles".into(), Value::Object(dict));
                    }
                    if top_junk {
                        root.insert("legacy_top".into(), json!(true));
                    }

                    PropMigrationCase {
                        start_version: start,
                        settings: Value::Object(root),
                        profiles,
                        inline_invalid_ids,
                        has_profiles_key,
                        top_junk,
                        monitor,
                        logging,
                        app,
                        source,
                        channel,
                        order_active,
                        browser_conflict,
                        http_conflict,
                    }
                },
            )
    }

    impl PropMigrationCase {
        /// 预写磁盘夹具：方案文件仅 start≥6 需要（start≤5 由 v5→v6 拆分生成）；
        /// tasks 夹具按需创建（.order.json / 浏览器任务占名 / 用户直连任务占名）
        fn setup_disk(&self, config_dir: &Path) {
            let base = config_dir.parent().unwrap_or(config_dir);
            if self.start_version >= 6 {
                let profiles_dir = config_dir.join("profiles");
                std::fs::create_dir_all(&profiles_dir).unwrap();
                for (id, seed) in &self.profiles {
                    let rendered = seed.render_file_v6(id);
                    // 生成器漂移防线（同内联形态）
                    assert_eq!(
                        legacy_http_config_present(&rendered),
                        seed.http.legacy_present(),
                        "种子直连形态与 legacy_http_config_present 口径漂移（生成器缺陷）"
                    );
                    std::fs::write(
                        profiles_dir.join(format!("{id}.json")),
                        rendered.to_string(),
                    )
                    .unwrap();
                }
            }
            let conflict_target = self.profiles.first().map(|(id, _)| id.clone());
            let needs_tasks = self.order_active.is_some()
                || (self.browser_conflict || self.http_conflict) && conflict_target.is_some();
            if needs_tasks {
                std::fs::create_dir_all(base.join("tasks")).unwrap();
            }
            if let Some(active) = &self.order_active {
                std::fs::write(
                    base.join("tasks").join(".order.json"),
                    format!(r#"{{"order":["t1"],"active":"{active}"}}"#),
                )
                .unwrap();
            }
            if let Some(id) = conflict_target {
                if self.browser_conflict {
                    std::fs::create_dir_all(base.join("tasks").join("browser")).unwrap();
                    std::fs::write(
                        base.join("tasks")
                            .join("browser")
                            .join(format!("{id}.json")),
                        r#"{"type":"browser","task_id":"x","name":"内置浏览器任务"}"#,
                    )
                    .unwrap();
                }
                if self.http_conflict {
                    std::fs::create_dir_all(base.join("tasks").join("http")).unwrap();
                    std::fs::write(
                        base.join("tasks").join("http").join(format!("{id}.json")),
                        r#"{"type":"http","task_id":"x","name":"用户自建"}"#,
                    )
                    .unwrap();
                }
            }
        }
    }

    /// 列出目录树下残留的 .tmp_ 临时文件名（原子写失败必须无残留）
    fn prop_tmp_residue(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                out.extend(prop_tmp_residue(&path));
            } else if name.starts_with(crate::config::TMP_PREFIX) {
                out.push(name);
            }
        }
        out
    }

    /// 任务 id 分配预言机：按 [`migrate_v9_to_v10`] 的真实分配口径推演每个
    /// 「配过直连」方案最终绑定的任务 id——方案文件名排序遍历、预占三桶
    /// （browser/scripts/http 磁盘夹具）、内容一致复用、-2..-20 递增让位。
    ///
    /// 任务 id 与方案 id 分属不同命名空间（tasks/http/ vs profiles/），前序方案
    /// 的 -2 后缀产物可与后续方案 id 撞名（级联让位），故断言必须按本预言机
    /// 的分配结果而非「方案 id ± 固定后缀」对照。
    ///
    /// 返回：(方案 id → 任务 id、迁移后 http 桶的全部文件名、本轮新建的文件名)。
    fn prop_alloc_http_task_ids(
        case: &PropMigrationCase,
    ) -> (
        std::collections::HashMap<String, String>,
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    ) {
        let first_id = case.profiles.first().map(|(id, _)| id.clone());
        let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut files: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        // 磁盘夹具预占：浏览器任务占名 / 用户自建直连任务占名
        if let Some(id) = &first_id {
            if case.browser_conflict {
                taken.insert(id.clone());
            }
            if case.http_conflict {
                taken.insert(id.clone());
                files.insert(
                    id.clone(),
                    serde_json::json!({"type":"http","task_id":"x","name":"用户自建"}),
                );
            }
        }
        let mut alloc = std::collections::HashMap::new();
        let mut created_files = std::collections::HashSet::new();
        // 迁移按方案文件**路径**排序遍历（`lab-2.json` < `lab.json`，'-' < '.'），
        // 前序产物的文件名参与后续方案的让位判定——排序键是文件名而非方案 id
        let mut ordered: Vec<&(String, PropProfileSeed)> = case.profiles.iter().collect();
        ordered.sort_by(|a, b| {
            let fa = format!("{}.json", a.0);
            let fb = format!("{}.json", b.0);
            fa.cmp(&fb)
        });
        for (id, seed) in ordered {
            if !seed.http.legacy_present() {
                continue;
            }
            // 内容一致复用：tasks/http/<id>.json 已存在且六字段与方案内联值一致
            //（含前序方案在本轮写出的产物，口径同 reusable_existing_task）
            if let Some(existing) = files.get(id) {
                let same = |task_key: &str, seed_value: &str| {
                    existing.get(task_key).and_then(Value::as_str).unwrap_or("") == seed_value
                };
                if same("url", &seed.http.url)
                    && same("headers", &seed.http.headers)
                    && same("body", &seed.http.body)
                    && same("success_pattern", &seed.http.success)
                    && same("failure_pattern", &seed.http.failure)
                    && same("crypto_script", &seed.http.crypto)
                {
                    alloc.insert(id.clone(), id.clone());
                    continue;
                }
            }
            let chosen = if !taken.contains(id) {
                id.clone()
            } else {
                (2u32..=20)
                    .map(|n| format!("{id}-{n}"))
                    .find(|c| !taken.contains(c))
                    .expect("生成器方案数 ≤ 3，不会耗尽 -2..-20 后缀")
            };
            taken.insert(chosen.clone());
            // 记录本轮新建任务的产物内容（供后续方案的内容一致复用判定）
            let rendered = seed.render_inline_v5();
            files.insert(chosen.clone(), build_legacy_http_task(&chosen, &rendered));
            created_files.insert(chosen.clone());
            alloc.insert(id.clone(), chosen);
        }
        let all_files: std::collections::HashSet<String> = files.into_keys().collect();
        (alloc, all_files, created_files)
    }

    /// 收集目录树下全部文件内容（相对路径 → 字节），供幂等重跑前后对照
    fn prop_snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let rel = format!("{prefix}{name}");
                if path.is_dir() {
                    walk(&path, &format!("{rel}/"), out);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.push((rel, bytes));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, "", &mut out);
        out.sort();
        out
    }

    /// Ok 路径的关键字段保全断言（④）：逐版本按迁移函数语义白名单对照
    fn prop_assert_fields_preserved(
        case: &PropMigrationCase,
        config_dir: &Path,
        out: &Value,
    ) -> Result<(), proptest::test_runner::TestCaseError> {
        let start = case.start_version;
        let base = config_dir.parent().unwrap_or(config_dir);

        // 顶层：profiles 键必须移除；未知顶层键原样保留（去留由 schema 决定）
        prop_assert!(
            out.get("profiles").is_none(),
            "迁移后 settings 不得残留 profiles 键"
        );
        prop_assert_eq!(
            out.get("legacy_top").is_some(),
            case.top_junk,
            "未知顶层键必须原样保留"
        );

        // ---- monitor 段 ----
        let mon = &out["global"]["monitor"];
        if start <= 5 {
            // 改名保全：旧名移除、新名值恒等；缺省键不凭空出现
            match case.monitor.check_interval {
                Some(v) => {
                    prop_json_eq!(
                        mon["check_interval"],
                        json!(v),
                        "check_interval_seconds → check_interval 保全"
                    );
                    prop_assert!(mon.get("check_interval_seconds").is_none(), "旧名必须移除");
                }
                None => prop_assert!(mon.get("check_interval").is_none(), "缺省键不得凭空出现"),
            }
            match case.monitor.enable_tcp {
                Some(v) => {
                    prop_json_eq!(
                        mon["tcp_enabled"],
                        json!(v),
                        "enable_tcp_check → tcp_enabled 保全"
                    );
                    prop_assert!(mon.get("enable_tcp_check").is_none());
                }
                None => prop_assert!(mon.get("tcp_enabled").is_none()),
            }
            match case.monitor.enable_http {
                Some(v) => {
                    prop_json_eq!(
                        mon["http_enabled"],
                        json!(v),
                        "enable_http_check → http_enabled 保全"
                    );
                    prop_assert!(mon.get("enable_http_check").is_none());
                }
                None => prop_assert!(mon.get("http_enabled").is_none()),
            }
            match case.monitor.enable_local {
                Some(v) => {
                    prop_json_eq!(
                        mon["local_check_enabled"],
                        json!(v),
                        "enable_local_check → local_check_enabled 保全"
                    );
                    prop_assert!(mon.get("enable_local_check").is_none());
                }
                None => prop_assert!(mon.get("local_check_enabled").is_none()),
            }
            match &case.monitor.ping_targets {
                Some(v) => {
                    prop_json_eq!(
                        mon["tcp_targets"],
                        json!(v),
                        "ping_targets → tcp_targets 保全"
                    );
                    prop_assert!(mon.get("ping_targets").is_none());
                }
                None => prop_assert!(mon.get("tcp_targets").is_none()),
            }
            match &case.monitor.test_urls {
                Some(v) => {
                    prop_json_eq!(
                        mon["http_targets"],
                        json!(v),
                        "test_urls → http_targets 保全"
                    );
                    prop_assert!(mon.get("test_urls").is_none());
                }
                None => prop_assert!(mon.get("http_targets").is_none()),
            }
            // v5 url|expected 整串拆分：目标数组、期望映射、非空即启用派生
            match &case.monitor.url_items {
                Some(items) => {
                    let mut targets = Vec::new();
                    let mut expected = serde_json::Map::new();
                    for (u, e) in items {
                        let u = u.trim();
                        if u.is_empty() {
                            continue;
                        }
                        targets.push(Value::String(u.to_owned()));
                        if let Some(exp) = e {
                            expected.insert(u.to_owned(), Value::String(exp.trim().to_owned()));
                        }
                    }
                    prop_json_eq!(
                        mon["url_targets"],
                        Value::Array(targets.clone()),
                        "url_check_urls → url_targets 拆分"
                    );
                    prop_json_eq!(
                        mon["url_expected_responses"],
                        Value::Object(expected),
                        "url_check_urls → url_expected_responses 拆分"
                    );
                    prop_json_eq!(
                        mon["url_enabled"],
                        json!(!targets.is_empty()),
                        "v5 语义：列表非空即启用"
                    );
                }
                None => {
                    prop_assert!(mon.get("url_targets").is_none());
                    prop_assert!(mon.get("url_expected_responses").is_none());
                    prop_assert!(mon.get("url_enabled").is_none());
                }
            }
            if case.monitor.deprecated {
                for f in [
                    "access_log",
                    "block_proxy",
                    "network_check_timeout",
                    "check_auth_url",
                    "auth_url_targets",
                ] {
                    prop_assert!(mon.get(f).is_none(), "废弃字段 {f} 必须删除");
                }
            }
            prop_assert_eq!(
                mon.get("future_probe").is_some(),
                case.monitor.junk,
                "monitor 段未知键原样保留"
            );
        } else {
            // start≥6：迁移链不触碰 monitor，渲染值恒等保留
            prop_assert_eq!(
                mon,
                &case.monitor.render(start),
                "start≥6 时 monitor 段不得被改动"
            );
        }

        // ---- logging / app 段 ----
        if start <= 5 {
            let log = &out["global"]["logging"];
            match case.logging.retention {
                Some(v) => {
                    prop_json_eq!(
                        log["retention_days"],
                        json!(v),
                        "log_retention_days → retention_days 保全"
                    );
                    prop_assert!(log.get("log_retention_days").is_none());
                }
                None => prop_assert!(log.get("retention_days").is_none()),
            }
            prop_assert_eq!(log.get("future_log").is_some(), case.logging.junk);

            let ap = &out["global"]["app"];
            match case.app.port {
                Some(v) => {
                    prop_json_eq!(ap["port"], json!(v), "app_port → port 保全");
                    prop_assert!(ap.get("app_port").is_none());
                }
                None => prop_assert!(ap.get("port").is_none()),
            }
            match case.app.auto_open_browser {
                Some(v) => {
                    prop_json_eq!(
                        ap["auto_start_browser"],
                        json!(v),
                        "auto_open_browser → auto_start_browser 保全"
                    );
                    prop_assert!(ap.get("auto_open_browser").is_none());
                }
                None => prop_assert!(ap.get("auto_start_browser").is_none()),
            }
            if case.app.deprecated {
                for f in [
                    "shell_path",
                    "lightweight_tray",
                    "minimize_to_tray",
                    "proxy",
                ] {
                    prop_assert!(ap.get(f).is_none(), "废弃字段 {f} 必须删除");
                }
            }
            prop_assert_eq!(ap.get("future_app").is_some(), case.app.junk);
        } else {
            prop_assert_eq!(
                &out["global"]["logging"],
                &case.logging.render(start),
                "start≥6 时 logging 段不得被改动"
            );
            prop_assert_eq!(
                &out["global"]["app"],
                &case.app.render(start),
                "start≥6 时 app 段不得被改动"
            );
        }

        // ---- v6→v7：旧仓库源改写；其余值恒等 ----
        let expected_url = if start <= 6 && case.source == PropSourceUrl::Old {
            PROP_NEW_SOURCE_URL
        } else {
            case.source.render()
        };
        prop_json_eq!(
            out["global"]["updater"]["release_source_url"],
            json!(expected_url),
            "更新源地址保全/改写"
        );

        // ---- v7→v8：playwright 归一 chromium；其余恒等 ----
        let expected_channel = if start <= 7 && case.channel == PropChannelSeed::Playwright {
            "chromium"
        } else {
            case.channel.render()
        };
        prop_json_eq!(
            out["global"]["browser"]["browser_channel"],
            json!(expected_channel),
            "浏览器渠道保全/归一"
        );

        // ---- 方案文件：拆分 / 改名 / 绑定 / 直连搬移逐项对照 ----
        // start≤5 的方案文件由拆分生成（无 profiles 键则无文件）；start≥6 为预写
        let profiles_on_disk = if start >= 6 {
            true
        } else {
            case.has_profiles_key
        };
        if profiles_on_disk {
            let first_id = case.profiles.first().map(|(id, _)| id.clone());
            // 任务 id 分配预言机：任务与方案分属不同命名空间，其他方案的合法产物
            // 可能占用本方案 id（级联让位），「未迁移不得造任务」类断言须豁免
            let (task_alloc, all_http_files, created_files) = prop_alloc_http_task_ids(case);
            for (id, seed) in &case.profiles {
                let path = config_dir.join("profiles").join(format!("{id}.json"));
                let raw = match std::fs::read_to_string(&path) {
                    Ok(raw) => raw,
                    Err(e) => {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "方案文件 {id}.json 必须存在（拆分或预写）: {e}\ncase: {case:?}"
                        )));
                    }
                };
                let p: Value = match serde_json::from_str(&raw) {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "方案文件 {id}.json 必须可解析: {e}\ncase: {case:?}"
                        )));
                    }
                };

                prop_json_eq!(p["id"], json!(id), "id 必须与文件名一致");
                prop_json_eq!(p["name"], json!(seed.name), "name 保全");
                prop_json_eq!(p["username"], json!(seed.username), "username 保全");
                prop_json_eq!(p["password"], json!(seed.password), "password 保全");

                if start <= 5 {
                    // 改名三键：旧名移除、新名值恒等；新旧并存时保留新键丢弃旧键
                    match &seed.carrier {
                        PropCarrierSeed::Absent => {
                            prop_assert!(p.get("isp").is_none() && p.get("carrier").is_none());
                        }
                        PropCarrierSeed::Old(v) => {
                            prop_json_eq!(p["isp"], json!(v), "carrier → isp 保全");
                            prop_assert!(p.get("carrier").is_none(), "旧名 carrier 必须移除");
                        }
                        PropCarrierSeed::Both(_, new) => {
                            prop_json_eq!(p["isp"], json!(new), "新旧并存时保留 isp 新值");
                            prop_assert!(p.get("carrier").is_none());
                        }
                    }
                    match &seed.gateway_ip {
                        Some(v) => {
                            prop_json_eq!(
                                p["gateway_ip"],
                                json!(v),
                                "match_gateway_ip → gateway_ip 保全"
                            );
                            prop_assert!(p.get("match_gateway_ip").is_none());
                        }
                        None => {
                            prop_assert!(
                                p.get("gateway_ip").is_none()
                                    && p.get("match_gateway_ip").is_none()
                            );
                        }
                    }
                    match &seed.wifi_ssid {
                        Some(v) => {
                            prop_json_eq!(p["wifi_ssid"], json!(v), "match_ssid → wifi_ssid 保全");
                            prop_assert!(p.get("match_ssid").is_none());
                        }
                        None => {
                            prop_assert!(
                                p.get("wifi_ssid").is_none() && p.get("match_ssid").is_none()
                            );
                        }
                    }
                    prop_assert!(
                        p.get("carrier_custom").is_none(),
                        "废弃字段 carrier_custom 必须删除"
                    );
                } else {
                    // start≥6：文件内容除 v9 迁移写回外不被触碰（改名键恒等保留）
                    match &seed.carrier {
                        PropCarrierSeed::Absent => prop_assert!(p.get("isp").is_none()),
                        PropCarrierSeed::Old(v) | PropCarrierSeed::Both(_, v) => {
                            prop_json_eq!(p["isp"], json!(v), "start≥6 时 isp 恒等保留");
                        }
                    }
                    match &seed.gateway_ip {
                        Some(v) => prop_json_eq!(
                            p["gateway_ip"],
                            json!(v),
                            "start≥6 时 gateway_ip 恒等保留"
                        ),
                        None => prop_assert!(p.get("gateway_ip").is_none()),
                    }
                    match &seed.wifi_ssid {
                        Some(v) => {
                            prop_json_eq!(p["wifi_ssid"], json!(v), "start≥6 时 wifi_ssid 恒等保留")
                        }
                        None => prop_assert!(p.get("wifi_ssid").is_none()),
                    }
                    prop_assert!(
                        p.get("carrier").is_none()
                            && p.get("carrier_custom").is_none()
                            && p.get("match_gateway_ip").is_none()
                            && p.get("match_ssid").is_none(),
                        "start≥6 时不得出现 v5 旧名键"
                    );
                }

                // v9→v10：内联直连 → 独立任务（绑定 id 由预言机推演）
                let configured = seed.http.legacy_present();
                if configured {
                    let expected_task = match task_alloc.get(id) {
                        Some(t) => t.clone(),
                        None => {
                            return Err(proptest::test_runner::TestCaseError::fail(format!(
                                "配过直连的方案 {id} 必须在预言机中有任务分配\ncase: {case:?}"
                            )));
                        }
                    };
                    let task_path = base
                        .join("tasks")
                        .join("http")
                        .join(format!("{expected_task}.json"));
                    if !task_path.exists() {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "配过直连的方案 {id} 必须产出任务文件 {expected_task}.json\ncase: {case:?}"
                        )));
                    }
                    let task: Value =
                        serde_json::from_str(&std::fs::read_to_string(&task_path).unwrap())
                            .unwrap();
                    if created_files.contains(&expected_task) {
                        prop_json_eq!(task["type"], json!("http"));
                        prop_json_eq!(
                            task["task_id"],
                            json!(expected_task),
                            "任务 id 须与文件名一致"
                        );
                        let expected_method = if seed.http.method.eq_ignore_ascii_case("POST") {
                            "POST"
                        } else {
                            "GET"
                        };
                        prop_json_eq!(
                            task["method"],
                            json!(expected_method),
                            "非 POST 一律归一 GET"
                        );
                        prop_json_eq!(task["url"], json!(seed.http.url), "直连地址保全");
                        prop_json_eq!(task["headers"], json!(seed.http.headers), "请求头保全");
                        prop_json_eq!(task["body"], json!(seed.http.body), "请求体保全");
                        prop_json_eq!(
                            task["success_pattern"],
                            json!(seed.http.success),
                            "成功判据保全"
                        );
                        prop_json_eq!(
                            task["failure_pattern"],
                            json!(seed.http.failure),
                            "失败判据保全"
                        );
                        prop_json_eq!(
                            task["crypto_script"],
                            json!(seed.http.crypto),
                            "凭据变换脚本保全"
                        );
                        prop_json_eq!(
                            task["ignore_https_errors"],
                            seed.http.ignore.map(Value::Bool).unwrap_or(Value::Null),
                            "非布尔证书策略迁移为 null（跟随全局）"
                        );
                        prop_json_eq!(
                            task["auth_url"],
                            json!(""),
                            "任务 auth_url 刻意留空：方案认证地址仍属方案"
                        );
                        let name_src = if seed.name.trim().is_empty() {
                            expected_task.as_str()
                        } else {
                            seed.name.as_str()
                        };
                        prop_json_eq!(task["name"], json!(format!("{name_src} 直连")));
                        // 新任务必须追加进排序表（顺序稳定）
                        let order_raw =
                            std::fs::read_to_string(base.join("tasks").join(".order.json"))
                                .unwrap_or_default();
                        let order: Value = serde_json::from_str(&order_raw).unwrap_or(Value::Null);
                        let listed = order["order"].as_array().is_some_and(|a| {
                            a.iter().any(|v| v.as_str() == Some(expected_task.as_str()))
                        });
                        prop_assert!(listed, "迁移新建的任务必须追加进 .order.json 排序表");
                    } else {
                        // 复用磁盘夹具的用户自建同名任务：内容一致（全空串）且不得改写
                        prop_assert_eq!(
                            task,
                            serde_json::json!({"type":"http","task_id":"x","name":"用户自建"}),
                            "内容一致时必须复用用户任务且不改写"
                        );
                    }
                    prop_json_eq!(
                        p["active_http_task"],
                        json!(expected_task),
                        "方案必须绑定迁移产出的直连任务"
                    );
                    for key in LEGACY_HTTP_KEYS {
                        prop_assert!(
                            p.get(key).is_none(),
                            "内联键 {key} 必须搬走并清除，否则重跑重复搬移"
                        );
                    }
                } else {
                    prop_assert!(
                        p.get("active_http_task").is_none(),
                        "未配置直连不得凭空绑定"
                    );
                    // 未迁移的方案保持原样：内联空键留着无害（v10 schema 忽略，保存时自然消失）
                    for (k, v) in seed.http.render_map() {
                        prop_json_eq!(p[k], v, "未配置直连的方案文件不得被改动（{}）", k);
                    }
                    // 同名文件属「用户自建任务占名」或其他方案的合法产物（级联让位）时豁免
                    if !all_http_files.contains(id) {
                        prop_assert!(
                            !base
                                .join("tasks")
                                .join("http")
                                .join(format!("{id}.json"))
                                .exists(),
                            "没填过直连地址不得凭空造任务"
                        );
                    }
                    prop_assert!(
                        !base
                            .join("tasks")
                            .join("http")
                            .join(format!("{id}-2.json"))
                            .exists()
                            || all_http_files.contains(&format!("{id}-2")),
                        "未迁移不得产生 -2 后缀任务（除非为其他方案的合法产物）"
                    );
                }

                // v8→v9：全局任务绑定到活跃方案（仅 start≤8 执行）
                if first_id.as_deref() == Some(id.as_str()) {
                    match (&case.order_active, start) {
                        (Some(tid), s) if s <= 8 => {
                            prop_json_eq!(
                                p["active_task"],
                                json!(tid),
                                "原全局启用任务必须绑定到活跃方案"
                            );
                        }
                        (Some(_), 9) => {
                            prop_assert!(
                                p.get("active_task").is_none(),
                                "start=9 时 v8→v9 已跳过，不得凭空绑定"
                            );
                        }
                        _ => prop_assert!(p.get("active_task").is_none(), "无全局选择不得凭空绑定"),
                    }
                } else {
                    prop_assert!(
                        p.get("active_task").is_none(),
                        "非活跃方案不得被 v8→v9 触碰"
                    );
                }
            }

            // 非法 id：跳过、不写文件（R4 路径穿越防线）
            for invalid in &case.inline_invalid_ids {
                prop_assert!(
                    !config_dir
                        .join("profiles")
                        .join(format!("{invalid}.json"))
                        .exists(),
                    "非法 Profile id 不得写出文件"
                );
            }
        }
        Ok(())
    }

    /// 构造畸形输入（属性 M2）：以最小 v5 形态为基座，按 (kind, sub) 施加畸变
    fn prop_build_malformed(start: u32, kind: u8, sub: u8) -> Value {
        let base = || {
            serde_json::json!({
                "config_version": start,
                "active_profile_id": "default",
                "global": {
                    "monitor": {
                        "check_interval_seconds": 30,
                        "ping_targets": ["1.1.1.1"],
                        "url_check_urls": ["http://a|OK"],
                    },
                    "logging": { "log_retention_days": 7 },
                    "app": { "app_port": 50721 },
                    "pause": { "enabled": true },
                    "browser": { "browser_channel": "chromium", "headless": true },
                },
            })
        };
        match kind {
            0 => {
                let mut v = base();
                v["config_version"] = json!("9");
                v
            }
            1 => {
                let mut v = base();
                v["config_version"] = json!(-1);
                v
            }
            2 => {
                let mut v = base();
                v["config_version"] = json!(1.5);
                v
            }
            3 => {
                let mut v = base();
                v["config_version"] = Value::Null;
                v
            }
            4 => {
                let mut v = base();
                v["config_version"] = json!(11u32);
                v
            }
            5 => {
                let mut v = base();
                v["config_version"] = json!(u64::MAX);
                v
            }
            6 => {
                let mut v = base();
                match sub {
                    0 => v["global"]["monitor"]["check_interval_seconds"] = json!("oops"),
                    1 => v["global"]["monitor"]["ping_targets"] = json!("not-array"),
                    2 => v["global"]["logging"]["log_retention_days"] = json!("x"),
                    3 => v["global"]["app"]["app_port"] = json!("x"),
                    4 => v["global"]["pause"]["enabled"] = json!(5),
                    5 => v["global"]["browser"]["headless"] = serde_json::json!({}),
                    _ => {
                        v["global"]["monitor"]["url_check_urls"] = serde_json::json!([42, null, {}])
                    }
                }
                v
            }
            7 => match sub % 4 {
                0 => Value::Null,
                1 => json!([1, 2, 3]),
                2 => json!("just-a-string"),
                _ => json!(42),
            },
            8 => {
                let mut v = base();
                match sub % 3 {
                    0 => v["profiles"] = json!(5),
                    1 => v["profiles"] = json!([]),
                    _ => v["profiles"] = serde_json::json!({"ok": {"name": "ok"}, "bad": 42}),
                }
                v
            }
            _ => {
                let mut v = base();
                let mut nested = json!({"leaf": 1});
                for _ in 0..u32::from(sub) + 2 {
                    nested = json!({ "n": nested });
                }
                v["deep_unknown"] = nested;
                v
            }
        }
    }

    proptest! {
        #![proptest_config(prop_config())]

        /// 【属性 M1】合法旧版本配置经迁移链：
        /// ① 永不 panic，合法输入必产出 Ok（失败传播口径由属性 M2 与
        ///    test_migrate_v9_to_v10_write_back_failure_blocks_version_commit 覆盖）；
        /// ② 幂等：v10 原版本重入为严格 no-op（值与磁盘树均不变）；版本回退
        ///    v1 的强制整链重入至多补齐首轮被跳过的内容键控改写，两次重入达不动点；
        /// ③ 版本号单调：终版恒为 CURRENT 且写回 value（CFG-1）；
        /// ④ 关键字段按各版本迁移语义保全（见 prop_assert_fields_preserved）。
        /// 附加横切不变量：迁移层永不写 settings.json（哨兵内容不变）、
        /// 原子写无 .tmp_ 残留、产物可被当前 schema 解析（G4）。
        #[test]
        fn prop_migration_chain_roundtrip(case in prop_arb_migration_case()) {
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path();
            let config_dir = base.join("config");
            std::fs::create_dir_all(&config_dir).unwrap();
            // 迁移层只写方案/任务文件，settings.json 由调用方落盘：哨兵内容全程不变
            let sentinel = r#"{"sentinel":true}"#;
            std::fs::write(config_dir.join("settings.json"), sentinel).unwrap();
            case.setup_disk(&config_dir);

            let mut value = case.settings.clone();
            let result = run_migrations(&config_dir, &mut value);

            // 横切：settings.json 哨兵不变；原子写不留 .tmp_ 残留
            prop_assert_eq!(
                std::fs::read_to_string(config_dir.join("settings.json")).unwrap(),
                sentinel,
                "迁移层永不写 settings.json"
            );
            prop_assert!(prop_tmp_residue(&config_dir).is_empty(), "config 树不得残留 .tmp_ 临时文件");
            prop_assert!(prop_tmp_residue(&base.join("tasks")).is_empty(), "tasks 树不得残留 .tmp_ 临时文件");

            // ① 合法输入必成功（Err 仅由磁盘故障/畸形产物触发，生成器已排除）
            let version = match result {
                Ok(v) => v,
                Err(e) => {
                    return Err(proptest::test_runner::TestCaseError::fail(format!(
                        "合法旧版本配置必须迁移成功，实际 Err: {e}\ncase: {case:?}"
                    )));
                }
            };

            // ③ 版本号单调：链终版本恒为 CURRENT，且写回 value
            prop_assert_eq!(version, crate::config::CURRENT_CONFIG_VERSION, "start<10 时终版必为 CURRENT");
            prop_assert_eq!(
                value.get("config_version").and_then(Value::as_u64),
                Some(u64::from(crate::config::CURRENT_CONFIG_VERSION)),
                "config_version 必须写回 value（CFG-1）"
            );
            // G4（start≤5 的产物自检）/ 生成器类型合法性（start≥6）共同保证
            prop_assert!(
                serde_json::from_value::<SettingsData>(value.clone()).is_ok(),
                "迁移产物必须可被当前 schema 解析"
            );

            // ④ 关键字段保全
            prop_assert_fields_preserved(&case, &config_dir, &value)?;

            // ② 幂等（两层口径）：
            // a) 产线口径：迁移完成的 v10 配置按原版本重入 → 前置检查放行，
            //    值与磁盘树严格 no-op；
            // b) 异常续跑口径：版本强行回退 v1 整链重入 → 内容键控的 v6→v7 /
            //    v7→v8 语义改写与 start=9 时跳过的 v8→v9 绑定可能补齐（首轮因
            //    起始版本被跳过的效果），但第二次强制重入必须达不动点；
            //    磁盘层面对强制重入只保证 settings.json 哨兵与无 .tmp_ 残留
            //   （v8→v9 补绑定会合法新增方案写回与备份目录）。
            let mut before = prop_snapshot_tree(&config_dir);
            before.extend(prop_snapshot_tree(&base.join("tasks")));
            before.sort();

            let first = value.clone();
            let rerun = run_migrations(&config_dir, &mut value);
            prop_assert!(
                matches!(rerun, Ok(v) if v == crate::config::CURRENT_CONFIG_VERSION),
                "v10 原版本重入必须被前置检查放行为 CURRENT: {rerun:?}"
            );
            prop_assert_eq!(value.clone(), first, "v10 原版本重入必须值不变");
            let mut after = prop_snapshot_tree(&config_dir);
            after.extend(prop_snapshot_tree(&base.join("tasks")));
            after.sort();
            prop_assert_eq!(after, before, "v10 原版本重入不得改写磁盘（方案/任务/备份均稳定）");

            // 异常续跑：版本回退 v1 强制整链重入，两次重入达不动点
            if let Some(obj) = value.as_object_mut() {
                obj.insert("config_version".into(), Value::from(1u32));
            }
            let rerun2 = run_migrations(&config_dir, &mut value);
            prop_assert!(rerun2.is_ok(), "强制整链重入不得失败");
            prop_assert!(
                serde_json::from_value::<SettingsData>(value.clone()).is_ok(),
                "强制重入产物必须可被当前 schema 解析"
            );
            let second = value.clone();
            if let Some(obj) = value.as_object_mut() {
                obj.insert("config_version".into(), Value::from(1u32));
            }
            let rerun3 = run_migrations(&config_dir, &mut value);
            prop_assert!(rerun3.is_ok(), "第二次强制重入不得失败");
            prop_assert_eq!(value, second, "强制重入必须达不动点（第二次重入值不变）");
            let sentinel_after = std::fs::read_to_string(config_dir.join("settings.json")).unwrap();
            prop_assert_eq!(sentinel_after, sentinel, "强制重入同样不得写 settings.json");
            prop_assert!(
                prop_tmp_residue(&config_dir).is_empty() && prop_tmp_residue(&base.join("tasks")).is_empty(),
                "重入后不得残留 .tmp_ 临时文件"
            );
        }

        /// 【属性 M2】畸形输入（类型错位 / 根形状 / profiles 形状 / 深嵌套 /
        /// 非法版本号形态）：永不 panic，Ok/Err 符合既定口径，settings.json
        /// 哨兵不变、无 .tmp_ 残留（不产出半配置）。
        #[test]
        fn prop_migration_malformed_inputs((start, kind, sub) in (1u32..=9, 0u8..=9, 0u8..=6)) {
            let tmp = tempfile::tempdir().unwrap();
            let config_dir = tmp.path().join("config");
            std::fs::create_dir_all(&config_dir).unwrap();
            let sentinel = r#"{"sentinel":true}"#;
            std::fs::write(config_dir.join("settings.json"), sentinel).unwrap();

            let mut value = prop_build_malformed(start, kind, sub);
            let original = value.clone();
            let result = run_migrations(&config_dir, &mut value);

            prop_assert_eq!(
                std::fs::read_to_string(config_dir.join("settings.json")).unwrap(),
                sentinel,
                "畸形输入路径下迁移层同样永不写 settings.json"
            );
            prop_assert!(prop_tmp_residue(&config_dir).is_empty(), "畸形输入路径不得残留 .tmp_");

            match kind {
                // 垃圾版本号（字符串/负数/浮点/null）：as_u64 失败按缺省 v1 全链迁移
                0..=3 => {
                    prop_assert!(
                        matches!(result, Ok(v) if v == crate::config::CURRENT_CONFIG_VERSION),
                        "垃圾版本号按缺省 v1 处理并迁移成功: {result:?}"
                    );
                    prop_assert_eq!(
                        value.get("config_version").and_then(Value::as_u64),
                        Some(u64::from(crate::config::CURRENT_CONFIG_VERSION)),
                        "迁移成功后版本号必须写回"
                    );
                }
                // 未来版本（11 / u64::MAX 截断为 u32::MAX）：前置检查放行、值原样保留
                // （产线由 caller 的 VersionTooHigh 拦截，此处钉住迁移层自身口径）
                4 | 5 => {
                    let expected = if kind == 4 { 11 } else { u32::MAX };
                    prop_assert!(
                        matches!(result, Ok(v) if v == expected),
                        "未来版本 {expected} 应原样放行: {result:?}"
                    );
                    prop_assert_eq!(value, original, "版本前置检查放行时不得改动 value");
                }
                // 类型错位：start≤5 时 G4 产物自检必拒（错位字段均落在 schema 解析路径）
                6 => {
                    if start <= 5 && sub < 6 {
                        prop_assert!(result.is_err(), "start≤5 的已知字段类型错位必须被 G4 自检拒绝: {result:?}");
                    } else {
                        // start≥6 无 G4 门（产线由 caller 的 from_value 复检拦截）；
                        // sub=6 的 url_check_urls 非字符串条目被拆分逻辑跳过、可成功
                        prop_assert!(result.is_ok() || result.is_err(), "类型错位至多 Err，绝不 panic");
                    }
                }
                // 根形状畸形：G4 无法解析非对象根 → 必 Err（备份保留供回滚）
                7 => {
                    prop_assert!(result.is_err(), "非对象根必须返回 Err: {result:?}");
                }
                // profiles 形状畸形：拆分跳过或跳过非法条目 → 迁移成功完成
                8 => {
                    prop_assert!(
                        matches!(result, Ok(v) if v == crate::config::CURRENT_CONFIG_VERSION),
                        "profiles 形状畸形不得中断迁移: {result:?}"
                    );
                }
                // 深嵌套未知键：迁移不递归进入，安全完成
                9 => {
                    prop_assert!(
                        matches!(result, Ok(v) if v == crate::config::CURRENT_CONFIG_VERSION),
                        "深嵌套未知键不得中断迁移: {result:?}"
                    );
                }
                _ => unreachable!("kind 生成域为 0..=9"),
            }
        }
    }
}
