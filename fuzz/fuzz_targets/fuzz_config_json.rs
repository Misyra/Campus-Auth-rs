//! fuzz 目标：配置 JSON 反序列化
//!
//! 覆盖配置系统加载路径的两个核心反序列化入口（与
//! `src/config/service.rs` 的实际读取面一致）：
//! - `SettingsData`：settings.json 完整 schema（`from_value`，迁移后 /
//!     `PATCH /api/config` 载荷）
//! - `ProfileData`：profiles/{id}.json 单方案 schema（`from_str` 读盘）
//!
//! 注意：`RuntimeConfig` 是全局配置 + 活跃 Profile 的运行时合并快照，
//! 本身不走反序列化（无 Deserialize 实现），不在本目标范围。
//!
//! 输入先按原始字节直接喂给 `from_slice`（覆盖截断 JSON / 非法 UTF-8 /
//! 非 JSON 字节等读盘边界），再按 `Value` 分发到两个类型，验证任意
//! 结构化 JSON 下反序列化均不 panic（拒绝路径返回 Err 是预期行为）。

#![no_main]
#![warn(clippy::all)]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

use campus_auth::config::schema::{ProfileData, SettingsData};

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    let _ = serde_json::from_value::<SettingsData>(v.clone());
    let _ = serde_json::from_value::<ProfileData>(v);
});
