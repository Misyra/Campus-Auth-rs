//! 配置 schema 版本迁移 pipeline（v5 → v6）
//!
//! 启动时若 `settings.json` 的 `config_version` 低于当前版本，按 `MIGRATIONS` 顺序
//! 执行迁移函数，将旧结构转换为新结构并写回。迁移是幂等的：Profile 文件使用覆盖写入，
//! `settings.json` 的 `config_version` 更新是 commit point。

use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::Value;

use crate::config::ConfigError;

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
        for (id, profile) in profiles.iter_mut() {
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
                for f in ["shell_path", "lightweight_tray", "minimize_to_tray", "proxy"] {
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
            map.insert(to.to_string(), v);
        }
    }
}

/// 迁移前递归备份整个 config 目录到 `.backup.v5.{timestamp}`
///
/// 仅用于迁移失败时的手动回滚，备份目录以 [`crate::config::BACKUP_PREFIX`] 前缀命名，
/// 不会被 `load_all_profiles` 等逻辑误读。
fn backup_config_dir(config_dir: &Path) -> std::io::Result<PathBuf> {
    let backup_dir = config_dir.join(format!(
        "{}{}",
        crate::config::BACKUP_PREFIX,
        Local::now().format("%Y%m%d%H%M%S")
    ));
    // 极端情况下时间戳冲突则跳过（极少发生）
    if backup_dir.exists() {
        return Ok(backup_dir);
    }
    copy_dir_recursive(config_dir, &backup_dir)?;
    Ok(backup_dir)
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
                .map(|n| n.to_string_lossy().starts_with(crate::config::BACKUP_PREFIX))
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
