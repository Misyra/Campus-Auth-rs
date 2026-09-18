# 用户指南

> 适用于 Rust 重写版 `campus-auth`（`v5.0.0`，单 binary + Python Worker 子进程）。Python 版 `main.py` / `start.exe` / `update.exe` 已不在本仓库出现，本文已按当前实现重写。

## 1. 启动与命令行

可执行文件：`campus-auth`（Windows 为 `campus-auth.exe`，另有 `campus-auth-helper` 辅助更新替换，无需手动调用）。

```bash
# 完整模式（Web 控制台 + 托盘 + Engine，默认）
campus-auth

# 轻量模式（仅 Engine + 托盘，Web 按需启动；macOS 自动降级为完整模式）
campus-auth --mode lightweight

# 单次登录（执行当前方案绑定的任务一次后退出）
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
campus-auth --startup-action none         # 默认：启动后待机，等控制台点「启动检测」
```

> **`app.startup_action` 默认 `none`、`auto_switch` 默认 `false`**（2026-09-16 起）：新安装不自动开始检测，也不自动切换方案。两者都只影响**缺失该字段的新配置**——磁盘上已写的值优先，升级不会改动既有设置。需要「开机即自动重连」请在「设置 · 系统」把「启动后执行」改为「开始检测」；需要多网络自动切换在「方案」页开启。注意开机自启注册的命令行不带 `--startup-action`，故自启场景下启动动作完全取自该配置项。

完整参数见 `campus-auth --help`（定义于 `src/launcher.rs::CliArgs`，实现于 `src/main.rs`）。

Windows release 为 GUI 子系统：双击 `campus-auth.exe` 不弹控制台，若已有实例在运行则直接在浏览器打开其 Web 控制台；从终端启动时会自动附着父控制台，`--status` / `--stop` 输出可见（`src/main.rs::attach_parent_console`）。

### 运行时目录

默认 `base_path` 为可执行文件所在目录；可用 `--base-path` / `CAMPUS_AUTH_BASE_PATH` 覆盖。目录结构：

```
<base_path>/
├── config/                  # settings.json + profiles/*.json + .auth_token（鉴权）+ llm.json
├── tasks/
│   ├── browser/             # 浏览器任务（*.json）
│   ├── scripts/             # 脚本任务（type=script）
│   └── scheduled/           # 定时任务调度历史（history/）
├── logs/                    # 按日归档（受 logging.retention_days 控制，默认 7 天）
│   └── login_history/       # 登录历史
├── environment/             # uv 可执行文件 + 状态文件（python-runtime-state.json / python-preferences.json / ocr.enabled）
├── python_worker/           # Worker 工程目录：源码 + .venv（虚拟环境）+ captures/（AI 页面捕获）+ debug/（调试快照）
└── update/                  # last_check.json（上次检查状态）+ staging/（下载暂存）
```

> `.venv` 与 `captures/` / `debug/` 都在 `python_worker/` 下，**不在** `environment/`；`environment/` 只放 uv 与运行时状态。Playwright 浏览器放在各自平台的默认缓存（Windows `%LOCALAPPDATA%\ms-playwright`，macOS `~/Library/Caches/ms-playwright`，Linux `~/.cache/ms-playwright`），仅 Docker 通过 `PLAYWRIGHT_BROWSERS_PATH=/ms-playwright` 改到镜像内。

`settings.json` 为 v9 schema（`src/config/mod.rs::CURRENT_CONFIG_VERSION`，字段定义见 `src/config/schema.rs`），`config_version` 字段驱动迁移；密码字段落盘为 `ENC:` 前缀密文（`aes-gcm` + `zeroize`）。

## 2. Web 控制台

地址：`http://127.0.0.1:50721`（`app.port`；本地端口被占用或被 Windows 保留时由系统自动分配可用端口，实际地址见启动日志；Docker 默认固定监听 `0.0.0.0:50721`）。首次启动走初始化向导。

界面分五处，每处只编辑一类数据（避免同一份配置有多个可写入口）：

| 导航 | 编辑对象 | 存储 / 接口 |
|------|----------|-------------|
| 仪表盘 | —（状态总览与手动操作） | — |
| **方案** | 账号、密码、认证地址、匹配规则、登录方式、直连参数 | `config/profiles/*.json`，`/api/profiles/*` |
| **任务** | 浏览器任务 / 脚本 / 定时任务 / AI 生成浏览器任务 | `tasks/`，`/api/tasks`、`/api/scripts`、`/api/scheduler/jobs` |
| **设置** | 检测 / 浏览器 / 任务与环境 / 系统 / 网络与更新 / 外观 | `config/settings.json`，`/api/config` |
| 关于 | —（版本、更新与卸载） | — |

> 账号属于**方案**而非全局设置：登录时使用的是「活跃方案」的账号，切换方案即切换账号。因此填账号请到「方案」页。

### 运行模式（设置 · 系统）

把一组相关设置打包成两个预设，一键切换；手动改过其中任一设置后自动显示为「自定义」。

| | 默认模式 | 调试模式 |
|---|---|---|
| 浏览器后台运行 | 开启 | 关闭（看得见浏览器窗口） |
| 登录后保持浏览器进程 | 关闭 | 开启（便于反复查看现场） |
| 启动后执行 | 开始检测 | 无操作（手动跑一次才看得见过程） |
| 日志级别 | INFO | DEBUG |
| 开机自启动 | 开启 | 关闭 |
| 低资源模式 | 关闭 | 关闭（两组取值相同，切换时不会列入改动清单） |
| 启用暂停时段 | 开启 | 关闭（随时手动复现不被拦截） |

> 暂停时段只切换**启用开关**，不覆盖起止时间（默认 23:00–06:00，用户可自调）；上表与 `frontend/src/utils/runMode.ts` 的 `RunModeSettings` 一一对应。

切换前会列出**实际将要改动**的项（未变化的不列），确认后才执行。实现要点：多数项（浏览器 / 保持进程 / 启动后执行 / 暂停开关）经 `PATCH /api/config` 一次提交；**日志级别必须走 `PUT /api/config/log-level`**（仅 PATCH 只落盘、不热更新 tracing filter，会出现"界面显示 DEBUG、实际按 INFO 过滤"的假象）；**开机自启走 `POST /api/autostart/*`，Windows 下会真实写注册表**（macOS 写 LaunchAgent plist、Linux 写 XDG desktop 文件）。故三者非原子——任一步失败会提示"部分设置可能已生效，请检查后重试"。

刻意**不含 `strict_login_mode`**：它决定"何时触发登录"，属功能行为而非可观测性；把它放进调试模式会在证据不足时也尝试登录，可能在没预期的时机拉起浏览器。

鉴权：启动时生成随机 token 持久化于 `config/.auth_token`（`0600`），前端经 `/api/auth/token` 懒取并在 `X-Auth-Token` / `Bearer` / `?token=` 中携带；`GET /api/health`、`GET /api/auth/token` 等少数端点豁免，其余 `/api/*` 与 `/ws/*` 强制校验（`src/web/auth.rs`）。

## 3. 多网络方案（Profiles）

入口：`GET /api/profiles`（列表，响应含 `active_profile` / `auto_switch`）/ `POST /api/profiles/{id}`（新建）/ `GET /api/profiles/{id}`（响应含 `has_password`），切换活跃方案用 `POST /api/profiles/switch`，更新用 `PUT /api/profiles/{id}`（可用 `clear_password: true` 显式清除已保存密码），前端为「方案」页。

「方案」页进入时**直接展示当前活跃方案**的编辑器（改账号是这一页最高频的用途）；顶栏下拉可切换方案，「当前使用」徽标标出自动登录实际使用的那个。点「返回方案列表」查看或新建其它方案。

- 每个 Profile 含可选 `auth_url`（固定登录网址）、可选 `trigger_url`（自定义重定向触发地址）、`username`/`password`（加密存储）、`isp`、`gateway_ip`/`wifi_ssid` 匹配规则、`active_task`（本方案用哪个浏览器任务，留空回退内置 `default`）与登录方式（浏览器自动化 / 直连请求，后者见 `docs/guides/http-login-guide.md`）。
- 这些字段**只在「方案」页编辑**；`GET /api/config` 顶层仍会扁平回传活跃方案的凭据（兼容既有客户端），但界面已不再从那里读写。
- 浏览器登录网址**填写即直接使用，留空即跟随重定向**。留空时默认访问 `http://www.msftconnecttest.com/connecttest.txt`，可在“重定向高级设置”用 `trigger_url` 覆盖；触发地址必须为明文 `http`。Worker 首导航到触发地址并跟随门户跳转，`{{LOGIN_URL}}` 同步为触发地址；若常规公网探测全失败但本地网卡已连接，会谨慎启动一次浏览器触发门户，而不是一直显示“没网”（`docs/guides/task-writing-guide.md` 重定向登录）。
- 匹配：按 `gateway_ip` 优先、其次 `wifi_ssid`（`src/config/profiles.rs`），约束数越多优先级越高（无用户可配的 `priority` 字段）；`auto_switch` 开启时 Engine 按 `monitor.profile_check_interval` 周期检测并自动切换（默认 **180 秒**，可配范围 60–600），切换后重置登录失败去重状态。**`auto_switch` 默认关闭**（2026-09-16 起，新配置生效）；关闭时方案页卡片可直接点击切换，开启时改由自动匹配决定（卡片不可手点）。
- `default` 为保底 Profile，不可删除。

## 4. 任务系统

### 两类任务

- **浏览器任务**（`tasks/browser/*.json`，`type=browser`）：Playwright 步骤序列，见《任务编写指南》。**校园网自动登录使用的就是这一类**。
- **脚本任务**（`tasks/scripts/*.json`，`type=script`）：`script_path` 或 `content` + `binary_path` + `args` + `work_dir` + `timeout`（`src/tasks/models.rs::ScriptTaskConfig`）。用于定时执行的辅助动作（打卡、签到等），**不参与登录认证**。

> 历史 `type=shell` 已移除：遇到时反序列化明确报错并提示改用 `script`（`src/tasks/models.rs`）。同目录下曾有的 `shell` 任务需改写为 `.sh`/`.bat`/`.py` 脚本经 `binary_path` 执行。

管理端点：`GET /api/tasks`、`POST /api/tasks`、`GET/PUT/DELETE /api/tasks/{id}`、`POST /api/tasks/order`、`POST /api/tasks/import`、`GET /api/tasks/export/{id}`、`POST /api/tasks/{id}/execute`（通用，浏览器/脚本均走 `TaskExecutor::execute`）；脚本面板复用上述 `tasks` 端点并另接 `GET /api/scripts/binaries`、`GET/PUT/DELETE /api/scripts/{id}`、`POST /api/scripts/run`（见 `docs/guides/task-manual.md`、`docs/guides/custom-script-guide.md`）。
「用哪个浏览器任务」由各方案的 `active_task` 决定（在「方案」页的方案编辑器「登录方式」里选），没有全局端点。

### 日常操作

- **任务**：新建、编辑、复制、删除、排序、导入/导出单个任务。「任务」页分「浏览器任务」「脚本」「定时任务」「AI 生成浏览器任务」四个标签页，**只管编辑**；用哪个任务登录由方案决定（见下）。
- **定时任务**：「任务」页的「定时任务」标签页，按 cron 调度**浏览器与脚本两类**任务（`src/scheduler`，状态在 `tasks/scheduled/`；创建时按 `target_id` 关联任务，类型由任务本体推导）。
- **何时执行**：网络监测 Offline/Captive 时自动执行当前方案绑定的浏览器任务；仪表盘“登录”按钮（`POST /api/login`）、“执行指定任务”（`POST /api/tasks/{id}/execute`）为手动触发。

### 录制器：不手写 JSON

录制器只负责把你点选的元素整理成 **AI 提示词**，任务 JSON 由大模型生成后再导入。

1. 安装 Tampermonkey；
2. 在「设置 · 任务与环境」页「安装录制器脚本」；
3. 打开校园网登录页，点浮动按钮开始录制，按提示点选账号框、密码框、验证码、登录按钮等；
4. 点`📋 复制 AI 提示词`，粘贴给大模型生成任务 JSON（也可用「AI 生成浏览器任务」页完成）；
5. 把任务导入「任务」页，再到「方案」页的方案编辑器「登录方式」里为当前方案选中它，验证一次（`resources/tools/task-recorder.user.js`）。

## 5. 浏览器自动化与调试

- 环境就绪分四层判断：Python 解释器能启动、Worker/Playwright 包能真实导入、当前 pyproject/uv.lock 指纹已验证、存在 Playwright Chromium 或可用的系统 Edge/Chrome。已有系统浏览器只会免去 Chromium 下载，不会跳过 Python 包检查。
- 每次真正拉起 Worker 前都会重新探测；若依赖缺失或清单变化，会先执行 `uv sync` 并复验。首次启动/健康检查仍失败时，当前请求会强制修复并重试一次，连续失败才进入熔断。
- Playwright 渠道：`msedge`（默认）、`chromium`、`chrome`、`firefox`、`webkit`，支持自定义可执行文件路径与 `browser_args`（每行一个，`#` 注释，Worker 侧过滤敏感参数）。
- 调试：`POST /api/debug/start`（前置环境就绪检查，缺失自动引导）、`POST /api/debug/step` / `stop` / `run_all`；前端调试面板单步执行并展示 `steps`，支持导出反馈包（含截图、MHTML、日志）。
- 反馈包：`POST /api/debug/feedback-bundle` 采集页面快照与日志，打包 `debug/` 归档。

## 6. 验证码（OCR）

- 仅 `ocr` 步骤需要；依赖 `ddddocr`（约 120MB，不预声明，用时经应用内安装，用完可卸载）。
- 在「设置 · 任务与环境」页安装，装好后可用“验证码识别”上传截图试识别；OCR 偏好独立保存并通过 `uv add/remove` 对齐，安装失败不会阻断非 OCR 浏览器任务，也不会为了 OCR 单独下载 Chromium。

## 7. 系统托盘与开机自启

- 托盘菜单自上而下：**状态**（引擎 · 网络，登录进行中时追加登录态；只读信息行，引擎运行中为正常深色、未运行时灰化）、分隔线、**启动监测 / 停止监测**（随引擎状态切换文本）、**手动登录**（等同控制台「手动登录」按钮，来源标记为手动）、**打开控制台**、**退出**；轻量模式支持按需唤醒 Web 控制台。左键单击托盘图标直接打开控制台（`src/tray`）。托盘不再提供更新检查入口，更新走关于页或设置 · 网络与更新。
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

更新 staging 为 `update/staging/`（相对 `base_path`），helper 以 `campus-auth-helper` 完成自替换；uv 与 Python 运行时状态落在 `environment/`，Worker 虚拟环境在 `python_worker/.venv`。

## 10. 常见问题

### Playwright / Chromium 下载失败

项目经 `uv` 与多镜像（`npmmirror` / 清华 PyPI）尝试下载；失败时可在「设置 · 任务与环境」查看分层状态并执行“重新同步”，或检查 `environment/` 权限与代理设置。Docker 镜像构建时已预装 Chromium，宿主机部署按需等待首次下载完成。

### 服务提示已启动

```bash
campus-auth --status
campus-auth --stop
campus-auth --force   # 终止后抢占
```

### 认证不成功

1. 账号/密码是否正确（`ENC:` 解密后注入 `{{USERNAME}}`/`{{PASSWORD}}`）；
2. 若填写了固定登录网址，确认 `auth_url` 可达；若留空跟随重定向，确认自定义 `trigger_url`（如有）使用明文 `http`，也可直接恢复为空以使用默认触发地址；
3. `isp` 是否匹配；
4. 在 Web 控制台看实时日志与失败截图；
5. 临时关闭无头模式观察页面行为（`browser.headless`）。

### 日志不显示或有延迟

确认后端在运行、浏览器 WebSocket 已连上（`/ws/logs`、`/ws/status`），刷新后重新订阅；开发期 `cargo run --features no-embed` 与 `frontend npm run dev` 需分别启动。

### 多个校园网怎么配置

在「方案」页为每个网络创建 Profile，填 `gateway_ip` / `wifi_ssid` 匹配条件并开启 `auto_switch`；再为各 Profile 分别选择**浏览器任务**（方案编辑器的「登录方式」里选），而非为每环境各写一套任务 JSON。切换方案会连同任务一起切换。

### 保存任务时弹出安全警告

`eval` / `custom_js` 步骤可执行任意 JS，系统会展示代码要求确认，确认安全后保存。

### 自启动被拦截

Windows 自启动为计划任务，部分杀毒软件可能拦截，建议将 `campus-auth.exe` 加入白名单后重试 `--autostart enable`。

## 11. 相关文档

- [直连请求登录使用指南](http-login-guide.md) — 免 Python / 浏览器的 HTTP 直连登录（抓门户请求、占位符、成败判定、凭据变换脚本）
- [任务编写指南](task-writing-guide.md) — 步骤类型、变量、frame、success_condition、选择器建议
- [任务使用手册](task-manual.md) — 日常管理、录制器、调试
- [自定义脚本指南](custom-script-guide.md) — `script` 任务与 `POST /api/scripts/run`
- [项目结构与架构](../../AGENTS.md) — ServiceContainer 15 字段、Updater 通道、Bridge 协议
- [更新日志](../updatelog.md) · [更改日志](../changelog.md) · [已知问题](../known-issues.md)
