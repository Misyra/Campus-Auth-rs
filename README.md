# Campus-Auth 校园网自动认证工具

校园网（captive portal）自动认证工具。检测到需要认证时自动打开认证页面并登录，
支持多 Profile 自动匹配、定时任务、验证码 OCR、系统托盘与 Web 控制台。

Rust 重写版为便携式单二进制 + Python 子进程（浏览器自动化），解压即用。

## 特性

- **自动认证**：断网/被劫持时自动触发登录，支持重试、冷却与失败去重提醒
- **多 Profile**：按网关 IP / WiFi SSID 自动匹配，支持手动切换与优先级排序
- **浏览器自动化**：Playwright 驱动，支持完整的步骤序列（导航 / 填表 / 点击 / 验证码 / 断言）
- **验证码识别**：OCR 识别登录验证码，识别失败自动重试整个流程
- **定时任务**：cron 表达式调度，支持打卡签到等日常自动化
- **Web 控制台**：内置 Web UI（Vue 3），支持状态查看、任务编辑、日志与实时 WebSocket
- **系统托盘**：常驻托盘，一键启动/停止监测、打开控制台、退出
- **AI 任务生成**：视觉模型按捕获页面自动生成浏览器任务（`POST /api/ai/capture` → `POST /api/ai/generate`，见设置页）
- **自动更新**：版本检查与增量更新

## 快速开始

### 便携版

1. 从 Release 下载便携包并解压
2. 双击 `campus-auth.exe` 启动（或命令行运行）
3. 首次启动在系统托盘或 Web 控制台 `http://127.0.0.1:50721` 配置学校认证信息
4. Python Worker（Playwright）首次需要时自动修复；验证码任务再到「设置·环境」按需启用 OCR

### 从源码构建

```bash
# 编译（需要 frontend/dist 存在，或先用 --features no-embed 跳过）
cargo build

# 前端构建（生成 frontend/dist 供嵌入）
cd frontend && npm install && npm run build && cd ..

# 运行
cargo run
```

> 要求：Rust 1.85+（Edition 2024，源码编译最低版本）、Node.js（构建前端时）。仓库通过 `rust-toolchain.toml` 固定 1.98 构建工具链，装有 rustup 时本地与 CI 自动采用该版本，自编译环境只需满足 1.85+ 即可。

### Docker 部署

```bash
# 拉取预构建镜像并启动（后台）
docker compose pull
docker compose up -d

# 日志与健康检查
docker compose logs -f
curl http://localhost:50721/api/health
```

Web 控制台 `http://localhost:50721`，数据持久化于命名卷 `campus-auth-data`（`config/` / `tasks/` / `logs/`）。

默认拉取 `ghcr.io/misyra/campus-auth-rs:prerelease` 多架构镜像。需要固定版本时设置
`CAMPUS_AUTH_IMAGE=ghcr.io/misyra/campus-auth-rs:v5.0.0-alpha.10`；需要从当前源码构建时使用：

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```
宿主机目录挂载、host 网络等进阶用法见 [docker/README.md](docker/README.md)。

```bash
# 仅用 Docker CLI（不使用 compose）
docker build -t campus-auth .
docker run -d --name campus-auth -p 50721:50721 -v campus-auth-data:/data campus-auth
```

## 使用说明

- **Web 控制台**：默认 `http://127.0.0.1:50721`（端口冲突自动 +1 重试，`CAMPUS_AUTH_PORT` / `--port` 可覆盖）
- **Profile**：每个 Profile 含认证页 URL（`auth_url`）与可选劫持触发地址（`trigger_url`，非空即重定向模式）、用户名/密码（AES-256-GCM 加密落盘）、网关/SSID 匹配与 `active_task`
- **任务**：三类 `type`（`browser` 浏览器自动化 / `script` 自定义脚本 / `shell` Shell 命令），定时任务为浏览器任务的 cron 调度视图；API 统一为 `GET/POST /api/tasks`、`POST /api/scripts/run`、`GET /api/shells`
- **单次登录**：`campus-auth --mode login-once` 执行一次活跃任务后退出；`--status` / `--stop` / `--autostart` 见 `campus-auth --help`（`--mode` 可选值：`full` / `lightweight` / `login-once`，见 `campus-auth --help`）
- **更新通道**：设置页 `updater.channel`（`stable` 正式版 / `prerelease` 测试版 / `all` 全通道最新），`auto_check_enabled` 为总开关，`GET /api/update-state` 回放上次检查时间

## 项目结构

```
campus-auth/
├── src/                 # Rust 控制平面（监测 / 登录 / 调度 / 配置 / Web / 托盘）
├── frontend/            # Vue 3 + TypeScript + Vite Web 控制台
├── python_worker/       # Python Worker 子进程（Playwright + OCR）
├── tests/               # Rust 集成测试
├── docs/                # 文档（changelog / 已知问题 / 任务编写指南 / plan-next 活跃计划 / archive 归档，AI 见设置页与 src/ai）
├── resources/           # 静态资源（图标 / 脚本）
└── openapi.json         # Web API 契约
```

更多技术细节（架构、模块划分、开发规范、测试）见 [AGENTS.md](AGENTS.md)。

## 贡献

- 提交规范：Conventional Commits，中文描述（详见 [AGENTS.md](AGENTS.md) 的 Git 规范）
- 开发命令：`cargo build` / `cargo test` / `cargo clippy -- -D warnings` / `cargo fmt`
- CI：`cargo fmt --check` + `clippy --all-targets -D warnings` + `cargo test`（含 `rust-tests-unix` 与 `e2e-login-chain` mock→二进制→Worker 全链路）+ 前端构建（`vue-tsc` + `vite build`）+ `vitest` + `compileall` + `uv run pytest`（见 `.github/workflows/ci.yml`）
- 面向用户的版本更新见 [docs/updatelog.md](docs/updatelog.md)，逐项开发更改见 [docs/changelog.md](docs/changelog.md)，已知问题见 [docs/known-issues.md](docs/known-issues.md)

## 许可证

本项目为个人开源工具，仅用于合法的校园网身份认证场景。请遵守校园网络使用规范。
