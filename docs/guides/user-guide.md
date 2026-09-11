# 用户指南

> 适用于 Rust 重写版 `campus-auth`（`v5.0.0-alpha.8`，单 binary + Python Worker 子进程）。Python 版 `main.py` / `start.exe` / `update.exe` 已不在本仓库出现，本文已按当前实现重写。

## 1. 启动与命令行

可执行文件：`campus-auth`（Windows 为 `campus-auth.exe`，另有 `campus-auth-helper` 辅助更新替换，无需手动调用）。

```bash
# 完整模式（Web 控制台 + 托盘 + Engine，默认）
campus-auth

# 轻量模式（仅 Engine + 托盘，Web 按需启动；macOS 自动降级为完整模式）
campus-auth --mode lightweight

# 单次登录（执行活跃任务一次后退出）
campus-auth --mode login-once

# 查询 / 停止已运行实例
campus-auth --status
campus-auth --stop

# 强制抢占（终止已运行实例后启动）
campus-auth --force

# 开机自启
campus-auth --autostart enable
campus-auth --autostart disable
campus-auth --autostart        # 查询当前状态

# 覆盖监听与目录
campus-auth --port 50721 --host 127.0.0.1 --base-path D:\campus-auth-data
# 等价环境变量：CAMPUS_AUTH_PORT / CAMPUS_AUTH_HOST / CAMPUS_AUTH_BASE_PATH

# 启动后不自动打开浏览器 / 不显示托盘
campus-auth --no-browser
campus-auth --no-tray

# 启动动作覆盖（覆盖 settings.json 的 app.startup_action）
campus-auth --startup-action monitor      # 启动后进入监测
campus-auth --startup-action login_once
campus-auth --startup-action none
```

完整参数见 `campus-auth --help`（定义于 `src/launcher.rs::CliArgs`，实现于 `src/main.rs`）。

Windows release 为 GUI 子系统：双击 `campus-auth.exe` 不弹控制台，若已有实例在运行则直接在浏览器打开其 Web 控制台；从终端启动时会自动附着父控制台，`--status` / `--stop` 输出可见（`src/main.rs::attach_parent_console`）。

### 运行时目录

默认 `base_path` 为可执行文件所在目录；可用 `--base-path` / `CAMPUS_AUTH_BASE_PATH` 覆盖。目录结构：

```
<base_path>/
├── config/                  # settings.json + profiles/*.json + .auth_token（鉴权）
├── tasks/
│   ├── browser/             # 浏览器任务（*.json）
│   ├── scripts/             # 自定义脚本任务（browser/script/shell 的脚本类落此处）
│   └── scheduled/           # 定时任务调度历史等
├── logs/                    # 按日归档（受 logging.retention_days 控制）
├── environment/             # uv / .venv / Playwright 浏览器（按需生成）
└── update/                  # last_check.json（上次检查状态）+ staging/（下载暂存）
```

`settings.json` 为 v6 schema（`src/config/schema.rs`），`config_version` 字段驱动迁移；密码字段落盘为 `ENC:` 前缀密文（`aes-gcm` + `zeroize`）。

## 2. Web 控制台

地址：`http://127.0.0.1:50721`（`app.port`；本地端口被占用或被 Windows 保留时由系统自动分配可用端口，实际地址见启动日志；Docker 默认固定监听 `0.0.0.0:50721`）。首次启动走初始化向导，之后在「设置」页管理全部配置。

鉴权：启动时生成随机 token 持久化于 `config/.auth_token`（`0600`），前端经 `/api/auth/token` 懒取并在 `X-Auth-Token` / `Bearer` / `?token=` 中携带；`GET /api/health`、`GET /api/auth/token` 等少数端点豁免，其余 `/api/*` 与 `/ws/*` 强制校验（`src/web/auth.rs`）。

## 3. 多网络配置方案（Profiles）

入口：`GET /api/profiles` / `POST /api/profiles` / `GET /api/profiles/active`，前端为“配置方案”页。

- 每个 Profile 含 `auth_url`（认证页）、可选 `trigger_url`（重定向型门户，非空即重定向模式）、`username`/`password`（加密存储）、`isp`、`gateway_ip`/`wifi_ssid` 匹配规则与 `active_task`。
- 重定向模式：`trigger_url` 为明文 `http` 触发地址（如 `http://www.msftconnecttest.com/connecttest.txt`），Worker 首导航到该地址并跟随 302 到真门户，`{{LOGIN_URL}}` 同步为触发地址；监测跳过 `auth` TCP 探测、登录跳过预检，劫持判定优先于断网（`docs/guides/task-writing-guide.md` 重定向模式）。
- 匹配：按 `gateway_ip` 优先、其次 `wifi_ssid`（`src/config/profiles.rs`），约束数越多优先级越高；`auto_switch` 开启时 Engine 每 60s 检测并自动切换，切换后重置登录失败去重状态。
- `default` 为保底 Profile，不可删除。

## 4. 任务系统

### 三类任务

- **浏览器任务**（`tasks/browser/*.json`，`type=browser`）：Playwright 步骤序列，见《任务编写指南》。
- **脚本任务**（`tasks/scripts/*.json`，`type=script`）：`script_path` 或 `content` + `binary_path` + `args` + `work_dir` + `timeout`（`src/tasks/models.rs::ScriptTaskConfig`）。
- **Shell 任务**（同目录，`type=shell`）：`command` + `shell_path` + `timeout`（`ShellTaskConfig`）。

管理端点：`GET /api/tasks`、`POST /api/tasks`、`GET/PUT/DELETE /api/tasks/{id}`、`POST /api/tasks/order`、`POST /api/tasks/import`、`GET /api/tasks/export/{id}`、`POST /api/tasks/active/{id}`、`POST /api/tasks/{id}/execute`（通用，浏览器/脚本/Shell 均走 `TaskExecutor::execute`）；`GET /api/scripts` / `/api/shells` 为同数据在脚本面板的视图过滤（见 `docs/guides/task-manual.md`、`docs/guides/custom-script-guide.md`）。

### 日常操作

- **任务管理 / 设置·任务**：新建、编辑、复制、删除、排序、导入/导出单个任务；将某个任务设为活跃任务（`POST /api/tasks/active/{id}`）。
- **定时任务**：独立页，按 cron 调度浏览器任务（`src/scheduler`，状态在 `tasks/scheduled/`）。
- **何时执行**：网络监测 Offline/Captive 时自动执行活跃任务；仪表盘“登录”按钮（`POST /api/login`）、“执行指定任务”（`POST /api/tasks/{id}/execute`）为手动触发。

### 录制器：不手写 JSON

1. 安装 Tampermonkey；
2. 在「设置·任务」页「安装录制器脚本」；
3. 打开校园网登录页，点浮动按钮开始录制，按提示点选账号框、密码框、验证码、登录按钮等；
4. 结束录制后保存为任务并设为活跃任务验证一次（`resources/tools/task-recorder.user.js`）。

## 5. 浏览器自动化与调试

- 环境就绪分四层判断：Python 解释器能启动、Worker/Playwright 包能真实导入、当前 pyproject/uv.lock 指纹已验证、存在 Playwright Chromium 或可用的系统 Edge/Chrome。已有系统浏览器只会免去 Chromium 下载，不会跳过 Python 包检查。
- 每次真正拉起 Worker 前都会重新探测；若依赖缺失或清单变化，会先执行 `uv sync` 并复验。首次启动/健康检查仍失败时，当前请求会强制修复并重试一次，连续失败才进入熔断。
- Playwright 渠道：`msedge`（默认）、`chromium`、`chrome`、`firefox`、`webkit`，支持自定义可执行文件路径与 `browser_args`（每行一个，`#` 注释，Worker 侧过滤敏感参数）。
- 调试：`POST /api/debug/start`（前置环境就绪检查，缺失自动引导）、`POST /api/debug/step` / `stop` / `run_all`；前端调试面板单步执行并展示 `steps`，支持导出反馈包（含截图、MHTML、日志）。
- 反馈包：`POST /api/debug/capture` 采集页面快照与日志，打包 `debug/` 归档。

## 6. 验证码（OCR）

- 仅 `ocr` 步骤需要；依赖 `ddddocr`（约 120MB，不预声明，用时经应用内安装，用完可卸载）。
- 在「设置·环境」页安装，装好后可用“验证码识别”上传截图试识别；OCR 偏好独立保存并通过 `uv add/remove` 对齐，安装失败不会阻断非 OCR 浏览器任务，也不会为了 OCR 单独下载 Chromium。

## 7. 系统托盘与开机自启

- 托盘常驻操作：打开控制台、查看状态、退出；轻量模式支持按需唤醒 Web 控制台（`src/tray`）。
- macOS：托盘按用户决策禁用（`tray-icon` 要求主线程 NSApplication 事件循环，与 tokio 冲突），轻量模式自动降级为完整模式，Web 入口仍可用。
- Linux：依赖 GTK3 / libayatana-appindicator（`TrayManager::spawn` 内 `gtk::init` + glib 主循环），无桌面环境时托盘不启动但 Web 仍可用。
- 开机自启：`--autostart enable/disable`（`src/utils/platform` 三端实现；Windows 为计划任务/VBS，macOS 为 LaunchAgent，Linux 为 systemd/autostart）。

## 8. AI 任务生成

入口：设置页 AI 任务生成（`GET /api/ai/llm-config` 读配置、`PUT /api/ai/llm-config` 保存、`POST /api/ai/capture` 捕获页面、`POST /api/ai/generate/stream` 流式生成）。

- 配置文件：`<base>/config/llm.json`（`api_keys_enc` 按服务商加密落盘，另含 `provider` / `base_url` / `model` / `max_tokens`，见 `src/ai/mod.rs`）；
- 配置：先选择服务商再填写模型与 Key；不同服务商的 Key 各自加密保存，切换服务商会自动使用对应 Key。最长输出默认 16K，复杂页面可选 32K；
- 流程：`POST /api/ai/capture` 经 Bridge 触发 `page_capture` 落 `captures/latest/`（MHTML + 原始 HTML + 结构化控件摘要 + 脱敏局部 HTML + 资源 + 截图），视觉模型优先读取结构摘要和局部 HTML，再生成浏览器任务 JSON，经 AI 安全规则与 `validate_task` 双重校验后回喂自纠（`src/ai/generate.rs`）；
- 依赖：需先经 `page_capture` 捕获页面，生成失败可在前端预览/编辑后走 `POST /api/tasks/import` 入库。

## 9. 自动更新与通道

入口：关于页「检查更新」（`GET /api/check-update` 按通道拉清单，`GET /api/update-state` 回放上次检查时间）与「立即更新」（`POST /api/system/update`）。

- 通道（`config.global.updater.channel`）：`stable` 仅正式版（`releases/latest` 单包语义）、`prerelease` 仅预发布、`all` 正式+预发布一起按 semver 取最高；`all` 在 releases 列表为空时回退单包口径（`src/updater/check.rs::fetch_manifest_for_channel`）。
- 总开关：`auto_check_enabled` 关闭后后台循环与启动检查均静默，仅保留手动检查；`check_interval_hours==0` 仅做启动检查（`src/updater/mod.rs` 的 `due_now` 语义）。
- 状态落盘：每次检查无论成败均刷新 `update/last_check.json`（UTC RFC3339，`last_check_at`/`has_update`/`latest_version`/`error`），前端据此展示“上次检查”。
- 代理：显式 `proxy_url`（支持非本机）优先，回退旧 `proxy_port` 兼容（`resolved_proxy_url()`）；监测与更新代理解耦（`monitor.disable_proxy` 默认直连）。

镜像目录：`~/.cache/campus-auth`（XDG）或项目内 `environment/`，更新 staging 为 `update/staging/`，helper 以 `campus-auth-helper` 完成自替换。

## 10. 常见问题

### Playwright / Chromium 下载失败

项目经 `uv` 与多镜像（`npmmirror` / 清华 PyPI）尝试下载；失败时可在「设置·环境」查看分层状态并执行“重新同步”，或检查 `environment/` 权限与代理设置。Docker 镜像构建时已预装 Chromium，宿主机部署按需等待首次下载完成。

### 服务提示已启动

```bash
campus-auth --status
campus-auth --stop
campus-auth --force   # 终止后抢占
```

### 认证不成功

1. 账号/密码是否正确（`ENC:` 解密后注入 `{{USERNAME}}`/`{{PASSWORD}}`）；
2. `auth_url` / `trigger_url` 是否可达（劫持型门户须用 `http` 触发地址）；
3. `isp` 是否匹配；
4. 在 Web 控制台看实时日志与失败截图；
5. 临时关闭无头模式观察页面行为（`browser.headless`）。

### 日志不显示或有延迟

确认后端在运行、浏览器 WebSocket 已连上（`/ws/logs`、`/ws/status`），刷新后重新订阅；开发期 `cargo run --features no-embed` 与 `frontend npm run dev` 需分别启动。

### 多个校园网怎么配置

在“配置方案”页为每个网络创建 Profile，填 `gateway_ip` / `wifi_ssid` 匹配条件并开启 `auto_switch`；为各 Profile 分别绑定 `active_task`，而非为每环境各写一套任务 JSON。

### 保存任务时弹出安全警告

`eval` / `custom_js` 步骤可执行任意 JS，系统会展示代码要求确认，确认安全后保存。

### 自启动被拦截

Windows 自启动为计划任务，部分杀毒软件可能拦截，建议将 `campus-auth.exe` 加入白名单后重试 `--autostart enable`。

## 11. 相关文档

- [任务编写指南](task-writing-guide.md) — 步骤类型、变量、frame、success_condition、选择器建议
- [任务使用手册](task-manual.md) — 日常管理、录制器、调试
- [自定义脚本指南](custom-script-guide.md) — `script` / `shell` 三类任务与 `POST /api/scripts/run`
- [项目结构与架构](../../AGENTS.md) — ServiceContainer 15 字段、Updater 通道、Bridge 协议
- [更新日志](../updatelog.md) · [更改日志](../changelog.md) · [已知问题](../known-issues.md)
