# 自定义脚本指南

> 本文档对应当前实现：`tasks/scripts/` 下的 `script` 任务（`src/tasks/models.rs: TaskKind::Script`），经 `TaskExecutor::execute_script` 起本地子进程执行；登录渠道侧由 `src/login/script_login.rs` 复用它（**同一条执行路径**，不是第二套实现）。历史 `type=shell` 已移除，遇到时反序列化明确报错（`src/tasks/models.rs`）。

## 1. 简介

脚本任务让你不经浏览器执行本地动作，有两种用法：

| 用法 | 谁触发 | 描述 |
|------|--------|------|
| **定时/手动辅助动作** | 定时任务、任务页「立即运行」、`POST /api/tasks/{id}/execute` | 打卡、签到、调用任意可执行文件等 |
| **登录渠道**（自定义脚本） | 网络监测、仪表盘「登录」、`POST /api/login` | 把**登录本身**交给脚本（见第 2 节） |

两种用法共用同一份脚本任务与同一条执行路径（解释器回退、路径约束、超时与进程树回收、输出截断全都一样）；区别只在"谁触发"和"脚本要不要按登录契约写"。

与浏览器任务相比，脚本任务：

- **资源更低** — 不拉起 Playwright / Chromium
- **更快** — 纯本地进程
- **更灵活** — 任意解释器或可执行文件（经 `binary_path` 指定）

支持的文件脚本扩展名：`py` / `bat` / `cmd` / `sh` / `exe` / `com`（`src/tasks/executor.rs::is_supported_ext`）。`ps1` / `powershell` / `pwsh` 不在支持范围——经 `binary_path` 传入也会被映射为 `ps1` 后拒绝，请改用 `bat` 包装。

## 2. 用脚本登录（自定义脚本渠道）

方案编辑器的「登录方式」有三张卡片：**浏览器自动化** / **直连请求** / **自定义脚本**。选第三张即把登录整个交给脚本任务——程序只负责起进程、看退出码、然后做一次登录后网络验证。

### 2.1 契约（写脚本就照这个来）

| 项 | 约定 |
|----|------|
| **凭据来源** | 环境变量：`CAMPUS_USERNAME`、`CAMPUS_PASSWORD`、`CAMPUS_ISP`、`CAMPUS_AUTH_URL`（取当前方案的账号/密码/运营商/认证地址；后两个可能为空串，照空串处理，别依赖"变量不存在"） |
| **怎么做模板替换** | **不做**。`{{USERNAME}}` 一类占位符是浏览器任务专属（由 Worker 的变量解析器处理），脚本里请读环境变量 |
| **成功判定** | 退出码 `0` = 本次尝试成功；随后程序仍会做**登录后网络验证**，只有真的通到公网才记「登录成功」 |
| **失败判定** | 退出码非 `0` = 本次尝试失败，按方案的重试策略重发（与直连渠道「未命中成功标识」同类），重试预算耗尽才记失败。**重试次数与间隔在「设置 · 网络检测」里**（默认最多重试 3 次，间隔逐次翻倍）——所有渠道共用这一份策略 |
| **超时** | 用脚本任务自己的「超时」（秒，钳制 1~3600，默认 60）；超时后按平台强杀整棵进程树 |
| **输出** | `stdout`/`stderr` 末尾（最多 200 字符）会写进**登录历史**的消息里；其中出现密码的地方会被抹成 `***` |
| **环境隔离** | 子进程以最小环境变量启动（`env_clear` 后只注入 `PATH`/`HOME`/`TEMP` 等），主进程的 Web token、代理密码**不会**继承；上面四个 `CAMPUS_*` 是唯一的额外注入 |
| **工作目录** | 脚本任务的 `work_dir`；留空则用脚本所在目录（内联内容时为临时目录） |

方案侧仍然必须有账号与密码：它们是脚本唯一的凭据来源，空着会被登录前的配置校验直接拦下（提示「账号为空 / 密码为空」）。**绑定关系**同样必填——脚本渠道和直连渠道一样**没有内置兜底脚本**，未绑定即「脚本登录不可用」，保存方案时也会被拒绝。

### 2.2 一个可跑的登录脚本（Python）

门户若是普通的 HTTP 登录表单，脚本就是一次 `requests` 式请求：

```python
import os
import urllib.parse
import urllib.request

username = os.environ["CAMPUS_USERNAME"]
password = os.environ["CAMPUS_PASSWORD"]
auth_url = os.environ.get("CAMPUS_AUTH_URL", "")

# 用不到认证地址时（不填也能跑），自己写门户的登录接口
login_url = "http://10.100.51.1:801/eportal/portal/login"
query = urllib.parse.urlencode({
    "callback": "dr1003",
    "login_method": "1",
    "user_account": f",0,{username}",
    "user_password": password,
})
with urllib.request.urlopen(f"{login_url}?{query}", timeout=20) as resp:
    body = resp.read().decode("utf-8", "replace")

print(body[:200])
# 退出码即成败：非 0 会按重试策略重发（重试次数见「设置 · 网络检测」）
if '"result":1' not in body:
    raise SystemExit(1)
```

要点：

- 门户拒绝凭据时**要 `SystemExit(1)`**（或任何非 0 退出码）。安静地打印一行错误然后正常结束，会被当成"脚本跑通了"，白等一轮网络验证。
- 需要登录前先踢掉旧会话（「IP 已在线，拒绝重复登录」类门户）就在脚本里先发一次下线请求——直连渠道的「退出登录请求」在脚本里可以用普通代码表达。
- 密码要参与加密/签名（Dr.COM、eportal 变体）就自己在脚本里算——Python 生态比直连渠道的沙箱脚本自在得多，这正是脚本渠道相对直连渠道的优势。

### 2.3 什么时候该用脚本、什么时候用直连

| 场景 | 建议 |
|------|------|
| 门户就是一次普通 HTTP 请求，能抓到请求形状 | 用**直连请求**：不用写代码、不依赖 Python，界面里填表即可 |
| 请求前要先取令牌、要按门户逻辑加密密码、要多步跳转才拿到登录参数 | **自定义脚本**最省事 |
| 门户有验证码 / 动态表单 / 前端加密逻辑复杂 | 用**浏览器自动化**，别硬啃 |
| 只是想定时打卡、签到 | 脚本任务即可，**不必**把它绑成登录渠道 |

### 2.4 排障

- 登录历史里的消息形如 `登录脚本 portal-login 退出码 1：<输出末尾>`，先看退出码与输出。
- 想看脚本完整的 stdout/stderr：到任务页 · 脚本，选中该脚本点「立即运行」，输出会留在编辑页侧栏（比登录历史里的截断版完整）。
- 「脚本无法执行: …」是连进程都没起来（脚本文件不存在、扩展名不支持、解释器缺失），这类失败不重试。
- 「登录脚本 X 执行超时（N 秒）」会重试；超时值调大仍超时就说明脚本在等一个回不来的请求。

## 3. 任务字段

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

## 4. 通过 Web 界面创建

1. 打开 Web 控制台 → 侧栏「任务 → 脚本」；
2. 点「新建脚本」进入编辑页。**脚本 ID 就是文件名**（1~64 位字母、数字、下划线或连字符，与任务 ID 同一套判据），必须先填上它，脚本才会落盘——空 ID 时页头会提示「还缺 脚本 ID（1~64 位字母、数字、下划线或连字符），补齐前改动不会保存」，不会留下一个没名字的脚本文件；
3. 名称、描述、执行程序与脚本内容都是**改动自动保存**（停手半秒落盘，头部状态字显示「已保存 · 刚刚」），没有保存按钮。ID 一旦落盘即固定（「定时任务」按它引用脚本），要换名字请删除后重建；
4. 落盘后可在列表行尾 ⋯ 里点「立即运行」执行一次（`POST /api/scripts/run`），退出码 0 视为成功；stdout 与 stderr 作为执行结果显示在编辑页侧栏（**不进日志页**，见第 6 节）；
5. 要拿它当登录渠道：到「方案」页 → 「登录方式」→ 选「自定义脚本」→ 在「登录脚本」下拉里选中它，保存方案。

## 5. 通过 API 创建

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

把脚本绑成登录渠道（`login_channel=script` + `active_script_task`）：

```bash
curl -X PUT http://127.0.0.1:50721/api/profiles/dorm \
  -H "Content-Type: application/json" \
  -H "X-Auth-Token: $TOKEN" \
  -d '{"login_channel": "script", "active_script_task": "portal-login"}'
```

保存时后端会校验这个 id **存在且确实是脚本任务**：绑到直连任务或浏览器任务上会被 400 拒绝（仅查存在性会放行，等到登录那一刻才发现拿错了配置）。

## 6. 执行与日志

- 统一执行：`POST /api/tasks/{id}/execute`（浏览器/脚本通用）；`POST /api/scripts/run`（脚本直跑，临时任务不落盘）；登录渠道则由登录编排器触发。
- 成败判定：按**子进程退出码**，`0` 视为成功（`src/tasks/executor.rs::run_command`）。
- 输出去向：`stdout` / `stderr` 被合并进 `TaskResult.output`（各截断到 `OUTPUT_TRUNCATE_LEN` = 500 字符，超出为 `stdout\nstderr`）随**执行响应的 HTTP 响应体**返回，也是任务历史里显示的内容。**该输出不经 `tracing` 记录、不进 WebSocket 推送**，因此前端实时日志面板与 `GET /api/logs` 看不到脚本的 stdout/stderr——排障请看执行结果（或登录历史消息），而非日志面板。Web 界面的「立即运行」会把这一次的输出就地留在编辑页侧栏（连带退出码与耗时），失败时提示里也带上输出末行。
- 环境隔离：子进程以最小环境变量启动（`env_clear` 后仅注入 `PATH`/`HOME`/`TEMP` 及 Windows 关键目录变量），不继承主进程的 token、代理密码等；登录渠道会在此基础上叠加 `CAMPUS_*` 四个变量（见第 2.1 节）。
- 超时：走 `tokio::process` 超时取消，超时时 Windows 以 `taskkill /T`（带 `CREATE_NO_WINDOW`）递归杀进程树，Unix 上以独立进程组 `killpg` 回收整棵子树。登录被取消（点「取消登录」/应用退出）时，子进程随执行 future 一起被丢弃并回收（`kill_on_drop` + Job Object）。

## 7. 常见问题

**Q: 脚本执行失败？**

- 类型/扩展名选错（`ps1` 被拒、`.bat` 在 Unix 上显式拒绝）；
- `content` 与 `script_path` 均空（保存时校验拒绝）；
- `binary_path` / `script_path` 元字符注入或路径穿越（服务端拦截）；
- 退出码非 0（按失败记录，看执行结果定位）；
- 权限或网络问题。

**Q: 会弹窗口吗？**

程序自身 release 版为 Windows GUI 子系统（`src/main.rs` 的 `windows_subsystem = "windows"`），双击主程序不弹控制台。但**脚本子进程本身没有显式设置 `CREATE_NO_WINDOW`**（`src/tasks/executor.rs` 仅在超时强杀的 `taskkill` 调用上设置该标记），因此它是否出现控制台窗口取决于脚本类型与宿主环境——例如启动 `cmd.exe` / 控制台版 `python.exe` 时可能短暂出现窗口。若你的脚本必须静默运行，请自行规避：用 `pythonw.exe`、`.vbs` 包装，或把控制台脚本改为不写终端的实现（脚本路径上不支持 `ps1`，见上文）。

**Q: 如何注入账号密码？**

分两种用法：

- **登录渠道**（第 2 节）：读 `CAMPUS_USERNAME` / `CAMPUS_PASSWORD` / `CAMPUS_ISP` / `CAMPUS_AUTH_URL` 环境变量。
- **其它触发方式**（定时任务 / 立即运行）：**没有模板替换**，`content` 原样写入临时文件、`args` 原样传参，`{{USERNAME}}` 一类占位符不会被解析。需要凭据请自己在脚本里读配置（或改用登录渠道）。

（`{{USERNAME}}` / `{{PASSWORD}}` / `{{ISP}}` / `{{LOGIN_URL}}` 是**浏览器任务**的特性，由 Python Worker 的 `variable_resolver.py` 在步骤层解析，脚本路径不经过它。）

子进程环境变量中确实存在 `USERNAME`，但那是**操作系统登录名**（`std::env::var("USERNAME")`，见 `collect_minimal_env`），不是当前方案的账号，勿混用——方案账号在 `CAMPUS_USERNAME` 里。

**Q: 超时怎么调？**

`timeout` 字段（钳制 `1..3600` 秒），默认 60。

**Q: 登录脚本会不会拖慢登录？**

脚本超时就是它的上限（默认 60 秒），另外整个登录会话还有 `browser.login_timeout` 的总超时兜底（`设置 · 浏览器`）。脚本内部该设的连接超时请自己设（如 `urlopen(..., timeout=20)`），否则网络半死时会一直等到超时。

**Q: 想知道刚才那次登录走的哪条渠道？**

看仪表盘实时日志与登录历史的消息文本：浏览器渠道是「步骤 N/M: …」，直连渠道是「直连请求成功（HTTP 200 OK）」/「直连请求失败: …」，脚本渠道是「登录脚本 <id> 退出码 N」。（登录历史目前不单独记渠道字段，只能从消息辨认。）

## 8. 相关文档

- [任务编写指南](task-writing-guide.md) — 浏览器任务 JSON 与步骤契约
- [直连登录指南](http-login-guide.md) — 免代码的 HTTP 登录渠道
- [任务使用手册](task-manual.md) — 日常管理、录制器、调试
- [用户指南](user-guide.md) — 启动、Profile、更新通道
