//! fuzz 目标：配置迁移链（v5 → v10）
//!
//! 入口为 `config::migration::run_migrations(config_dir, &mut Value)`：
//! 生产侧输入是用户 settings.json 的解析结果（半信任——可能被手工编辑、
//! 部分损坏或携带旧版本残留结构），迁移链必须对任意 JSON 结构做到
//! 「返回 Err 或幂等完成」，绝不 panic（panic 即启动失败死循环）。
//!
//! 盘面依赖处理：迁移链会写 config_dir（备份、Profile 拆分落盘），无法
//! 纯内存化；本目标在进程内泄漏一个共享 tempfile 目录上运行——迁移函数
//! 均按「先判断是否已迁过再落盘」设计为可安全重跑（见 migration.rs 模块
//! 注释），跨输入复用目录恰好同时覆盖「全新目录」与「已迁移目录重跑」
//! 两类状态。温度控制：单个输入体积被 max_len 限制，目录内文件数量
//! 随 profiles 字段规模线性增长，libFuzzer 的 -rss_limit 与执行数上限
//! 兜底。

#![no_main]
#![warn(clippy::all)]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

use campus_auth::config::migration::run_migrations;

/// 进程级共享的临时配置目录（进程退出时由 TempDir 析构清理）
fn shared_config_dir() -> &'static std::path::Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("创建 fuzz 临时配置目录失败"))
        .path()
}

fuzz_target!(|data: &[u8]| {
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    // 返回值（迁移后版本号 / ConfigError）均为预期分支，只关注不 panic
    let _ = run_migrations(shared_config_dir(), &mut value);
});
