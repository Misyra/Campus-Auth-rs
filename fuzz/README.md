# Fuzz（cargo-fuzz）

对三个高风险纯逻辑面做模糊测试。`fuzz/` 是独立 crate（独立 `Cargo.toml` / `Cargo.lock`，不污染根依赖），通过 path 依赖根库 `campus-auth`（`default-features = false` + `no-embed`：跳过 tray/GTK 与前端嵌入，只链接 config / bridge 的纯逻辑面）。

## 环境准备

```bash
rustup toolchain install nightly --component rust-src   # cargo-fuzz 需要 nightly + rust-src
cargo install cargo-fuzz
```

## 三个 target 与覆盖面

| target | 生产入口 | 覆盖面 |
|--------|----------|--------|
| `fuzz_parse_ipc` | `src/bridge/process.rs::parse_and_send_line` 的解析链（该函数为私有 async，依赖 mpsc 通道，本 target 复刻其调用序列） | NDJSON 行 → `serde_json::from_str::<Value>` → 按 `id`/`event` 分类 → `from_value::<IpcResponse>` / `from_value::<IpcEvent>`；响应 `result.data` 再走 `StructuredResult` 反序列化。覆盖 Worker stdout 可能出现的任意内容（非 JSON、截断、超大整数、深度嵌套、非法 UTF-8） |
| `fuzz_config_json` | `src/config/service.rs` 的 settings.json / profiles/{id}.json 反序列化面 | `from_value::<SettingsData>`（迁移后 settings.json 与 `PATCH /api/config` 载荷）、`from_value::<ProfileData>`（方案文件）。`RuntimeConfig` 是运行时合并快照（无 Deserialize），不在范围内 |
| `fuzz_migrate` | `src/config/migration.rs::run_migrations(config_dir, &mut Value)`（公开纯数据入口） | 迁移链 v5→v10 对任意 JSON 结构的健壮性：必须「返回 Err 或幂等完成」，绝不 panic（panic 即启动失败死循环）。盘面依赖在进程内共享的 tempfile 目录上跑——迁移函数按可安全重跑设计，恰好同时覆盖全新目录与已迁移目录重跑两类状态 |

## 运行

冒烟（CI 同款，分钟级）：

```bash
cd fuzz
cargo +nightly fuzz run fuzz_parse_ipc    -- -max_total_time=180 -max_len=16384 -rss_limit_mb=2560
cargo +nightly fuzz run fuzz_config_json  -- -max_total_time=180 -max_len=16384 -rss_limit_mb=2560
cargo +nightly fuzz run fuzz_migrate      -- -max_total_time=180 -max_len=16384 -rss_limit_mb=2560
```

深跑（本地手动，小时级；语料持续累积在 `fuzz/corpus/<target>/`）：

```bash
cargo +nightly fuzz run fuzz_parse_ipc -- -max_total_time=3600 -max_len=16384 -rss_limit_mb=2560
# 建议逐 target 跑，跑完把 crash artifact（fuzz/artifacts/ 下的文件）作为回归用例
# 保留在语料目录：cp fuzz/artifacts/* fuzz/corpus/<target>/
```

崩溃复现（单文件回放，不经 fuzz 循环）：

```bash
cargo +nightly fuzz run fuzz_parse_ipc fuzz/artifacts/crash-xxx
```

## CI

`.github/workflows/fuzz.yml`：每周一 03:00 UTC 定时 + 手动 dispatch（可传 `max_total_time` 秒数加大时长）。三 target 并行各跑 180 秒冒烟，发现崩溃时上传 `fuzz/artifacts/` 作为 artifact。语料不缓存（冒烟语料可快速重建，缓存会无限膨胀）。

## 发现崩溃怎么办

1. 先用单文件回放确认可复现；
2. 检查是否已有语料覆盖同类输入（避免重复上报）；
3. 生产代码修复归 `src/` 维护者处理（fuzz harness 不做防御性掩盖——它只是探针）。
