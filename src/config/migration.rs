//! 配置 schema 版本迁移 pipeline（v5 → v6 → v7 → v8 → v9 → v10）
//!
//! 启动时若 `settings.json` 的 `config_version` 低于当前版本，按 `MIGRATIONS` 顺序
//! 执行迁移函数，将旧结构转换为新结构并写回。迁移是幂等的：Profile 文件使用覆盖写入，
//! `settings.json` 的 `config_version` 更新是 commit point。
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
            if let Some(m) = monitor.as_object_mut() {
                if let Some(Value::Array(items)) = m.remove("url_check_urls") {
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
    if let Some(dir) = backup_dir {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::warn!("清理迁移备份目录失败: {e}");
        }
    }

    Ok(())
}

/// 若 `from` 字段存在则重命名为 `to`（值整体搬移）
///
/// 接收 `&mut Value`，内部按需取 object map，兼容 `profiles` 与 `global` 两种调用场景。
fn rename_field(obj: &mut Value, from: &str, to: &str) {
    if let Some(map) = obj.as_object_mut() {
        if let Some(v) = map.remove(from) {
            if map.contains_key(to) {
                tracing::warn!("迁移重命名跳过：{to} 已存在，丢弃旧字段 {from}");
            } else {
                map.insert(to.to_string(), v);
            }
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
    let mut candidate = config_dir.join(format!("{}{}", crate::config::BACKUP_PREFIX, stamp));
    let mut n: u32 = 2;
    while candidate.exists() {
        candidate = config_dir.join(format!("{}{}-{n}", crate::config::BACKUP_PREFIX, stamp));
        n += 1;
    }
    candidate
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
    if updater.get("release_source_url").and_then(Value::as_str) == Some(OLD_SOURCE_URL) {
        if let Some(obj) = updater.as_object_mut() {
            obj.insert(
                "release_source_url".to_string(),
                Value::String(NEW_SOURCE_URL.to_string()),
            );
        }
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
    if browser.get("browser_channel").and_then(Value::as_str) == Some("playwright") {
        if let Some(obj) = browser.as_object_mut() {
            obj.insert(
                "browser_channel".to_string(),
                Value::String("chromium".to_string()),
            );
        }
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
        let json = serde_json::to_string_pretty(&profile)?;
        std::fs::write(&profile_path, json)?;
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
        Err(e) => {
            // 方案目录不可读：不动任何东西，留给下次启动重试（迁移版本号未提交）
            tracing::warn!(
                path = %profiles_dir.display(),
                error = %e,
                "v10 迁移跳过：方案目录不可读"
            );
            return Ok(());
        }
    };

    let mut created: Vec<String> = Vec::new();
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
                tracing::warn!(path = %http_dir.display(), error = %e, "v10 迁移：创建直连任务目录失败");
                continue;
            }
            let task_path = http_dir.join(format!("{task_id}.json"));
            match serde_json::to_string_pretty(&task) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&task_path, json) {
                        tracing::warn!(
                            path = %task_path.display(),
                            error = %e,
                            "v10 迁移：写入直连任务失败，保留方案里的原配置"
                        );
                        continue;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "v10 迁移：序列化直连任务失败");
                    continue;
                }
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
        match serde_json::to_string_pretty(&profile) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!(
                        profile_id = %profile_id,
                        error = %e,
                        "v10 迁移：写回方案失败（直连任务已就绪，下次启动会重试绑定）"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(profile_id = %profile_id, error = %e, "v10 迁移：序列化方案失败")
            }
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
    if let Ok(text) = serde_json::to_string_pretty(&json) {
        if let Err(e) = std::fs::write(&path, text) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "v10 迁移：写入任务排序表失败（不影响任务可用，仅顺序不固定）"
            );
        }
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
}
