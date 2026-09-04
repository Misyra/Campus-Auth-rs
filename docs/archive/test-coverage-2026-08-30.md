# 功能测试覆盖清单（2026-08-30）

> 配套文档：[bug-report-2026-08-30.md](bug-report-2026-08-30.md)（缺陷详情）。
> 测试环境：`tests/mock-servers/full-portal/server.py`（原 `mock_portal/server.py`，已于 2026-09-03 搬迁，`mock_portal/README.md` 为兼容重定向）模拟认证门户（127.0.0.1:18765，带图片验证码 + captive 语义）；
> 主实例 `target/debug/campus-auth.exe --no-tray`（127.0.0.1:50721）；全新安装实例（临时目录 + 50733）；
> 假更新源（127.0.0.1:18699，自定 latest.json 格式）。GUI 黑盒操作使用 ZCode 内置浏览器。

---

## 一、已测试清单（29 项）

### 登录与状态机（核心链路）

| # | 项目 | 结果 |
|---|------|------|
| 1 | 手动登录（API 编排路径，含验证码 OCR 识别填入） | ✅ 9.3s 成功，OCR 6/6 一次命中 |
| 2 | 掉线自动重连（captive → 引擎自动登录 → online） | ✅ 两次实测，≤15s 恢复 |
| 3 | 登录失败重试（验证码错误 → 重试 → 成功） | ✅ 强制失败后 39s 重试成功，`retry=1` |
| 4 | 取消登录（重试等待期 cancel） | ✅ `cancelled / 用户取消` |
| 5 | 一次性登录 `POST /api/login/once` | ✅ 同步返回 `once:true, success:true` |
| 6 | 登录失败冷却（连续 3 次失败 → 300s 冷却） | ✅（日志证据） |
| 7 | 登录后网络验证（post-login probe） | ✅ "登录后网络验证通过：Online" |
| 8 | Worker 崩溃恢复（强杀 python worker → 自动 respawn） | ✅ state=error → 重新拉起 → 登录成功 |

### 网络监测

| # | 项目 | 结果 |
|---|------|------|
| 9 | HTTP 探测（204=Pass / 200=Captive 语义） | ✅ |
| 10 | TCP 探测（含 race 语义：开放+关闭端口混合 → Pass） | ✅ `tcp=Pass` |
| 11 | URL 内容探测（url_expected_responses 匹配） | ✅ `url=Pass` |
| 12 | 监测状态翻转与上报（offline ↔ online、probe 计数） | ✅ 223+ 次探测无漏发 |
| 13 | 登录前 auth_url 可达性预检 | ✅（探测日志 `auth_url=reachable`） |

### Profile（配置方案）

| # | 项目 | 结果 |
|---|------|------|
| 14 | 创建 / 删除 / 列表 | ✅ |
| 15 | 切换（凭证随 active profile 生效：user/auth_url 跟随） | ✅ |
| 16 | 网络检测 detect（真实网关 IP + SSID 采集、匹配规则） | ✅ |
| 17 | 自动切换开关 auto-switch | ✅ |

### 任务系统

| # | 项目 | 结果 |
|---|------|------|
| 18 | 浏览器任务执行（登录编排路径） | ✅（变量模板见 BUG-3） |
| 19 | Shell 任务执行 | ✅ exit 0 |
| 20 | 脚本任务执行（内联 python，.venv 自动引导） | ✅ |
| 21 | 任务导入（含校验：空 steps 拒绝并记入 failed） | ✅ |
| 22 | 任务导出 | ✅ |
| 23 | 任务排序（`/api/tasks/order`） | ✅ 顺序落盘 |
| 24 | 自定义脚本路由（列表 / binaries / 创建 / 运行） | ✅ |
| 25 | PowerShell/.ps1 拒绝（安全校验） | ✅ 400 明确报错 |

### OCR

| # | 项目 | 结果 |
|---|------|------|
| 26 | 任务内 ocr 步骤（截图 → ddddocr → 填入） | ✅ |
| 27 | 独立识别接口 `POST /api/ocr/recognize`（base64 传图） | ✅ 3692 精确命中 |

### 定时任务

| # | 项目 | 结果 |
|---|------|------|
| 28 | cron 调度准点触发 + 结果持久化 + 任务卡状态显示 | ✅ 01:13:00 触发 |
| 29 | 执行历史 API（`/api/scheduler/jobs/{id}/history`） | ✅ 数据正常（UI 显示见 BUG-4） |

### 系统管理与安全

| # | 项目 | 结果 |
|---|------|------|
| 30 | 鉴权：token 签发、X-Auth-Token 校验、重启后 token 保留 | ✅ |
| 31 | SSRF 防护：background/fetch-url 拒绝非 HTTPS/回环 | ✅ 400 |
| 32 | 背景图上传 / 删除（multipart） | ✅ |
| 33 | 日志级别热更新（DEBUG ↔ INFO） | ✅ |
| 34 | 开机自启 enable/disable（注册表写入与还原） | ✅ |
| 35 | 系统信息 / 日志拉取 API | ✅ |
| 36 | 环境按需引导 `POST /api/install/playwright`（dev 回退路径） | ✅ |

### 生命周期与更新

| # | 项目 | 结果 |
|---|------|------|
| 37 | 全新安装首启（目录结构 / 默认 Profile / .auth_token / .runtime_port） | ✅ |
| 38 | 初始化协议流转（init-status agreed:false → agree → true） | ✅ |
| 39 | 单实例互斥锁（同 base-path 第二实例 exit 1） | ✅ |
| 40 | `--stop` 优雅退出 | ✅ |
| 41 | `/api/system/restart` 重启 | ✅ |
| 42 | **自动更新闭环（P0）**：假源 manifest → check-update（semver 比较）→ 下载 + zip SHA256 校验 → 暂存（pending 含解压后 exe SHA 与 original_args）→ 重启后"启动时已应用更新 v5.0.0-alpha.2"→ staging/pending 清理 → 服务与 token 完整 | ✅ 全链路 |

### GUI（内置浏览器黑盒）

| # | 项目 | 结果 |
|---|------|------|
| 43 | 仪表盘（状态卡/快捷操作/登录历史/实时日志 WS） | ✅ |
| 44 | 网络测试按钮 | ✅ toast"测试完成" |
| 45 | 通知历史面板 | ✅ 渲染正常（关闭交互见观察项 O-3） |
| 46 | 任务管理页（列表/使用中状态/步骤文档） | ✅ |
| 47 | 设置·账号页（凭证回显/密码掩码） | ✅ |
| 48 | 设置·监测页（布局：左探测/右登录前检测） | ✅ |
| 49 | 配置方案页（方案卡/自动切换状态/三步引导） | ✅ |
| 50 | 自定义脚本页（列表/帮助文档） | ✅ |
| 51 | 定时任务页（列表/上次结果红标/空态） | ✅ |
| 52 | 外观页（深色主题即时切换/恢复浅色） | ✅ |
| 53 | 关于页（版本/系统信息/自启状态联动） | ✅ |

## 二、未测试清单

### 无法测试（环境限制）

| 项目 | 原因 |
|------|------|
| 系统托盘（菜单/图标/双击） | 主实例以 `--no-tray` 运行；托盘交互需真实桌面会话（用户已豁免） |
| 自动更新 GitHub 真实源 | 依赖外网与真实发布；已用本地假源闭环替代（回环 http 放行路径正是为此设计） |
| 全新安装的**环境真实引导**（uv 下载 + uv sync + Playwright 浏览器安装） | 需外网且耗时长；本次 fresh 实例仅验证初始化/生命周期，未触发引导 |
| GUI 文件选择（外观页"选择图片"等） | IAB 不支持 file chooser；上传已用 multipart API 验证 |

### 未覆盖（可补测）

| 项目 | 说明 |
|------|------|
| 调试面板截图显示缺口定案 | debug 会话因 BUG-3 无法走到截图步骤；静态证据（Worker 发 `path`、前端读 `data.url`、无截图服务路由）待 BUG-3 修复后实测定案 |
| 更新失败回滚分支（.bak 保留） | 成功闭环已验证；失败分支（SHA 不符/下载中断/替换失败）未模拟 |
| Bridge 空闲超时回收（300s） | 崩溃恢复已实测；空闲回收仅代码层确认，未实机等待 |
| 配置版本迁移（config_version 5→6） | 迁移函数未触发 |
| OCR 安装/卸载开关 | 会改动 .venv（卸载 ddddocr 再装回），风险大于收益 |
| 端口修改 + `.runtime_port` 跟随 | 需改运行中实例端口，会打断测试现场 |
| Profile 自动切换实测（wifi_ssid/网关变化触发） | 需模拟网络环境变化；API 开关已验证 |
| WebSocket 断线重连 | 仅被动验证连接成功 |
| 卸载流程（`/api/uninstall`） | 破坏性（清理自启/缓存/数据），未执行 |
| 任务编辑器 GUI（新建/编辑表单交互） | 仅 API 层验证 CRUD |

## 三、本轮新增观察项（非缺陷，建议关注）

- **O-1** `POST /api/profiles/{id}` 要求 body 内重复传 `id`（与路径参数冗余）。
- **O-2** `POST /api/tasks/import` 载荷格式完全错误（如包一层 `{"tasks":[...]}`）时静默返回 `imported:0, failed:[]`，无任何提示；单条字段错误则有 failed 记录。
- **O-3** 通知历史面板不支持点击外部/Escape 关闭，导航跳页后仍保持展开。
- **O-4** `POST /api/profiles/detect` 返回的 `ssid` 为 hex 编码字符串，前端/用户不可读。
- **O-5** debug 会话不注入系统变量（并入 BUG-3 影响面）。

## 四、测试现场说明

- 主实例（50721）与模拟门户（18765）保持运行，可直接打开 [127.0.0.1:50721](http://127.0.0.1:50721) 复核；停止：`taskkill /IM campus-auth.exe /F`（注意会同时结束主实例）。
- 新增可复用资产：`mock_portal/test_phase_a1~a3.py`（分批回归脚本）、假更新包在 `%TEMP%/update-pkg/`（latest.json + zip，配 18699 端口自起服务即可复测更新）。
- 测试新增数据：任务 `t-shell`/`t-script`、监测探测含 TCP/URL 目标、updater 配置已还原官方源；全新安装实例目录 `%TEMP%/campus-fresh-e2e`（已 --stop，可删除）。
