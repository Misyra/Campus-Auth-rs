# 自定义脚本指南

> 本文档对应当前实现：`tasks/scripts/` 下的 `script`（文件脚本）与 `shell`（命令字符串）两类任务（`src/tasks/models.rs: TaskKind::Script/Shell`），经 `TaskExecutor` 分派执行；`python`/`shell` 文本不再作为独立任务 `type`。

## 1. 简介

自定义脚本让你无需启动浏览器即可完成认证（直接发 HTTP 请求、调本地可执行文件等）。与浏览器任务相比：

- **资源更低** — 不拉起 Playwright / Chromium
- **更快** — 纯本地进程 + 网络请求
- **更灵活** — 任意解释器或可执行文件（经 `binary_path` / `shell_path` 指定）

支持的文件脚本扩展名：`py` / `bat`/`cmd` / `sh` / `exe`/`com`（`src/tasks/executor.rs::build_script_command`）；`ps1`/`powershell`/`pwsh` 不在支持范围，会映射为 `ps1` 后被校验拒绝，请改用 `bat` 包装或 `shell` 类型。

## 2. 两类任务

| 类型 | 含义 | 关键字段 |
|------|------|----------|
| `script` | 文件脚本 | `script_path`（相对 `tasks/scripts/` 或绝对路径）或 `content`（内联内容落临时文件执行）、`binary_path`（解释器/启动器覆盖）、`args`、`work_dir`、`timeout` |
| `shell` | Shell 命令 | `command`（必填）、`shell_path`（覆盖全局/系统默认）、`timeout` |

`timeout` 钳制到 `1..3600` 秒（`src/tasks/models.rs`）。

`script` 的 `binary_path` 为空时：`py` 自动回退到项目内 `python_worker/.venv` 的 Python（会触发 `environment::ensure_python_runtime` 按需引导），其余扩展名回退到系统默认（`py→python`、`bat→cmd.exe`、`sh→sh`、`exe→直接执行`）。

`shell` 的 `shell_path` 为空时按系统默认解析（Windows 优先 `pwsh→powershell→cmd`（`find_in_path` 探测），Unix 取 `$SHELL`，见 `src/tasks/executor.rs::resolve_shell` / `default_shell`）。

## 3. 通过 Web 界面创建

1. 打开 Web 控制台 → 任务管理或自定义脚本页；
2. 新建脚本，填 `task_id`（`^[a-zA-Z0-9_-]{1,64}$`）、名称、描述、类型与内容；
3. 保存后可在列表页直接“执行”验证（`POST /api/tasks/{id}/execute` 通用执行，不限浏览器任务）。

`script` 任务额外支持“临时直跑”：`POST /api/scripts/run` 构造临时 `ScriptTaskConfig` 执行并透出结果（`src/web/routes/scripts.rs`），适合一次性验证。

## 4. 通过 API 创建

`script` 任务示例（内联内容）：

```bash
curl -X POST http://127.0.0.1:50721/api/tasks \
  -H "Content-Type: application/json" \
  -H "X-Auth-Token: $TOKEN" \
  -d '{
    "type": "script",
    "task_id": "http_login",
    "name": "HTTP 登录",
    "content": "import httpx\nr=httpx.post(\"http://10.0.0.1/login\", data={\"username\":\"{{USERNAME}}\",\"password\":\"{{PASSWORD}}\"})\nprint(r.text)",
    "timeout": 30
  }'
```

`script` 任务示例（指定解释器与参数）：

```bash
curl -X PUT http://127.0.0.1:50721/api/scripts/http_login \
  -H "Content-Type: application/json" \
  -d '{
    "name": "HTTP 登录",
    "script_path": "http_login.py",
    "binary_path": "C:\Python312\python.exe",
    "args": ["--verbose"],
    "work_dir": "C:\campus"
  }'
```

`shell` 任务示例：

```bash
curl -X POST http://127.0.0.1:50721/api/tasks \
  -H "Content-Type: application/json" \
  -d '{"type":"shell","task_id":"curl_login","name":"curl 登录","command":"curl -X POST http://10.0.0.1/login -d username={{USERNAME}} -d password={{PASSWORD}}"}'
```

`curl -X POST http://127.0.0.1:50721/api/login -d "username=test&password=123"` 等 shell 命令在 `shell` 类型中直接填 `command` 即可；PowerShell 逻辑请写入 `bat` 脚本或在 `shell` 的 `command` 中显式调用 `powershell.exe -Command`。

## 5. 执行与日志

- 统一执行：`POST /api/tasks/{id}/execute`（三类任务通用；`GET /api/tasks` 返回摘要，`GET /api/tasks/{id}` 返回完整配置）。
- 脚本直跑：`POST /api/scripts/run`（临时任务，不落盘）。
- 日志：执行输出经 `tracing` 与 WebSocket 推送，前端日志面板与 `GET /api/logs` 可查；超长 `stdout`/`stderr` 按 `OUTPUT_TRUNCATE_LEN` 截断。
- 超时：脚本超时走 `tokio::process` 超时取消，Unix 上以进程组 `killpg` 回收整棵子树（`src/tasks/executor.rs`）。

## 6. 常见问题

**Q: 为什么脚本执行失败？**

- 类型/扩展名选错（`ps1` 被拒、`.bat` 在 Unix 上显式拒绝）；
- `content` 与 `script_path` 均空（保存时校验拒绝）；
- `binary_path` / `script_path` 元字符注入或路径穿越（服务端拦截）；
- 权限或网络问题（看日志定位）。

**Q: 会弹窗口吗？**

Windows 上子进程带 `CREATE_NO_WINDOW`，不弹控制台窗口。

**Q: 如何注入账号？**

脚本/命令中直接写 `{{USERNAME}}` / `{{PASSWORD}}` / `{{ISP}}` / `{{LOGIN_URL}}` 模板（与浏览器任务同一变量解析，`src/tasks/executor.rs` + `python_worker/variable_resolver.py` 契约）；运行时由活跃 Profile 注入。环境变量 `USERNAME` 等亦会注入子进程。

**Q: 超时怎么调？**

`timeout` 字段或系统设置页的脚本超时（钳制 `1..3600` 秒）。

## 7. 相关文档

- [任务编写指南](task-writing-guide.md) — 浏览器任务 JSON 与步骤契约
- [任务使用手册](task-manual.md) — 日常管理、录制器、调试
- [用户指南](user-guide.md) — 启动、Profile、更新通道
