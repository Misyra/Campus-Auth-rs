# 更新日志

## 开发中（2026-08-14 第四轮：Python 精简 + 全面运行 + 日志/弹窗修复）

### Python Worker 依赖精简

- **移除未使用的 `cryptography`**：Python 端全部源码无 `cryptography`/`Cipher`/`decrypt`/`encrypt` 引用，`ddddocr` 依赖链亦不含它，属纯多余依赖。从 `ocr` extra 移除，连带清理传递依赖 `cffi`、`pycparser`
- **清理未使用 import**：`step_handlers.py` 顶部 `import base64` 移除（`playwright_worker.py` 的 `base64` 被 `handle_ocr_recognize` 使用，保留）
- 运行时依赖收敛为仅 `playwright`（含必要的 `greenlet`/`pyee`/`typing-extensions` 传递依赖）；`pytest` 及其传递依赖仅属 dev 组

### 全面运行验证（find problems）

- 编译 + 独立 base_path 后台启动，逐一探测 30+ 个 HTTP/WS 接口均正常；核心 CRUD（任务/Profile/调度/配置）通过；密码字段加密存储且详情清空保护；WebSocket 首帧状态快照正常；网络接口检测正确；优雅关闭按序退出
- 未发现阻断性 bug；两项 WARN 属预期（平台无发布下载包、`app.port` 不支持热更新）

### WebUI 日志延迟 + 弹终端修复

- **后端消除弹终端**：`detect.rs` 的 `run_command`、`uv.rs` 新增 `uv_command` 辅助函数 （`uv --version`/`uv sync`/`run_uv_command` 复用）、`python.rs` 的 `playwright install` 均补 `CREATE_NO_WINDOW`，网络检测与环境引导不再弹出黑色控制台窗口
- **修复日志延迟**：`DashboardView` 移除每 3 秒 HTTP 轮询 `fetchLogs()`（整体替换与 WebSocket 实时推送冲突）；`useLogs` 解除 `Object.freeze`（响应迟钝）+ 新增 `initialized` 门控，历史未拉取完前丢弃 WS 实时日志避免乱序
- **修复自动滚动**：`watch` 改直接监听原始 `logs.length`（而非惰性 computed），新日志 push 即触发滚动；日志面板改 CSS Grid 对齐（时间/级别/来源/消息），新增级别左边框色条、平滑滚动、badge 配色优化

### 构建产物位置调整

- **`build.ps1` 默认输出目录改为项目根目录 `dist/`**（原 `dist-portable/`），解压即用的便携版直接在根目录；打包完成后额外将 `campus-auth.exe` 复制一份到项目根目录，方便直接运行测试
- `.gitignore` 新增 `/dist/` 忽略规则（`*.exe` 已覆盖主程序，补此规则避免资源文件被误跟踪）

## 开发中（2026-08-14 第三轮：todo 批次四测试补强）

### 测试补强

- **web/routes handler 层测试**：为 `config.rs`（monitor 前后端字段映射往返一致性 + `json_merge` 合并/删除语义）、`system.rs`（tracing JSON 日志解析与噪音过滤、背景图扩展名 Content-Type/magic 识别、文件名路径安全、URL SSRF 私有地址判定）、`scheduler.rs`（任务历史记录字段映射，抽出可测纯函数 `map_history_records`）新增 18 个单元测试
- **python_worker pytest**：新增 `tests/` 目录（41 个用例），覆盖 `models.py`（StepConfig/TaskConfig 的 `type` 别名、extras 透传、code→script 合并、往返序列化）、`variable_resolver.py`（替换/链式递归/循环保护/深度上限）、`playwright_worker.py`（超时秒↔毫秒归一化、`_is_truthy`、浏览器参数构建与黑名单过滤、取消注册表/pending 上限、IPC 响应/事件序列化）、`step_handlers.py`（错误分类、取消检查、处理器别名映射）。`pyproject.toml` 新增 `[dependency-groups].dev`（pytest）与 `[tool.pytest.ini_options]`（`pythonpath`）

## 开发中（2026-08-14 第三轮：todo 批次一~三）

### 死代码激活（修复而非删除）

- **`PartialSnapshot::Uptime` 接线**：uptime 定时器每秒同时写入 Metrics 与推送状态快照，WebSocket 状态 `uptime_seconds` 与 `/api/system` 现保持一致
- **`ConfigReloadSignal` 信号去重**：`switch_profile` 改用 `ProfileSwitched` 信号，调度器不再因切 Profile 全量重载任务；移除与调度器 `task_change_rx` 通道重复的死变体 `TasksChanged`
- **Bridge `last_activity` 接入用途**：空闲回收计时器改从真实最后活动时刻起算剩余时长，避免计时器启动延迟压缩实际空闲时间
- **`PasswordCrypto::decrypt` 收敛**：非 zeroizing 版本标记 `#[cfg(test)]` 为测试专用，防止生产误用

### 行为类修复

- **关闭序列**：发 Shutdown 后先取消应用级关闭令牌（让在途登录 task 协作退出）、await Engine 完全退出再关 Bridge，消除关闭期错误洪泛风险
- **双层 CORS 合并**：移除 `app.rs` 外层硬编码 50721 白名单，仅保留内层 `mirror_request()`，放行 vite dev / 局域网来源
- **URL 探测流式读取**：`resp.bytes()` 全量下载改 `resp.chunk()` 逐块累计，最多 64KB 即停，避免大响应白耗带宽
- **`CaptchaFailed` 可重试**：OCR 验证码识别失败（与网络/导航失败同属瞬时性）改为重试整个流程，纳入 `max_retries` 预算
- **托盘刷新去重**：仅当影响 tooltip/图标/菜单的字段（engine/network/login）变化才请求 OS 线程刷新
- **迁移写回 fsync**：新增同步版原子写，配置迁移 commit 后 fsync 落盘
- **Axum 关闭超时 abort**：`stop_axum` 超时后真正 `abort()` 挂起的 serve task，不再仅记日志
- **scheduler 同步 fs 迁移**：`save_task` / `delete_task` / `toggle_task` / `update_last_run` / `add_history_record` 改 async + `spawn_blocking`，不再阻塞 tokio worker 线程
- **launcher unwrap 防御**：3 处 `state.container.as_ref().unwrap()` 改防御性错误/降级，缺容器不 panic

### 工程化收尾

- **E1 openapi.json 生产可用**：`rust-embed` 嵌入根目录 `openapi.json`（启用 `include-exclude` feature），新增 `/openapi.json` 路由，前端兜底 fetch 不再拿到 SPA index 而静默降级 `version="unknown"`
- **E2 根目录 README.md**：新增用户/贡献者入口文档

### 修复

- **死代码激活（修复而非删除）**：`Notifier` 接入 EngineInner（登录失败按 Profile 去重提醒，成功/切 Profile 重置）；`ResultAction::Exhausted` 从 `unreachable!()` 变真实终态（可重试 + 预算耗尽 → "重试耗尽"收尾）
- **M5 轻量模式端口**：托盘按需启动 Axum 后同步实际端口到 `.instance`，`--status`/`--stop` 不再读失配端口
- **F5 配置覆盖**：前端切/存 Profile 前检查 dirty，有未保存修改先弹确认
- **M8 配置互斥**：`reload()` 内获取 `save_mutex`，消除读写混合快照
- **M2 启动双初始化**：`load_and_merge_config` 改为轻量读取 settings.json，不再创建临时 ConfigService
- **密钥 TOCTOU**：生成密钥加尽力而为文件锁（独立 `.lock` + 非阻塞 `try_lock`，失败降级 warn 不阻断）
- **密钥冲突（Python/Rust 版）**：`.enc_key.rs` 缺失时自动继承 Python 旧版 `.enc_key`（base64→raw 32B 落盘），两版共用密钥、旧密码可解
- **SSRF 缺口**：repo 远程 URL 校验补 IPv6 链路本地拦截（`is_unicast_link_local`）
- 设置页子路由误弹确认、顶栏 `N/undefined` 重连显示、5 个未使用依赖移除、8 处前端 `as any`、遗留目录清理（186MB）

### 新增

- `.github/workflows/ci.yml`（fmt + clippy -D warnings + cargo test + 前端构建 + python 语法检查）
- `build.ps1` 便携版打包脚本；`frontend/src/api/types.generated.ts` 入 .gitignore
- openapi.json 补齐 5 个缺失路由（`/api/tasks/{id}/execute`、`/api/repo/fetch`、`/api/repo/task`、`/api/worker/stop`、`/ws/logs`）

### 测试

- `status/snapshot.rs`（apply_partial 全变体）、`updater/check.rs`（版本比较/平台选择）、`web/error.rs`（状态码/错误码/响应体/From）
- `config/migration.rs`（迁移重命名/拆分/幂等 5 个）、`web/routes/repo.rs`（SSRF 面 + URL 归一化 7 个）、`bridge/process.rs`（IPC 解析 5 个）
- 全量 274 测试通过，clippy `-D warnings` 零警告

## v5.0.0-alpha.1

Rust 重写版首个迭代，对齐原项目 v4.2.3 功能并修复历史遗留问题。

### 新增功能

- **goto 步骤类型**：执行过程中显式跳转指定 URL，与 `navigate` 共用处理器，支持 `url` / `value` / `wait_until` 字段
- **assert_text 步骤类型**：等待页面出现指定文本（`document.body.innerText` 包含匹配）
- **success_condition 成功判定**：任务声明变量名后，登录成功以 `eval` 步骤 `store_as` 写入的变量真值判定，替代默认网络检测兜底
- **post_login_delay 配置**：登录后等待 portal 生效的延迟（默认 5s，可配置 0-60）
- **worker 停止端点**：`POST /api/worker/stop` 手动关闭浏览器进程，前端新增「浏览器常驻」开关与「立即关闭浏览器」按钮

### 修复

- **Bridge 并发竞态（F1–F4）**：响应解析失败回收在途请求、cancel 通知可靠发送、shutdown 哨兵 id 隔离、主循环退出优雅回收 Worker
- **调度器（F5/F9/F10）**：重载前触发到期任务避免漏触发、浏览器定时任务加超时上限、到期任务并发限制（上限 4）
- **配置读写**：`has_password` 反映密码可解密、monitor 字段不覆盖存储值、密码加密失败显式报错
- **`.gitignore` 误忽略源码**：运行时目录模式未锚定根目录，导致 `src/config`、`src/environment` 两个核心模块从未进入版本控制，已修复并补入
- **更新器**：校验解压产物存在后再写 pending 更新
- **静态资源缓存**：`index.html` 改 `no-store`，避免引用旧 bundle 名导致前端停在旧版本

### 重构

- Python Worker：StepConfig 补齐 `frame` / `char_range` 字段，OCR 支持字符集限制，一次性命令提取 `_cancel_session` 上下文管理器，删除 `script_runner.py` 死代码
- 前端：`useUi` init 防重入、WebSocket 重连全量刷新、`useProfiles` 未保存改动确认、任务列表拖拽排序
