# Campus-Auth 校园网自动认证工具（Rust 重写版）

## 项目概述

Campus-Auth 是一个校园网自动认证工具。Rust 重写版为单 binary crate + Python 子进程（浏览器自动化），便携版解压即用。Rust 侧负责控制平面（网络监测、登录状态机、调度、配置、Web API、系统托盘），Python 侧作为按需执行插件负责浏览器自动化（Playwright）和 OCR（ddddocr）。

当前版本：5.0.0-alpha.6

## 技术栈

| 领域 | 选型 |
|------|------|
| 语言 / Edition | Rust 2024, MSRV 1.85 |
| 异步运行时 | tokio（按需裁剪，原 full：rt-multi-thread / macros / net / fs / process / signal / sync / time / io-util，见 Cargo.toml） |
| HTTP 框架 | axum 0.8 + tower-http |
| HTTP 客户端 | reqwest (rustls-tls) |
| 序列化 | serde + serde_json |
| 原子配置 | arc-swap |
| CLI | clap (derive) |
| 日志 | tracing + tracing-subscriber |
| 错误处理 | thiserror (库) + anyhow (应用层) |
| 加密 | aes-gcm (AES-256-GCM) |
| 敏感数据清零 | zeroize |
| 时间 / Cron | chrono + cron |
| 系统托盘 | tray-icon |
| 前端嵌入 | rust-embed |
| 进程管理 | tokio::process |
| 文件锁 | fs4 |
| 前端 | Vue 3 + TypeScript + Vite |

## 开发命令

```bash
# 编译
cargo build

# 运行
cargo run

# 运行测试
cargo test

# 运行指定测试
cargo test test_config_service

# 代码检查
cargo clippy -- -D warnings

# 格式化
cargo fmt

# 跳过前端嵌入的检查（开发时 frontend/dist 不存在）
cargo check --features no-embed

# 前端开发
cd frontend && npm run dev

# 前端构建
cd frontend && npm run build
```

## 代码规范

### 命名约定

| 类型 | 风格 | 示例 |
|------|------|------|
| 函数/变量 | `snake_case` | `get_profile`, `check_interval` |
| 类型/Trait | `PascalCase` | `RuntimeConfig`, `ScheduleEngine` |
| 常量 | `UPPER_SNAKE_CASE` | `DEFAULT_TIMEOUT`, `PROJECT_ROOT` |

### 注释与文档

- 所有注释、doc comment、文档均使用**中文**
- 每个模块必须有 `//!` 模块级 doc comment（一行摘要）
- 公共 API（struct、enum、fn、trait）必须有 `///` doc comment
- 行内注释解释"为什么"而非"是什么"，写在代码上方
- 标记约定：`TODO:`、`FIXME:`、`HACK:`（全大写 + 冒号）

### 错误处理

- 库/模块级错误用 `thiserror` 定义 `enum XxxError`
- 应用层（main、启动编排）用 `anyhow::Result`
- 错误传播优先用 `?`，避免 `.unwrap()`（测试除外）

### Lint

启用 `clippy` 全部默认规则，CI（`.github/workflows/ci.yml`）要求 `cargo clippy --all-targets -- -D warnings` 零警告（另含 `cargo fmt --check` / `cargo test` / `frontend build + vitest` / `compileall` / `uv run pytest`）。

## 项目结构

```
campus-auth/
├── Cargo.toml
├── openapi.json              # Web API 契约（手写 baseline，前端 typegen 数据源）
├── build.ps1                 # 便携版打包脚本
├── src/
│   ├── lib.rs                # 库入口：聚合全部模块 + 统一 ServiceHandle
│   ├── main.rs               # CLI 解析 → 启动分发
│   ├── helper_main.rs        # 更新替换助手（独立 binary：campus-auth-helper）
│   ├── app.rs                # Axum 服务器构建 + 托盘初始化
│   ├── container.rs          # ServiceContainer: Arc 共享状态（13 服务 + Metrics/uptime 2 横切，共 15 字段）
│   ├── launcher.rs           # 启动状态机 (full / lightweight / once)
│   ├── logging.rs            # 日志子系统（初始化 / 动态级别 / WS 广播 Layer）
│   ├── engine/               # 调度引擎（单 tokio task + select!，含 slot.rs 可替换句柄槽）
│   ├── monitor/              # 网络监测（TCP/HTTP/URL 探测）
│   ├── login/                # 登录编排（状态机、去重、抢占、重试）
│   ├── config/               # 配置系统（ArcSwap + 加密 + 迁移）— 源码模块，对应运行时 /config（.gitignore / 锚定，勿混淆）
│   ├── web/                  # Web API + WebSocket（routes/ 按域拆分：config/profiles/login/monitor/scheduler/tasks/scripts/system/autostart/debug/history/repo/background/uninstall/ocr 等，细粒度 state 注入）
│   ├── scheduler/            # 定时任务（独立 tokio task）
│   ├── tasks/                # 任务管理 — 源码模块，对应运行时 /tasks（.gitignore / 锚定）
│   ├── network/              # 网络接口
│   ├── bridge/               # Python Bridge（NDJSON IPC）— Rust 侧桥，对应 python_worker/ 执行侧
│   ├── status/               # StatusManager: 状态快照 + watch 推送
│   ├── environment/          # 环境管理器（uv/python 按需安装）— 源码模块，对应运行时 /environment（.gitignore / 锚定，空目录按需生成）
│   ├── updater/              # 版本更新（检查 + 下载 + 应用）
│   ├── tray/                 # 系统托盘（tray-icon）
│   └── utils/                # 工具（PID 文件锁、平台特定代码）
├── frontend/                 # Vue 3 + TypeScript + Vite — public/ 静态资源，dist/ 为 Vite 构建产物（rust-embed 嵌入，.gitignore 忽略），与 resources/ 职责分离
├── python_worker/            # Python Worker 子进程（Playwright + OCR）— 执行侧，对应 Rust 侧 src/bridge/，IPC 契约见 python_worker/README.md
├── tests/                    # 集成测试（common/ 共享辅助）+ fixtures/ 隔离基座模板 & mock-servers/ 轻量门户（统一测试入口，见 tests/README.md）；mock_portal/ 已搬迁至 tests/mock-servers/full-portal/（根保留 README 重定向）
├── docs/                     # 文档（changelog / 已知问题清单 / 任务编写指南 / plan-next 活跃计划 / archive 归档，已兑现 docs/archive/）
├── resources/                # 随二进制分发的静态资源（icons/ 托盘与浏览器图标、tools/ 脚本，rust-embed 嵌入，区别于 frontend/public 与 frontend/dist）
└── .github/workflows/        # CI（fmt + clippy + test + 前端构建 + vitest + pytest）
```

## 架构要点

### ServiceContainer（13 服务 + 2 横切，共 15 字段拓扑排序）

所有服务通过 `Arc` 构造注入，无延迟绑定、无全局变量。构造顺序（见 `src/container.rs`，文件头与 `new()` 注释均为 15）：

ConfigService → ProfileService → LoginHistoryService → StatusManager → TaskManager → BridgeSupervisor → EnvironmentManager → TaskExecutor → MonitorService → LoginOrchestrator → SchedulerService → UpdaterService → Engine

另有横切组件：`Metrics`（运行指标，`Arc<Metrics>`）与 uptime 定时器（`CancellationToken` child）。二者与 13 服务合计 15 字段，即 `src/container.rs:1` 的“15 服务拓扑排序”口径。AutoStartService / DebugSessionManager / TaskRegistry / TaskHistoryStore / WebSocketManager 未独立成服务，相关功能由 TrayManager / Scheduler / Bridge 内聚实现。

新增服务时在链中插入 `Arc::new(...)` 即可。

### ServiceHandle 统一模式

可启停的后台服务遵循统一句柄（见 `src/lib.rs`）：

```rust
struct ServiceHandle {
    stop_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
}
```

`BridgeSupervisor` / `SchedulerService` / `TrayManager` 使用 `ServiceHandle`（`stop_tx: watch` + `stop_with_timeout`）。`Engine` 为可崩溃重启特例，使用 `EngineSlot(Arc<ArcSwapOption<EngineHandle>>)` + `EngineHandle { engine: Arc<Engine>, join_handle, completed: CancellationToken }`，命令经 `slot.dispatch/try_dispatch` 派发（见 `src/engine/slot.rs` 与 `src/engine/mod.rs`），`ServiceHandle` 的 `watch` 语义不适用于 Engine。

服务主循环用 `tokio::select!` 同时等待业务逻辑和停止信号。

### 模块间通信

| 方式 | 用途 |
|------|------|
| `Arc<T>` 构造注入 | 服务间长期依赖 |
| channel (mpsc/oneshot) | 一次性命令/结果 |
| `Arc<ArcSwap<T>>` | 高频读低频写的共享快照（仅 ConfigService 的 RuntimeConfig） |

禁止跨模块直接访问内部状态或共享 `Mutex`；服务内部可用 `tokio::sync::Mutex` / `std::sync::Mutex`（按需 `into_inner` 处理 poison），见 `bridge` / `scheduler` / `status` 现状。

### Engine

单 tokio task，`tokio::select!` 等待：命令 channel > 网络检查定时器 > Profile 切换检测。登录重试由 LoginOrchestrator 内部管理。定时任务由独立的 SchedulerService 负责。

### Python Bridge

NDJSON IPC 协议：Rust 通过 stdin 发命令，Worker 通过 stdout 返结果。Bridge Supervisor 懒加载 Worker，空闲超时后关闭释放内存。Worker 崩溃不影响 Rust 控制平面。

### 前端嵌入

Vite 构建产物通过 `rust-embed` 编译进二进制。开发时可用 `--features no-embed` 跳过嵌入。

## 测试规范

```bash
# 全部测试
cargo test

# 指定模块
cargo test config::

# 带输出
cargo test -- --nocapture
```

- `tests/` 目录放集成测试（`assert_cmd` + `predicates`）
- `src/` 内 `#[cfg(test)] mod tests` 放单元测试
- 临时目录用 `tempfile`
- 异步测试用 `#[tokio::test]`，时间控制用 `tokio::time::pause()`

## Git 规范

### 分支策略

| 分支 | 用途 |
|------|------|
| `master` | 主分支，稳定版本 |

功能分支：`git checkout -b feat/my-feature`（从 master 创建）

### Commit Message 格式

Conventional Commits，中文描述：

```
<type>: <中文描述>
```

| Type | 含义 |
|------|------|
| `feat` | 新功能 |
| `fix` | 缺陷修复 |
| `refactor` | 重构 |
| `docs` | 文档变更 |
| `style` | 代码格式调整 |
| `test` | 测试相关 |
| `chore` | 构建/工具链/依赖 |
| `perf` | 性能优化 |
| `ci` | CI 配置变更 |

规则：
- 一次 commit 只做一件事
- 句末不加句号
- BREAKING CHANGE 加 `!`：`feat!: 重构配置结构`
- 不得添加 Co-authored-by 或任何 AI 相关标记

## 常见陷阱

### 编译与嵌入

- `rust-embed` 需要 `frontend/dist/` 存在才能编译。开发时用 `cargo check --features no-embed` 跳过
- Python Worker 的 **stdout 是 IPC 通道**，不能用于日志输出（用 stderr）

### 配置系统

- ConfigService 的 `ArcSwap<RuntimeConfig>` 是唯一的配置权威源，不要在其他服务缓存配置快照
- 读取配置：`config_service.runtime().load()`，不要自行持有 `Arc<RuntimeConfig>`

### 系统托盘

- `tray-icon` 只接受 raw RGBA 像素，需要用 `image` crate 解码 PNG 后传入
- 托盘事件循环需在独立线程运行（tray-icon 要求）

### 版本号

版本号在 `Cargo.toml` 的 `version` 字段。升级时同步修改：
1. `Cargo.toml` — `version = "x.x.x"`
2. `docs/changelog.md` — 新增版本条目
