//! fuzz 目标：NDJSON IPC 行解析链
//!
//! 复刻 `src/bridge/process.rs::parse_and_send_line` 的解析链路：
//! `serde_json::from_str::<Value>` → 按 `id` / `event` 字段分类 →
//! `from_value::<IpcResponse>` / `from_value::<IpcEvent>`，再对响应
//! `result.data` 做 `StructuredResult` 反序列化（Worker 侧回包的实际消费面）。
//!
//! 生产侧 `parse_and_send_line` 为私有 async 函数（依赖 mpsc 通道），
//! 无法从外部 crate 直接调用；本目标覆盖它触达的同一套 serde 反序列化
//! 表面，包括全部派生结构（IpcResponse / IpcEvent / IpcResult /
//! StructuredResult / Outcome）与 Value 解析的边界（深度嵌套、超大整数、
//! 非法 UTF-8 截断等）。

#![no_main]
#![warn(clippy::all)]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

use campus_auth::bridge::ipc::{IpcEvent, IpcResponse, StructuredResult};

fuzz_target!(|data: &[u8]| {
    // 与生产读取路径一致：先按 UTF-8 解码，非法字节直接丢弃
    let Ok(line) = std::str::from_utf8(data) else {
        return;
    };
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return;
    }

    let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
        return;
    };

    if let Some(id_val) = v.get("id") {
        // 生产逻辑先提取 id（as_u64 失败仅记日志），再尝试 IpcResponse
        let _id = id_val.as_u64();
        if let Ok(resp) = serde_json::from_value::<IpcResponse>(v.clone()) {
            // 响应 data 可能携带 StructuredResult，与 api.rs 的消费面一致
            let _ = serde_json::from_value::<StructuredResult>(resp.result.data);
        }
    } else if v.get("event").is_some() {
        let _ = serde_json::from_value::<IpcEvent>(v);
    }

    // 反序列化成功与否都不允许 panic：该链路面对的是 Python Worker 的
    // stdout（第三方库可能 print 任意内容），崩溃即控制平面失效
});
