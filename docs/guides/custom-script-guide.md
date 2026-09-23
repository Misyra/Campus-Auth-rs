# 自定义脚本指南

> 本文档对应当前实现：`tasks/scripts/` 下的 `script` 任务（`src/tasks/models.rs: TaskKind::Script`），经 `TaskExecutor::execute_script` 起本地子进程执行。历史 `type=shell` 已移除，遇到时反序列化明确报错（`src/tasks/models.rs`）。

## 1. 简介

脚本任务让你不经浏览器执行本地动作（打卡、签到、调用任意可执行文件等）。与浏览器任务相比：

- **资源更低** — 不拉起 Playwright / Chromium
- **更快** — 纯本地进程
- **更灵活** — 任意解释器或可执行文件（经 `binary_path` 指定）

**脚本不参与登录认证。** 校园网登录由「方案」页里的**登录方式**负责：浏览器自动化（按任务步骤操作网页）或**直连请求**（在 Rust 进程内发 HTTP，无需写代码、无需 Python/浏览器）。脚本请用于定时执行的辅助动作。

支持的文件脚本扩展名：`py` / `bat` / `cmd` / `sh` / `exe` / `com`（`src/tasks/executor.rs::is_supported_ext`）。`ps1` / `powershell` / `pwsh` 不在支持范围——经 `binary_path` 传入也会被映射为 `ps1` 后拒绝，请改用 `bat` 包装。

## 2. 任务字段

| 字段 | 含义 |
|------|------|
| `content` | 内联脚本内容（写入临时文件执行，后缀按 `binary_path` 推断，上限 100 KB） |
| `script_path` | 脚本路径。相对路径基于 `tasks/scripts/`；路径存在时经 canonicalize 校验必须仍位于 `tasks/scripts/` 内（防 symlink 越界），**绝对路径同样受此约束**，指向别处会报「script_path 越界」 |
| `binary_path` | 解释器 / 启动器覆盖；为空时按下表回退 |
| `args` | 命令行参数 |
| `work_dir` | 工作目录，为空时用 `script_path` 所在目录（`content` 场景为临时目录） |
| `timeout` | 超时秒数，钳制到 `1..3600`，默认 60（`src/tasks/models.rs`） |

`content` 与 `script_path` 二选一，均空时保存校验拒绝。

`binary_path` 为空时的回退（`src/tasks/executor.rs::build_script_command`）：

| 扩展名 | 回退行为 |
|--------|----------|
| `py` | 项目内 `python_worker/.venv` 的 Python（触发 `environment::ensure_python_runtime` 按需引导，**不安装 Playwright 浏览器**） |
| `bat` / `cmd` | `cmd.exe`（仅 Windows；Unix 上显式拒绝 `cmd.exe`） |
| `sh` | `sh` |
| `exe` / `com` | 直接启动 |

## 3. 通过 Web 界面创建

1. 打开 Web 控制台 → 侧栏「任务 → 脚本」；
2. 新建脚本，填脚本 ID（`^[A-Za-z][A-Za-z0-9_]*$`）、名称、描述、执行程序与内容；
3. 保存后可在列表页点「运行」立即执行一次（`POST /api/scripts/run`）。

## 4. 通过 API 创建

`script` 任务示例（内联内容）：

```bash
curl -X PUT http://127.0.0.1:50721/api/scripts/checkin \
  -H "Content-Type: application/json" \
  -H "X-Auth-Token: $TOKEN" \
  -d '{
    "type": "script",
    "name": "每日签到",
    "content": "from urllib.request import urlopen\nwith urlopen(\"http://example.com/checkin\", timeout=30) as r:\n    print(f\"HTTP {r.status}\")",
    "timeout": 30
  }'
```

`script` 任务示例（指定解释器与参数）：

```bash
curl -X PUT http://127.0.0.1:50721/api/scripts/checkin \
  -H "Content-Type: application/json" \
  -d '{
    "type": "script",
    "name": "每日签到",
    "script_path": "checkin.py",
    "binary_path": "C:\\Python312\\python.exe",
    "args": ["--verbose"],
    "work_dir": "C:\\campus"
  }'
```

脚本内容直跑（不落盘，适合一次性验证）：

```bash
curl -X POST http://127.0.0.1:50721/api/scripts/run \
  -H "Content-Type: application/json" \
  -d '{"script": "print(1)"}'
```

## 5. 执行与日志

- 统一执行：`POST /api/tasks/{id}/execute`（浏览器/脚本通用）；`POST /api/scripts/run`（脚本直跑，临时任务不落盘）。
- 成败判定：按**子进程退出码**，`0` 视为成功（`src/tasks/executor.rs::run_command`）。
- 输出去向：`stdout` / `stderr` 被合并进 `TaskResult.output`（各截断到 `OUTPUT_TRUNCATE_LEN` = 500 字符，超出为 `stdout\nstderr`）随**执行响应的 HTTP 响应体**返回，也是任务历史里显示的内容。**该输出不经 `tracing` 记录、不进 WebSocket 推送**，因此前端实时日志面板与 `GET /api/logs` 看不到脚本的 stdout/stderr——排障请看任务详情/执行结果，而非日志面板。
- 环境隔离：子进程以最小环境变量启动（`env_clear` 后仅注入 `PATH`/`HOME`/`TEMP` 及 Windows 关键目录变量），不继承主进程的 token、代理密码等。
- 超时：走 `tokio::process` 超时取消，超时时 Windows 以 `taskkill /T`（带 `CREATE_NO_WINDOW`）递归杀进程树，Unix 上以独立进程组 `killpg` 回收整棵子树。

## 6. 常见问题

**Q: 脚本执行失败？**

- 类型/扩展名选错（`ps1` 被拒、`.bat` 在 Unix 上显式拒绝）；
- `content` 与 `script_path` 均空（保存时校验拒绝）；
- `binary_path` / `script_path` 元字符注入或路径穿越（服务端拦截）；
- 退出码非 0（按失败记录，看日志定位）；
- 权限或网络问题。

**Q: 会弹窗口吗？**

程序自身 release 版为 Windows GUI 子系统（`src/main.rs` 的 `windows_subsystem = "windows"`），双击主程序不弹控制台。但**脚本子进程本身没有显式设置 `CREATE_NO_WINDOW`**（`src/tasks/executor.rs` 仅在超时强杀的 `taskkill` 调用上设置该标记），因此它是否出现控制台窗口取决于脚本类型与宿主环境——例如启动 `cmd.exe` / 控制台版 `python.exe` 时可能短暂出现窗口。若你的脚本必须静默运行，请自行规避：用 `pythonw.exe`、`.vbs` 包装，或把控制台脚本改为不写终端的实现（脚本路径上不支持 `ps1`，见上文）。

**Q: 如何注入账号密码？**

**脚本任务不做模板替换**：`content` 原样写入临时文件、`args` 原样传参，`{{USERNAME}}` 一类占位符不会被解析。请在脚本内自行读取或硬编码凭据。

（`{{USERNAME}}` / `{{PASSWORD}}` / `{{ISP}}` / `{{LOGIN_URL}}` 是**浏览器任务**的特性，由 Python Worker 的 `variable_resolver.py` 在步骤层解析，脚本路径不经过它。）

子进程环境变量中确实存在 `USERNAME`，但那是**操作系统登录名**（`std::env::var("USERNAME")`，见 `build_minimal_env`），不是当前方案的账号，勿混用。

**Q: 超时怎么调？**

`timeout` 字段（钳制 `1..3600` 秒），默认 60。

**Q: 想让脚本参与登录可以吗？**

不可以。登录认证只走方案的「登录方式」（浏览器自动化 / 直连请求），且浏览器渠道执行的是**方案绑定的浏览器任务**（`active_task`），脚本不在其中——`embed_task_config` 只嵌入浏览器任务，脚本即使被写进该字段也会由登录解析判为不可用并回退内置任务。需要免代码的 HTTP 登录请用**直连请求**。

## 7. 相关文档

- [任务编写指南](task-writing-guide.md) — 浏览器任务 JSON 与步骤契约
- [任务使用手册](task-manual.md) — 日常管理、录制器、调试
- [用户指南](user-guide.md) — 启动、Profile、更新通道
