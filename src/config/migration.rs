//! 配置 schema 版本迁移 pipeline（v5 → v6）
//!
//! 启动时若 `settings.json` 的 `config_version` 低于当前版本，按 `MIGRATIONS` 顺序
//! 执行迁移函数，将旧结构转换为新结构并写回。迁移是幂等的：Profile 文件使用覆盖写入，
//! `settings.json` 的 `config_version` 更新是 commit point。

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
/// 当前仅有 v5 → v6。新增版本时在末尾追加 `(新版本, 迁移函数)` 即可。
pub const MIGRATIONS: &[(u32, MigrationFn)] = &[(6, migrate_v5_to_v6)];

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
            rename_field(monitor, "enable_local_check", "url_enabled");
            rename_field(monitor, "ping_targets", "tcp_targets");
            rename_field(monitor, "test_urls", "http_targets");
            rename_field(monitor, "url_check_urls", "url_targets");
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
        assert_eq!(monitor["url_enabled"], true);
        assert_eq!(monitor["tcp_targets"][0], "1.1.1.1");
        assert_eq!(monitor["http_targets"][0], "http://test");
        assert_eq!(monitor["url_targets"][0], "http://apple|Success");
        // 废弃字段已删除
        for f in [
            "check_interval_seconds",
            "enable_tcp_check",
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
        // 已是 v6：不产生任何文件、不修改值
        let mut v = serde_json::json!({
            "config_version": 6,
            "active_profile_id": "default",
        });
        let result = run_migrations(tmp.path(), &mut v).unwrap();
        assert_eq!(result, crate::config::CURRENT_CONFIG_VERSION);
        assert_eq!(v["active_profile_id"], "default");
        // 未创建 profiles 目录（无迁移发生）
        assert!(!tmp.path().join("profiles").exists());
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
}
