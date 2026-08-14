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
- **自动更新**：版本检查与增量更新

## 快速开始

### 便携版

1. 从 Release 下载便携包并解压
2. 双击 `campus-auth.exe` 启动（或命令行运行）
3. 首次启动在系统托盘或 Web 控制台 `http://127.0.0.1:50721` 配置学校认证信息
4. Python Worker（Playwright / OCR）按需自动安装，无需手动配置

### 从源码构建

```bash
# 编译（需要 frontend/dist 存在，或先用 --features no-embed 跳过）
cargo build

# 前端构建（生成 frontend/dist 供嵌入）
cd frontend && npm install && npm run build && cd ..

# 运行
cargo run
```

> 要求：Rust 1.85+（Edition 2024）、Node.js（构建前端时）。

## 使用说明

- **Web 控制台**：默认 `http://127.0.0.1:50721`（端口冲突自动 +1 重试）
- **Profile**：每个 Profile 含认证页 URL、用户名、密码、网关/SSID 匹配规则与认证任务
- **任务**：底层认证任务与定时任务分开管理，定时浏览器任务用于打卡/签到等日常自动化
- **单次登录**：`campus-auth --mode once` 执行一次登录后退出；`--status` / `--stop` 管理后台实例

## 项目结构

```
campus-auth/
├── src/                 # Rust 控制平面（监测 / 登录 / 调度 / 配置 / Web / 托盘）
├── frontend/            # Vue 3 + TypeScript + Vite Web 控制台
├── python_worker/       # Python Worker 子进程（Playwright + OCR）
├── tests/               # Rust 集成测试
├── docs/                # 文档（changelog / 已知问题 / 任务编写指南）
├── resources/           # 静态资源（图标 / 脚本）
└── openapi.json         # Web API 契约
```

更多技术细节（架构、模块划分、开发规范、测试）见 [AGENTS.md](AGENTS.md)。

## 贡献

- 提交规范：Conventional Commits，中文描述（详见 [AGENTS.md](AGENTS.md) 的 Git 规范）
- 开发命令：`cargo build` / `cargo test` / `cargo clippy -- -D warnings` / `cargo fmt`
- CI：fmt + clippy（`-D warnings`）+ 全量测试 + 前端构建 + Python 语法检查
- 变更记录见 [docs/changelog.md](docs/changelog.md)，已知问题见 [docs/known-issues.md](docs/known-issues.md)

## 许可证

本项目为个人开源工具，仅用于合法的校园网身份认证场景。请遵守校园网络使用规范。