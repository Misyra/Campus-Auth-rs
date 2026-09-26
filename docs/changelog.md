# 更改日志

> 本文件记录每一次代码、配置、接口与文档更改，供开发和问题追溯；面向用户的版本更新摘要见 `docs/updatelog.md`。历史轮次继续保留于本文件（`docs/archive/` 已于 2026-09-17 删除，历史归档材料随之不可追溯），活跃计划见 `docs/plan-next.md` + `docs/known-issues.md`。最新活跃为“v5.0.2”。

## 开发中（2026-09-26 直连渠道 ISP 支持）

### 背景

- 任务仓库新增景德镇陶瓷大学（Dr.COM eportal）直连任务需要把方案的运营商映射为门户账号后缀（`@cmcc` 等）；此前 ISP 只有浏览器渠道可用（`build_worker_config` 下发给 Worker），直连请求拿不到。引擎**不做任何写死映射**：`isp` 原样透传，门户侧表示法由任务凭据变换脚本自行映射。

### 登录

- `login/http_login.rs`：`HttpLoginRequest` 增加 `isp` 字段（方案运营商原样透传，未选择为空串），`from_task` 增加 `isp` 参数；`{isp}` 注册为内置模板占位符（下线请求 / 前置请求同样可用），凭据变换脚本 `ctx` 增加 `isp` 键。
- 调用点同步：正式登录（`login/mod.rs`）传 `profile.isp`；直连测试端点（`web/routes/http_tasks.rs`）跟随方案 isp（测试面板的账号可覆盖、运营商不单独提供，与正式登录同口径）。
- 测试：`ctx.isp` 透传、`from_task` 映射与 trim 断言（http_login 模块 33 用例全过；全仓 1119 过）。

### 前端 / 文档

- `loginChannel.ts`：`HTTP_TEMPLATE_PLACEHOLDERS` 增加 `{isp}`、`HTTP_CRYPTO_CTX_FIELDS` 增加 `isp`；任务编辑器（HttpTaskFields）与直连向导（HttpLoginWizard）的脚本文案同步说明 isp 的语义与映射责任。
- `docs/guides/http-login-guide.md`：占位符表、前置请求与退出登录请求的内置占位符清单补 `{isp}`。

## 开发中（2026-09-25 首次启动配置向导）

### 背景

- 用户需求：首次启动除协议确认外，追加一份分步配置向导——选登录方式 →（浏览器渠道）Python/浏览器环境准备 → 输入学校名并对两个任务仓库测速、择优拉索引做关键词匹配 → 有匹配提示导入（复用仓库导入 UI）、无匹配回退默认任务 → 完成时切换到调试模式并提示后续操作路径；直连渠道走同一流程。存量用户处理口径经确认：**仅全新安装显示**（同意协议即视为配置完成，中途退出不再续显）。

### 前端

- `SetupWizard.vue`：由单页协议确认重构为五步向导（协议 → 登录方式 → 环境准备 → 任务匹配 → 完成），沿用全屏阻断 + `!agreed` 显隐门槛；步骤条交互对齐 `HttpLoginWizard`（已到达可回看、未到达禁用）。协议步保留原文案，同意改走 `agreeWizardTerms`（写 `.agreed` 但不关向导）；环境准备步仅浏览器渠道显示（直连免 Python/浏览器），Python 侧采用 fire-and-forget `POST /api/environment/bootstrap` + 2s 轮询 `init-status` 的 stage/progress（同步端点最长可达数十分钟不能 await），浏览器侧复用 `GET /api/browsers` 探测/选择 + `POST /api/install/playwright` 安装，选择与 `useRunMode` 同口径整段 PATCH 后回读。
- 登录渠道切换遵循 `validate_login_task_binding` 合并态校验：浏览器渠道在方式步即时写 `login_channel`；直连渠道延后到任务导入成功后与 `active_http_task` **同一次** PATCH（新增 `configApi.patchProfileBinding` 承载方案域直改），无匹配导入时经确认保持浏览器渠道并在完成步如实展示。
- 任务匹配步：新增 `utils/repoSpeed.ts`——对 GitHub/Gitee 两个预设镜像源并行计时拉索引（复用 `GET /api/repo/fetch` 代理链路），`pickFastestSource` 选成功且最快者（全失败提示离线可重试）；匹配口径抽为 `useRepoImport.filterRepoTasks` 纯函数与弹窗共用（连续子串 includes，name/description/author/tags）。有匹配列出条目并打开复用的 `RepoImportModals`（预置胜出源 + 关键词 + 自动拉索引），导入经 `afterImport` 回调绑定方案（浏览器绑 `active_task`，直连同提交渠道+绑定）并自动进入完成步，不跳编辑器；无匹配时浏览器渠道提示回退内置默认任务，直连渠道提示可浏览全部任务或稍后再配。
- 完成步自动应用「调试模式」预设（`useRunMode.applyRunMode("debug")`），展示后续指引（方案页填凭据 → 设置·系统切回默认模式）；第 2 步起提供「跳过向导」。
- 新增 `components/common/BrowserIcon.vue`：按 channel 渲染 `public/icons/` 的品牌 SVG（chromium/msedge/chrome/firefox/webkit，未收录渠道回退通用图标），向导浏览器列表与「设置 · 浏览器」卡片共用（原先设置页内联的 v-if 链收敛到此，新增渠道图标只改一处）。
- `useRepoImport.ts`：`showRepoImport` 新增可选 `opts`（`source`/`keyword`/`autoFetch`/`afterImport`），`afterImport` 存在时导入收尾不打开编辑器不跳路由（回调失败仍如实提示「任务已导入但后续配置失败」）；`fetchRepoIndex` 新增 `keepSearch`（预置关键词不被拉取复位）。
- `useUi.ts`：`finishWizard` 拆为 `agreeWizardTerms()`（返回成功与否）+ `closeWizard()`。
- 样式：`wizard.css` 增补步骤条/渠道卡/环境清单/浏览器列表/测速结果/匹配列表/完成步样式（窄屏单列适配）。
- 任务匹配步视觉与无匹配引导优化（实测反馈）：学校名输入框接入全局 `.form-group` 皮肤（此前裸 input 无边框圆角，观感突兀）；无匹配提示重构为结构化面板——「当前无适配『X』的任务」+ 选项列表（浏览器渠道含「尝试默认任务」「尝试自定义任务」，直连渠道仅后者）+「详细请查看文档」链接（浏览器→`/docs/tasks/browser` 新增 `DOCS.taskBrowser`，直连→既有 `httpLogin`，排障→`faqLogin`；`/docs/tasks/browser` 已对文档站导航核实）+「浏览全部任务」按钮（两渠道均提供）。
- 测试：新增 `utils/repoSpeed.test.ts`（选源 9 例 + 测速 4 例）；`useRepoImport.test.ts` 增补向导预置打开、afterImport 接管/失败语义与 `filterRepoTasks` 用例。注意：匹配为连续子串 includes，缩写（如「电子科大」）不是全名子串——用例中已注释说明。

### 文档

- `docs/guides/user-guide.md`：增补「首次启动向导」小节（五步流程、调试模式含义与切回路径、跳过语义）。

## 开发中（2026-09-25 全项目功能 Bug 审查修复）

### 背景

- 用户要求全面审查并实测修复：8 个审查/验证子代理（分批并发 ≤2）覆盖 src/ 全模块、frontend/ 全量、python_worker/ 全量与前后端契约，产出 37 个确认问题 + 13 个待确认项；随后 13 个待确认项全部实测（临时测试跑完即删），实锤 7 / 部分成立 5 / 证伪 1（带出 2 个次生缺陷）。完整审查与实测证据在 `docs/reports/full-audit-2026-09-25.md`（本地不提交）。
- 本轮共修复 40 项（5 个 P1 + 33 个 P2/P3），另有少量低风险项（SSRF 代理面收敛等）按保守口径处理。

### 一、登录与引擎（login/engine）

- **P1** `login/session.rs` emit：终态关浏览器补渠道门控（`uses_bridge`）——此前脚本/直连会话终态会关掉共享 Worker 上无关的浏览器（任务页立即运行的浏览器任务、keep_alive 常驻门户页受害）；补正反两条单测。
- **实锤** `login/session.rs`：`bridge.execute` 的 `Err(WorkerCrashed | WorkerStartupTimeout)` 不再直接终态失败，映射为 `Outcome::NetworkError` 走既有重试+回收路径（Worker 死亡实际表现为 Err，原实现让浏览器自动登录一次崩溃即判死且不消耗重试预算）；其余 Err 保持终态。
- **实锤** `login/session.rs`：`verify_network_after_login` 纳入 `login_timeout` 剩余预算（post_login_delay + 探测原先完全在总超时之外，最坏多挂约 1 分钟）。
- **实锤** `login/mod.rs`：会话 panic 补偿改为**无条件**写结果槽——被抢占后 panic 的窄竞态不再让 `await_result` 永挂、自动登录失效到重启。
- **P2** `login/session.rs`：Worker 侧取消终态改用 `make_cancelled_result`，保留 cancel_reason（「用户取消」「被更高优先级登录抢占」不再被笼统的「登录已取消」覆盖）。
- **P2** `login/mod.rs`：清除四处残留的旧「13s」抢占预算注释（实际 18s）；`launcher.rs` 取消原因 `engine_crashed` 改中文。

### 二、更新器与助手（updater/helper）

- **P1** 上传包版本闸门改用**包内真实版本**：新增 `updater/version_info.rs`（手写 PE VERSIONINFO 解析，零新依赖，非 Windows 返回 None），`apply_uploaded_package` 不再拉远程清单、以提取版本做闸门并写入 pending.version——修掉「远程未发版时自编译包被误拒 / 远程有新版时可装入降级包」双向失真；`VersionUnrecognized` 映射 400。
- **实锤** `updater/check.rs`：通道候选按 semver 降序逐个尝试，`PlatformNotAvailable`/`ChecksumUnavailable` 回退次高（原先最高候选缺平台资产即整体失败，跨通道回退覆盖）；releases 列表请求复用 403/429 限流判定（`RateLimited` 带 retry_after）。
- **P2** `updater/mod.rs`：`apply_update` 幂等分支对齐 `ensure_helper_for_shutdown` 校验（pending 可读 + staging exe 存在），staging 失效返回新错误 `StalePending`（400）而非假装成功；卸载助手 spawn 失败时调用新增的 `restore_after_failed_uninstall()` 复位 `update_cancelled`/`update_in_progress`，修掉「卸载中止后更新被永久拒绝」。
- **实锤次生** `helper_main.rs`：`--apply-update` 下 pending 缺失/损坏 fail-fast 保留现场（不再走必然被 SHA 闸门拒绝的「按 CLI 参数继续」死路径，不再 cleanup 销毁可重试的 staging）；`wait_for_process_exit`/`verify_staging_sha256` 失败原因写入 helper.log（GUI 子系统 stderr 不可见）。
- **P2** `web/routes/uninstall.rs`：`disable_autostart` 改 `modify_settings_tx`（原锁外读改写与并发 PATCH 丢更新）；注释修正 `apply.rs`/`system.rs` 两处过时描述。

### 三、Web / 配置 / 杂项后端

- **P1** `web/routes/config.rs`：`POST /api/pure-mode` 落盘后补 `reload()`——原实现 ArcSwap 快照滞后，纯净模式开关实际不生效（Worker 永远拿旧值）。
- **P2** `web/ssrf.rs`：`is_restricted_ip` 覆盖 NAT64（64:ff9b::/96）与 6to4（2002::/16）内嵌 IPv4；代理路径不再跟随重定向（DNS 钉扎经代理失效，3xx 原样返回，剩余风险写明）。
- **P2** `logging.rs`：日志清理的「活跃文件」判定改按日期后缀 == 今天（原 `name == "app.log"` 是死代码，Unix 下 retention=0 会删掉正在写的当日日志）；retention `u64→u32` 防回绕；0 = 仅保留当天语义写入 doc comment。
- **P2** `ai/prompt.rs`：`find_login_anchor` 字节偏移容错回退字符边界（Unicode 折叠长度变化不再 panic）。
- **P2** `utils/platform.rs`：`reg delete` 后 `reg query` 复核——退出码 1 不再一律当幂等成功，真实失败（键值仍在）会报错。
- **P2** `scheduler/mod.rs`：定时任务 timeout 按绑定任务类型钳制并回写（浏览器 [1,600]、脚本 [1,3600]，与执行侧常量一致）；**补上 `scheduler/task.rs` `map_history_records` 输出 `duration` 字段**（落盘数据本就带值，前端耗时列此前永不渲染）；`cron_loop.rs`/`tasks/executor.rs`/`tray/mod.rs` 三处注释修正（凭据注入实情、Job Object 成功路径同样回收、监测文案）。

### 四、Python Worker

- **P1** `worker_main.py` `_command_timeout`：预算存在时 `0.9 × rust_timeout_ms` 全程权威，删除 `min(base, …)` 的 270s 地板（预算 600s 的长任务不再在 270s 被 Python 判死；close_browser 8s→7.2s 行为保持）。
- **实锤** `playwright_worker.py` `_to_ms`：timeout ≤0 回退默认值（原 0 → 步骤 1ms 瞬间失败，前端仅警告不阻断可写入 0）。
- **P2** `worker_main.py`：畸形命令防护——`params` 非对象 / `method` 非 str 回错误响应而非杀死整个 Worker；task 异常与 emit 失败路径补发回包防悬挂。
- **P2** `playwright_worker.py`：`_close_session` 与热恢复清理 `_wired_page_ids`（page 对象 id 复用不再漏绑 dialog 处理器）；close_browser 内部兜底让位命令级自愈（7.2s 前先触发，慢关闭不再被打成半关闭）；feedback_capture 补 cancel_id 注册 / 取消空闲回收计时 / finally detach；page_capture 分阶段取消检查；资源快照递归 childFrames + 逐 frame HTTP 回补（iframe 门户离线副本不再缺资源）。
- **实锤** 新增 `_sanitize_error()`：四个异常出口统一剥 URL query/fragment——Playwright 异常文本里的门户重定向 token 不再进 IPC、WS 推送与磁盘登录历史。
- 文档：`python_worker/README.md` 命令清单 14→15 项（补 `test_redirect`）、「轻量旁路可并发」改为如实的「不占槽位但串行排队」；两处过时注释修正。

### 五、前端

- **P1** `api/index.ts`：`systemApi.update` 超时放宽 600000ms（默认 30s 必然中断 >30s 的更新包下载，更新永远失败在半路）。
- **P2** 三个任务面板行尾菜单 document 监听器改 `onBeforeUnmount` 直接摘除（原 watch 写法卸载时永不执行，监听器泄漏）；批量导入展示后端 `failed` 明细、`imported=0` 不再弹绿色成功；HttpLoginWizard 步骤 1 文案改为可行的操作指引（原指向被向导遮住的编辑器）；AppTopbar「凭证→凭据」、重试按钮 title 与实际行为对齐；AiTaskView `crypto.randomUUID` 非安全上下文兜底；仪表盘「开始检测时长→已检测时长」+ `formatDuration` 中文紧凑式（测试同步）；`scheduledTasksApi` 四处 id 插值补 `pathSegment`。
- **P3 清扫**：useCarrierField / useRepoImport 过时注释、useScripts 重复注释、半角括号统一、删除无调用方的 `executeTask`/`executingIds` 死代码。

### 六、验证

- `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` 零告警；`cargo test` 全绿（lib 1068 + helper 12 + 集成套件，含本轮新增约 40 条用例）。
- `vue-tsc --noEmit` 0 错误、`vitest` 451 例全绿、`npm run build` 成功。
- `python_worker`：`uv run pytest` 220 例全绿（基线 200 + 新增 20）、`compileall` 通过。

## 开发中（2026-09-24 文档站链接修正）

### 背景

- 用户反馈：「脚本的链接根本没有那个文档啊，你去文档站写一下文档」——脚本渠道卡片上的「脚本登录文档」按钮指向 `https://campus-auth.misyra.com/docs/guides/custom-script`，而文档站（`Misyra/campus-auth-website`）的章节只有 getting-started / profiles / tasks / automation / maintenance / reference / faq / performance，**从来没有 `guides` 章节**，点过去落回默认文档首页。
- 文档站侧已新增 `2.5-script-login.md`（「脚本登录」，路由 `/docs/profiles/script-login`）并补 `LEGACY_REDIRECTS['guides/custom-script']` → 新路由，两处修复互为兜底：旧链接在文档站重定向，新链接由本仓指向正确路由。
- 顺带把用户给出的三条渠道定位写进文档：**浏览器自动化**给想开箱即用的小白/简单门户；**直连请求**给需要轻量运行的场景；**自定义脚本**给自己维护登录逻辑的用户。

### 一、文档站地址收敛到单一事实源

- `frontend/src/utils/constants.ts` 新增 `DOCS`（`gettingStarted` / `profiles` / `httpLogin` / `scriptLogin` / `faqLogin`），由 `DOCS_BASE` + `docUrl()` 拼出。此前这些地址以字面量散在四个组件里（`AboutView` / `HttpLoginWizard` / `SetupWizard` / `LoginChannelField`），漂移没有任何拦截——正是这次「链接指向不存在的路由」能悄悄发出去的原因。
- 四个组件改用该常量（模板侧走 `:href`）。注释里写明跨仓约束：**改这里的路径必须同时确认文档站存在该路由**，因为文档站的 `pnpm check:docs` 只扫它自己的源码，覆盖不到本仓；删改文档站路由时由它的 `LEGACY_REDIRECTS` 保留旧路径。
- 这一处修的正是脚本渠道的按钮（`/docs/guides/custom-script` → `/docs/profiles/script-login`），直连渠道的按钮同步改走常量（原地址本身是对的）。

### 二、验证

- `vue-tsc --noEmit` 零错误、`vitest` **449** 例全绿（文档地址不进单测，等价于未改行为）。
- 文档站侧：`pnpm check:docs`（28 篇 / 36 条路由，相对链接与绝对路由全部有效）、`tsc -b`、`eslint`（0 error）、`vite build` + `prerender`（33 页，含 `/docs/profiles/script-login`）、`smoke`（7 项）、`smoke-routes`（33 条路由：无控制台错误、无裂图、无横向溢出）。

## 开发中（2026-09-24 CI 跨平台 clippy 修复）

- **现象**：推送后 CI 的 `Rust Tests (macos-latest)` 与 `Rust Tests (ubuntu-22.04)` 都卡在「Clippy 零警告（-D warnings）」，`error: constant UNINSTALL_TEMP_PREFIX is never used`（`src/helper_main.rs:449`，`-D dead-code` implied by `-D warnings`），exit 101。
- **根因**：该常量只被 `#[cfg(windows)] fn spawn_uninstall_phase2` 使用（unix 走单段直删，没有第二段），而常量本身没加 `cfg`。Windows 上引用存在、不告警；macOS/Linux 上整块被门掉，常量就成了死代码。
- **为什么本地没发现**：Windows 单平台的 `clippy -D warnings` 结构上看不见这类问题——只有 CI 的跨平台矩阵会红。本地试图用 `cargo clippy --target x86_64-unknown-linux-gnu` 复现也走不通（`tray-icon` 的 `pango-sys` 需要交叉 pkg-config 与 sysroot，本机没有），所以这次只能靠 CI 兜住。
- **修复**：常量加 `#[cfg(windows)]`，并在注释里写明「unix 单段直删、不加 cfg 会让跨平台 clippy 红」，避免后来者把它当成多余的门删掉。
- 验证：本机 `fmt --check` / `clippy --all-targets --features no-embed -D warnings` / `cargo test --bin campus-auth-helper`（12 例）全绿；跨平台结论以随后一轮 CI 为准。

## 开发中（2026-09-24 全功能实测排查）

### 背景

- 用户诉求：「再全面细致排查一遍各项功能，你使用 agent-browser 实际启动试试，确保各个功能没有问题」。
- 做法：`agent-browser`（CDP，无 Playwright 依赖）在真实实例上点完所有页面并复核落盘结果 + 91 条路由全量扫 + 集成测试真起二进制；完整清单、截图与证据在 `docs/reports/full-audit/README.md`（本地不提交）。
- 结论：界面侧无崩溃 / 白屏 / 死链 / 未处理异常，91 条路由状态码全部符合预期；**但脚本渠道在默认配置下完全跑不起来**，本轮修掉。

### 一、脚本任务默认解释器跑不起来（真缺陷）

- **现象**：新建脚本任务（默认「执行程序 = Python (项目内解释器)」）→「立即运行」报 `IO 错误: program path has no file name`；方案绑定该任务并选「自定义脚本」渠道 → `POST /api/login` 0.2 秒即终态失败，消息 `登录脚本 audit-probe 无法执行: IO 错误: program path has no file name`。把解释器换成下拉里的具体 python 路径后，同一任务同一方案 5.4 秒登录成功（`退出码 0：… user=20230001 PW=*** isp= auth=`）。
- **根因**：`ScriptsPanel.vue` 把「项目内解释器」这一项存成**空串**，而 `build_script_command` 的四个分支写的是 `cfg.binary_path.clone().unwrap_or_else(|| default)`——`unwrap_or_else` 只兜 `None`，`Some("")` 被当作真实路径交给 `Command::new("")`。同一字段在三处各有解释（`uses_project_python` 与 `binary_to_ext` 早就把空串当未指定），只有执行分支不是。
- **为什么全绿却没拦住**：三份 e2e fixture 的脚本任务都是手写的 `"binary_path": null`（走 `None` 分支），且此前**没有任何测试真的跑过一次脚本任务**（`tests/*.rs` 没有 `POST /api/scripts/run` 用例）。直连渠道有 `http_login_chain`、浏览器渠道有 `login_chain`，脚本渠道只有单测。
- **修复**（`src/tasks/executor.rs`）：新增 `explicit_binary()`，把 `binary_path` 的**空白串按未指定**处理（trim + 过滤空串），`py` / `bat` / `cmd` / `sh` 四个分支统一走它；`execute_script_with_env` 增加前置判定，解析出的程序名为空时返回
  `环境能力错误: 未解析到可执行程序（脚本扩展名 .py）：项目内解释器不可用，且任务未指定「执行程序」`，不再把空路径交给 `spawn` 换一句英文平台错误。
- **回归测试 3 例**：空白 `binary_path` 在 `py` / `bat` / `sh` 三处都回退默认（逐字断言 program 与 args）、`binary_to_ext` 对空白仍推断为 `py`。

### 二、界面上与「脚本登录」自相矛盾的过时文案

- `ScriptsPanel.vue` 空列表说明原为「…支持 Python、Shell 或任意可执行程序；**要登录校园网请改用「方案」里的登录方式，不必写代码**」，「快速上手」原为「脚本只做辅助动作（打卡、签到等），**不参与登录认证**。」——而方案编辑器「自定义脚本」渠道的未绑定提示恰恰引导用户到这一页（`去新建 →`）。改为：脚本既能做辅助动作、也能当登录脚本，并补上退出码判定与重试口径；`useScripts.ts` 里同源的过时注释一并更正。

### 三、删除定时任务残留执行历史文件

- 真机证据：`DELETE /api/scheduler/jobs/sched_mufj7xv2_s411` 后 `tasks/scheduled/history/sched_mufj7xv2_s411.json` 仍在盘上——界面已无入口查看它，文件却永久残留；id 一旦被复用（旧版本按序号命名）会把上一个任务的历史当成自己的。
- `SchedulerService::delete_task` 现连同 `history/{id}.json` 一并清理（**尽力而为**：历史删除失败只告警，不影响任务删除结果）。`tests/scheduled_tasks.rs` 的生命周期用例补断言（删前手写历史文件，删后必须不存在）。

### 四、验证

- `cargo fmt --check` 干净；`cargo clippy --all-targets --features no-embed -- -D warnings` 零告警。
- `cargo test --features no-embed`：lib **1039** 例（上轮 1036，+3 为本轮新增回归）+ helper 12 例全绿；`http_login_chain` / `instance_lifecycle` / `scheduled_tasks` / `bridge_supervisor` / `updater_channels` / `smoke_test` 全绿。
- 前端：`vue-tsc` 零错误、`vitest` **449** 例、`vite build` 通过、`audit.mjs` 复核 `ghosts=0 dead=36` 未变。
- 本轮未覆盖：`tests/login_chain`（浏览器渠道真机链路）在本机走 skip 分支——其 `preflight()` 要求 `locate_python()` 命中的解释器能 `import PIL, ddddocr`，而仓库 `python_worker/.venv` 只装了 playwright（PIL/ddddocr 是可选能力，按设计不默认声明），以 CI 的 `e2e-login-chain` 作业为浏览器渠道的覆盖依据；卸载全流程演练未复跑，只验了守卫拒绝路径。

### 五、本轮登记的待办（详见 `docs/plan-next.md`）

- 登录历史不记渠道字段（三渠道只能从消息文本辨认）；「立即运行」后「最近结果」不刷新（后端异步执行、前端立刻拉取）；「设置 · 任务与环境」的「当前任务」对直连/脚本渠道显示原始任务 ID（直连的是自动生成的 `untitled_N`）；系统设置页部分开关不在可访问性树里；`TaskError::IoError` 全量人话化。

## 开发中（2026-09-24 登录新增第三种渠道：自定义脚本）

### 背景

- 用户诉求：「给登录任务增加一种渠道，就是自定义脚本，允许使用自定义脚本进行登录」。
- 此前登录只有两条路：浏览器自动化（按浏览器任务操作网页）与直连请求（程序内发一次 HTTP）。二者之间有个明显的空档——**请求形状本身能用直连表达、但构造请求要先按门户逻辑加密/签名/多步取参**的门户：直连的凭据变换脚本跑在无网络、无文件的 JS 沙箱里（不能发第二个请求、不能读文件、算力受限），写不出来的就只能退回浏览器。脚本渠道正是补这一段：把登录整个交给用户写的脚本，用自己的解释器、自己的库，程序只负责起进程、看退出码、再做一次登录后网络验证。
- 落地方式沿用直连那一套骨架（**进程内完成、不经 Bridge/Worker、不要求 Python 环境与浏览器**），因此会话状态机、抢占、重试、取消、登录后网络验证这些既有机制一行没改就复用了。

### 一、渠道枚举与方案字段

- `LoginChannel` 增加 `Script`（serde 字面量 `script`，`src/config/schema.rs`），并补两个判定方法把散落的渠道判断收口：`is_in_process()`（免 Python/浏览器，等于 `Http | Script`）与 `binding_field()`（该渠道用哪个字段承载"登录怎么做"）。`LoginChannel` 的另外两处调用点一并改走它：`monitor` 的认证入口预检（`auth_url` 对脚本同样只是可选输入，不能因其留空把有效方案误判为配置缺失）与前端 `channelNeedsRuntimeEnvironment`。
- `ProfileData` / `ProfileSnapshot` / `ProfileSummary` 增加 `active_script_task`（空 = 未绑定）。`ProfileData` 是 `#[serde(default)]`，**存量方案文件照旧解析**，无需迁移；新字段随下一次保存落盘（与 `active_http_task` 同款，未绑定时写空串而非省略）。
- `ProfileSnapshot` 的 Debug 已脱敏构造不变（新字段不进 Debug 输出，它不是凭据）。

### 二、脚本登录的执行（`src/login/script_login.rs`）

- 新增 `ScriptLoginPlan { task: ScriptTaskConfig, extra_env }`：由登录编排器在准备阶段构造（`resolve_script_task` 取任务 + `login_env` 取凭据），随会话参数下发；`Debug` 手写（`extra_env` 里躺着明文密码，派生 Debug 会在任何一次 `plan:?` 日志里把它打出来，与 `ProfileSnapshot` 同款处理）。
- **凭据经环境变量下发**：`CAMPUS_USERNAME` / `CAMPUS_PASSWORD` / `CAMPUS_ISP` / `CAMPUS_AUTH_URL`（四个名字由 `login_env` 显式列出，测试用 `LOGIN_ENV_KEYS` 对账）。脚本任务本身**不做** `{{USERNAME}}` 一类模板替换（那是浏览器任务在 Worker 侧的特性），故这是脚本唯一的凭据来源；四个变量改动必须同步指南与前端 `SCRIPT_LOGIN_ENV_VARS`。
- **成败按退出码**：`0` → `Outcome::Success`，随后照旧走登录后网络验证（`worker_config` 由编排器置 `{}`，`has_explicit_success_condition()` 恒为 false）——脚本"自称成功"与"真的通了"由后者裁决，与浏览器渠道同口径；非 0 → `Outcome::AssertionFailed`（可重试、**不**回收 Worker），与直连「未命中成功标识」同一档，受方案重试预算约束。连进程都没起来（`ScriptNotFound` / `UnsupportedExtension` / IO）判终态失败——重试不会让缺失的脚本文件出现；`ExecutionTimeout` 判可重试（`NavigationTimeout`）。
- **不新建第二套脚本执行实现**：`TaskExecutor` 增加 `execute_script_with_env(cfg, extra_env)`，把原来的 `execute_script` 收敛为"额外环境变量为空"的调用；额外变量叠在 `build_minimal_env`（已改名 `collect_minimal_env` + `build_minimal_env_with`）之上，`env_clear` 的隔离语义不变。登录脚本与任务页「立即运行」因此共用解释器回退、`tasks/scripts/` 路径约束、按任务串行的执行锁、超时与进程树回收、输出截断全套行为。
- `ScriptRunnerApi` trait（`src/tasks/mod.rs`，紧邻 `TaskApi`）：会话的 `SessionDeps` 守着"依赖 trait 化且非 Option"的既有约定，而真装配一个 `TaskExecutor` 需要 `BridgeSupervisor` / `EnvironmentManager` 全套服务——trait 化后状态机单测可注入替身。实现只有 `TaskExecutor`（方法名刻意不同于固有方法，避免自递归）。
- 登录历史消息里带脚本输出的**尾部 200 字符**，且**先抹掉本次注入的密码**（脚本打印含凭据的 URL 是常见写法，历史要落盘）：`redact_secret` + `tail_snippet`（压成单行、超长留尾）。

### 三、会话状态机与编排器

- `SessionParams` 增加 `script_plan`（与 `http_plan` 互斥）；`SessionDeps` 增加 `script_runner`；尝试循环的 `work` future 多一臂：脚本渠道 → `script_login::run_once`。
- `is_http` → **`uses_bridge`**（`http_plan.is_none() && script_plan.is_none()`，只有浏览器渠道为真）：取消时"要不要 `bridge.cancel`"、失败时"要不要 `force_recycle`"两处闸门都改走它。**这是必须的**——脚本渠道的失败若被判成"网络错误强制回收"，会去杀另一条正在跑的浏览器登录的 Worker。
- `LoginOrchestrator` 注入 `script_runner`（`container.rs` 传 `executor.clone()`，与调度器同一个实例，连"同任务串行"的执行锁都是同一把）；`submit` 里 `use_script` 与 `use_http` 并列，进程内渠道统一跳过浏览器预检、环境初始化与 auth_url 预检（`in_process` 一个变量表达）。
- `validate_profile` 增加脚本分支：未绑定脚本任务即终态失败（**脚本渠道没有内置兜底脚本**，与直连同理）；账号密码对脚本渠道同样必需（它们是 `CAMPUS_*` 的唯一来源）。新增 `resolve_script_task`，与 `resolve_http_task` 完全同构（空绑定 / 任务不存在 / 类型不对都给面向用户的明确文案，不回退）。

### 四、Web 层

- `validate_http_task_binding` → **`validate_login_task_binding`**：接收渠道 + 两条绑定，按渠道取对应字段与期望类型（覆盖三条保存路径：`POST` / `PUT /api/profiles/{id}` / `PATCH /api/config`）。错误文案对直连保持逐字不变（`直连任务 X 不存在或不是直连任务`），脚本侧同构（`脚本任务 X 不存在或不是脚本任务`），空绑定提示 `请为脚本渠道选择一个脚本任务`。
- `ProfileCreateBody` / `ProfileUpdateBody` / `PATCH /api/config` 白名单与类型校验、`GET /api/config` 扁平响应、`ProfileSummary` 一并带上 `active_script_task`；`login_channel` 的枚举错误文案改为「仅支持 browser、http 或 script」。
- 方案分享载荷清空 `active_script_task`（与 `active_task` / `active_http_task` 同口径：那是本机任务 ID）；导入侧仍忽略该字段，`login_channel` 非法值的报错文案同步。

### 五、前端

- 方案编辑器「登录方式」由两张渠道卡变三张（渠道网格改 `repeat(auto-fit, minmax(200px, 1fr))`，窄屏自动折列，不必再加一档断点）；新增「登录脚本」下拉（首项「未绑定（脚本登录不可用）」）与未绑定/任务已删除的当场提示、脚本登录文档入口。渠道面板类名 `http-channel-panel` → `channel-panel`（直连与脚本共用同一套外观，不再多一份副本）。
- `loginChannel.ts` 收敛渠道展示：`loginChannelLabel` / `ShortLabel`（新增「自定义脚本」/「脚本」）、新增 `loginChannelIcon`（收窄成 `"chrome" | "globe" | "code"` 字面量联合，否则宿主的 `IconApp` name 联合类型编译不过）与 `loginChannelHint`；方案列表卡的渠道徽标改由这三个函数渲染（原先内联三元写死了两个渠道）。`channelNeedsRuntimeEnvironment` 对 `script` 返回 false（仪表盘不再对脚本渠道提示"环境未就绪"）。
- 新增 `scriptTaskOptions` 与 `SCRIPT_LOGIN_CONTRACT_NOTE`（环境变量、退出码、网络验证、日志脱敏四件事一次讲清，面板 `?` 气泡直接用）；`SCRIPT_LOGIN_ENV_VARS` 与后端四个名字对账（有测试）。
- 保存闸口加脚本分支（未绑定即拒绝保存并 toast）；方案编辑器认证地址的说明对脚本渠道改说"它只作 `CAMPUS_AUTH_URL` 传给脚本"；导入预览改为按渠道渲染，并在直连/脚本方案后追加一段说明「绑定的任务不随方案分享，导入后请到对应任务页重选」——导出按契约清空了绑定，不点名的话用户导入完点登录只会看到一句"未绑定"。
- **「设置 · 任务与环境」的「当前任务」改为按渠道取**（顺手修掉一个被第三个渠道暴露出来的既有失真）：它原先**恒取浏览器任务的 `active_task`**，于是直连渠道（本轮之前就如此）与脚本渠道下，卡片会显示一个登录时**根本不会执行**的任务名，用户据此排查会走错方向。现按 `login_channel` 分别取 `active_task` / `active_http_task` / `active_script_task`；直连与脚本侧直接显示任务 id（任务页列表里的标识），浏览器侧沿用友好名称；未绑定则显示「未绑定（直连登录不可用）」这类明确文案而非「内置默认任务」。渠道卡网格改 `auto-fit` 后 `audit.mjs` 复核 `ghosts=0 dead=36` 未变。

### 六、测试

- Rust（lib +14 例）：`script_login` 六例（环境变量契约与空值注入、退出码 → outcome、失败不回收 Worker、密码脱敏、消息只留尾部且单行、Debug 不泄密码）、`schema` 两例（枚举字面量与两个判定方法、存量方案缺字段仍可解析）、`profiles` 路由四例（未绑定/绑错类型/绑定正确落盘/分享载荷清空脚本绑定）、`config` 路由一例（`PATCH` 脚本渠道的绑定校验与"落 Profile 而非全局设置"）。
- 前端（vitest +8 例）：标签/短标签/图标/悬停说明三渠道映射、环境判定、环境变量清单与契约文案、脚本任务下拉首项、`useProfiles` 的脚本保存闸口（未绑定拒绝且不发请求、绑定后带字段提交、直连既有闸口未放宽）。
- 全量：`fmt --check` 通过、`clippy --all-targets --features no-embed -D warnings` 零告警、`cargo test --features no-embed` lib **1036** 例（上轮 1020）+ helper 12 例全绿、`vue-tsc` 零错误、`vitest` **449** 例、`vite build` 通过。

### 七、真机验证（独立实例，不动用户正在跑的实例）

- **做法**：`cargo build --target-dir target/verify` 单独编一份 exe（用户彼时正跑着 `target/debug/campus-auth.exe`，文件被锁，也**不该**去动他的数据目录），以 `--base-path target/verify-base --port 50751 --no-browser --no-tray` 起一个隔离实例，全部通过 API 建脚本任务与方案。这是本功能唯一"真的起过进程"的证据——脚本渠道的执行、环境变量注入、退出码判定、脱敏、重试预算，四件事都只有真机能串起来验。
- **成功路径**：`login_channel=script` + 绑一个内联 `.bat`（`binary_path: cmd.exe`，`echo SCRIPT-LOGIN user=%CAMPUS_USERNAME% …` + `exit /b 0`）→ 5.2 秒返回
  `登录成功（登录脚本 login-probe 退出码 0：SCRIPT-LOGIN user=20230001 isp= auth=）`。
  **一句话里同时印证四件事**：脚本真跑了、`CAMPUS_USERNAME` 注入成功（`user=20230001`）、`CAMPUS_ISP`/`CAMPUS_AUTH_URL` 按契约注入空串（方案没填）、退出码 0 之后网络验证兜底确实执行并确认在线。
- **失败路径**：`exit /b 3` → **4 次尝试 / 35.1 秒**（1 次 + 重试 3 次，间隔 5→10→20 秒，与「重试间隔逐次翻倍」的既有策略一致），终态
  `重试耗尽（共 4 次尝试）: 登录脚本 login-fail 退出码 3：trying with PW=*** portal said no`；把 `retry.max_retries` 改为 1 后同一脚本变成「共 2 次尝试」——**重试预算确实由全局策略控制**，脚本渠道没有旁路。
- **脱敏**：上面两条消息里的 `PW=***` 与登录历史里落盘的内容一致（`GET /api/history` 复核），密码没有以明文进历史。
- **绑定闸口**：未绑定 / 绑到浏览器任务 / 绑到不存在的 id / `PATCH /api/config` 切脚本不给绑定，四条路径**全部 400**，文案分别是「请为脚本渠道选择一个脚本任务」「脚本任务 default 不存在或不是脚本任务」「脚本任务 nope 不存在或不是脚本任务」与同上第一条。
- **运行期任务消失**：绑定后删掉那个脚本任务再登录 → 0.0 秒即以
  `方案绑定的登录脚本 login-probe 不存在，请在「任务 · 脚本」里重新选择` 失败（终态、不重试），与 `resolve_script_task` 的实现一致。
- **真机跑出来的一个真缺陷（已修）**：第一次用**一位密码**的方案做脱敏验证时，脚本打印的 `isp=` 被改成了 `is***=`——`redact_secret` 用的是 `String::replace`（纯子串替换），短密码的每个字符都会命中无关字段名，把消息改烂。单测当时用的是 `hunter2`，看不出来。改为**只在词边界上替换**（命中子串前后若仍是字母/数字/下划线则不替换：`PW=<密码>` 照样命中，`isp` 里的 `p` 不受影响），补 2 例回归测试（短密码不切碎字段名、延长标识符内的一段不算密码出现），并在真机上重跑同一场景确认 `PW=*** isp= auth=`。
- 收尾：验证实例已停止、`target/verify-base` 已删除；构建目录 `target/verify` 留在盘上（`target/` 已 gitignore）。用户那个 19:33 起跑的实例全程未被触碰。

### 有意不做 / 遗留

- **登录历史不记渠道字段**：三条渠道现在只能从消息文本辨认（「步骤 N/M」/「直连请求成功」/「登录脚本 X 退出码 N」）。加字段要动历史 schema 与历史页，与本轮目标无关，仍留在 `plan-next.md`。
- **脚本渠道不做"不重试"的表达**：退出码只有"成功/失败"两态，凭证无效也会按重试预算重发（与浏览器渠道的验证码失败同类）。真要做需要给退出码约定第三语义，而约定越细用户越容易写错；当前用全局重试策略兜底，并在指南里讲清。
- **脚本子进程仍不保证静默**：与「立即运行」同一条路径，未设 `CREATE_NO_WINDOW`，控制台版解释器可能闪现窗口（既有行为，见 `custom-script-guide.md` 第 7 节）。

## 开发中（2026-09-24 未提交更改的全面审查与修复：六条 P1 + 一批 P2/P3）

### 背景

- 用户诉求：「全面审查一下当前更改」→「你修复优化一下」。审查对象是当时工作区里那 95 个文件 / +6985 −3706 的全部未提交改动（卸载模式、任务页四面板、样式统一、文档三件套）。
- 做法：**独立复跑**全部验证块 + **四路并行对抗性复核**（前端逻辑层 / 前端视图与样式 / Rust web·tasks·login·updater / 文档与代码一致性），每条结论都回到实现代码复核判据后才写进报告；报告在 `docs/reports/current-changes-review-2026-09-24.md`（本地不提交）。
- 复跑结果：`fmt --check` 通过、`clippy --all-targets --features no-embed -D warnings` 零告警、`cargo test --features no-embed` lib **1014** + helper **12** 全绿、`vue-tsc` 零错误、`vitest` **424**、`vite build` 通过、`audit.mjs` **ghosts=0 dead=36**、任务仓库四个远端索引实测 **9 / 1 / 9 / 1**（Gitee 侧 `haust` 已可达）——**与 changelog 原先的声称全部一致**，故本轮不动那批数字。
- 本轮修的是复核**新发现**的问题，其中六条属"会丢数据 / 不可逆 / 功能失效"级别。

### 修复：P1（每条都配了回归测试）

- **跨桶 `untitled_N` 撞 id 会静默删掉另一类任务的文件**（数据丢失，且几乎必然发生）：浏览器任务与直连任务各自只在**自己那一类**的列表里取号，都从 `untitled_1` 起算；而后端 `save_task` 写盘时会删掉**另外两个桶**里同 id 的文件（那条清理是给"用户主动改任务类型"用的，注释假设"同 id = 换类型"）。于是"两类各新建过第一个任务"之后，改一下直连任务的地址就会把 `tasks/browser/untitled_1.json` 删掉，界面上什么都不说。
  - 前端取号改为对**三类 id 的并集**去重：`useTaskDirectory.allTaskIds()`，`createTask` / `duplicateTask`（浏览器）与 `createHttpTask` / `duplicateHttpTask`（直连）四处都换成它——`duplicateTask` 的 `_copy` 后缀走的是同一条路径，一并修掉。
  - 后端删别桶残留时改走 `tracing::warn!`（原先 `let _ = remove_file(..)` 完全无声）：这条路径正常来自"改类型"，一旦来自撞车，日志就是唯一的追查线索。
- **「保留配置与任务」保留的配置解不开密码**：第一步 `POST /api/uninstall` 原先**无条件**删除 `~/.campus_network_auth`（其中 `.enc_key.rs` 是方案密码 AES-256-GCM 的主密钥，路径与 `base_path` 无关）。勾选"保留"时 `config/` 留下来、密钥却被删掉，下次启动用新密钥解旧密文必然失败——用户得把所有方案密码重填一遍，而 updatelog 承诺的是"换目录重装后直接可用"。
  - `POST /api/uninstall` 新增可选 body `{ keep_user_data }`（`CleanupRequest`，缺省 `false` = 原语义，老调用方不受影响）；勾选时跳过密钥目录并在步骤结果里明说"已保留（保留的配置需要它才能解密）"。
  - 前端 `uninstallApi.uninstall(keepUserData)` 与 `purge` 传**同一个变量**（两处各写各的默认值正是这类 bug 的来源），`useUninstall` 的单测钉住"两步收到同一个值"。
  - 顺带修文档：user-guide 原先写"只删程序文件"（实际是"除五个数据目录外的一切照删，含未知文件"），并把密钥目录的处置写准。
- **不可逆步骤排在可能失败的步骤之前**：前端顺序是"先清系统残留（删密钥目录 / 自启动 / 浏览器缓存）→ 再 purge（spawn 助手）"，而 `spawn_helper` 在助手缺失时必然失败（有单测）——于是"凭据已删、程序还在"，白丢一轮且不可回滚。
  - `purge_uninstall` 在守卫之后**先查卸载助手是否在位**，缺失即 400 + 可执行的文案（"请重新解压完整发布包后再卸载（程序文件未被删除）"），不再等到 spawn 那一步才报 500。
  - `detect` 回报 `helper.{label,path,exists}`；前端把它合成 `blockReason`（与守卫拒绝同一个展示位），助手缺失时按钮直接禁用并说明原因——连确认框都不弹。
- **`cancel_pending_update()` 挡不住"卸载后被更新回来"**：原实现只判"此前有没有 pending"就返回 `true`，而两条路径仍可让更新在卸载中发生：① 更新助手**已经 spawn**（`--apply-update --pid … --staging … --target …`，CLI 参数优先于 pending，助手最多等 60s），用户在这窗口里点卸载，助手醒来读不到 pending 但 CLI 的 staging/target 仍生效；② 取消之际有一次下载在途，它跑完后**无条件**写 `pending.json` 并 spawn 助手。这正是该函数存在的理由。
  - 新增 `update_cancelled` 标记（`UpdaterService`）：`cancel_pending_update` 先落标记、再抢占下载互斥，然后才清理；`apply_update` / `apply_uploaded_package` / `finalize_staged_package`（三条落盘路径的唯一出口）/ `ensure_helper_for_shutdown` **四处复查**该标记，命中即 `UpdaterError::Cancelled`（409）并清掉解压产物。
  - 返回值改为"**确实**取消掉了"：清理后复查 `has_pending_update()` 与 staging 是否真的消失（清理是 best-effort，不复查就会把"没删掉"报成"已取消"）。
  - 取消失败时不再沉默：`purge` 响应新增 `pending_update_left`，前端回执里明说"程序可能在退出后被更新助手重新安装"——否则用户以为卸干净了、下次开机却看到程序还在。
- **定时任务新建的 `_isNew` 会卡死**：`onSaved` 是 `_isNew` 唯一的翻转点，而 `run()` 在"被后一发顶掉"时先于它返回；于是首发 POST 的成功被丢弃后，之后**每次**自动保存都会 POST 一个已存在的 id → 409，面板"存不下去"直到重开编辑器（另三个面板用幂等 PUT，同一条竞态能自愈——这是四面板分化处）。
  - `autosave.run`：落盘成功时 `onSaved` 的调用只保留"这份草稿仍在编辑器里"这一个条件（成功是关于草稿的**事实**，与"由哪一发接管界面态"无关），基线/状态字仍受序号判断保护。
  - `useScheduledTasks.persist`：create 遇 409 降级为 PUT 继续（`isConflictError`，按 409 / `CONFLICT` 判），不再弹一句假的"已存在"；非冲突错误照旧报错、不偷偷降级。
- **脚本面板：打字停顿会锁死 ID**（ID 落盘后不可改、输入框随即禁用）：闸口只看"当前值恰好合法"，而 `camp` 在打字途中就是合法 ID、stub 内容也非空——停手 500ms 即落盘，`campus` 再也打不完，只能删掉重来（还留一份 `camp.json`）。
  - `scriptDraftGaps` 新增可选上下文 `{ idPending }`；`useScripts` 持有 `scriptIdPending`（新建态为真），面板在输入框**回车 / 失焦**时调 `commitScriptId()` 收口。
  - `commitScriptId` 延后一拍（`nextTick`）执行：失焦与"点另一个按钮"在同一轮事件里，用户点的是「放弃」时草稿已被清空，这里就什么都不做——否则会先创建一份再把它删掉。
  - 提示文案同时改准（原先写"字母开头、只能用下划线"，比后端 `is_valid_task_id` 严，正是 2026-09-23 修过的那类死角）。

### 修复：P2 / P3

- **保存闸比执行闸松：请求头 256 KiB vs 64 KiB**（存得下、必然登不上；指南写的又正是 64 KiB）：`tasks::MAX_HTTP_HEADERS_BYTES` 对齐到 64 KiB，并补 `login_and_task_size_limits_agree` 把"两处必须同值"钉住（URL / 请求头 / 请求体三档一起比）。
- **在途载荷相同就不再发一遍**：debounce 到点会清掉 `pendingChanges`、基线要等响应才更新，于是"在途"这段窗口里 `flush`（换编辑对象 / 关闭编辑器）会重复发一次同样的请求——对三个 PUT 面板是白跑，对定时任务是第二次 POST 一个刚建出来的 id（409 假报错）。`autosave` 记 `inFlight{seq,mark}`，命中即不重发。
- **`clear()` 使在途那一发作废**：`resetMachine` 顺带推进 `seq`，迟到响应不再把 `lastSavedFingerprint` 写成已丢弃草稿的指纹、也不再调 `onSaved`（注释同时写明：这**不能**撤回已发出的请求，"放弃新建"仍可能创建文件——接口的硬限制，故文案不承诺"什么都没写"）。
- **detached 一发的失败不再被序号判断吞掉**：失败分支的 `seq` 检查只用来决定"要不要写状态字"，日志与"上一份的改动未保存"提示照发（原先 `mine !== seq` 直接 return，注释却写着"已切走也照样出声"）。
- **`DATA_DIR_NAMES` 改由 `utils::paths` 常量拼出**并加 `test_data_dir_names_match_paths`：五个名字是"保留数据"与"删除其余一切"的分界线，两边各写一份时改一处漏一处，后果是用户勾了保留却丢那一项。
- **守卫拒绝 cargo 构建输出**：`is_cargo_target_dir`（路径含 `target` 组件**且**祖先有 `Cargo.toml`——只按组件名判会误伤用户自建的 `D:\target\`）。开发实例住的 `target/debug/` 里既没有 `.git` 也没有 `Cargo.toml`，原先会放行并删掉整个构建产物目录（含那份实例的数据目录）。
- **`InvalidTaskId` 的 400 补齐**：`GET /api/tasks/{id}` 与 `GET /api/tasks/export/{id}` 原先用 `has_task` 短路，畸形 id 报 404「任务不存在」，与 PUT/DELETE 的 400 矛盾；改为交给加载路径判（`InvalidTaskId → 400`、`TaskNotFound → 404`），`web/error.rs` 补一条映射断言（把映射改回去则测试失败）。
- **危险步骤 / 凭据脚本的提示移到编辑器内**：HEAD 里浏览器任务保存前会弹「检测到危险步骤」、直连任务会弹「任务包含凭据变换脚本」，改成自动保存后那个时机**不存在**了（函数仍在、零引用），等于静默去掉了一道提示。改为常驻提示：浏览器任务在 JSON 卡下方列出"第 N 步 evaluate"（`useTasks.dangerousSteps`，纯解析、不碰 `jsonError`），直连任务在凭据脚本框下方说明执行环境与"改一个字就自动保存"。
- **非每日 cron 不再被无关编辑静默改写成每日**：编辑页对"表单表达不了"的表达式有明示，但载荷**总是**带表单值、PUT 又是按字段合并——于是"打开只在周一跑的任务、只改个名字"就把调度改成天天 08:00。`scheduledDraftPayload` 改为：原表达式表达不了 **且** 时间控件未被触碰 → 原样带回原表达式（新增界面态 `_originalSchedule` 记录载入时时分），动过时间控件才按表单值改写。
- **列表刷新加世代号**：`useTaskDirectory.fetchDirectory` 与 `loadScheduledTasks` 允许重叠（落盘后的 `force` 刷新 + 守卫内的自然刷新），两个响应到达顺序不保证——较旧的快照后到会让"刚编辑的任务"从列表消失，对账逻辑判定它已被删除 → 关掉编辑器、未落盘的改动随之丢失（`plan-next` 早先把这列为"存疑未验证"，本轮确认机制成立）。改为只有最新那一发的响应才允许写列表，过期快照（含其失败提示）直接丢弃。
- **文档同步**：`AGENTS.md` 补 `uninstall/` 与 `browser.rs` 模块、routes 域列举补 `http_tasks`、`docs/` 描述去掉不存在的 `archive` 并补 `assets/` + `promo/`，新增「卸载与更新助手」陷阱节（清单单一事实源 / 别在 `target/` 下试卸载 / 两段式与 cmd 引号 / 保留数据必须保留密钥目录）、Updater 节补取消语义；`plan-next.md` 的 openapi 指标由过期的 88/103 更正为 91/106 并登记本轮；`task-manual.md` 的定时任务与脚本小节按实现改准（"按上方时间"、ID 需回车 / 失焦确认）；`frontend/src/api/types.ts` 的退出登录顺序注释（原写"在凭据变换脚本之后"，实际排**最前**）、`http_login.rs` 的步骤编号（`// 2.` 曾物理排在 `// 1.` 之前）与"三处都用它"（实为四处）一并修正；`ScriptsPanel` 的导入提示补 `.cmd`（`pickFile` 已接受它）。
- **仓库卫生**：`.gitignore` 补 `*.bak`；仓库根的 `--full-page`（202 KB，实为 PNG）与 `campus-auth.exe.bak`（26.5 MB）移到已忽略的 `docs/reports/stray-from-root/`（不删，用户可自行处置）。

### 验证

- `cargo fmt --check` 通过；`cargo clippy --all-targets --features no-embed -- -D warnings` 零告警；`cargo test --features no-embed`：lib **1020 例**（上轮 1014，+6：`login_and_task_size_limits_agree` / `test_data_dir_names_match_paths` / `test_is_cargo_target_dir` / `test_validate_install_dir_rejects_cargo_target` / `test_cancel_pending_update_clears_state_and_reports_truthfully` / `test_cancelled_update_refuses_all_entry_points`）、helper bin **12 例**、各集成测试全绿。
- 前端：`vue-tsc` 零错误、`vitest` **441 例**（上轮 424，+17）、`npm run build` 通过。
- 复跑说明：Rust 测试仍在独立 `CARGO_TARGET_DIR=target/review` 下跑，用户正在使用的 `target/debug/campus-auth.exe` 全程未被覆盖（前后核对 mtime 未变）。

### 有意不做 / 遗留

- **「放弃新建」不撤回已发出的请求**：那一发已经在路上，脚本 / 任务文件仍会被创建（要真取消得给四个面板各配 `AbortController`）。本轮的改动是让 `clear()` 不再让迟到响应改写状态，文案也不承诺"什么都没写"。
- **改任务类型（把 JSON 的 `type` 换成另一类）后本面板的编辑器仍会被对账逻辑关掉**：任务确实换了桶、本面板列表里不再有它——提示措辞"任务已不存在"不准，但行为比"每改一处都 PUT 一个不在本类型的 id"要好。已登记 `known-issues.md`。
- **`python_worker/captures/`（AI 生成任务的页面捕获）随 `python_worker/` 一起被删**：勾选「保留配置与任务」不含它，已登记 `known-issues.md`。
- **Docker 形态下的卸载未评估**：容器内 `install_dir` 指向 `/app`，守卫会放行（有 exe、无 `.git`），该形态是否符合预期未知。
- **未重跑真机目检与卸载演练**：`docs/reports/ui-audit/visual-check.py`（12 路由）、`docs/reports/uninstall-e2e/rehearse.ps1`（34 项）都在盘上、时间戳为当日，未重跑（前者要起实例 + Playwright，后者会弹阻塞式系统提示框）——卸载链路的结论以"代码 + 脚本 + 单测"为准。

## 开发中（2026-09-24 前端 UI 风格全面统一：组件契约层收敛 + 死代码清理 + 断点归尺）

### 背景

- 用户诉求：「准备全面统一前端界面 UI 风格，你先探索，拿到结论」——探索结论见 `docs/reports/ui-audit/conclusion.md`（本地不提交）。
- 探索用的是规则级证据而非目检：`docs/reports/ui-audit/audit.mjs` 用 Vue 编译器 AST 取模板 class、再与样式表选择器集合对账，产出「幽灵类 / 死类 / token 逃逸 / 断点分布」四张表。
- **总判断：token 层是健康的，问题在组件契约层**——同一个视觉角色有 2–15 套平行实现。`base.css` 之外的字面 hex 只有 11 处、字面 rgba 只有 9 处（多为不可 token 化的黑色遮罩），六组 token（颜色/圆角/间距/字号/动效/层级）覆盖完整。

### 一、卡片头三套并存 → 一套

- **`.card-header h3` 全仓没有任何规则**。`card.css` 只写了 `.card-header h2`，于是四个任务面板编辑页的 `<h3>` 落到浏览器 UA 默认（`1.17em` ≈ 16.38px / `font-weight: bold` 700），而设置页卡头是 14px/600、其余页面的 `<h2>` 是 16px/600——同一个视觉角色三种渲染，且任务面板那一种**从未被有意选择过**。
  - 反讽之处：`scheduled_tasks.css` 的注释写着「与其余三个子页同一套组件、**同一个 16px 标题**」——作者以为在用 16px。
  - 修法：`card.css` 把 `h3` 纳入 `.card-header h2, .card-header h3` 同一条规则；`settings-card-header` / `appearance-card-header` 的标题字号从 `--text-base` 提到 `--text-lg`。全站卡头标题统一 16px/600。

### 二、页面外壳：入场动画从"各处手抄"改为结构选择器单一出口

- `.page-content` 此前**只挂了一个入场动画、没有任何布局**，且只被七个页面里的五个使用（`AppearanceView` / `AiTaskView` 的根节点不带该类）。结果是 `ai_task.css` 手抄了一份 `animation: pageEnter`，而外观页干脆没有——同一个转场三种待遇：设置页切 Tab 无过渡、任务页四个 Tab 里唯独 AI Tab 有。
- 改为三个 router-view 落点各一条结构选择器（`layout.css`）：`.content-wrapper > *`（顶层页面）、`.settings-form > *`（设置页 Tab）、`.tasks-page > :not(.tasks-narrow-nav)`（任务页面板）。删掉 `.page-content` 的动画声明与 `ai_task.css` 的手抄副本。
- 收益：新增页面/面板自动获得一致转场，且设置页切 Tab 从此也有了过渡（此前完全没有）。

### 三、小标签（chip）三份逐字重复 → 全局一套

- `HttpTaskFields.vue` 的 `.http-chip`（scoped）与 `views/tasks/HttpTasksPanel.vue` 的 `.http-chip`（scoped）**规则体逐字相同**，`HttpLoginWizard.vue` 的 `.wz-chip` 只差容器类名——同一个"等宽词条"视觉三份平行实现。根因是 scoped 样式无法跨组件复用。
- 新增 `frontend/src/styles/components/chip.css`（并进 `index.css` 的组件层）：`.chip` / `.chip--fn` / `.chip--dense`（表格行内密集档，原 `.tsk-chip`）/ `.chip--muted`（原 `.tsk-chip--muted`）/ `.chip-row` / `.chip-row--inline` / `.chip-row-label`。三个文件的 scoped 副本与 `tasks.css` 的 `.tsk-chip` 一并删除，模板改引用共享类。

### 四、提示条（note）四份同族 → 全局一套

- `.wz-warn`（HttpLoginWizard）与 `.http-risk-note`（HttpTaskFields）**规则体逐字相同**；`.http-task-alert`（LoginChannelField）是同族第三份（只差 padding 8px 10px 与 `align-items:center`）；`.browser-info-tip` 是同款的 accent 色调变体（只差字号与一个多余的 margin-top）。
- `misc.css` 新增 `.note` / `.note--warn` / `.note--info`，四处改用共享类。**`--danger` 与 `--flush` 两个变体故意不预先声明**——当前没有使用者，本仓的教训正是"留着一堆没人用的平行样式"。`settings-load-failed` 按其"带操作按钮的阻断横幅"语义保留独立形态（文件内已注明分工）。

### 五、布尔开关：**核查结论是"本来就统一"，问题在别处**

- 探索阶段曾据 `<input type="checkbox">` 单行判断"MonitorSettings 8 个全是裸复选框"，**这是错的**：25 个 checkbox **全部**走 `<label class="toggle">` + `.toggle-slider` 标准开关（用 AST 取祖先 label 复核，`未走标准开关 = 0`）。已修正结论文档。
- 真正的分歧在**两种实现的开态不一致**：`ToggleSwitch.vue`（按钮型）是纯色 `background-color: var(--accent)`，而 `.toggle`（label 型）是 `accent→accent-hover` 渐变 + `0 0 12px` 发光。现把按钮型对齐到 label 型（同一组声明），两者的 `transition` 也一并补上 `border-color`/`box-shadow`，开态不再"边框与发光瞬间跳变"。
- 顺带清掉一个幽灵修饰类：`ProfilesView.vue` 的 `<label class="toggle compact">`——`.compact` 只在 `custom-select.css` 里定义，对 `.toggle` 完全无效。

### 六、清理死 CSS（零引用，且已排除运行期拼接 / Transition / JS 施加）

- **删除 `styles/pages/settings/account.css` 整个文件**（并从 `index.css` 摘掉 import）：`/settings/account` 自「账号移交配置方案页」起只是重定向到 `/profiles`，文件里 `current-profile-hint` / `hint-link` 零引用。
- 删除 36 个零引用规则块（脚本 `prune.mjs` 按精确选择器删除，删后断言全样式表花括号配平）：`settings/tasks.css` 的 `.task-mini-grid` / `.mini-stat*` / `.task-panel-actions` / `.task-quick-actions` / `.task-overview-row|info|desc` / `.ocr-recognize`（共 2.8 KB）、`settings/common.css` 的 `.settings-panel-grid--task`、`settings/monitor.css` 的 `.settings-detect-columns`、`settings/environment.css` 的 `.env-status-summary`、`settings/browser.css` 的 `.shell-custom-input*` / `.status-custom` / `.browser-desc`、`about.css` 的 `.update-section` / `.uninstall-item-size`、`profiles.css` 的 `.editor-divider`、`notification.css` 的 `.notify-icon`（3 条）、`dashboard.css` 的 `.network-status-banner .status-icon`、`log-viewer.css` 的 `.log-name` / `.log-filter-chip`（3 条），以及 `misc.css` 的 `.ws-kicked-banner` / `.text-secondary` / `.text-success` / 旧别名 `.monospace-textarea`、`btn.css` 的 `.btn-lg`。合计约 4.2 KB 规则 + 整个文件。
- **把"注释说退役、代码还在用"的一处真正收口**：`settings/common.css` 写着「旧 `.settings-monospace-textarea` 退役」，而 `BrowserSettings.vue`（2 处）与 `MonitorSettings.vue`（2 处）用的正是它、文档指定的新名字 `.textarea--mono` 反而零引用。现四处改用 `.textarea--mono`，旧类删除。
- **不再保留动态 class 误报**：`badge-script/badge-browser`（`'badge-' + task_type`）、`chip-*`（`'chip-' + ev.type`）、`level-*`/`log-*`（`'log-' + level`）、`source-frontend`、`step-*`（DebugPanel 状态函数返回）、`checking/connected/idle`（后端状态串）、`detected/not_detected`（`useRedirectTest` 的 status）、`dragging`/`exit-overlay`/`has-custom-bg`/`no-backdrop-filter`（JS `classList`）、`modal-fade-*`（Vue `<Transition>`）、`saved`/`saving`/`tsk-autosave`（`autosave.ts` 状态字）——审计脚本改为逐条列出**来源**的 `RUNTIME_APPLIED` 表，而不是一个沉默白名单。

### 七、幽灵类：18 个 → 0

- **补样式**：
  - `.rust` / `.tokio` / `.vue`（AboutView 技术栈徽标）：`about.css` 只给 `python / fastapi / pyinstaller / websockets / pydantic / playwright / uv / ddddocr` 配了色——Rust 重写后模板已换成 Rust 栈，配色没跟着换，三个徽标因此**没有颜色**。现把死掉的 python 时代配色（`fastapi` / `pyinstaller` / `pydantic`）换成 `rust` / `tokio` / `vue`，并把 8 条×4 行的平行规则收敛成"每条只声明一个 `--tech` 品牌色、底色/描边/文字统一由它 `color-mix` 派生"。
  - `.tsk-cell-flex`（HttpTasksPanel 的 `<th>`/`<td>`）：与 `.tsk-col-flex` 成对，却全仓没有规则——"请求"这一格拿不到任何单元格级约束。现补 `min-width: 0`，并给该 `<td>` 补上同排邻居都有的 `.tsk-cell-ellipsis`。
- **删空钩子**（引用了但全仓无规则、且父级已提供全部样式）：`.tech-item`（8 处空壳 div，`.tech-stack` 本身已是 flex）、`.appearance-card`（4 处，与 `appearance-section-card` 同挂）、`.appearance-sliders`、`.channel-section`、`.http-fields`、`.wz-body`、`.ai-config-body`、`.repo-item-author` / `.repo-item-tags`（`.repo-item-meta` 已提供 flex+gap+弱色）、`.update-progress`、`.run-mode-card`、`.browser-error-row`、`.ocr-result-label`；`.loading` 改用已有的 `.hint`（它本来就是一行状态文字，不是 `.loading-state` 那种居中大块）。

### 八、断点归尺：11 个取值 → 4 个

- 收敛前全仓出现 `520 / 640 / 720 / 768 / 860 / 900 / 960 / 980 / 1024 / 1100 / min-1080`，其中 5 个只出现在单个文件里，结果是同一类"窄屏降级"在不同页面于不同宽度触发。
- 定为四档并写进 `responsive.css` 文件头：**640（手机窄屏）/ 768（窄屏，侧栏收成图标条）/ 900（平板窄区）/ 1100（平板宽区）**，反向只有 `min-1100` 一档（"够宽才开双列"）。
- 映射规则：就近落到某一档，且**只允许让降级更早发生**（隐藏的元素集并集只多不少）。`scheduled_tasks.css` 原有 1100/980/860 三档，其中 980 与 860 都是"平板窄区"的同一意图，且 860→768 的映射会让 768–900 这一段反而多留「最近结果」（名称列掉到 90px）——故合并为 1100/900 两档，名称列在每一档都只更宽。`ai_task.css` 的 520→640、960→900；`environment.css` 的 720→768；`min-1080`→`min-1100`；`responsive.css` 的 1024 并入 1100 档，两个重复的 768 块合并为一块。

### 九、顺带做掉 `docs/plan-next.md` 里的前端项

- **`useConfig` 保存期间的在途编辑被静默丢弃**（原报告 P3，修法已明确但需同步调整快照用例）：`saveConfig` 在 `await patch` **之后**才用"当前 `config`"写 `savedSnapshot`，于是 PATCH 在途期间的编辑被当成"已保存基准"、`dirty` 被置 false——实际从未提交，用户看到"已保存"却什么也没存。现改为 `await` **之前**取 `submittedSnapshot`，且保存成功后 `dirty = 当前值 !== submittedSnapshot`（`suppressDirty` 窗口内被抑制的 watcher 不会补跑，故必须自己算一次）。新增回归用例，并**验证过它在旧实现下会失败**（临时改回旧行为跑一遍，确认该用例真的咬得住）。
- **删除/失效任务的编辑器不自动关**（四个面板同一口径）：正在编辑的任务在别处被删掉（手改磁盘 / 另一实例 / 刷新后已不在列表里）时，编辑器仍开着，用户每改一处都 PUT 一个不存在的 id、吃 404 与失败提示，草稿却留在页面上。新增 `frontend/src/utils/draftReconcile.ts`（纯判定 + 7 例单测），四个面板各挂一个列表对账 watcher。
  - **判据是「曾经在列表里、现在不在了」而不是「现在不在列表里」**：新建草稿在"首次落盘成功"与"下一次列表刷新"之间本来就不在列表里，用后者会把刚建好的任务立刻踢出编辑器。
  - 走 `clearXDraft()` 而不是 `closeXEditor()`——后者的语义是"退出即落盘"，对一个已被删除的任务再发一次 PUT 只会再吃一次 404。
- **`validateConfig` 覆盖到位**（原 P1 项的另一半）：此前只查端口范围与代理地址格式，而设置页各数字输入框声明的 `min`/`max` **拦不住手工输入与程序化赋值**（页面用 `<form @submit.prevent>`，没有表单提交校验这条路径）。新增 `frontend/src/utils/configRanges.ts` 作为区间**单一出处**，并把校验接入 `validateConfig`。
  - **分级**：`errors` 只放定义上不可能正确的值（NaN / 非整数，以及端口越出 1–65535——超出必然起不来）；越出"界面建议区间"只降级为警告。依据是后端这些字段是**裸 u32 直收、不做任何钳制**（`src/config/schema.rs`），前端若把建议区间当硬闸门，就会出现"昨天还能保存的配置今天保存不了"——`retry.max_retries: 0`、`worker.idle_timeout_seconds: 0` 都合法且有人用。
  - 端口那项**已经漂移过**：输入框写 `min="1024"`、校验放行 1–65535。现统一为 1–65535，低于 1024 给"需要管理员权限"的警告（而不是硬错误，否则会拦住既有配置）。
  - 新增 `configRanges.test.ts`（10 例）：7 例是分级与边界行为，另有 3 例**读设置页模板反向对账**——「界面声明的区间 == 表里的区间」「表里每一项都有对应输入框（没有死校验）」，两侧任何一处改动导致不一致都会在 CI 失败。
- **`parseCronToSchedule` 补时/分区间校验**：原判据只查"纯数字 + 每日 + 5 字段"，于是 `99 99 * * *` 被判为**有效**，编辑页显示 "99:99"、原样存成一条永远匹配不上的 cron，任务从此静默不执行。现补 `0-59` / `0-23` 区间（2 例回归测试）。
- **补齐 `useTasks` / `useScripts` 的自动保存单测**（此前这两个面板**没有测试文件**，`useScheduledTasks` / `useHttpTasks` 已有）：`useScripts.test.ts`（6 例）覆盖缺口拦落盘、补齐后落盘一次、改回原样不发请求、落盘后状态字转 saved、删除当前编辑对象关编辑器、取消确认不删；`useTasks.test.ts`（5 例）覆盖 JSON 非法不落盘且标红、修正后恢复落盘、指纹判据、删除关编辑器、取消不删。
- **核查后确认"保存防连点"已满足，未作无谓改动**：保存按钮 `:disabled="busy.save"` + `saveAbort`/`saveSeq` 的"后发接管"语义（比硬拒绝更正确——硬拒绝会把用户更新的一次编辑丢掉）；设置页各「重试」按钮在 `v-else-if` 分支里、与 loading 分支互斥，加载中根本不在 DOM 里；更新页的检查/更新/重载按钮均有 `:disabled`。逐条核实后只记录结论。

### 有意不做 / 遗留

- ~~**`autosave` 状态机仍未抽成共享控制器**（`plan-next.md` 登记项）~~：**2026-09-24 已落地**，见下方同名一节。上轮"先把 `useTasks` / `useScripts` 的单测补齐（抽取值最高的前置条件）"的判断成立——测试就位后替换确实是机械的：四个面板的自动保存测试文件一行未改即通过。
- ~~未做真机目检~~：**2026-09-24 已补上**（Playwright 巡检 12 条路由），并因此发现一处规则级证据看不到的问题（任务页 / 设置页的入场动画内外两层同时播）。见下方同名一节。

### 测试与验证

- `npx vitest run`：**398 例全绿**（上轮 367，+31：`draftReconcile` 7、`configRanges` 10、`useScripts` 6、`useTasks` 5、`useConfig` +1、`scheduledDraft` +2）。
- `npx vue-tsc --noEmit -p tsconfig.app.json` 零错误（新测试里踩到本仓已记录过的坑：`vi.fn(async () => ...)` 推断出零参签名，断言 `mock.calls[0][0]` 触发 TS2493——mock 的参数需显式声明）。
- `npm run build` 通过；对构建产物 `dist/assets/index-*.css` 逐条断言：新增规则（`.card-header h2,h3`、`.chip`、`.chip--dense`、`.chip-row`、`.note`、`.note--warn`、`.note--info`、`--tech-rust`、`.content-wrapper>*`、断点 900/1100）全部存在；已删规则（`current-profile-hint`、`settings-monospace-textarea`、`.tsk-chip`、`browser-info-tip`、`http-chip`/`wz-chip`、`ws-kicked-banner`、`.btn-lg`、`log-filter-chip`、`task-mini-grid`、`.log-name`、散落断点 980/860/1024/520）全部消失。
- 审计脚本自身的两处修正（记此备查）：① 初版把 `url("http://www.w3.org/2000/svg")` 里的 `.w3` / `.org`、`url("/logo.png")` 里的 `.png` 当成类选择器，产生 3 个假幽灵/假死类；已在抽取前剥掉 `url(...)` 与 `@import` 字符串。② 初版无法区分 `:class` 表达式里的**标识符**（三元条件、对象键）与真正的类名字面量，据此把 `isConfigDone` / `currentMode` 一类报为幽灵类——改为只有静态 `class="..."` 属性才参与幽灵类判定。
- 本轮自身的一处过删（记此备查）：`prune.mjs` 按"选择器名零引用"判定，把 `.task-overview-*` 整簇删掉了，但模板里 `TaskEnvironmentSettings.vue` **仍在用**其中 7 个类（`.task-overview-card` / `-compact` / `-left` / `-label` / `-name` / `-right` / `-actions`）——重跑审计时 ghosts 从 0 反弹到 8 才发现。已从 `git diff` 取回原规则、只恢复模板真正使用的那部分（`.task-overview-row` / `-info` / `-desc` / `.task-overview-left .task-overview-desc` 确为零引用，保持删除）。教训：死代码清理必须在**改完之后重跑审计**，而不是删完就算。

## 开发中（2026-09-24 更新助手增加卸载模式：「卸载」从此真的卸载）

### 背景

- 用户诉求：「能否优化一下更新助手，把卸载功能加进去」。
- **原状**：`POST /api/uninstall` 只清理 `base_path` **之外**的系统残留（自启动 / `~/.campus_network_auth` / Playwright 浏览器缓存），然后提示「删除程序所在文件夹即可完成卸载」——程序目录本身交给用户手动删。做不了的原因很硬：**Windows 不允许删除运行中的 exe**，而程序文件与 `config/ tasks/ logs/ environment/` 同在一个目录里，必须先退出进程才能动。
- 而"等主进程退出后操作文件"正是更新助手已有的能力（`--apply-update` 的整条骨架），故扩展成**两种模式互斥的同一个 binary**，而不是再加一个可执行文件——两者共用"等 PID 退出""路径守卫""best-effort 日志"三块逻辑，发布包少一个文件就少一处需要同步的版本管理。

### 一、助手新增 `--uninstall` 模式

- CLI：`--uninstall`（与 `--apply-update` 互斥）+ `--keep-user-data` + 两段式内部参数 `--uninstall-phase2` / `--install-dir`。`main` 拆成模式分发（`run_apply_update` / `run_uninstall`），原更新主体逻辑未改。
- **两段式（仅 Windows）**：执行删除的进程不能住在被删的目录里。第一段（在安装目录内）等主进程退出后把自己**复制**到 `%TEMP%\campus-auth-uninst-<pid>\`，spawn 第二段即退出；第二段位于安装目录之外，因此可以删掉整个安装目录（含第一段那份文件）。复制而非移动：rename 跨卷会失败，而系统临时目录与安装目录不保证同卷。unix 没有这个限制（运行中的可执行文件可 unlink），单段直删。
- **助手最后一份文件**的处理（真机演练改写过一次，见下）：运行中的 exe 无法自删，故派发 `cmd /c ping -n 3 … & del /f /q … & rmdir /q …`，等本进程退出后由系统命令删掉临时副本与它所在的目录；派发失败（或路径含 cmd 会二次解析的字符 `% & ^ | < > " !`）才退回 `MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)` 登记。
  - **`MoveFileExW` 不能当主路径**：真机演练实测，非管理员账户下它以 `ERROR_ACCESS_DENIED` 失败（该标志要求调用者属于 Administrators 组或 LocalSystem）。原先把它当主路径时，"重启后删除"对普通用户等于没做，每次卸载都会在 `%TEMP%` 留下一个几 MB 的副本——**这类问题只有真机跑得出来**，单测与静态审阅都看不见（派发"成功"、代码路径全走通）。
  - **cmd 的引号规则也踩了一次**：第一版用 `raw_arg("/c \"\"{line}\"\"")`（外层加一对引号），结果命令派发成功却什么都没删。原因是 cmd 的 `/c` 只在"命令行首字符是引号"时才做首尾引号剥离，而剥掉的恰好是内层路径的引号，把整行拆坏。修正为不加外层包裹（首字符是 `ping`，cmd 原样执行，等价于在控制台手敲）。为把这条钉住，新增 `test_spawn_delayed_delete_removes_file`——**真的起一次 cmd**并断言文件与目录消失（路径刻意带空格，覆盖 `C:\Users\John Doe\…` 这类最常见形态），另有 `test_spawn_delayed_delete_skips_unsafe_path` 覆盖特殊字符路径的放弃分支。cmd 引号规则不属于 `CommandLineToArgvW` 语义，靠推理写不对，只能靠真进程测。
  - 登记/派发前都先确认目标**确实位于系统临时目录内**，防止"有人在安装目录里手工跑 `--uninstall-phase2`"把正经安装位置当成残留删掉。
- **结果提示框**：主进程已退出，这是唯一还能传达信息的出口，故成功与失败都弹（`MessageBoxW` 零新依赖——`windows-sys` 已启用 `WindowsAndMessaging` 与 `Storage_FileSystem` 两个 feature）。文案由 `uninstall::report_text` 生成（纯函数、可单测）：全删时列出程序目录与各数据目录；保留数据时**点名保留了哪几项**（否则"卸载完成"与"目录还在"自相矛盾）；失败项**逐条列出**（每一项都意味着现场还留着东西，并提示可手动删除）。
- 卸载日志落 `%TEMP%\campus-auth-uninstall.log`：安装目录（含 `logs/`）正在被删，日志不能留在里面。`HelperLog` 因此拆出 `open_at(path)`。
- **不用 `helper.lock`**（更新模式那把互斥锁）：它落在 `<base>/update/helper.lock`，正是要被删掉的东西之一——持锁删除锁文件在 Windows 上必然失败；且卸载期间主进程已退出，不可能再有第二次卸载。互斥改由"路由先取消待应用更新"承担。

### 二、删除范围与守卫（`src/uninstall/`，**单一事实源**）

- 为什么单独成模块：卸载弹窗要**明确列出**将删除的内容（用户拍板的口径），而真正执行删除的是另一个进程。两处各写一份清单必然漂移，故清单与守卫都在此，两个 binary 共用（界面据此渲染，助手据此执行）。
- **整目录删，不枚举包内文件**：发布包除 exe 外还带 `resources/`、`docs/`、`python_worker/`，**以及 `src/` 与 `frontend/` 两份源码**。枚举清单只要漏一项，用户就会拿到"卸载完了但目录还在、里面躺着不认识的文件"，而漏项几乎必然（包结构一改就得同步改清单，没有任何机制强制）。故：全删 = `remove_dir_all(install_dir)`；保留数据 = 逐项删子项、只跳过数据目录名（未知文件照删，口径仍是"整个目录消失，只保数据"）；删空后目录若已空则一并删掉（用户选的是"保留数据"，不是"保留一个空文件夹"）。
- **守卫**（误删比残留严重得多）：文件系统根目录、用户主目录、系统临时目录、**源码仓库**四类一律拒绝；并要求目录下确实有主程序或助手。最后一条是真实踩点：仓库根放着一个早期 `campus-auth.exe`，其 base_path 就是仓库根——天真实现会把 `E:\Campus-Auth-rs\src\` 与 `frontend\` 一起删掉。守卫在**两处**执行：Web 路由（用户还在界面上，能当场看到拒绝原因）与助手（纵深防御，防 CLI 参数被绕过）。
- 删除带重试（5 次 × 400ms）：首次失败最常见的原因是"文件仍被占用"（并发唤醒的更新助手、杀软扫描）。

### 三、路由与前端

- 新增 `POST /api/uninstall/purge`（body `{ keep_user_data }`）。顺序刻意如此，每步都有具体理由：
  1. **守卫先跑** → 拒绝原因在界面上就能看到；
  2. **取消待应用更新**（新增 `UpdaterApi::cancel_pending_update`，复用 `apply::cleanup_after_apply`）——**不取消就会出真 bug**：退出时 `graceful_shutdown` 的 `ensure_helper_for_shutdown` 见到 `pending.json` 存在就会唤醒**更新**助手，把用户刚卸载的程序又"更新"回来并重启；
  3. **spawn 卸载助手**（助手等本进程退出后才动手）；
  4. **优雅关闭本进程** + 看门狗兜底（照 `restart_app` 的模式，响应在关闭信号发出前构造）。
- `GET /api/uninstall/detect` 响应从数组改为对象：`{ items, program, data, blocked }`——多了程序目录与用户数据目录的逐项清单（含路径与存在性）与守卫拒绝原因。
- 既有 `POST /api/uninstall`（清系统残留）**保留且语义不变**：它同时是"重置环境"的入口（清掉浏览器缓存与自启动后程序照常可用），前端流程把它当第一步自动接着调 `purge`。
- 前端把整条流程抽成 `composables/useUninstall.ts`（+13 例单测）：留在 `AboutView` 里时那段逻辑有真实时序（确认 → 清理 → 卸程序 → 界面失去后端），且每步失败语义不同——**残留清理失败不阻断卸载**，卸程序失败必须说清"程序文件一个都没删"。卸载弹窗改为：逐项列出将删除的内容（程序目录 / 五个用户数据目录 / 系统残留）、**「保留配置与任务」勾选**（默认不勾 = 默认真卸载；勾选后对应行的标签立刻从「将删除」变成「保留」，后果在按下按钮前看得见）、确认框**逐项点名**删除内容、执行中不可关窗、启动后翻到"程序即将退出"回执（列出将要删除的清单）。
- 一处测试逼出来的修正：`extractApiError(e, fallback)` 在异常**有 message 时原样返回 message**，fallback 只在无 message 时生效——"程序文件未被删除"放在 fallback 里等于没写，用户只会看到一句「助手缺失」而无从判断删没删。改为写进正文。
- 一处审计逼出来的修正：数据目录行右侧标签原先把类名藏在辅助函数返回值里（`dataTag(d.exists).cls`），死类审计按模板字面量判定，于是 `.tag-kept` 被报成死类。类名字面量挪回模板（`tag-kept / tag-exists / tag-missing` 三元），audit 回到 `ghosts=0 dead=36`。

### 测试与验证

- `cargo test --lib --features no-embed`：**1014 例全绿**（本轮新增的 lib 用例为 `src/uninstall/mod.rs` 15 例 + `src/web/routes/uninstall.rs` 2 例；另 `campus-auth-helper` bin 新增 2 例。逐文件差值合计 +29，含既有权衡改动带来的少量用例调整）。
  - 覆盖了真实误删风险：`.git`/`Cargo.toml` 存在即拒绝、根目录拒绝、非安装目录拒绝、保留数据时数据整棵留存而未知文件仍被删除、目录删空即删目录、数据与安装目录分离（`--base-path` 指到别处）时两边各自处理。
- `cargo test --bin campus-auth-helper --features no-embed`：**12 例全绿**（新增 2 例真起 cmd 的删除用例；纯 `cargo test` 也会跑到它们，CI 的 windows 作业上生效）。
- `cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo fmt --check` 通过。
- openapi.json 已同步 `/api/uninstall/purge`——**是测试逼出来的**：`web::tests::openapi_json_matches_route_table` 在路由表与 openapi 不一致时直接失败（该测试此前就存在，这次正好发挥作用）。
- 前端：`vue-tsc` 零错误；`vitest` **424 例全绿**（+13）；`npm run build` 通过；`docs/reports/ui-audit/audit.mjs` 复核 `ghosts=0 dead=36`（新增 6 个在用的类，删掉 1 个失效的 `.uninstall-final-hint`）。
- **真机端到端卸载演练：34 项断言全过**（`docs/reports/uninstall-e2e/rehearse.ps1`，本地不提交）。为不碰用户正在运行的实例，用独立 `--target-dir target/e2e` 编了一份 exe + helper，在仓库外的临时"安装目录"上跑全链路：
  - 三轮：全删 / 保留数据 / 守卫拒绝。逐项断言 **detect 清单形状**、**purge 后主进程退出**、**安装目录真的消失**、**保留数据时 `config/` 与 `tasks/` 整棵存活而程序文件（含一个"非程序文件"）被删**、**守卫在真机上以 HTTP 400 拒绝且一个文件都没删**、**助手卡在系统提示框上**、**关掉提示框后助手自行退出**、**`%TEMP%` 无残留**。
  - 演练脚本自身也踩过两个坑（记此备查）：API 信封是 `{data: <载荷>}` 而载荷里也有 `data` 字段，少解一层；以及"临时副本是否被清掉"**不能查一次就下结论**——删除是异步的（cmd 要等本进程退出），必须轮询；收尾也**不能**替它擦掉残留，否则会把假阳性洗成通过。
- **未跑 `cargo build` / 默认 feature 的 `cargo test` 到 `target/debug`**：那里的 `campus-auth.exe` 是用户的运行实例（那也正是本功能要解决的问题本身），全程只写 `target/e2e`；前后核对 `target/debug/campus-auth.exe` 时间戳未变（`2026/9/23 23:47:02`）。

### 有意不做 / 遗留

- **路径含 cmd 特殊字符时（`% & ^ | < > " !`）退回 `MoveFileExW`**：该路径在非管理员账户下会失败，于是那份临时副本会留在 `%TEMP%`（不可见，系统也会自行清理）。概率极低（要求用户名或路径里带这些字符），而"宁可留一个临时文件也不冒命令被拆坏的风险"是刻意的取舍——真要覆盖，得改用 `CreateProcessW` + 自建管道或写一个临时批处理，成本与风险都不成比例。
- 「保留配置与任务」只作用于用户数据目录；Playwright 浏览器缓存（可达数百 MB）与加密密钥目录仍按原有语义清理（它们是系统残留而非用户配置）。想"换目录重装且不重下浏览器"目前需手动保留缓存。
- 卸载**没有进度界面**：`purge` 返回后程序立即退出，删除由助手在后台完成，用户看到的最后画面是弹窗里的"程序即将退出"回执，之后是系统提示框。中间若某一步卡住（如杀软长时间扫描），用户只能等提示框——不做进度窗口是有意的：助手是无界面进程，加窗口等于引入一整套 GUI 生命周期。

## 开发中（2026-09-24 自动保存状态机四份拷贝 → 共享控制器；首次真机目检）

### 背景

- 上一轮（同日「前端 UI 风格全面统一」）列了两项「有意不做 / 遗留」：`autosave` 状态机抽共享控制器、真机目检。本轮把两项都做掉，并把真机目检发现的问题一并修掉。

### 一、`autosave` 状态机四份拷贝 → `createAutosaveController`

- 四个面板各自的 `autosaveTimer + autosaveSeq + pendingChanges + lastSavedFingerprint + flushPendingAutosave`（连注释都是逐字重复）收敛到 `utils/autosave.ts` 的共享控制器，深度 watcher 也一并由控制器持有——面板只剩「注入口径」三件：

  | 注入项 | 含义 | 各面板的差异 |
  |---|---|---|
  | `fingerprintOf` | 什么算**有改动**（载荷指纹，`null` = 构造不出载荷） | 浏览器任务可能返回 `null`（JSON 语法非法），其余三个恒为字符串 |
  | `blockReasonOf` | 什么算**发不出去**（闸口） | 浏览器任务是 JSON 闸口（提示要带解析器原话）；其余三个共用 `gapBlocker(gapsOf)` |
  | `persist` / `onSaved` | 往哪儿落盘 + 落盘后的界面态修正 | 定时任务多一个「首次 POST、之后 PUT」分支；`onSaved` 各自翻转 `_isNew`（浏览器任务还清 `jsonError`） |

- **控制器不持有缺口清单**：各面板的 `draftGapsNow`（状态字与缺口条的数据源）照旧自己算，控制器只在需要时问一次「发不出去的话该说什么」。上轮把「注入面不一致」列为推迟理由，核实后确实不一致，但差异只落在上表三处，不需要控制器知道任何面板细节。
- 各面板的 `autosaveState` 改由控制器提供；`useTasks` / `useHttpTasks` 各自声明的 `AutosaveState` 字面重复类型删掉，统一从 `utils/autosave` 取。
- 面板侧的 API 收敛成四件：`markBaseline(draft)`（打开 / 新建后登记"当前内容 == 磁盘那份"）、`markUnsaved()`（导入覆盖等已知与磁盘不同步）、`saveNow(draft)`（模板替换这类程序化改动，不等 debounce）、`flush("switch" | "close")`、`clear()`。`saveNow` 顺带撤掉在途定时器——旧实现里"载入模板后 500ms 还会再补一发"那次重复请求随之消失。

### 二、顺手修掉的两个真问题

- **`useScheduledTasks` 的对账 watcher 挂在 `loadScheduledTasks` 里面**：该函数每成功拉取一次就跑一遍（自动保存每次落盘后也会拉），于是每拉一次 `watch` 一次、`reconcileState` 也重建一次——watcher 与基线状态随拉取次数线性累积，同一份"任务已被删除"被反复判定。现移到模块作用域，与另外三个面板同构。
- **`clear()` 只复位状态机会漏掉一轮迟到的 watcher**（补控制器单测时暴露）：深度 watcher 是异步批处理的，`改字段 → 立刻 clear`（删除 / 放弃新建正是这条路径）时那一轮回调尚未执行；只复位状态机的话它会读到**残留的草稿**（基线已空 → 判定有改动）再排一次落盘，"删除后又被写回一次"。四个面板原来都靠"紧接着把 ref 置空"绕过，属于会踩空的隐式约定。现由控制器的 `clear()` 自己关掉草稿（watcher 回调读的是**运行当下**的 `draft.value`，置空即堵死那一轮），并把这条写进注释与回归用例。

### 三、首次真机目检（含一处规则级证据看不到的问题）

- 起用户正在用的 debug 实例（`target/debug/campus-auth.exe`，数据目录在 exe **旁边**即 `target/debug/config/`），用 Playwright（chromium 已在 `ms-playwright` 缓存中）**只读**巡检 12 条路由并逐项断言计算样式；脚本与产物在 `docs/reports/ui-audit/`（本地不提交）。
- **查出的问题：任务页 / 设置页的入场动画内外两层同时播。** `.content-wrapper > *` 与 `.settings-form > *` / `.tasks-page > :not(.tasks-narrow-nav)` 在这两个页面进入时同时命中（外壳与内部面板一起挂载），嵌套的 `animation` 位移相加、透明度相乘——同一个转场在这两个页面明显更"重"，其余页面只播一次。
  - 探针本身也踩了一次坑：查"哪些元素匹配某选择器"的写法会骗人（CSS 规则删掉后选择器照样命中 DOM），必须读**计算值**全量扫描才能看见真实附着情况。
  - 修法：**只留 `.content-wrapper > *` 一条规则**。嵌套动画只能二选一，留外层（覆盖全部页面）；内层那份换来的只是"切 Tab 时淡入"——那是上一轮新增的便利、不是既有行为，而切 Tab 本来就是瞬时换内容。收完实测 8 条路由各恰好 1 个 `pageEnter` 元素。
- 目检结果（全部通过）：12 条路由全部正常挂载（`h1` 与路由一致）；**135 条控制台消息中 0 error / 0 warning、0 个未捕获异常**；卡片标题实测 `16px/600`；关于页 8 枚技术徽标各得其色（`uv` 是刻意的中性灰——uv 本社品牌即为单色，`--tech-uv: var(--accent)`，核查后确认不是缺色缺陷）；1100 / 900 / 760 三个窄口无横向溢出。
- **新建草稿路径的实机确认**（本轮改动的核心）：点「新建任务」后状态字为「尚未创建 · 改动后自动保存」（`autosaveLabel` 的 `isNew` 分支）、「调试运行」禁用、「删除」变「放弃」；**全程不动任何字段 → 数据目录里一个文件都没多**（`untitled_1.json` 不存在），即 `markBaseline` 对"新建后没改过"的抑制在真机上成立。巡检脚本只读，不写任何数据。

### 测试与验证

- `npx vitest run`：**411 例全绿**（上轮 398，+13：`utils/autosave` 的控制器用例——debounce / 指纹 / 闸口 / detached / 慢响应后到 / 失败提示 / `saveNow` / `markUnsaved` / `clear` / `gapBlocker`）。
- **四个面板的自动保存测试文件一行未改即全部通过**（`useTasks` 5 / `useScripts` 6 / `useHttpTasks` 6 / `useScheduledTasks` 15，共 32 例）——这是「替换是机械的」这一说法唯一的证据形式。可复核：四个文件的 mtime 为 `useHttpTasks` 09-23 23:18、`useScheduledTasks` 09-24 00:42、`useTasks` 与 `useScripts` 均 09-24 01:29，**均早于本轮开工时间**（本轮净改动 8 个文件：四个 composable + `layout.css` + `ai_task.css` + `utils/autosave.ts` + `utils/autosave.test.ts`）。
- `npx vue-tsc --noEmit -p tsconfig.app.json` 零错误；`npm run build` 通过。
- Playwright 真机巡检：`docs/reports/ui-audit/visual-check.py` → `visual-check.json` + `shots/*.png`（12 路由截图、控制台计数、计算样式断言、转场单源计数）。
- 未跑 `cargo test` / `cargo build`：本轮未动 Rust；且按 AGENTS.md，`target/debug/campus-auth.exe` 正被用户实例占用，`cargo test --features no-embed` 会把它覆盖成不内嵌前端的构建。改动仅前端，debug 构建运行期读盘 `frontend/dist`，`npm run build` 即生效、无需重编。

## 开发中（2026-09-24 定时任务改为「列表页 + 二级编辑页」：四个子页终于是同一套交互）

### 背景

- 用户实机截图指出：「定时任务怎么还是弹窗，能不能和前面两个一样改成打开新页面，一级页面显示列表」。
- 任务页四个子页里，浏览器任务 / 直连任务 / 脚本早已是「整页列表 + 点行进二级编辑页 + 改动自动保存」，只有定时任务还是「表格 + 新建/编辑弹窗 + 取消/保存」——同一个页面的四套交互，用户得记住自己现在在哪一套里。列表本身上一轮已换成 `tsk-*` 共享版式，留下的就是这个弹窗与它背后的"显式保存"编辑模型。

### 实现

- **新增 `frontend/src/utils/scheduledDraft.ts`**（纯函数，342 行）：编辑模型 `ScheduledTaskDraft`（含 `_isNew` / `_originalCron` / `_originalCronInvalid` 三个界面态）、`emptyScheduledDraft` / `scheduledDraftFromServer` / `switchDraftTargetKind` / `switchDraftTrigger`，以及**缺口判定**与**落盘载荷**——`parseCronToSchedule` / `clampStartupForm` / `STARTUP_FORM_LIMITS` 一并从 `useScheduledTasks` 搬来（`clampTimeout` 新增）。
  - 校验从"保存按钮的三个 toast 分支"改成**发请求之前可判定的缺口**：debounce 到点才打一个注定 400 的请求，用户只会看到一句莫名的报错。
  - 数字区间越界计入缺口而**不静默钳制**——静默钳制会在"界面显示 1、磁盘上是 5"之间留下一处说谎（状态字那条规矩要避免的正是它）。载荷里仍保留一次钳制作最后一道闸。
- **`useScheduledTasks` 改为草稿 + 自动保存模型**（与 `useScripts` / `useHttpTasks` 同构）：`scheduledTaskDraft` / `autosaveState` / `draftGapsNow` / `isNewScheduledDraft` + `persistDraft(draft, { detached })` / `flushPendingAutosave(switch|close)` / `showScheduledTaskEditor` / `closeScheduledTaskEditor` / `clearScheduledDraft`。首次落盘 POST、之后同 id PUT；`scheduledLoaded` 记录列表是否已拉取过（供深链解析）。
  - 删掉弹窗时代的 `showScheduledTaskModal` / `editingScheduledTask` / `scheduledTaskFormLoading` / `openCreateScheduledTask` / `openEditScheduledTask` / `closeScheduledTaskModal` / `saveScheduledTask` / `onTimeChange`（时间控件的 handler 归视图）。
  - **编辑页的字段全部来自列表响应**：后端没有单任务 GET，故深链必须在列表就绪后再解析。
- **`useTaskEditorQuery` 新增可选 `ready` 门**：默认取任务目录 `loaded`（三面板不变），定时任务注入自己的 `scheduledLoaded`——未就绪时不判定"任务不存在"，否则冷启动深链会被误报为"已被删除"并抹掉参数。
- **新增 `frontend/src/views/tasks/ScheduledTasksPanel.vue`**（584 行），删除 `views/ScheduledTasksView.vue`（工作区删除，未入索引）；路由 `tasks-scheduled` 改指新面板。列表态保持上一轮的表格（名称 / 类型 / 触发 / 目标 / 超时 / 最近结果 / 启用 / 操作），点行与 ⋯ 里的「编辑」都改为进编辑页；编辑态 = 面包屑「返回定时任务」+ 页头（标题 / 状态字 / 运行 / 执行历史 / 删除或放弃）+ 缺口条 + `tsk-grid` 两列（主列三张卡：基本信息 / 任务配置 / 执行设置；侧栏：执行与状态 / 快速上手）。
  - 侧栏给出**「下次执行」**（每日表达式本地按当前时刻推算今天还是明天）与**今日成功 x/N**（启动触发，取列表回填字段）；启停开关与列表那一列改的是同一个字段，走自动保存。
- **删除弹窗专属样式**：`.sch-form`（卡片摞叠）、`.sch-form .card-body > .form-row:last-child`（弹窗高度收边）、`.modal-container:has(.sch-form)`（92vh 放宽）——三条都是给弹窗高度打的补丁，页面上没有这个约束。
- 顺带：新面板不再自带 `.page-content`（它是四个子页里唯一一个嵌套了该类的，TasksView 已提供），并新增真实存在的 `.sch-mono`——模板里此前写的 `mono` 全仓**没有任何规则**（搜不到 `.mono {}`），cron 表达式其实一直没等宽。

### 顺带修掉的真缺陷

- **非每日 cron 表达式会被静默改成每日**：`parseCronToSchedule` 原先只检查"分/时字段是否纯数字"，于是 `0 8 * * MON`（只在周一跑）、`0 8 1 * *`（每月 1 号）、`0 0 8 * * *`（秒级 6 字段）都被判为"表单能表达"，编辑页不提示、一保存就把调度语义改成天天跑。现按三条判据收紧：分/时必须纯数字、**日期/月/星期三段必须是 `*`**、字段数必须是 5。这不只是文案问题——它决定"保存"到底是一次无损写回还是一次无声的调度改写。
- **列表里的「目标已不存在」现在同时是保存闸口**：此前只有保存时后端会拦死引用（列表侧标红），前端仍会把死引用写回去一次再被拒。

### 测试与验证

- `frontend/src/utils/scheduledDraft.test.ts`（30 例）：解析三判据（步进 / 区间 / 列表 / 星期限制 / 月内某天 / 4 字段 / 6 字段 / 垃圾输入）、钳制、空草稿与列表构造（含非每日标记）、切换类型清空目标、列表「触发」列文案（每日表达式 → 每天 HH:MM，表达不了的原样显示）、缺口逐项（含"未就绪时不判定目标不存在"）、载荷（cron vs 启动、名称 trim、描述保留、越界钳制、不含界面态字段）。
- `frontend/src/composables/useScheduledTasks.test.ts`（15 例，重写）：缺口拦落盘 → 补齐后 POST → 同 id 转 PUT；刚载入不动不发请求；改回原样不发请求；目标不存在按缺口拦下；换编辑对象补发在途改动且不污染新草稿；关编辑器补发（退出即生效）与缺口态下出声；打开不存在的任务给提示；删除当前编辑对象会关编辑器、新建草稿上的删除是「放弃」；取消确认不删；「已排入执行」的提示语义；运行连点只发一次；加载成功标记就绪 / 失败不标记并就绪提示一次（notifier 每轮只响一次，故 `beforeEach` 复位 `scheduledLoaded` 与会话态）。
- 中途踩到的两个坑（记此备查）：`vi.fn(async () => [])` 的返回类型被推断为 `never[]`，`mockResolvedValue([task])` 因此报"不能赋给 never"——mock 的**参数与返回类型要显式写出来**；以及 JSDoc 注释里写了 `*/5 * * * *`，其中的 `*/` 提前关闭了块注释，报出来的却是几十条"无效字符"（真凶在 200 行之前）。
- `npm run typecheck` 零错误、`npm run test` **367 例全绿**（上轮 329，+38）、`npm run build` 通过（产物 `ScheduledTasksPanel-*.js` 19.7 kB）。
- 顺带把列表「触发」列从 `每天 ${task.cron}`（后端存 5 字段，铺出来是"每天 0 8 * * *"）改成 `scheduledTriggerLabel`：每日表达式渲染成「每天 08:00」，**表达不了的表达式原样显示**（那正是用户要去改它的理由）。
- **真机目检 30/30 全过**（新增 `docs/reports/ui-check/ca-sched-panel-check.py`，本地不提交）：打到本机正在运行的 dev 实例上，`/api/scheduler/**` 与 `/api/tasks` 打桩、写请求被拦下并断言载荷（不碰用户真实定时任务）。覆盖：列表 3 行且**页面上不再有弹窗**（`.modal-overlay` 不可见、旧弹窗表单类 `.sch-form` 零命中）、触发列渲染成「每天 08:00 / 每天 22:30 / 启动后执行」、目标已不存在有副行、点「新建定时任务」进的是二级编辑页（面包屑「返回定时任务」、状态字按缺口改口、缺口条点名「任务名称、目标任务」）、补齐后落盘（POST 载荷含程序生成的 id 与非每日表达式原样不动的 `cron: "0 8 * * *"`）、落盘后地址栏带上 `?task=`、深链 `?task=s1` 直达且侧栏给出「触发 每天 08:00 / 下次执行 今天 08:00 / 任务 ID」、切触发方式落盘（PUT 载荷 `cron: ""` + 三个启动参数）与侧栏改口「今日成功 x/N」、面包屑返回后 `?task=` 清掉。截图四张（列表 / 新建草稿 / 已有任务 / 启动触发）逐张过目。

### 有意不做 / 遗留

- **执行历史仍是弹窗**：它是只读视图（不是编辑），入口在列表 ⋯ 与编辑页头部，两处都保留；改成第三层页面会让"返回"变成两级。
- **自动保存状态机现在是第四份拷贝**（`useTasks` / `useHttpTasks` / `useScripts` / `useScheduledTasks`）：本轮按四个面板同构照搬了一份，抽成 `utils/autosave` 共享控制器的收益因此更大——已登记 `docs/plan-next.md`，建议下一轮单独做（它要动三个正在工作的面板，不宜与本轮混在一起）。
- **列表不加搜索框**：定时任务通常个位数，搜索框在没有可搜之物时只是噪音。

## 开发中（2026-09-24 任务仓库索引按类别拆分：浏览器任务与直连任务各一份）

### 背景

- 用户诉求：「任务仓库导入里两类方案共享一个 json，容易误解，能否使用两份 `index.json` 隔离」。
- 混合索引的真实代价：条目靠**可选的** `type` 字段分类、缺省当 `browser`——贡献者忘写 `type` 就得到「列在浏览器列表里、点导入又被文件级类型校验拒掉」的死条目；用户也会在错的 Tab 里搜不到自己学校；UI 为此长期背一句解释性文案（「当前只显示直连任务条目（浏览器任务请到「浏览器任务」Tab 导入）」）。两类任务的**任务文件结构本就不同**（浏览器步骤 vs HTTP 请求形状），共用一个 catalog 只是历史原因。
- 用户确认的取舍（二选一）：`index.json` 继续等于浏览器任务（老客户端只读它，行为不变），新增 `index.http.json`；Gitee 直连索引一并补齐。

### 实现：任务仓库（`Misyra/campus-auth-tasks`，GitHub + Gitee 双端推送）

- **索引按类别拆分**（`d3eca57`）：`index.json` / `index.gitee.json` 恢复为纯浏览器索引（9 条，老条目一条未改、不带 `type`）；新增 `index.http.json` / `index.http.gitee.json`（直连任务，条目保留 `type: "http"` 与 `source`）。对已发布的老客户端**行为不变**（它只读 `index.json`），且顺带修掉一个已发布的真实缺陷：v5.0.2 及更早的导入弹窗不按类型分流，`haust` 混在 `index.json` 里会被当成浏览器任务列出、导进去是一份步骤为空的空任务。
- **顺带补上镜像落后**：`gitee/master` 此前停在 `8e9f9ae`（`44b04c1` 的 haust 提交没推过去），Gitee 上 `tasks/haust.json` 实为 404、`index.gitee.json` 里也没有该条目——国内镜像用户看不到这条直连任务。本次推送一并补齐。
- **文档**：README 新增「索引文件」表（4 个地址）、任务列表按类别分两张、写明条目只能进对应索引、`screenshot` 只属浏览器条目；`submit-task.md` 的 Step 6 / 6a / 6b / 7 与验证清单改为按类别；`doc/task-writing-guide.md` 的截图一节标明只适用浏览器任务。
- **修正两处死链**（`18d6ecf`）：README 的「任务录制器」与「任务编写指南」原指向旧 Python 仓库 `Misyra/Campus-Auth` 的 `tools/` 与 `doc/`，而该仓库这两个目录都不存在（GitHub contents API 实测 404）。录制器改指 `Campus-Auth-rs` 的 `resources/tools/task-recorder.user.js`，指南改指本仓库的 `doc/task-writing-guide.md`。

### 实现：前端

- **索引地址改为「类别 × 源」两维**（`utils/constants.ts`）：删掉 `TASK_REPO_INDEX_URL` / `TASK_REPO_INDEX_URL_GITEE` 两个扁平常量（其语义就是"唯一一份索引"，正是要根治的东西），源表每项改为 `indexUrls: Record<TaskRepoKind, string> | null`，新增 `presetRepoIndexUrl(kind, source)`（自定义源/未知源回空串）。四个地址经 `githubRaw()` / `giteeRaw()` 由 owner/name 拼出，不再各写一份字面量；`TaskRepoSourceId` 现由 `TaskRepoMirrorId` 派生，`TaskRepoKind` 上移到 constants 并由 `useRepoImport` 以 `RepoKind` 别名导出。
- **切类别与切源都重取地址**（`useRepoImport.ts`）：`showRepoImport(kind)` 与 `selectRepoSource(source)` 统一走 `applyPresetIndexUrl()`。**这是本轮最容易出错的地方**——原实现里 `selectRepoSource` 无条件写 `preset.indexUrl`，不改的话在直连 Tab 上切一下来源，列表拉到的就是浏览器索引。
- **索引状态不再三义**：新增 `loaded` 标记，区分「还没点加载」「加载成功但这一类暂无条目」「拉取失败 / 格式错」。空数组过去与"格式不正确"共用一句提示（`索引为空或格式不正确`），而拆分后"这一类暂时没有条目"是合法状态（新仓库、镜像源尚未收录）；失败提示带上类别名（「获取直连任务索引失败: …」），因为两类索引地址不同、说了类别才知道该看哪个来源。
- **异类条目从静默过滤改为计数提示**：新增 `foreignRepoTaskCount` + 弹窗一行警告（`--warning-text`）。索引文件只承载一类条目，出现不符即文件写错了——静默吞掉会让用户对着短列表猜"我的学校去哪了"。
- **删掉混合索引的解释性文案**：`kindEmptyHint`（「当前只显示直连任务条目…」）随拆分一起删除；弹窗标题、空态与警告文案统一由 `repoKindLabel(kind)` 派生，不再各写一份措辞。

### 测试与验证

- `npm run typecheck` 零错误。中途踩到一个类型收窄陷阱：`const DEFAULT_KIND: RepoKind = "browser"` 会让 ref 里该字段的类型窄成字面量 `"browser"`，于是多处 `kind === "http"` 被判为"无重叠比较"——改为在属性处 `as RepoKind` / `as TaskRepoSourceId` 收口（与原实现同口径）。
- `npm run test` **329 例全绿**（上轮 320 例，+9）：`taskRepo.test.ts` 改为按 (类别, 源) 断言四个索引地址、两类文件不得同名、`presetRepoIndexUrl(…, "custom")` 回空串，并新增源码断言锁住弹窗不再出现"混合索引"文案；`useRepoImport.test.ts` 新增「索引地址：类别 × 源」describe（切类别换文件、类别内切源、自定义源跨类别保留）与「索引状态与异类条目」describe（空索引合法且不弹失败提示、非数组算格式错、失败提示带类别名、打开弹窗复位 `loaded`、异类计数），`toastOnly` 借 `vi.hoisted` 暴露以便断言消息内容；`beforeEach` 补 source / repoKind / url / loaded 复位（这些用例会改它们，不复位会互相串味）。
- `npm run build` 通过。
- **线上契约实测**：四个 raw 地址逐一 GET 校验——`index.json` 9 条、`index.gitee.json` 9 条、`index.http.json` 1 条、`index.http.gitee.json` 1 条，确认前端拼出的地址真能取到拆分后的文件（Gitee 侧同时验证了镜像已跟上）。
- 未做真实浏览器点击验证：本次改动是地址派生与状态文案，既有浏览器级回归（`docs/reports/ia-verify/verify_repo_import_ui.py`）覆盖的是选择器 / 分段控件 / hint 渲染，本次未动其结构。

## 开发中（2026-09-23 任务页深度复核：三面板一致性、一揽子真实缺陷与死代码）

### 背景

- 用户要求「还有没有什么需要优化的，你检查一下」。做法：对这一轮未提交的方案 G 改动做四路并行复核（三面板行为矩阵 / CSS 死代码与令牌 / Rust 后端未提交 diff / 文档与实现一致性），再把确认的问题逐条修掉。
- 确认的**会丢数据、会报错**的问题集中在两类：换编辑对象时在途的自动保存被丢弃；前端校验规则比后端严，导致"后端收得下、前端存不上"。

### 修复：前端真实缺陷

- **换编辑对象时在途改动静默丢失**（三面板统一）：`useTasks` / `useHttpTasks` 的 watcher 在 `draft.id` 变化时只 `clearTimeout` 后 return，而「停手半秒落盘」的承诺在「换对象」这一瞬间必须显式兑现——在 A 任务里打字后 500ms 内点另一条（或走仓库导入 / 浏览器前进后退切 `?task=`），那半秒的编辑既没落盘也没提示地消失；`closeTaskEditor` 的冲刷条件还被嵌在 `if (autosaveTimer)` 里，而 timer 已被上一次切换清成 null。现按 `useScripts` 的口径给三者都加 `flushPendingAutosave(reason)`（`useScripts` 原来的 `flushPendingAutosave(notify)` 一并改成同一签名），并在 `showTaskEditor` / `createTask` / `showHttpTaskEditor` / `createHttpTask` / `showScriptEditor` / `importScript` 入口调用。
  - **`switch` 与 `close` 语义分开**：关闭编辑器正常落盘、被拦时出声；换编辑对象走 **detached** 落盘——那一发的响应回来时新草稿已经在编辑中，共享状态（`lastSavedFingerprint` / `autosaveState` / `_isNew`）不该再被旧草稿改写（指纹被旧草稿覆盖会让刚新建、用户还没碰过的新草稿被误判成"有改动"，凭空写一次）。`persistDraft(draft, { detached })` 保留列表刷新与失败提示。
  - 判据不再只看 `pendingChanges`：深度 watcher 是异步批处理的，"改字段"与"换对象"落在同一 tick 时它还来不及置位；改为 `!pendingChanges && fingerprint(draft) === lastSavedFingerprint` 才跳过——载荷指纹是**当下**的事实。
- **仓库导入撞 id 用了未清洗的原始条目名**（`useRepoImport.ts`）：直连分支归一化后的 `id` 只含 `[A-Za-z0-9_-]`，撞车时却拼 `${rawId}_${n}`——条目名含中文（很常见）时第二次导入同一条目必然被后端 `is_valid_task_id` 拒掉，且报的是"任务不存在"。改为拼清洗后的 `id`（与浏览器分支同口径）。
- **浏览器面板状态字在 JSON 为空 / 语法错误时说谎**：`autosaveLabel(..., [])` 恒传空缺口，而这两种情况 `persistDraft` 一个字节都不写，状态字却仍是「改动自动保存」。新增 `jsonGate` 计算（空 → 「JSON 配置」、语法错 → 「JSON 语法」）喂给状态字，与直连 / 脚本的缺口优先口径一致。
- **脚本 ID 规则前后端不同口径**：`SCRIPT_ID_PATTERN` 要求"字母开头 + 不收连字符"，而后端 `is_valid_task_id` 是 `^[A-Za-z0-9_-]{1,64}$`。id 为 `my-script` / `2fa` 的脚本（`POST /api/tasks`、导入、或历史上手工放进 `tasks/scripts/`）打开后缺口恒非空 → 自动保存永远被跳过，而它点名的字段在面板上是**禁用**的（落盘后 ID 固定）→ 改不动也存不下，且没有任何报错。规则对齐后端；`scriptDraft.test.ts` 的用例改为钉住"数字开头 / 连字符必须放行"这条回归护栏；`scriptDraft.ts` 的模块注释顺带修正脚本落盘文件名（是 `<id>.json`，`<id>.<ext>` 只出现在导出文件名上）。
- **脚本「立即运行」永远弹成功**：`scriptsApi.run` 被声明成 `MutationResult`，而 `runScript` 读 `data?.message`（后端返回的是 `TaskResult`，没有 message 字段）→ 无论脚本成败都弹绿色的"执行完成"。改为按 `TaskExecuteResult.success` 分流、失败带上退出码与输出末行；接口类型同步改成 `TaskExecuteResult`。
- **脚本输出无处可看**（连带修）：脚本 stdout/stderr 只在这一次响应里（进程输出**不进日志页**），面板不留下来用户就没有任何地方能看到"脚本为什么失败"。侧栏「执行与调试」新增最近一次「立即运行」的结果块（复用 dashboard 的 `.history-item / .history-status / .history-info` 词汇，新增 `.history-output` 承载输出正文），并带 id 判断——换脚本后不把上一次的结果显示在新脚本名下。同时修正面板里两处"stdout 与 stderr 都进日志页"的错误文案。
- **定时任务手动「运行」的 toast 语义**（known-issues E2）：后端 `spawn_manual_run` 后立刻回 `data("ok")`，前端读 `data?.message` 恒为 undefined → 永远弹"执行成功"。改为「已触发执行，结果见「执行历史」」；`scheduledTasksApi.run` 的类型去掉后端已不再返回的 `run_id`。
- **浏览器面板四处「调试」入口没有 busy 守卫**：同一次会话连点两下会发两次 `debugApi.start`（`useDebug` 自己不做重入保护），而脚本 / 直连的运行类按钮都在途禁用。统一加 `:disabled="debug.loading.value"`（列表行图标 / 行尾菜单 / 编辑页 JSON 卡 / 侧栏）、图标在途切 `refresh` + `spin`。
- **「加载默认模板」一键覆盖且立即写盘**（浏览器面板）：脚本面板同性质动作会弹一次红色确认，浏览器面板直接替换手写好的 JSON 并落盘、没有撤销。加同款确认（当前内容非空且与模板不同才弹）。
- **文件导入的类型口径**：浏览器面板把整包丢给后端，混进来的直连 / 脚本条目会被后端照单收下并落到别的列表里，而本列表按 `task_type` 过滤 → 列表毫无变化、toast 却说"已导入 N 个任务"。改为与直连面板同构的 `isBrowserImportEntry` 过滤 + 「忽略 N 个非浏览器条目」（`type` 缺省视为浏览器任务，与导出端点 / 磁盘文件的历史形态一致）。
- **脚本面板**：导入之后补 `?task=`（覆盖既有脚本时 `_isNew` 为 false，面板那个"落盘后补 query"的 watcher 不触发，改到一半刷新会掉回列表态）——`importScript` 现返回打开的 id；「导出」补 busy 守卫（连点会落两份同名文件）；行尾菜单文案改「导出脚本文件」；空态按钮「导入文件」→「导入」（与工具栏及其他面板一致）。
- **菜单监听泄漏**：浏览器 / 脚本面板在菜单打开时切页会永久留下 `document` 的 `pointerdown` / `keydown`（Esc 会调到已卸载组件的作用域）。补 `onBeforeUnmount`（直连面板本来就有）。
- **删除浏览器任务的确认文案**：浏览器任务被删后，绑它的方案会**静默**回退到内置 `default`（`src/login/mod.rs` 的 `resolve_active_task` 只记一条 warn），而直连任务的确认文案会警告"绑定它的方案将无法再用直连方式登录"。补一句"若有方案绑定它，那些方案会回退到内置的 default 任务"。
- **工具栏里的「分享适配」不像按钮**（用户实机指出）：它是外链 `<a class="btn btn-sm btn-ghost">`，而 `btn-ghost` 同时抹掉底色**与**边框（`border: 1px solid transparent`）——实测盒模型与相邻的「导入 / 仓库导入」完全一致（同为 36px 高、同 padding、same top），但那个 108px 的盒子看不见，于是变成"夹在两个按钮中间的裸文字"，看着不像能点。两个面板（浏览器 / 直连）都改成普通次要按钮（`btn btn-sm`），去向仍由 `title` 说明。
- **定时任务表格在窄视口下画到卡片外，且名称列被压成 0px**（本轮实测确认，比复核时估的更严重）：8 列全是 px 固定宽（和 742px），而 `table-layout: fixed` 下表格宽 = `max(100%, Σ列宽)`——1100px 视口起表格就比卡片宽，900px 下 742px 的表格画在 596px 的卡片外；**更要紧的是名称列（`auto`）被压到 0px**（1200px 下只剩 80px、1050px 下 2px），最该看的列先消失。改法（按实测定，见 `ca-sched-geo.py`）：文字列一律百分比、只把装不下就失去意义的列钉 px（类型 92 / 启用 72 / 操作 132），并逐级让列——≤1100 藏「超时」，≤980 藏「类型 + 目标」且「触发 / 最近结果」钉回下限（110 / 96px），≤860 藏「最近结果」（信息在行尾 ⋯ 的「查看历史」里），操作列在 ≤980 收到 96px（本页只有两个按钮）。实测各视口表格 ≤ 卡片、名称列 92→300px、页面级零横向滚动。

### 修复：后端

- **mtime → RFC3339 在超范围时间戳上 panic**（`tasks/loader.rs`）：`chrono` 的 `From<SystemTime>` 内部是 `timestamp_opt(..).unwrap()`，超出可表示范围（约 ±26 万年）直接 panic。mtime 是文件系统给的任意 i64（Linux / Docker 下 `touch -d @1e13`、tar 里解出的畸形 mtime 都能造出来），而列表扫描跑在 `spawn_blocking` 里 → panic 被 `JoinError` 吞成「任务目录扫描失败（返回空列表）」→ **三桶任务全部从列表消失**（文件还在），且只要那个文件在就永久复现。改用 `DateTime::from_timestamp`（越界返回 `None` → 留空 → 前端显示「—」，与"读不到 mtime"同路，纪元前的负值也照收），补单测钉住不 panic。
- **`order_tasks` 跨两次加锁的读改写**：原实现 `load_order` → 清空 → 拼接 → `save_order`，而 `load_order` 根本不持锁。与自动保存的 `PUT /api/tasks/{id}`（`save_task` 会把新 id 追进排序表）并发时丢更新——用户看到「拖完排序，另一类任务顺序莫名回退」。载荷本来就是全量的（三组必须全传），改为直接用请求体构造 `OrderData` 整体替换。
- **退出登录请求的执行顺序**（直连任务新功能）：原来排在凭据变换脚本**之后**——而脚本的输入之一正是"抓到的登录页原文"，旧会话仍在线时这类门户会把登录页重定向到「已在线」页，脚本据此产出的字段全是错的（本功能要治的正是这类门户）。移到整个流程最前（先于抓登录页与脚本），`models.rs` 与执行侧注释同步说明"因此下线请求只支持内置占位符"；补一条"下线必须排在登录页抓取之前"的测试，并把流程注释重新编号（1 下线 → 2 脚本 → 3 前置请求 → 4 模板渲染 → 5 发送 → 6 成败判定）。
- **`InvalidTaskId` 映射 400**：它表示"id 形态不合法"（用户可改），与 `TaskNotFound`（资源不存在）挤在 404 里会把排查方向带偏（自动保存打来一个畸形 id 时尤其明显）。
- **`logout_request` 保存闸口补测试**：前端每次保存都会带上这个字段（未配置时为 `null`），形状 + 体积双闸此前只有 validate / 执行层的测试，loader 层（字段名取错、体积常量串位、`null` 未放过）零覆盖。
- `models.rs` 的 `pre_request` 文档示例 `"method": "get"` → `"GET"`（`HttpRequestMethod` 是 `rename_all = "UPPERCASE"`，照抄示例必然反序列化失败、任务在列表里静默消失）。

### 清理：死代码与令牌

- 删除本轮重写后确已无引用的规则（逐条以全仓 `*.vue` / `*.ts` 词边界匹配确认为零引用）：`responsive.css` 的 980px 块（`.tasks-grid` / `.task-item` / `.task-info` / `.task-actions`，旧卡片列表退役）、`.settings-panel-grid--task`、`.settings-detect-columns`、`.wizard-steps` / `.step-label` / `.step-line`；`form.css` 的 `.input-with-action`（含 480px 媒体块）、`.radio-group` / `.radio-option` / `.radio-text` / `.radio-label` / `.radio-desc`、`.hint--mt`、`.form-group--min140`；`badge.css` 的 `.scheduled-task-type` / `.binary-badge` / `.binary-default`；`misc.css` 的 `.hover-lift`；`card.css` 的 `.card-header-sub` / `.glass-card`（含降级分支）；`drag.ts` 的 `onDragEnd` 里对 `.drop-before / .drop-after` 的清理（那两条类已不存在，成了空操作）。
- `.icon-xs` 双定义收敛：`misc.css`（11px）与 `tasks.css`（14px）同名不同值，按 `index.css` 导入顺序 14px 生效、11px 那份从未生效。删掉两份规则，唯一消费点（ProfilesView 匹配信息行的 4 个图标）改用已有的 `.icon-sm`（14px）——行为不变。
- `modal.css` 浅色主题的 `--bg-glass` 覆盖改走 `rgba(var(--slate-rgb), α)`（原为裸三元组，数值正是浅色主题的 `--slate-rgb`）；`form.css` 的 field-help 问号字号 `9px` → `var(--text-2xs)`，并把过期的"14px 里塞 8px"注释改成现状。
- `.skip-link` 此前是死代码（有样式、无实例）：在 `App.vue` 补上真实的跳到主内容链接（`<a class="skip-link" href="#main">`）并给 `<main>` 加 `id="main"` + `tabindex="-1"`，键盘用户不必逐个 Tab 穿过整条侧栏。
- 表格版式两处：悬停底色从 `tr` 移到 `td`（底角半径写在 `td` 上，只有单元格自己的背景会被它裁圆——画在行盒上时卡片底部圆角外会露出方角），半径改用 `--radius-xl` 与卡片对齐；空态那一行不再有手型光标与悬停变色（它不是数据行、没有 `@click`，整块 200px 高区域"看起来能点"却毫无反应）。

### 文档同步

- 三份指南补「退出登录请求」这一组参数（此前整条新功能只存在于 changelog）：`http-login-guide.md` 的「五组」→「六组」+ 新增 3.6 节、`user-guide.md` 的字段清单、`updatelog.md` 新增用户可见条目；同时修正"一次登录只发一次请求"的边界承诺（现在最多三次：下线 → 前置 → 登录，同一连接池顺序发出）。
- 修正三处与代码冲突的文档：脚本落盘名（`tasks/scripts/<id>.json`，正文在 `content` 字段里）、脚本 stdout/stderr **不进日志页**（`custom-script-guide.md` 同文自相矛盾，一处已按对的写法给出）、文件导入与仓库导入的区别（只有仓库导入会当场打开编辑页；三类的文件导入各按自己的类型/扩展名收，浏览器任务那条本轮补上了过滤）、侧栏子页名与标签一致（「AI 生成」而非全称）、任务页状态字补齐 6 种。
- `plan-next.md` 删掉已完成却仍列在"有意不做"里的「直连任务参与拖拽排序」，并补上 `logout_request` 字段；`known-issues.md` 更新动态 `import()` 的唯一来源、E2 / E3 的措辞（页面已改表格）、登记"落盘后 ID 固定的历史裸 `.py` 脚本若 ID 含点号则无法保存"这一遗留死角。

### 验证

- `cargo fmt` + `cargo clippy --all-targets -- -D warnings` 零告警；`cargo test` 全绿（lib 999 例），新增 3 例：`test_secs_to_rfc3339_out_of_range_is_empty_and_does_not_panic`（判据放在纯函数上——Windows 的 `SystemTime` 上限本身就在 chrono 范围内，造不出越界值）、`test_validate_http_logout_request_gate`、`logout_request_precedes_login_page_fetch`。
- `npm run typecheck` 零错误、`npm run test` **320 例**全绿（`useHttpTasks.test.ts` 新增 1 例：在途改动 + 立刻切对象仍落盘、且不污染新草稿指纹；`scriptDraft.test.ts` 的 ID 用例改为后端同口径）、`npm run build` 通过。
- 实机回归 112 → **144 项断言全过**（`docs/reports/ui-check/ca-taskpage-check.py`，`/api/**` 全 mock、不碰用户任务文件）。本轮新增 32 项：换编辑对象时**把 500ms debounce 掐掉后**改动仍落盘（证明确实来自切换那一下的补发）、JSON 清空 / 语法错时状态字改口、调试启动中按钮置灰且图标转圈、加载默认模板先弹确认且取消后内容不变、脚本行尾菜单文案、运行失败如实提示 + 输出就地显示、空态行不显示手型光标且悬停不变色、悬停底色画在单元格上、跳到主内容链接是首个可聚焦元素且目标存在、导入只提交本类条目并报忽略条数、删除确认说明方案回退、定时任务表格在 8 个视口都不宽于卡片且名称列 ≥60px、两面板的「分享适配」与相邻按钮同底色同边框同高度。
- 边角量测脚本 `ca-sched-geo.py` / `ca-wrap-scroll.py`（本地，不提交）把"表格 vs 卡片 vs 内容区"的宽度差与内容区横向滚动的真凶分开量测：前者是本次修的列宽问题，后者是**隐藏的说明气泡 `::after`** 参与可滚动溢出（与本次改动无关，且浏览器任务面板在 820px 下同样有），已登记 `known-issues.md`。

## 开发中（2026-09-23 新建任务不再「点一下就落盘」：三面板统一为首次改动才创建）


### 背景

- 用户反馈「新建任务什么都没有改的话不自动保存」。实测确认了那条噪音路径：方案 G 下浏览器任务与直连任务的**新建**都是**立即落盘一份种子**（`tasksApi.save("untitled_N", seed)` → 再打开编辑器），于是"点开看一眼又退出"会在磁盘上留下一个没人改过的 `untitled_N.json`，列表里也多出一条「未命名任务」。
- 脚本面板早就是"补上 ID 后第一次自动保存才创建文件"，只有这两条不同口径——同一个页面的三个子页，新建的副作用不一致。
- 连带要一起处理的：种子不再落盘后，新建态的三个入口（删除 / 导出 / 调试运行）打过去都会因为"磁盘上没有这个 id"而 404。

### 实现

- **新建改成内存草稿（`useTasks` / `useHttpTasks`）**：`createTask` / `createHttpTask` 不再 PUT，只在内存里起一份草稿——id 仍是本地生成的 `untitled_N`（撞目录已有 id 递增）、`_isNew: true`，浏览器任务种子仍是"一步 `sleep` 占位 + `{{LOGIN_URL}}`"（后端 `validate_task` 拒绝空 steps / 空地址，种子必须能通过保存校验），直连种子仍带 `{gateway_host}` 占位地址。首次**真实改动**触发自动保存时才创建文件。
- **自动保存加"载荷指纹"判据**（与 `useScripts` 同口径）：新增 `lastSavedFingerprint`（磁盘上那份的载荷 JSON 串），新建草稿把种子的指纹当作"磁盘现状"。改动与它一致就不发请求——于是**新建后没动过零请求**，顺带解决"内容改回原样还补发一次 PUT"。首次落盘成功后把 `_isNew` 置 false（草稿从此是"已存在"，删除恢复删除语义）。
- **新建态的配套改道**：
  - 「删除」→「**放弃**」：不请求后端，直接丢草稿（`deleteTask` / `deleteHttpTask` / `deleteScript` 各自判定 `_isNew`，确认文案也换成"还没保存过，放弃后内容会丢掉"）。
  - 「导出」→ 走内存里的草稿（后端没有这个 id；浏览器任务在 JSON 语法错误时提示先修正）。
  - 「调试运行 / 调试」→ 禁用，`title` 说明"新建任务还没有落盘，改动后才会创建它"。
  - 状态字 → 「**尚未创建 · 改动后自动保存**」（`utils/autosave.autosaveLabel` 新增 `isNew` 参数，只在空闲态生效：保存中 / 已保存之后它已经存在了，不再提"尚未创建"；缺口优先于它，缺 ID 的新脚本仍先说要补什么）。
- **地址栏**：新建态**不写** `?task=`（磁盘上没有这个任务，写进去刷新就是「找不到任务」，分享出去的链接也是死的）；首次落盘后由面板 watcher 补上 `?task=<id>`，与"点行进入"完全一致。三个面板都补了这一步（脚本面板此前也缺）。
- **脚本面板补齐新建态的出口**：此前新建草稿的「运行 / 导出 / 删除」三个按钮都被 `canRun` 隐藏，只能靠返回——而返回会把合法内容真的存下来（"退出即生效"），想放弃却没有入口。现在新建态显示「导出（当前草稿）/ 放弃」。

### 验证

- `npm run typecheck` 零错误、`npm run test` **319 例**全绿（`autosave.test.ts` 新增 2 例：新建态改口、缺口优先于新建态）、`npm run build` 通过。
- 实机回归 91 → **112 项断言全过**（`/api/**` 全 mock，不碰用户任务文件），新增 21 项盯住新建路径：浏览器任务新建后**零写请求**、地址栏不带 `?task=`、状态字「尚未创建」、调试运行禁用、按钮改口「放弃」；未改动直接返回仍是零请求且不留文件；改动名称后 PUT `/api/tasks/untitled_1` 且载荷带上改后的名称与 `task_id`；落盘后地址栏补上 `?task=untitled_1`、状态字变「已保存 · 刚刚」、调试运行恢复可用、按钮改回「删除」；点「放弃」不发 DELETE 且回到列表；直连任务同口径（含 `type: "http"` 载荷）。
- 截图 `14-new-task-draft.png`（新建未落盘）/ `15-new-task-saved.png`（改动后落盘）（本地，不提交）。

### 有意不做

- **「复制为新任务」与「仓库导入」仍是立即落盘**：这两个动作的用户意图就是"得到一份有内容的任务"（副本拷贝自既有任务、导入来自仓库条目），不是"起个空壳再慢慢填"。真正的噪音只在"新建一个空种子"这条路径上。

## 开发中（2026-09-23 文档事故与还原：changelog 头部被误截断）

### 事故

- 本轮记录 changelog 时用 PowerShell 拼接数组写回文件，写成 `@($lines[0..5], $entry, $lines[6..])` —— `@()` 里嵌了一个数组字面量，`-join` 对它调用 `ToString()` 得到 `System.Object[]`，**整个 docs/changelog.md 被覆盖成 3 行 51 字节**（原文件 3256 行 / 524 KB）。
- 受损的是**工作区未提交**的部分：`docs/changelog.md` 最后一次提交是当天 15:38（`a7635d3`），头部在途条目（本日与 09-20 各轮，相对 HEAD 合计 23 个 `##` 章节）都不在 HEAD 里。`docs/updatelog.md`、`docs/plan-next.md`、代码与配置**均未受影响**。

### 还原

- 基线取 HEAD（`git cat-file blob`，475 KB / 2976 行）。
- 在途条目从会话转录（`~/.dsh/sessions/--E-Campus-Auth-rs--/session.v4.jsonl.zstd`）里还原：每次对 changelog 的写操作都留在转录里，且**插入类写入的 `old_string` 就是插入点下方的锚点标题**。以「新条目标题 → 锚点标题」建链并向下走，直到锚点落在 HEAD 内容里为止，即可得到完整、有序的在途条目集合（14 条）与其原文。
- 结果：`## 开发中（2026-09-23 …）` 到 `## v5.0.2` 之间的 14 条全部回填，`## v5.0.2` 及以下与 HEAD **逐行一致**（脚本比对通过）；其中「直连任务纳入拖拽排序」的 `### 生效条件` 段取的是后续修订版（「（已完成）」）。
- 残留风险（已核对，影响可忽略）：条目**内部的**局部修订（非整条插入的 edit）若未在链上体现，可能仍是修订前的措辞；已按转录里最后一次出现的版本取自取，未逐条人工比对。

### 教训

- 写文件不要用「数组拼接 + join」这种把嵌套数组序列化的写法：**先 `[System.Collections.Generic.List[string]]` 或字符串拼接**，写完立刻 `Get-Content -TotalCount 6` + 行数回读自检（本次是回读时发现「lines now: 3」才暴露的）。
- 未提交的文档同样需要基线：改长文件前先 `Copy-Item` 一份到 `%TEMP%`（此前几轮有做，这次没做）。

## 开发中（2026-09-23 任务页风格统一：操作列居中 · 编辑弹窗卡片分区 · 执行历史同构）

### 背景

- 用户对着「浏览器任务」列表的操作列截图反馈「把当前页面风格统一一下……定时任务的（编辑页面），浏览器任务的操作让它居中，然后调试用之前 svg 的虫子图标」。逐条核对出四件事：
  1. **操作列靠右对齐**：本表首列（拖拽柄）是居中的，操作列却贴右边缘，同一行两侧的重心不对；操作列又是固定 132px 的窄列，三个 28px 图标按钮右对齐时左半留着明显空白。
  2. **同一个动作两种图标**：行内调试按钮用的是播放三角（`play`），而任务编辑页里做同一件事的「调试」按钮用的是虫子（`bug`）——三个调试入口（行尾 / JSON 卡片 / 侧栏）里两个用三角形。
  3. **定时任务编辑弹窗是全站唯一一处 `.form-section-title`**（13px + 整条下划线）：而「任务」页编辑页的分区是 `.card` + `.card-header` + `<h3>`（实测 16.4px / 700）。同一件事两套词汇、两档字号，正是"弹窗和编辑页不像一套东西"的来源。
  4. 顺带查出一个真 bug：**执行历史弹窗的条目样式一条都没生效**——`.history-header / .history-status(.success/.failed) / .history-time / .history-duration / .history-message / .history-list` 全挂在 `.scheduled-tasks-page` 选择器下，而弹窗经 `Teleport to body` 渲染，不在该容器内。于是「成功 / 失败」两个字被塞进 dashboard.css 的 20×20 图标槽、信息被挤成横排两列、`max-height: unset` 也没生效。

### 实现

- **操作列居中（`styles/pages/tasks.css`）**：`.tsk-actions` 由 `text-align: right` 改为 `center`；另补一条 `.tsk-table th.tsk-actions`——`.tsk-table th { text-align: left }`（0,1,1）压得住 `.tsk-actions`（0,1,0），只改前一条会留下一个左对齐的表头与按钮错位（与 `.sch-cell-enabled` 同一个坑）。四张列表（浏览器 / 直连 / 脚本 / 定时）共用该规则，切子页不跳形。
- **调试图标归位（`views/tasks/BrowserTasksPanel.vue`）**：行内调试按钮与侧栏「调试运行」改用 `bug`，与 JSON 卡片上的「调试」一致；脚本 / 定时任务的「运行」保持播放图标（那是"运行一次"，不是调试）。
- **定时任务编辑弹窗（`views/ScheduledTasksView.vue` + `styles/pages/scheduled_tasks.css`）**：三个分区改用 `.card` + `.card-header`（`<h3>`）+ `.card-body`，与任务编辑页同一套组件；弹窗宽度 default（560px）→ `lg`（720px）；字段行回到全局 `.form-row` 双列栅格（原来靠 `.form-row--flex` 按内容宽度左挤，「触发方式 / 超时」两个窄控件把整行右半留空）；「触发方式 · 执行时间 · 超时」并成一行——拆两行时弹窗内容比视口高，默认形态一进来就要滚动才能看到说明文字。
- **`styles/components/form.css`**：`.form-row--flex` 的 gap 12px → `var(--space-md)`（与 `.form-row` 的栅格 gap 同值，两种变体落在同一行时不出现两种列距）；新增 `.form-row--flex + .form-row--flex { margin-top }`（变体里子 `form-group` 的下边距被清零，两行 flex 行叠放会贴在一起）。
- **弹窗高度**：`.modal-container:has(.sch-form) { max-height: 92vh }`（与 `.modal-xxl` 同口径）。「启动后执行」形态比别的弹窗内容高，85vh 下实测量刚好差 60px；内容不满时高度仍由内容决定，该规则只在"差一点装不下"时生效。
- **执行历史弹窗改用共享结构**：条目与仪表盘登录历史同构（`.history-status` 放 `check-circle / x-circle` 图标 + `.history-info` 信息列），消息行按成功 / 失败走 `.history-profile` / `.history-error`——不再为本页新增任何类名。
- **删除死样式**：`scheduled_tasks.css` 里 47 行失效的 `.scheduled-tasks-page .history-*` 规则、`.form-section` / `.form-section-title` / `.sch-type-field`；`ScheduledTasksView.vue` 里对应的旧结构（`.history-header` / `.history-message`）一并移除。

### 验证

- `npm run typecheck` 零错误、`npm run test` 317 例全绿、`npm run build` 通过。
- 实机回归（真实二进制 + 真实前端 + Playwright，`/api/**` 全 mock）由 74 项扩到 **91 项断言全过**，新增：操作列表头与按钮同轴居中、行尾按钮顺序（编辑 / 调试运行 / 更多操作）且调试为虫子图标、弹窗三张 `.card`、分区标题字号 ≥16px、卡片间距 16px、弹窗宽 720px、**默认形态弹窗内部零滚动**、**「启动后执行」形态同样零滚动**且三字段等分、执行历史六项（两条记录 / 图标槽 20×20 内是 svg / 信息列竖排 / 成功色条 `rgb(16,185,129)` / 失败错误色 `rgb(239,68,68)` / 旧类名已不存在）。
- 目检截图（本地，不提交）：`sched-after-modal.png`（定时执行形态）、`sched-after-modal-startup.png`（启动后执行形态）、`sched-after-history.png`（执行历史）、`sched-after-modal-dark.png`（深色对照）。

## 开发中（2026-09-23 定时任务页改版 + 弹窗底面抬升）

### 背景

- 用户对着「新建定时任务」弹窗截图反馈「定时任务的界面也优化一下」。实测与复核出四件事：
  1. **弹窗比页面卡片更暗**：浅色主题 `--bg-modal: #e4e9f0`（与 `--bg-secondary` 同值）= 实测 228，而页面卡片 ≈246、页面底 238——叠在 `rgba(0,0,0,.6)` 遮罩上，弹窗读成"一块灰板子上面摆着白输入框"（用户截图里的"发灰"就是它）。深色主题反倒是"弹窗比卡片亮一档"，两边口径不一致。
  2. **列表还是旧卡片版式**：`.task-item` + 四个文字按钮（运行 / 查看历史 / 编辑 / 删除）平铺在行尾、开关无表头，与「任务」页三个面板的表格语言对不上（该页是唯一还在用这套旧版的页面）。
  3. **信息以一句话串起来**：`类型 · target_id · 每天 08:00 · 超时 60s · 上次: 成功`——目标只显示 id（要用户自己去别的页面对照名称），死引用（目标任务被删）没有任何提示，只能等触发时失败。
  4. 原生 `<input type="time">` 的指示图标是浏览器默认灰、贴着右内边距；`.form-section-title` 上的 `text-transform: uppercase` 与 0.5px 字距对中文是空转。

### 实现

- **列表改版（`views/ScheduledTasksView.vue`）**：改用 `tsk-*` 共享版式——整页表格 8 列（名称+描述 / 类型 / 触发 / 目标 / 超时 / 最近结果 / 启用 / 操作）、行尾 ⋯ 菜单（立即运行 · 查看历史 · 编辑 · 删除）、点行进编辑弹窗、页脚说明；工具行与三个面板同构（标题 + `?` 气泡 + 新建）。
- **信息变成列**：目标显示**任务名**（回退 id），目标任务已不存在时在副行标红「目标已不存在」——后端只在保存时拦死引用，列表里得自己认，否则这行看起来一切正常、直到触发才失败；触发列按 cron / 启动两种模式给主副行（每天 08:00 / 启动后执行 + 今日成功 x/y，或「表达式无效」）；最近结果给成功 / 失败 / 尚未执行三态徽标 + 时间；超时单独一列。
- **启用开关进表格**：单独一列并带表头（此前是一个没有任何标注的开关，浮在行尾）；停用行整行文字弱化，但开关与操作列保持原样（开关是恢复入口，跟着变淡会让人以为点不动）。
- **弹窗底面抬升（`styles/base.css`）**：浅色主题 `--bg-modal` 由 `#e4e9f0` 改为 `#fbfcfe`（等于卡片之上的一档，与深色主题口径对齐）。该 token 被弹窗、Toast、下拉菜单、向导共用，故浅色主题下这些"悬浮面"一起变亮。
- **弹窗内控件底色（`styles/components/modal.css`）**：新增 `[data-theme="light"] .modal-container / .confirm-dialog` 作用域内的 `--bg-glass` / `--bg-glass-heavy` 覆盖（10% / 17% 的 slate 调）。底面变近白后，控件若仍用 70% 白就与底色只差两个色阶，"这是输入框"只剩边框在传达；改令牌而不是逐个控件选择器，input / textarea / CustomSelect 触发器一并跟随，后续新增控件自动继承。深色主题不受影响（实测仍 `rgba(30,41,59,.97)` + `rgba(15,23,42,.5)`）。
- **表单细节**：分区标题去掉空转的 `uppercase` / 字距、字号提到 13px 并改用正文色；任务类型下拉不再与目标下拉平分宽度（180px）；执行时间字段收边（140px、等宽数字、原生指示图标降透明度并与右内边距留距）并补一句「每天到这个时间执行一次」；页面补 `?` 气泡（定时任务只调度浏览器任务与脚本，直连任务的验证入口在任务编辑器里）。
- **清理**：`.task-item` / `.task-list` / `.task-info` / `.task-desc` / `.task-actions` / `.scheduled-task-item*` 全部成为死样式后删除（该页是最后一个使用方）；`.tsk-table tbody tr.dragging` 补回来——列表从卡片改成表格后，拖拽中的行一度没有任何视觉反馈。

### 验证

- `npm run typecheck` 零错误、`npm run test` 317 例全绿、`npm run build` 通过。
- 实机（真实二进制 + 真实前端 + Playwright，`/api/scheduler/jobs` 与任务列表全 mock）：回归脚本从 58 项扩到 **74 项断言全过**，新增 16 项覆盖定时任务页——3 行渲染、8 列结构、目标解析为名称、死引用标红、停用行弱化、启用列居中、结果徽标三态、⋯ 菜单四项、弹窗三分区标题、执行时间字段存在、**弹窗底面取到近白**（`rgb(251,252,254)`，不再是与 `--bg-secondary` 同值的灰）、**弹窗内输入框与底面区分开**、窄屏四页零溢出。
- 目检截图：`docs/reports/ui-check/sched-after-list.png`（列表）、`sched-after-modal.png`（浅色弹窗）、`sched-after-modal-dark.png`（深色弹窗，确认未受影响）（本地，不提交）。
- 过程中修掉自己一处断言错（`.sch-cell-target` 同时命中了表头 `th`，而表头文字是「目标」）。

## 开发中（2026-09-23 列表底色统一：不再有"这一行颜色不一样"）

### 背景

- 用户对着浏览器任务列表截图问「为什么任务项颜色不一样」——只有一条任务时，数据行明显比表头与页脚深一档，像"被选中/悬停"了。实测（Playwright 取计算样式 + 截图取样，鼠标移到角落排除 `:hover`）：同一张卡片里有**三层底色**叠加——

  | 层 | 计算值 | 渲染（浅色主题实测） |
  |---|---|---|
  | 卡片玻璃面 | `rgba(255,255,255,.45)` | ≈ `RGB(246,248,251)` |
  | 表头 / 页脚 | 再叠 `--bg-glass`（`.7` 白） | ≈ `RGB(252,253,254)` |
  | 数据行 | 无自己的底色（透明） | 露出卡片本色 246 |

  行本身写的是 `rgba(0,0,0,0)`，所以"那一行的颜色"其实是**卡片本色**，而上下两条是"卡片本色 + 一层白"。

### 实现

- `styles/pages/tasks.css`：表头 `th` 与 `.tsk-foot` 去掉 `background: var(--bg-glass)`，整张表统一为卡片自身的表面色；结构改由「12px 弱化字 + 底边框」（表头）与「上边框」（页脚）表达。三面板共用这套规则，一次改完。
- 随之删掉表头两条圆角补偿规则（表头不再有底色，没有需要圆的东西）；数据行与页脚那两条**保留**——悬停时那一行会被染色，最后一行 / 页脚的圆角仍要有人承担。

### 验证

- 像素复测：卡片纵向扫描 y=175/200/230/260/300 全部 `RGB(246,248,251)`（改前为表头 252 / 行 246 / 页脚 252），卡片外仍是页面背景 238；计算样式里 `th` / `tr` / `td` / `.tsk-foot` 均为 `rgba(0,0,0,0)`。
- `npm run typecheck` 零错误、`npm run test` 317 例全绿、实机浏览器检查 **58 项全过**；截图 `docs/reports/ui-check/row-color.png`（本地，不提交）。

## 开发中（2026-09-23 侧栏二级导航子项放大）

### 背景

- 用户对着展开的「任务」分组截图（浏览器任务 / 直连任务 / 脚本 / 定时任务 / AI 生成）反馈「把下面的这些做大一点」。原值是刻意降档的：子项 13px / `padding: 7px 10px`（行高 33px），一级项 14px / `12px 16px`（行高 46px）——落差在实机里读成"字太小"，而不是"层级更低"。

### 实现

- `components/sidebar.css`：`.nav-child` 字号 `--text-md`(13px) → **`--text-base`(14px)**（与一级项同级）、内边距 `7px 10px` → **`10px 12px`**（行高 33 → **40px**）、圆角 `radius-sm`(6) → `radius-md`(8)（与一级项同款）、激活态字重 500 → 600；`.nav-children` 的 gap 2 → 4px、左内边距 `8px`→`10px`、外边距 `21px`→`20px`（文字仍略靠左于一级项标签，缩进关系不变）。
- 层级不再靠**字号降档**表达，改由「无图标 + 左侧 1px 细轨 + 弱化色」承担——这两条在任何字号下都成立，而字号降档一旦被读成"字小"就同时损失了可读性与层级感。
- 矮视口无需额外处理：`.nav` 本就是 `overflow-y: auto`（`layout.css`，上一轮为同样的问题加的）——子项变高后多占约 43px（5 项），必要时侧栏内滚动、不推走底部状态行。

### 验证

- 度量（Playwright，真实实例，只点「任务」行展开、不跳页）：子项行高 **33 → 40px**、字号 **13 → 14px**（一级项 46px / 14px），5 项共多占 43px；实测脚本见 `docs/reports/ui-check/ca-navchild-metrics.py`（本地，不提交）。
- `npm run typecheck` 零错误、`npm run test` 317 例全绿、实机浏览器检查 **58 项全过**。截图 `docs/reports/ui-check/nav-child-before.png` / `nav-child-after.png`（本地，不提交）。

## 开发中（2026-09-23 字段说明气泡（`?`）样式与层叠修复）

### 背景

- 用户对着浏览器任务列表标题旁的 `?` 气泡截图反馈「优化一下这个提示的样式」。实测（Playwright 量几何 + 像素扫描）：气泡宽 460px（`--wide`）、一句 38 字的中文说明折成两行半、末尾两个字单独落到第三行（「…自动登录才会用到／它。」），而 2280px 视口右侧空着一千八百多像素——上界既不跟文案长度走，也不跟可用空间走；气泡还是悬空一块，看不出它属于哪个 `?`。

### 实现

- **宽度策略（`components/form.css`）**：`width: max-content` + 上界取三者最小值——① 变体默认（普通 420px / `--wide` 620px）② 组件按触发点两侧实测剩余空间写入的 `--tip-max` ③ `calc(100vw - 64px)`。删掉"翻转时各再收紧 40px"的两条规则：左右摆的是气泡位置、不是它需要的宽度，真要收由 ② 的实测决定。`FieldHelp.vue` 的 `wanted` 同步改成 420/620（两处数值不一致会让组件在明明放得下时就把气泡压窄）。
- **箭头**：新增 `.field-help::before`，用"旋转 45° 的小方块"指向触发点（挡在气泡后的半块由后绘制的 `::after` 覆盖）。先写的 border 三角版本**实测根本没上色**（像素扫描：圆形右缘到气泡左缘之间全是背景色，而计算值一切正常），换成旋转方块后正常——四方向对称、接缝更服帖，翻转也不必各写一份 border 配色。钉住态（`--pinned`）同样显示箭头。
- **层叠修复**：卡片因 `backdrop-filter` 各自成为层叠上下文，气泡被限制在自己卡片那一层里——DOM 靠后的卡片（任务编辑器主列的「测试区」）会盖住前一张卡片中弹出的长说明的下半截，实测呈现为气泡底部一条横向亮度分界带（先怀疑 `backdrop-filter` 自身，注入 `backdrop-filter: none` 后band 依旧，才定位到层叠）。修法：`card.css` 里 `.card:has(.field-help:hover / :focus-visible / .field-help--pinned) { position: relative; z-index: var(--z-overlay) }`——悬停/钉住期间把所在卡片抬起来，不把气泡 teleport 到 body。
- **其它细节**：触发标记 14px/8px 字 → **15px/9px**、描边与底色各重一档（原来读成装饰而不是"能点的东西"）；圆角 `radius-md`(8) → `radius-lg`(10)；阴影改用主题令牌 `--shadow-float`（原先写死的 `0 4px 16px rgba(0,0,0,.25)` 在浅色主题下过重）；删掉 `letter-spacing: 0.01em`；行高 1.65 → 1.7；显式声明 `font-family: var(--font-sans)`（气泡会出现在等宽上下文旁边，不该跟着继承）；`responsive.css` 的 ≤640px 上界 320px → 420px 且视口兜底从 `-64px` 改 `-48px`（手机上白白浪费两成屏宽）。

### 验证

- 度量（Playwright，真实实例，只悬停不改数据）：气泡由"460px 夹到 3 行"变为 **484px / 61px 高（每段各一行、无孤字）**；`--wide` 长文案按 620px 呈现（约 51 字/行）；深色主题下取到深色令牌 `rgba(15,23,42,.96)`。
- 像素扫描确认箭头真的画出来了（`::before` 命中）与亮度带消失（气泡内部纵向取样从"上深下浅台阶"变为均匀 `RGB(39,49,67)`）。
- 回归：`npm run typecheck` 零错误、`npm run test` 317 例全绿、实机浏览器检查 **58 项全过**。截图 `docs/reports/ui-check/tip-after.png`、`tip-after-pinned.png`、`tip-caret-zoom.png`、`tip-wide.png`、`tip-dark.png`（本地，不提交）。
- 顺带修掉脚本自身的口径：`docs/reports/ui-check/ca-taskpage-check.py` 原来只从 `config/.runtime_port` 读端口，而**基座默认是 exe 目录**（`target/debug`），改为两处都看一眼（见同日「直连任务纳入拖拽排序」条目里那次踩坑）。

## 开发中（2026-09-23 直连任务纳入拖拽排序）

### 背景

- 用户要求「允许直连任务拖拽排序」。这是「直连请求重构为独立直连任务」当初写进 `docs/plan-next.md` 的遗留小项：`.order.json` 本来是三类任务**共用**的一份扁平 id 表（`loader.rs` 的 `list_all_tasks` 按 id 排序，与类型无关），缺的只是接口与前端——`POST /api/tasks/order` 的载荷只有 `{all, scripts}`，而 `order_tasks` 是先 `clear()` 再 `extend()` 整表替换，于是直连任务的 id 被顺手清掉，列表顺序回落为目录扫描顺序。

### 实现

- **后端（`src/web/routes/tasks.rs`）**：`OrderBody` 新增 `http: Vec<String>`（`#[serde(default)]`），`order_tasks` 一并写入。刻意**不给 `all` 改名**（它是"浏览器任务"的历史字段名）：改名会让「旧后端 + 新前端」这种组合（调试构建运行时读盘、前端先于后端更新时真实存在）在拖拽排序时因缺字段 400，而加字段做不到这点——旧前端不发 `http` 只会让直连顺序回落（与改动前行为一致），旧后端收到多出来的 `http` 也会忽略。
- **前端**：
  - `utils/drag.ts` 的 `DragSortOptions` 补 `http`，并把载荷构造抽成纯函数 `orderPayload(tasks, scripts, http)`（`utils/drag.test.ts` 3 例）。三个字段**全为必填**：漏传一组不会报错，只会静默清空那组顺序（排序看起来生效、刷新另一类任务才发现乱了），必填让 TypeScript 在编译期拦住。
  - `HttpTasksPanel` 新增拖拽列（`col.tsk-col-drag` + 行首柄），列表行接上 `useDragSort(httpTasks, { tasks: browserTasks, scripts, http: httpTasks })`；行下标按**全量下标**映射回 `httpTasks`（与另两个面板同口径，搜索过滤时不会挪错条目），页脚补「拖拽行首调整顺序」。
  - `BrowserTasksPanel` / `ScriptsPanel` 同步补上第三个分组（不补就等于它们拖拽时清空直连顺序）。
  - 直连列表的名称列宽 20% → 24%（scoped 覆盖）：它的名称格是两行（名称 + 描述副行），20% 会把描述挤成半句；剩余宽度仍由「请求」列（auto）吃。

### 验证

- `cargo fmt --all --check` 零差异、`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告、`cargo test --features no-embed --lib` **994 passed / 0 failed / 1 ignored**（新增 `test_order_tasks_without_http_group_is_accepted`，并把 `test_order_tasks_dedupes` 扩到三组交叉去重 → `["t1","t2","s1","h1"]`）。
- 前端 `npm run typecheck` 零错误、`npm run test` **33 文件 / 317 例**全绿（新增 `utils/drag.test.ts`）、`npm run build` 通过。
- 实机（真实二进制 + 真实前端 + Playwright，任务 API 全部 mock）：**58 项断言全过**，其中新增——直连列表按请求地址搜索后拖拽，POST 载荷 `http` 为 `["blank_http","lib_http","dorm_http"]`（中间那条未动、被拖条目落到末尾），且 `all` / `scripts` 两组同时带上（证明直连面板拖拽不会清空另两类顺序）；浏览器任务面板拖拽的载荷同样三组俱全。
- **只跑了 lib 测试与 clippy（check-only）**，未跑 `cargo test`（全量）：按 `AGENTS.md` 的告警，它会把 `target/debug/campus-auth.exe` 覆盖成不嵌前端的构建，而用户实例正锁着该路径。本改动要生效需重建后端并重启实例（见下）。

### 生效条件（已完成）

后端改了，运行中的实例是旧二进制。已按用户确认执行：优雅退出（`POST /api/system/shutdown`；`--stop` 因 `.instance` 里是重启交接留下的死 PID 而未命中，改用实例自报的 token 调接口）→ `cargo build`（默认特性，内嵌新前端）→ 原样重启。

**重启命令记录（踩过坑）**：基座默认是 **exe 所在目录**（`main.rs` 的 `resolve_base_path`：CLI 参数 > exe 目录 > 当前目录），开发形态下即 `target/debug`——**不能想当然按"仓库根"传 `--base-path`**。本轮第一次重启我误传了 `--base-path E:\Campus-Auth-rs`，实例随即指向了仓库根的那份测试基座（9 个浏览器任务 + 2 个脚本、无直连任务），与用户实际在用的 `target/debug` 基座（1 浏览器 + 1 直连）不是同一份数据。发现后立刻停掉并**不带参数**重启（`Start-Process target\debug\campus-auth.exe`，cwd 不影响结果），任务集与端口即回到原状，两个基座的任务文件与 `.order.json` 全程未被写入。

## 开发中（2026-09-23 任务页三面板全面复核：共享版式 + 脚本面板迁移方案 G + 六处缺陷修复）

### 背景

- 用户在方案 G（列表页 + 二级编辑页 + 自动保存）落地后要求「全面细致优化当前页面的重写内容，修复各种问题」。对三个面板（浏览器任务 / 直连任务 / 脚本）、`useTasks` / `useHttpTasks` / `useScripts`、`utils/drag`、`styles/pages/tasks.css` 做了一轮逐行复核，实际问题分三类：
  1. **状态判据错**：编辑态由 `?task=` 参数驱动（`isEditing = query 非空 || 草稿非空`），于是「返回列表」只清参数不清草稿 → 编辑页退不出去；而 `?task=` 指向已删除/错拼的 id 时「参数非空 + 草稿为空」→ 列表与编辑两个分支都不渲染，整页空白且无返回入口。
  2. **两个交互数学错误**：搜索过滤后拖拽用「过滤后下标」改「完整列表」顺序（挪错条目并把错序持久化到后端）；表格卡 `overflow: hidden` 裁掉行尾 `⋯` 菜单（越靠下的行切得越多，最末一行只剩顶边）。
  3. **三份实现各自漂移**：三个面板各持一份约 350 行 scoped 版式、一份自动保存状态字、一份缺口校验（`useHttpTasks.draftGaps` 与 `utils/httpTask.httpTaskDraftGaps` 双份），脚本面板整体仍停在旧的「主从两栏 + 显式保存 + 脏确认」模型。

### 实现

#### 一、共享版式（`styles/pages/tasks.css` 新增 `tsk-*` 组）

- 列表页 / 表格 / 列宽 / 行尾菜单 / 二级编辑页 / 面包屑 / 状态字 / 缺口条 / 侧栏键值块收敛为一份，三个面板的 scoped 样式只剩各自独有的部分（JSON 编辑器、词条、向导胶囊）。类名统一 `tsk-` 前缀（本文件是全局样式，`list-title` / `editor-head` 这类裸类名与其它页面撞名时由导入顺序决定谁生效）。
- 列宽口径三面板统一（拖拽 44px / 名称 20% / ID 14% / 弹性列 auto / 绑定 13% / 执行程序 11% / 最近修改 10% / 操作 132px），切子页时同一列落在同一 x 位置。窄屏降级改为按信息价值让位：≤1100px 先让「最近修改」（`col` 与单元格成对隐藏，否则 fixed 布局下会留一条空列），≤768px 再让绑定与 ID；弹性的描述列（浏览器/脚本）与请求列（直连）始终保留——它才是辨识一行的那一列（旧的 `≤1100px 隐藏请求列` 会让直连任务只剩「未命名直连任务」这种名字）。
- 行尾菜单不再被裁切：`.tsk-table-card` 改 `overflow: visible`，卡片圆角由表头 / 表尾单元格与 `.tsk-foot` 自己承担（`:has(.tsk-foot)` 时归页脚）。实测最后一行菜单会溢出卡片底部约 97px，属预期。
- 新增 `.tsk-muted`：此前面板借用 `about.css` 里全局定义的 `.muted`（那条规则还带 `margin-top`），等于把"关于页的排版规则"焊进列表单元格；改为任务页自有类名。
- 删除随之失效的旧版式：`.tlist-*`（方案 A 主从两栏）、`.tasks-grid`、`.help-tip`、`.help-content*`、`.task-editor*`（共约 380 行）。`.task-list` / `.task-item` / `.task-info` / `.task-desc` / `.task-actions` **保留**——「定时任务」页仍在使用，已就地注明以免被当死样式清掉。

#### 二、编辑态判据与 `?task=` 同步（新增 `composables/useTaskEditorQuery.ts`）

- 判据收敛为一条：**编辑态 = 草稿非空**。`?task=<id>` 只是"意图"，由三面板共用的 `useTaskEditorQuery` 解析：目录就绪后 id 存在则打开、不存在则提示「找不到任务「xxx」，它可能已被删除」并撤下参数；草稿被清空（关闭 / 删除当前任务）时同步撤下参数。`useTaskDirectory` 新增 `loaded`（"已拉取过"而非"列表非空"）作为存在性判定的前提——冷启动深链时列表本来就是空的，把"还没拉完"当成"不存在"会给错提示还抹掉用户的深链。
- 点行进入编辑时写 query 与发起打开是同一次动作，`openIfNeeded` 用 `opening` 标记去重（否则 query 变化的 watcher 会把同一次打开做两遍：两次详情请求、两次草稿赋值）。
- 面板侧：`closeEditor()` 改为「撤 query + 关编辑器」，`createX()` 新建后撤掉旧 query 再进入新草稿。

#### 三、脚本面板迁移方案 G

- 新增 `utils/scriptDraft.ts`（草稿形态 / 载荷互转 / 缺口校验），`useScripts` 改为自动保存：debounce 500ms 静默 PUT、`autosaveState` 四态、退出前冲刷在途 debounce。**与另两个面板的差异只在新建**：脚本 ID 是文件名、也是「定时任务」的引用值，故新建先给一份空 ID 草稿（不落盘），ID 合法后第一次自动保存才创建文件，落盘后 ID 固定——直连任务的 ID 没有外部含义，随便取 `untitled_N` 就行，脚本不行。
- 落盘判定改用**载荷指纹**（`scriptDraftPayload` 的 JSON 串）而非"比较草稿 id"：新建草稿的 ID 由用户输入，按 id 判"换了编辑对象"会把改名当切对象、首次落盘永远排不上期；指纹比较顺带免掉"内容改回原样也发一串 PUT"。
- 切换编辑对象前补发在途 debounce（丢的是停手不足半秒的那笔改动）；缺口拦住落盘时，切对象静默、**关闭编辑器出声**（一句「改动未保存：还缺 …」）——「退出即生效」不能在缺口态下变成静默丢弃。
- 缺口校验补齐三处此前不存在的判定：自定义执行程序路径为空（旧实现会静默存成 Python）、PowerShell / `.ps1`（前端拦过但没进过"保存闸口"）、内容按 UTF-8 字节数限 100 KB（后端按字节，中文不能按字符数蒙混）。
- `ScriptsPanel.vue` 重写为列表页 + 二级编辑页：列（拖拽 / 名称 / 脚本 ID / 描述 / 执行程序 / 最近修改 / 操作）、行尾 ⋯（立即运行 / 导出 / 删除）、搜索、深链 `?task=<id>`、编辑器三卡（基本信息 / 脚本内容 / 侧栏执行与调试 + 快速上手）。
- `router/editorGuard.ts` 空壳与 `useScripts` 的 dirty 快照三件套随之删除（两侧文件注释里「脚本面板迁移后一并删除」的条件已满足）。

#### 四、自动保存闸口与状态字（新增 `utils/autosave.ts`）

- 状态字三份 `switch` 收敛为 `autosaveLabel(state, gaps)`，**缺口优先**：有缺口时说的是「有 N 处待补全，改动暂未保存」而不是「改动自动保存」/「已保存 · 刚刚」。直连面板与脚本面板的编辑页顶部新增缺口条（列出缺什么），`useHttpTasks` 新增 `draftGapsNow`；状态字说谎比不显示状态字更坏——那是本轮修掉的静默丢改动的入口。
- `useHttpTasks` 的本地 `draftGaps` 删除，改用 `utils/httpTask.httpTaskDraftGaps`（两份判定曾各自演进，属"面板说能存、后端说不能"的温床）。
- `closeTaskEditor` / `closeHttpTaskEditor` 在缺口（或 JSON 语法错误）拦住落盘时补一次提示，不再静默丢弃。

#### 五、其它修正

- 拖拽排序把「模板行下标」映射回**全量下标**（浏览器任务与脚本两处）：`useDragSort` 按传入列表 id 做 splice 与持久化，过滤后的下标会挪错条目。实测（搜索态拖拽）POST 载荷 `all` 为 `["dorm_browser","lib_browser","default"]`——与被拖位置一致且未动被过滤掉的那条。
- 列表行「最近修改」不再由渲染层按 id 反查列表（O(n²) + 两处取数），改由 `buildHttpTaskRows` 带出 `modifiedAt`；脚本列表的 `Script` 类型补 `modified_at`。
- 浏览器任务列表的「点行进入编辑」此前只有名称格可点（页脚却写着"点行进入编辑"），且名称格用 `padding: 0 !important` + 专用按钮绕过行高——统一为整行可点、名称格回到普通单元格（行首拖拽柄与操作列 `@click.stop`），`?` 说明从自绘 span 换成 `FieldHelp`（可点击钉住）。
- 工具栏标题的 `margin: 0 auto var(--space-sm) 0` 让标题在 flex 行里整体上移 8px，改为 `margin: 0 auto 0 0`；搜索框改弹性宽（`flex: 1 1 180px; max-width: 260px`），窄屏不再与按钮组抢宽；补全 `type="button"`（历史上有过"缺 type 导致点击整页刷新"）。
- `IconApp` 新增 `search` 图标：三个面板的「搜索无匹配」空态此前没有可用的放大镜图标。
- 文档同步：`http-login-guide.md` 六处「点保存任务 / 未保存的草稿也能测 / 导入后仍需点保存」改为自动保存口径；`task-manual.md` 的任务页小节补「编辑即自动保存」与脚本 ID 规则；`custom-script-guide.md` §3 重写为「新建 → 命名 ID → 自动保存 → 立即运行」；`updatelog.md` 的「尚未发布」新增任务页改版与五条修复。

### 验证

- 前端：`npm run typecheck` 零错误；`npm run test` **32 文件 / 313 例**全绿（新增 `utils/autosave.test.ts` 4 例、`utils/scriptDraft.test.ts` 15 例，`utils/httpTaskList.test.ts` 补 mtime 断言）；`npm run build` 通过。
- **实机（真实二进制 + 真实前端 + Playwright，任务 API 全部 mock 以保护用户真实任务文件）**：50 项断言全过（脚本与截图在已忽略的 `docs/reports/ui-check/`，不提交）——覆盖 ①点行进入编辑 / ②返回回到列表且地址栏清参 / ③深链到不存在的任务不白屏且给出提示 / ④最后一行 ⋯ 菜单项可被命中（`elementFromPoint` 命中测试）且点击触发动作 / ⑤搜索态拖拽的顺序载荷正确且脚本组全量互传 / ⑥直连缺口态：缺口条 + 状态字改口 + 缺口未补不发请求 + 补齐后自动落盘且载荷 `type=http` / ⑦脚本新建立即编辑：空 ID 不落盘、命名后自动创建、落盘后 ID 禁用 / ⑧三面板在 900px 与 1600px 下零横向溢出、零 JS 报错。截图 `docs/reports/ui-check/01~08-*.png`（本地，不提交）。
- Rust 侧本轮**未改动**、也**未跑** `cargo test --features no-embed`：按 `AGENTS.md` 的告警，该命令会把 `target/debug/campus-auth.exe` 覆盖成不嵌前端的构建，而用户实例正在运行该路径（调试构建运行时读盘，故本次前端改动无需重编 Rust 即可生效，实机验证也正是在运行中的实例上做的）。

### 有意不做

- **直连任务不参与拖拽排序**（既有取舍，未变）：`POST /api/tasks/order` 的载荷只含浏览器任务与脚本 id。
- **脚本重命名**：ID 落盘后不可改，要换名需删除重建；做「改名 = 存新 id + 删旧 id」会引入一次会留下孤儿文件的失败窗口，不在本轮范围。
- **缺口态在「切换编辑对象」时静默**：只在关闭编辑器时提示，避免用户点列表里另一条时被弹一句（改动确实丢了，但那是用户主动切换）。

## 开发中（2026-09-20：修复新建浏览器任务被空 steps 校验拒绝）

- 修复：与直连任务同类问题——新建浏览器任务的种子 `steps` 为空数组，被后端 `validate_task` 的「steps 不能为空」闸口 400 拒绝。种子改带一个无害的占位步骤（`sleep` 1000ms，描述注明"编辑 JSON 时替换"），通过校验且执行无副作用。
- 验证：typecheck 零错误、vitest 293 全过、build 通过；重建二进制实测页面 200，新建流程落盘并跳入编辑页。

## 开发中（2026-09-20：修复新建直连任务被空地址校验拒绝）

- 修复：自动保存模式的新建直连任务种子 `url` 留空，被后端 `validate_task` 的「直连任务缺少请求地址」闸口 400 拒绝（"无法新建task"）。种子地址改为非空占位符 `{gateway_host}`（`NEW_TASK_PLACEHOLDER_URL`）：通过保存校验，又明确表达"待填"——执行时占位符无对应值原样保留，请求必然失败但不会误登录真实地址；编辑页测试按钮检测到占位符时提示先替换。
- 验证：typecheck 零错误、vitest 293 全过、build 通过；重建二进制实测 `/tasks/http` 200，新建流程落盘 `untitled_1` 并跳入编辑页。

## 开发中（2026-09-20：任务列表行高加大）

- 任务列表行内边距 10px → 18px（垂直方向，行高约 52px → 68px），表头 10px → 12px；浏览器任务名称格的自绘按钮内边距同步对齐。两个列表（浏览器/直连）同口径。列表是页面主体，行距太密显得单薄（截图反馈：单条任务时表格尤其显小）。

## 开发中（2026-09-20：任务列表操作列顺序对齐）

- 浏览器任务列表操作列图标顺序改为「编辑（pencil）· 调试（play）· 更多」，与 demo 的 ✎ ▶ ⋮ 一致（此前调试位用的是 bug 图标且排在编辑前）；⋯ 菜单里补「调试」项，行内不再放的调试动作仍可达。

## 开发中（2026-09-20：任务列表「最近修改」列）

- 任务列表补齐 demo 中的「最近修改」列（此前落地时因 `TaskSummary` 无时间字段而省略，与 demo 不一致）：后端 `TaskSummary` 新增 `modified_at`（任务文件 mtime，UTC RFC3339 秒级；`summary_from_value` / 裸 `.py` 摘要路径顺手读，详情路径不填避免多余摸盘），前端 `formatMtime` 按本地时区格式化为 `MM-DD HH:mm`（经 `Date` 解析修正时区，直接截字符串会把 UTC 当本地差 8 小时）。浏览器任务与直连任务两个列表各加一列（colgroup 同步 7/6 列比例），mtime 读不到时显示「—」。
- 验证：cargo lib 测试 993 全过（`get_task_detail` 等两处 mock 构造补齐新字段）、clippy -D warnings 零警告、fmt 零差异；前端 typecheck / vitest 293 / build 通过；重建二进制实测页面 200。

## 开发中（2026-09-20：任务列表列宽修复）

- 任务列表表格落地后与 demo 走样（真实单条数据下列宽失衡）：浏览器任务面板表头 6 列但行只渲染 5 个 td（描述列缺失、绑定 pill 误用描述列样式），且名称列无宽度约束被内容撑到 60%+ 行宽。两处列表（浏览器/直连）统一改 `table-layout: fixed` + `<colgroup>` 锁定列宽比例（浏览器：拖拽 44px / 名称 26% / ID 17% / 描述弹性 / 绑定 15% / 操作 132px；直连：名称 22% / ID 17% / 请求弹性 / 绑定 15% / 操作 104px），长名称/长地址按列省略；浏览器任务名称单元格去掉与描述列重复的内嵌描述行。
- 验证：typecheck 零错误、vitest 293 全过、build 通过，重建二进制实测页面 200。

## 开发中（2026-09-20：任务页方案 G——列表页 + 二级编辑页 + 自动保存）

### 编辑模型重构：自动保存（浏览器任务 / 直连任务）

- **编辑模型换轨**：浏览器任务与直连任务从「草稿 + 显式保存」改为**自动保存**——字段变更 debounce 500ms 静默 PUT，头部状态字 idle→saving→saved/error；「退出即生效」，没有保存按钮、没有「放弃未保存的修改？」确认。单用户本地工具无并发冲突，草稿/脏快照/确认弹窗整套仪式退役。
  - `useTasks.ts` / `useHttpTasks.ts` 重写：`useDirtySnapshot` 接线删除，新增深度 watch + debounce 的 `persistDraft`（在途请求按序号作废，慢响应不覆盖新状态）；`closeTaskEditor` / `closeHttpTaskEditor` 在退出时冲刷在途 debounce，兑现「退出即生效」。
  - 浏览器任务 JSON 文本框：语法合法才落盘，非法标红且提示「修正前不会保存」，修正后自动恢复落盘；直连任务缺口校验（请求地址等）未补齐时同样静默跳过（发了必 400）。
  - **新建即落盘**：`createTask` / `createHttpTask` 立即以 `untitled_N`（撞名自动递增）写入种子任务并跳入其编辑页；任务 ID 相应改为**创建时生成、不可修改**（原来「新建时可填 id」的窗口消失，ID 形态校验只剩种子路径需要）。
  - `editorGuard.ts`（FE2-9 离开确认守卫）退役：router.beforeEach 调用移除，`editorGuard.test.ts` 删除。守卫文件本身先是留了个注明迁移原因的空壳导出，后来在脚本面板迁移方案 G 时连同空壳一并删除（见上方「`router/editorGuard.ts` 空壳与 `useScripts` 的 dirty 快照三件套随之删除」那条）。脚本面板当时未迁移自动保存，暂不受影响。
  - `useRepoImport` 仓库导入改为**直接落盘 + 跳编辑页**（无草稿可写）：撞已有 id 自动加 `_N` 后缀（导入不再是覆盖语义），成功后 `router.push` 到 `?task=<id>`；对应单测改写为新契约（save 载荷 + 编辑器跳转断言）。
- **任务页结构（方案 G 终稿）**：浏览器任务与直连任务两个面板统一为「**全页列表 + 二级编辑页**」：
  - 列表态：整页任务表格铺满内容区（无右列、无帮助卡），浏览器任务列为拖拽柄/名称+描述/ID/绑定方案/操作（调试·编辑·⋯菜单），直连任务列为名称/ID/方法+地址摘要/绑定方案/操作（编辑·⋯菜单）；工具行仅搜索 + 导入/仓库导入（/分享适配）+ 新建；页脚一行「拖拽排序 · 点行进入编辑 · 改动自动保存」。
  - 编辑态：点行或新建进入，列表整页切走；面包屑「‹ 返回任务」返回；主列「基本信息卡 + JSON/请求配置卡」，右侧 320px 侧栏（调试运行/绑定方案/快速上手/字段速查）；直连任务保留配置向导入口与测试请求区。删除/导出收进编辑页头部右侧（低频操作弱化为 ghost）。
  - 两态由 `?task=<id>` 查询参数表达：刷新与深链直达编辑态，方案页「配置直连任务」等外部入口带参跳入即落编辑器，返回即列表；路由名/路径未变（`/tasks/browser` 等），仅新增 query 语义。
  - 浏览器任务列表的「绑定方案」列来自 `ProfileSummary.active_task` 反向聚合（直连任务沿用 `buildHttpTaskBindingIndex` 的 `active_http_task` 口径）；profiles 未就绪时显示「—」而非误导性的「未绑定」。
- `docs/compose/tasks-page-structure-options-v3.html`（方案 G demo，含浅/深主题与背景图模拟）与 v1/v2 两轮 demo 留档于 ignored 的 compose 目录。

### 验证

- 前端 `npm run typecheck` 零错误；`npm run test`（vitest）全绿（**293 passed**，含改写后的 useHttpTasks/useRepoImport 用例）；`npm run build` 通过。
- `cargo check --features no-embed` 零警告（本轮无 Rust 改动）；构建默认特性二进制并实测：`/tasks/browser` 返回 200，列表态/编辑态/自动保存状态字按方案 G 呈现。

## 开发中（2026-09-20：任务页统一主从两栏 + 直连任务退出登录请求）

### 任务页结构统一（方案 A：主从两栏）

- 任务页三个列表型面板（浏览器任务 / 直连任务 / 脚本）统一为**主从两栏**骨架：左列固定 320px 列表卡（数量 + 搜索 + 行 + 行尾 ⋯ 菜单），右列编辑器卡 / 帮助卡；窄屏（≤1100px）堆叠单列。此前浏览器任务与脚本面板为上下堆叠的旧两栏网格（`tasks-grid`），与直连面板的主从版式同页混用，切 Tab 时重心跳动。
- `styles/pages/tasks.css` 新增共享 `tlist-*` 样式组（`tlist-split` / `tlist-head` / `tlist-search` / `tlist-row` / `tlist-row-menu` 等），三个面板共用一份列表视觉；列宽从直连面板的 296px 收敛为 320px（三列不同宽会让切换 Tab 时列表内容跳位）。
- `BrowserTasksPanel.vue` 重写为主从两栏：列表行收为「拖拽柄 + 名称 + ID + 描述 + ⋯ 菜单」，编辑 / 调试 / 复制 / 导出 / 删除五个行内图标按钮（实测占 244px 行宽）收进 ⋯ 菜单；新增纯前端搜索（名称 / ID / 描述）；点行打开编辑器，选中行高亮对齐直连面板口径。帮助卡挪到右列（编辑器打开时替换显示）。
- `ScriptsPanel.vue` 同步重写为主从两栏：运行 / 导出 / 删除收进 ⋯ 菜单（立即运行为菜单首位），新增搜索与行选中态；编辑器与帮助卡的显隐关系与浏览器任务面板一致。
- 两个面板的 ⋯ 菜单沿用直连面板的交互契约：`pointerdown.stop` 阻止全局点外关闭监听吃掉菜单点击、菜单打开期间才挂全局监听、Escape / 点外关闭。

### 直连任务：退出登录请求（logout_request）

- `src/tasks/models.rs` 新增 `HttpActionRequest` 动作请求模型（method / url / headers / body / wait_secs），`HttpTaskConfig` 增加可选 `logout_request` 字段（`skip_serializing_if` 未配置不落盘）。动作请求与前置请求（`HttpPreRequest`）的本质差异在结果语义：前置请求**取值**（取不到即终态失败），动作请求**触达**（成败不判定、失败不拦登录）——退出登录正是为踢掉「IP 已在线，拒绝重复登录」的旧会话，下线没生效时登录仍值得一试。
- 执行顺序：凭据变换脚本 → **退出登录动作** → 前置请求 → 登录请求，全部落在同一条 keep-alive 连接（`build_client` 单 Client 口径不变）。下线放在前置请求**之前**：「IP 已在线」类门户连取令牌的接口都可能被旧会话挡住，先清场再取令牌才是正确因果链。
- `wait_secs` 支持下线后等待（0~30 秒，执行侧 `LOGOUT_MAX_WAIT_SECS` 与模型层 `MAX_WAIT_SECS` 双侧钳制）：部分门户下线异步生效，立即重连仍被旧会话占住。
- 下线请求与登录请求共用同一套占位符（`{username}` / `{password}` / 脚本产出字段 / `{local_ip}` 等），`needs_local_address` 把下线模板纳入网卡探测判定；日志经 `collect_secrets` / `redact_text` 脱敏（下线失败消息可能拼 URL）。
- `HttpLoginRequest::validate_logout_request` 形状 + 体积校验；`loader.rs` 的 `validate_task` 在 http 分支对 `logout_request` 做同口径「形状 + 体积」双闸（`url` 8KB / `headers` 256KB / `body` 256KB 上限，与 `pre_request` 一致）。
- 前端契约同步：`types.ts` 新增 `HttpActionRequest` 与 `HttpTaskConfig.logout_request`；`httpTask.ts` 草稿增加 `logout_method/url/headers/body/wait_secs` 平铺字段，`httpTaskPayload` 按地址留空 = null 互转（与前置请求同口径），`httpTaskDraftGaps` 校验等待秒数 0~30。
- `HttpTaskFields.vue` 新增「退出登录请求（可选）」折叠区块（第 ⑥ 节，与凭据变换脚本 / 前置请求同款高级项交互：地址留空 = 整块忽略，已配置常驻展开），含方法 / 地址 / 等待秒数 / 请求头 / POST 请求体字段与「下线失败不影响登录」说明。

### 验证

- `cargo fmt` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed --lib` 全绿（**993 passed / 0 failed / 1 ignored**）。
- 新增测试：`models.rs` 的 `logout_request_roundtrip_and_defaults`（serde 往返 + 缺省 + 未配置不落盘）、`logout_request_validate_checks_url_shape_and_wait`（地址 / 等待钳制）；`http_login.rs` 的 `logout_request_is_sent_before_login_request`（专用记录型 mock 门户断言下线先于登录、同连接、占位符渲染）、`logout_request_failure_does_not_block_login`（下线网络失败登录照常成功）、`from_task_carries_logout_request`（任务 → 执行参数映射）、`validate_logout_request_rejects_oversized_fields`（体积闸）。
- 前端 `npm run typecheck` / `npm run build` 通过。

## v5.0.2（2026-09-20 正式版发布）

自 `v5.0.1`（`55aedb0`）起共 1 个提交，功能改动仅一项（应用内更新「立即重启」后更新未生效修复），逐项记录见下方「开发中（2026-09-20 修复：应用内更新下载完成后重启仍为旧版本）」条目，其余为版本与文档同步。

### 版本提升

- 主程序版本由 `5.0.1` 提升为 `5.0.2`，同步 `Cargo.toml`、`Cargo.lock`、`frontend/package.json`、`openapi.json`（`info.version`，路径表未变）。
- 引用版本号的文档同步：`AGENTS.md`、`docs/plan-next.md`、本文件头（`README.md` / `docker/README.md` / `docs/guides/` 未写死版本号，无需变更）。
- `docs/updatelog.md` 新增 `## v5.0.2（2026-09-20）` 发布章节，从本文件「开发中」条目汇总用户可感知变化（仅更新修复一条）；经 `release.yml:197` 同款 awk 前缀边界提取验证：`v5.0.2` 精确命中该章节且不含相邻的 `v5.0.1` 正文。
- Python Worker 版本独立固定为 `1.0.0`，不随本次提升变动。

### 验证

- `cargo fmt --check` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed` 全绿（lib **924 passed / 0 failed / 1 ignored**）。
- 更新链路端到端实测（隔离实例，红 → 绿对照）：修复前旧 release 构建（v5.0.0）复现「立即重启 → 后继进程锁住 exe → 助手替换失败 os error 32 → 回退失败 → 更新作废且包被清理」；修复后同流程走通（助手备份 → 替换成功 → 启动新版本 → 探活清理备份 → 新进程存活且 exe 摘要与更新包一致，无后继进程日志）。过程与结论详见下方「开发中」条目。

## v5.0.1（2026-09-19 正式版发布）

自 `v5.0.0`（`81c330a`）起共 2 个提交，功能改动仅一项（启动动作 `login_once` 登录成功后退出程序），逐项记录见下方「开发中（2026-09-19 修复：设置页「登录一次后退出」登录成功后程序不退出）」条目，其余为版本与文档同步。

### 版本提升

- 主程序版本由 `5.0.0` 提升为 `5.0.1`，同步 `Cargo.toml`、`Cargo.lock`、`frontend/package.json`、`frontend/package-lock.json`、`openapi.json`（`info.version`，路径表未变）。
- 引用版本号的文档同步：`README.md` 与 `docker/README.md`（Docker 固定版本示例 `ghcr.io/misyra/campus-auth-rs:v5.0.1`）、`docs/guides/user-guide.md`、`AGENTS.md`、`docs/plan-next.md`（当前活跃改为 `v5.0.1`）、本文件头。
- `docs/updatelog.md` 的「尚未发布（开发中）」段落冻结为 `## v5.0.1（2026-09-19）` 发布章节，仅含 login_once 退出修复一条；经 `release.yml:197` 同款 awk 前缀边界提取验证：`v5.0.1` 精确命中该章节且不含相邻的 `v5.0.0` 正文。
- 顺带修复 `tests/smoke_test.rs` 的 `--version` 断言：原写死 `5.0.0`，版本提升即失败；改为 `env!("CARGO_PKG_VERSION")` 派生（同 `updater_channels` 的 mock 版本口径），此后版本提升不再破该测试。
- Python Worker 版本独立固定为 `1.0.0`，不随本次提升变动。

### 验证

- `cargo fmt --check` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed` 全绿（lib **923 passed / 0 failed / 1 ignored**；集成测试含 `updater_channels` 16 例全过）。

## v5.0.0（2026-09-18 正式版发布）

自 `v5.0.0-alpha.10`（`fb5240b`）起共 85 个提交，逐项记录见下方各「开发中」条目（保留原文不改写，按日期倒序）。本次发版只做版本与文档同步，无功能代码改动。

### 版本提升

- 主程序版本由 `5.0.0-alpha.10` 提升为 `5.0.0`，同步 `Cargo.toml`、`Cargo.lock`、`frontend/package.json`、`frontend/package-lock.json`、`openapi.json`（`info.version`，路径表未变）。
- 引用版本号的文档同步：`README.md`（Docker 固定版本示例 `ghcr.io/misyra/campus-auth-rs:v5.0.0`）、`docker/README.md`、`docs/guides/user-guide.md`、`AGENTS.md`、`docs/plan-next.md`（当前活跃改为 `v5.0.0`，章节标题「alpha.8 之后」改为「v5.0.0 之后」）、本文件头。
- `docs/updatelog.md` 的「尚未发布（开发中）」段落冻结为 `## v5.0.0（2026-09-18）` 发布章节，按不兼容变更 / 新功能 / 体验 / 性能 / 修复 / Docker 部署分节。经 PowerShell 复刻 `release.yml:197` 的 awk 提取逻辑验证：`v5.0.0` 提取到该章节且不含相邻的 `v5.0.0-alpha.10` 正文，`v5.0.0-alpha.10` 仍提取 19 行不变（前缀边界判定正确）。
- Python Worker 版本独立固定为 `1.0.0`，不随本次提升变动。

### 文档修正

- **`README.md` 使用说明的「重定向模式」表述与实现不符（事实性错误）**：原文称 `trigger_url`「非空即重定向模式」，漏掉「`auth_url` 与 `trigger_url` 均为空时按重定向登录」这条主路径（`5d6f962` 引入的默认行为），按字面理解会让新用户误以为留空不是重定向。改为按 `ProfileSnapshot::uses_redirect_login`（`src/config/runtime.rs:86-89`）的实际口径描述：填网址即直接使用、留空即跟随重定向，`trigger_url` 非空时兼容旧版显式重定向语义并以触发地址优先、须为明文 `http`。
- **`README.md` 新增「登录方式」说明**：补上 `browser` / `http` 二选一与直连请求的指引；「特性」列表补入正式版主推能力——**直连请求登录**（免 Python / 免浏览器）、**方案导入导出**（凭据不随包导出）、**运行模式预设**，此前列表停留在 alpha.10 时期口径。
- **`docs/updatelog.md` 顶部说明保留原样**：仍以「不迁移或改写此前的历史记录」为口径，发布章节与历史章节并排。

### 文案修正（录制器与教程指引）

- **「任务录制器」的描述高估了能力（5 处）**：录制器实际不产出任务，而是把点选的元素整理成一段 **AI 提示词**，须复制给大模型生成任务 JSON（`resources/tools/task-recorder.user.js:1365-1367` 与 `:2759` 自身即为此口径）。以下位置原称「自动生成任务」，会让用户以为点完即可得到任务：
  - `frontend/src/views/tasks/BrowserTasksPanel.vue`：「想自动生成步骤？可用 任务录制器 在登录页点选元素自动生成任务」→ 补上「复制 AI 提示词发给大模型」这一步。
  - `frontend/src/views/settings/TaskEnvironmentSettings.vue`（录制器卡片的产品说明）：原文「自动生成任务步骤」同上改正。
  - `resources/tools/task-recorder.user.js` 的 `@description`（Tampermonkey 安装页可见）：原「自动生成任务 JSON 或结构化文档」，改为「整理成 AI 提示词，交给大模型生成任务 JSON」——且「结构化文档」这个出口并不存在。
  - 同文件面板副标题「选取元素，生成任务配置」→「选取元素，生成 AI 提示词」。
  - 同文件帮助说明的步骤类型表：三处「导出为 X」改为「整理为 X」（录制器不导出任务）；并补回运营商行丢失的 `📶` 图标（原文 `<td> 运营商</td>`）。
- **`docs/guides/task-manual.md` 与 `docs/guides/user-guide.md` 的录制器流程漏了 AI 环节**：原第 5 步写「结束录制后将生成的步骤保存为任务」，直接跳过了「复制 AI 提示词 → 交给大模型 → 得到 JSON → 导入」这一必经链路。两处均补为完整步骤，并注明也可用内置的「AI 生成浏览器任务」页完成生成。
- **`AiTaskView.vue` 的排障指引指向了不存在的内容（两处）**：① 原文说「按照**视频教程**操作」，但全仓没有任何视频地址（`git grep 视频教程` 零命中），用户照做必然找不到；② 链接写 `设置 → 任务`，与实际 Tab 名「任务与环境」不符。现改为指向真实的使用教程视频与正确页面。
- **`frontend/src/views/settings/TaskEnvironmentSettings.vue` 按钮文案与同页说明自相矛盾**：按钮原写「导出编写指南」，而下方说明写「点击即下载」——`77269e6` 已把 `/api/docs/*` 三端点统一为 `attachment` 下载语义，按钮改为「下载编写指南」。
- **`docs/guides/user-guide.md` 残留旧术语**：`:139` 的「自动执行活跃任务」改为「执行当前方案绑定的浏览器任务」（`bfe7f52` 起启用任务已按方案绑定，`active_task` 语义即当前方案绑定，任务页已无全局启用态）。

### 新增（教程与作者入口）

- **「关于」页新增 B 站主页入口**：`https://space.bilibili.com/5608024`，链接与使用教程视频集中在 `frontend/src/utils/constants.ts` 的 `BILIBILI_SPACE_URL` / `TUTORIAL_VIDEO_URL` 单一事实源，避免多处各写一份地址而漂移。
- **使用教程视频入口放到任务页**：任务页「JSON 配置说明」的帮助条新增「使用教程」按钮（与「安装录制器」并列，`BrowserTasksPanel.vue`）——用户正是在这里装录制器、需要看怎么用。另在「设置 · 任务与环境」的录制器卡片、AI 生成页的排障提示各有一处入口。
- **视频链接去掉了分享追踪参数**：原分享链接含 `share_source=copy_web&vd_source=…`，只保留 `t=209`（03:29 起播，即录制器演示片段）。
- `IconApp` 注册表新增 `bilibili`（电视机身 + 天线 + 双眼，示意品牌轮廓，用于 B 站入口着色 `#fb7299`）与 `external-link`（通用外链图标，供后续入口复用）。
- `frontend/src/styles/pages/about.css`：`.about-links` 补 `flex-wrap: wrap`（链接由 3 个增至 4 个，窄屏不换行会溢出），新增 `.bilibili-link` 的图标着色。

### 主题色默认改为黑白单色

**背景**：用户要求「主题与配色」的主题色改为「日间黑、夜间白」。原默认是青色（深色 `#22d3ee` / 浅色 `#0891b2`）。

- **默认主题色改为单色哨兵 `MONO_ACCENT`（`"mono"`）**（`frontend/src/utils/constants.ts`）：主题色只存一个 hex，而 `theme` 可为 light/dark/auto，**纯 hex 无法同时表达「日间黑、夜间白」**（`auto` 下还要随系统实时切）。故新增哨兵值，由 `useAppearance::resolveAccentColor(isLight)` 在所有消费点解析为 `#000000` / `#ffffff`。哨兵不是合法 CSS 颜色，任何直接当色值用的位置都必须先解析。
- **`applyAppearance` 收口解析**（`frontend/src/composables/useAppearance.ts`）：先解析哨兵再做全部派生；`--on-accent` 仍走既有的 `pickOnColor`，单色下自然得到「黑底白字 / 白底黑字」，无需特判。
- **单色下悬停色单独取值**：`adjustColor` 会把通道钳制到 0..255，纯黑再 `-20` 仍是纯黑，主按钮悬停将失去颜色反馈；故单色按主题**反向**调亮度（浅色 `+31` → `#1f1f1f`、深色 `-25` → `#e6e6e6`），非单色维持原 `-20`。
- **派生色跟随，但发光与描边中性化**（用户口径）：单色下 `--shadow-accent` 改中性（浅色 `rgba(0,0,0,.15)`、深色 `rgba(255,255,255,.12)`），`--border-accent` / `-hover` / `-strong` 同样按 `border_intensity` 取中性灰；**非单色必须显式 `removeProperty` 清除内联值**，否则内联样式（优先级高于样式表）会残留，用户切回彩色主题色时描边不恢复青蓝。
- **`base.css` 的静态默认同步**：根（深色）与 `[data-theme="light"]` 两处的 `--accent` / `--accent-hover` / `--accent-rgb` / `--border-accent*` 全部改为黑白中性值。这些是**首屏兜底**（`theme-init.js` 只设 `data-theme`，其余交给本文件），不同步会导致首帧闪一下青色后被 JS 覆盖。
- **色板新增「黑白（日间黑 / 夜间白）」项**（`ACCENT_COLORS` 首位）：色块背景经 `swatchBackground` 解析为当前主题下的实际色（浅色显示黑、深色显示白），即「选了它现在会得到什么」。未用「左黑右白对半块」：勾选图标必然落在同色那一半而看不清。
- **修复色块勾选图标不可见（既有缺陷，4 个色块）**：勾选原继承文字色，深色主题下 `--text-primary` 是白色，叠在纯白色块（单色的夜间态、背景色的「纯白」`#f8fafc`）上完全看不见。现按色块实际底色经 `pickOnColor` 取对比色。
- **修复开关开态旋钮可能隐形**：旋钮用「永远白色」的 `--text-on-accent`，叠在 accent 轨道上；单色深色下轨道是纯白 → 白钮白轨。新增 `--toggle-knob-active`（**仅单色时注入**，避免改变其余强调色观感），label 型与按钮型两处旋钮同步引用。
- **修复原生 select 聚焦箭头残留青色**（`frontend/src/styles/components/form.css`）：两处内联 SVG 的 `stroke` 写死了旧强调色（深 `%2322d3ee` / 浅 `%230891b2`），改为白 / 黑。data URL 无法引用 CSS 变量，故用户改用彩色主题色时箭头仍是黑白（仅聚焦提示，可接受；已写入注释）。
- **`resolvedAccent` 用 ref 暴露**：模板若直接调读 `matchMedia` 的 `getEffectiveTheme()` 只能拿到求值当时的快照，`theme=auto` 下系统切换深浅色不会触发重渲染，色块与色值文字会停在旧值。改由 `applyAppearance` 统一写入 ref（`useUi.ts:373` 初始化时即调用），模板读它自动订阅。
- **`resetCard('theme')` 与 `cardDirty('theme')` 无需改动**：两者都按字段值与 `DEFAULT_APPEARANCE` 比较，`accent_color` 默认值换成哨兵后自动生效（选过彩色的用户点「恢复默认」会正确回到黑白）。
- **既有用户不受影响**：`loadStored` 浅合并只在**键缺失**时用默认值，已存 `#22d3ee` 等值的用户保持其原选择；仅新装与「恢复默认」后为黑白。

### 验证

- `npm run typecheck`（`vue-tsc -p tsconfig.app.json --noEmit`）零错误；`npm test` **212 passed / 23 files** 全绿。
- 主题色解析逻辑经 node 实跑核对（`vite-node` 临时脚本，跑完即删）：默认（mono）+ 浅色 → `accent=#000000 hover=#1f1f1f on-accent=#ffffff`；默认（mono）+ 深色 → `accent=#ffffff hover=#e6e6e6 on-accent=#0f172a`；青色（既有选择）→ `accent=#22d3ee`，派生不受影响。
- **未做浏览器实测**：`agent-browser` 在本机起不来（多次 `open` 超时、无 chrome 进程），故主题色的实际渲染（色块、开关旋钮、描边/发光观感）**未经真实浏览器确认**，仅由上条逻辑核对与 `base.css` 静态值一致覆盖到「变量取值正确」。建议发版前人工目视过一遍浅色/深色两态的外观页。
- `cargo build --release`（含前端嵌入）通过，产物内已含前端资源；`cargo fmt --check` 通过、`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告、`cargo test --features no-embed --lib` **908 passed / 0 failed / 1 ignored**。
- 发布说明提取口径复验：PowerShell 复刻 `release.yml:197` 的 awk 逻辑，`v5.0.0` 提取 94 行且不含 `v5.0.0-alpha.10` 正文，`v5.0.0-alpha.10` 仍为 19 行。
- **版本提升暴露的测试耦合（已修）**：`tests/updater_channels.rs` 的 mock 远程版本写死 `5.0.0`，而 `check_update` 只在 `remote > current` 时返回 `Some`——当前版本提升到 `5.0.0` 后该比较恒为假，2 个用例在三平台稳定失败（CI run `35349374028`，本地同样复现）。改为由 `env!("CARGO_PKG_VERSION")` 派生各远程 tag（patch+1 / minor+1 的 beta / 更旧 alpha / major+4），断言与注释同步去具体号，避免下次提升再次失效。变异验证：去掉 `patch+1` → 正是这 2 例失败。`cargo test --tests --features no-embed` 全绿（908 lib + 各集成 crate）。

### 说明

- 版本提升本身**未改动任何功能代码、IPC 契约、配置 schema 与 `openapi.json` 路径表**；其后的「文案修正」「新增（教程与作者入口）」「主题色默认改为黑白单色」三节含前端文案、样式、图标注册表与外观默认值的改动，均为用户可见变化，已同步写入 `docs/updatelog.md` 的 `v5.0.0` 章节。
- 注意：主题色默认值改动使 `DEFAULT_APPEARANCE.accent_color` 由 `#22d3ee` 变为哨兵 `mono`，属**外观默认值变更**，但不影响已存 localStorage 的用户（浅合并且仅补缺失键）。
- 已知问题（`docs/known-issues.md`，含 E2 定时任务手动运行的 toast 语义、E3 任务卡「上次」结果不即时刷新）**有意不写入发布说明**，仅在已知问题清单中保留。
- 未在本轮处理（记此备查）：`docs/changelog.md` 中 80 余个历史条目标题仍带「开发中（日期 …）」前缀，属已合入条目，保留以维持按日期倒序的可追溯性，不做批量改写。

## 开发中（2026-09-23 直连渠道新增「前置请求」：令牌绑连接的门户可直连 + 河南科技大学直连任务条目）

### 背景

- 用户给出一个实测有效的登录脚本（`heragehome/haust-auto-login`，河南科技大学裕达 / 大学掌体系，认证服务器 `10.100.51.1`）：先 `GET /api/csrf-token` 取令牌、再 `POST /api/account/login` 提交账号密码，并问能否收进默认任务仓库、在条目里标出原仓库地址。
- 这套协议的硬约束写在原脚本头部：**CSRF 令牌绑定 TCP 连接**——「取 token 与登录必须走同一条 keep-alive 连接，否则服务器返回 HTTP 400 `{"error":"CSRF token mismatch"}`」。而直连管线当时有两个拦路点：一次登录**只发一个请求**，且**抓登录页与发登录各自 `build_client` 一个 `reqwest::Client`**（连接池挂在 Client 上、不共享）——所以是能力缺失，不是配置能绕过的。
- 用户三选一确认路线：**改代码补「先取 token + 同一 client」能力**；提交方式：我开分支提 PR、他合并。
- 顺带发现一个真缺陷：`{local_ip}` / `{local_mac}` 此前只在「配了加密脚本」时才查询网卡，于是「模板里写了 `{local_ip}` 但没配脚本」的任务会**静默发出空 IP**（把本机 IP 当必填参数的门户会请求照发、门户照拒，用户看不出是哪个环节空了）。
- **过程中的一次越界与撤回（记此备查）**：用户提到"两个脚本"时只给了一个链接，我据此外推到另一个**私有**仓库里找第二套协议，并把由它反推出来的参数做成了仓库条目写进 PR。用户指出未经许可翻私有仓库后，该条目（连同索引条目与 README 行）已从分支与 PR 中撤下，分支 force-push、PR 说明改写，只保留来源可公开引用的这一条。教训：**用户没给的来源不要自己去找**，缺材料先问。

### 实现

- **数据结构（`src/tasks/models.rs`）**：新增 `HttpPreRequest`（`method` / `url` / `headers` / `body` / `extract` / `name`）与 `HttpTaskConfig.pre_request: Option<HttpPreRequest>`：`skip_serializing_if` 保证未配置时不往老任务文件里塞 `"pre_request": null`，老文件缺键解析为 `None`。形状判据（`validate` / `extract_path` / `placeholder_name`）放在 tasks 层：保存路径只拿到 JSON 值、执行路径在 login 层，两者必须同源（与 `auth_url` 的分工一致），否则会出现「存得下、登不上」的错位。
- **执行管线（`src/login/http_login.rs`）**：`build_client` 去掉 `timeout` 参数，超时改由每个请求的 `RequestBuilder::timeout` 单独给（抓登录页 5s / 前置请求 10s / 登录 20s）——**整次尝试只建一个 Client**（登录页抓取、前置请求、登录请求三处共用），连接池因此会复用同一条 keep-alive 连接；`fetch_login_page` / `send_http` 改为收 `&Client`；`run_once` 新增第 1.5 步「前置请求」（渲染模板 → 发送 → 按 `json:点号路径` 取值 → 注册成占位符 → 再渲染登录请求）。失败即终态，且用新抽出的 `abort_report` 统一组装报告——报告里带的是**卡住那一步**的请求与响应（前置请求失败时展示前置请求的地址与响应，而不是一片空白）。取到的值插入 vars 后自动进脱敏字典（`collect_secrets` 覆盖非内置键），令牌在日志、历史与结果面板里都是 `***`。
- **取值与校验**：取值只支持 `json:<字段>[.<字段>...]`，容忍 JSONP 包裹（`dr1003({...})`）与前后脏字符；新增 `validate_pre_request`（形状委托给 `HttpPreRequest::validate`，另加执行侧体积上限）与 `loader.rs::validate_task` 的 http 分支校验（保存/导入闸门）；新增 `MAX_HTTP_URL_BYTES` 并给主请求 URL 也补上体积上限（此前只有登录侧校验）。
- **修掉 `{local_ip}` 静默发空值**：新增 `HttpLoginRequest::needs_local_address()`——脚本要读 `ctx.local_ip`/`ctx.local_mac`，**或** url/headers/body/前置请求里写了 `{local_ip}`/`{local_mac}` 都要查一次网卡；登录路径（`src/login/mod.rs`）与测试端点（`src/web/routes/http_tasks.rs`）都改用它。
- **前端**：`api/types.ts` 新增 `HttpPreRequest` 与 `HttpTaskConfig.pre_request`；`utils/httpTask.ts` 草稿新增 6 个平铺字段，**地址留空即不需要前置请求**（不另设开关：开关与字段两份状态会各自漂移），载荷在地址为空时发 `null`，`httpTaskDraftGaps` 覆盖三种缺口（填了地址没填取值方式 / 取值方式前缀不是 `json:` / 只填了取值方式没填地址）；`HttpTaskFields.vue` 新增第 5 组「前置请求（可选）」（沿用第 4 组的高级折叠区样式，已配置则保持展开并显示「已配置」徽标，`?` 气泡讲清"令牌绑连接"与取值方式）。
- **任务仓库条目（`Misyra/campus-auth-tasks`，PR 交付）**：新增 `tasks/haust.json`（`type: "http"`，CSRF POST + 前置请求；带一个做 URL 编码的小脚本，因为模板替换不转义、密码含 `&`/`=` 会截断表单）；两份索引补 `type: "http"`（缺了就不会出现在「直连任务」子页的导入列表里）与 `source`（原仓库地址）；README 增补该任务说明与 `type` / `source` 字段。
- **前端「来源仓库」渲染**：`utils/repoSource.ts`（`repoSourceUrl` 只放行 http(s)、`repoSourceLabel` 去掉协议与 `www.`）+ `RepoTask.source` + `RepoImportModals.vue` 详情区渲染成 `rel="noopener"` 的外链。索引是远端数据，直接绑 `href` 等于让仓库维护者能往用户的点击路径上放 `javascript:`，故必须过白名单。

### 验证

- `cargo fmt` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed` 全绿（lib **987 passed / 0 failed / 1 ignored**，各集成测试套件全绿）。
- 新增单测（`src/login/http_login.rs`）：`pre_request_shares_connection_with_login_request`——起一个「令牌按**来源端口**记账、换连接即回 400」的 mock 门户，断言两次请求的来源端口相同、且令牌在报告里是 `***`；`csrf_portal_rejects_login_without_token` 作对照组（不带前置请求必然被拒，证明上一条不是白过）；`pre_request_missing_field_is_terminal_and_reports_its_own_exchange`、`pre_request_validation_rejects_empty_url_and_unknown_extract_prefix`、`pre_request_extract_tolerates_jsonp_and_dirty_body`、`needs_local_address_covers_script_and_placeholders`。`src/tasks/models.rs`：`legacy_http_task_without_pre_request_parses_as_none`（老文件兼容 + 不落 null）、`pre_request_extract_path_and_placeholder_name`、`pre_request_validate_checks_url_shape`，并让 http 往返用例覆盖新字段。前端新增 `utils/repoSource.test.ts`（含"不把索引原串直接绑 href"的源码断言）与 `httpTask.test.ts` 的前置请求映射/缺口 8 例。
- **端到端（真实二进制 + 真实前端 + 本地 mock；任务不落盘、不碰用户配置，走 `POST /api/http-tasks/test` 内联任务）**：
  - haust 条目 → 自写的 CSRF mock（令牌按来源端口记账）：`outcome=success`；mock 日志 `GET /api/csrf-token conn=3817` 与 `POST /api/account/login conn=3817` **同一条连接**，表单里密码 `p@ss&word=1` 被编码为 `p%40ss%26word%3D1`，`userIpv4=192.168.123.210`、`userMac=bc:38:98:39:ce:34` 均已填入（同时验证了 `{local_ip}` 修复与 URL 编码脚本）。
  - 该条目过了主程序的保存校验（`PUT /api/tasks/haust` 返回 200），验证后已从本地删除，未留残留。
  - 前端 `npm run typecheck` 零错误、`npm test` **31 文件 / 308 例**全绿、`npm run build` 通过；Playwright 实机（1600px）：编辑器第 5 组字段齐、地址填好后出现「已配置」徽标、GET 时不渲染「请求内容」、零 `pageerror`；截图 `docs/reports/ui-check/106-pre-request.png`（本地，不提交）。
- 文档：`docs/guides/http-login-guide.md` 新增 §3.5「前置请求」（机制、字段表、五条要点）、第 3 节改称五组、§9 边界条目改写（"两步式门户请用浏览器"→ 可用前置请求表达）、体积上限补前置请求；`task-manual.md` 与 `user-guide.md` 字段表同步；`docs/updatelog.md` 的「尚未发布」新增前置请求一条并改写原说明；`docs/plan-next.md` 同步模型字段、对外依赖与本轮遗留。
- **观察（非本轮引入）**：脚本产出的**短值**也会进脱敏字典，两位十六进制这类产出会把响应里同名的子串一并替换（实测某个两位十六进制产出把响应里的 `"192.168.123.210"` 回显成 `***2.168.123.210`）。不影响判定与登录，只是回显可读性差一点；若要收紧可给脱敏字典加最短长度阈值，本轮未动以免改变既有脱敏口径。

## 开发中（2026-09-23 直连任务页空态换脸：引导卡替掉「空列表列 + 散文说明卡」）

### 背景

- 用户实测截图（窗口约 1600px 宽）判「这一页有点丑」。实测诊断四件事：①左列列表卡 296px 的空态里塞着「共 0 个」+ 搜索框（没有可搜的东西）+ 两个按钮，而这两个按钮页头已有、右列说明卡里还有第三个「从仓库导入」——同一个动作全页出现三次；②右列说明卡 984px 且 `display: block` 单列：5 段正文各 942px、13px 灰字、无小标题与段间距，约 70 字/行，读成一坨；引导条内文字块 785px 后按钮被甩到 628px 外；③整页内容只有 498px 高，下方 600px 全空——真功能缩在 296px，一块「说明书」当了主角，主次颠倒；④**上一轮加的宽屏两列（`@container (min-width: 1000px)`）在这个窗口根本没触发**：卡片 984px、阈值 1000px，只差 16px，等于该优化对本人不可见。
- 同轮把 14 个路由在 2280px 下全量量过一遍（每页正文宽、顶层区块右缘、文本行墨迹宽、控件宽度分布）：内容全部铺满（正文 1976px、右缘 2248）、零横向溢出，**右侧留白问题已不存在**；仍存的是密度与口径问题（设置页页签 6×320px、AI 步骤条连接线各 819px、同类单行控件宽度 240–1940px、关于页 960px 居中）。这些与「关于」页本轮按用户指示未动，仅改直连任务页。

### 实现

- **空态换脸（`views/tasks/HttpTasksPanel.vue`）**：一条任务都没有时不再摆两栏，整页一张居中引导卡 `.http-guide-card`（`width: 100%` + `max-width: 900px; margin: 0 auto`）：标题「还没有直连任务」+ 一句定位 + 三步（①先拿到一条任务 ②用配置向导填「请求的形状」③回「方案」绑定）+ 两个入口（新建 / 从仓库导入）+ 卡尾 `<details>`「字段与函数速查」（默认收起）。**居中不算「右边空一块」**：两侧对称，且页头操作行仍满宽；铺满 1976px 会把中文行拉到百余字。
- **判据必须带 `!httpTaskDraft`**：只按条数判断时，点「新建直连任务」时还没有任何已保存任务，引导卡会把刚打开的编辑器顶掉（新建第一步就卡死）。同口径补齐左列：有草稿但 0 条已保存任务时不再渲染搜索框（`v-if="httpTasks.length"`）与「共 N 个」，改「暂无已保存任务」+「这条任务还没保存，保存后它会出现在这里」，避免读成「没有匹配「」的任务」。
- **右列散文说明退场**：删除 `.http-help-card` 整块（含 `.http-help-prose` 5 段、`.http-help-glossary`、跨列 `help-tip`、容器查询块），改为「字段速查卡」`.http-ref-card`——一行指路（限宽 620px）+ 两组 chips。信息不丢：绑定关系与归属本就在页头 `?` 气泡（`NOTICE_HELP` 未改）、「怎么开始」在引导卡三步、「字段细节」在编辑器内各字段的 `?`、「分享适配」的仓库说明在页头链接的 `title` 里。
- **CSS**：删除 `.http-list-empty*`、`.http-help-guide`、`.http-help-card`/`@container` 块、`.http-help-glossary h4:first-child`；新增 `.http-guide-*` 与 `.http-ref-lead`。`.http-chip*` 保留（编辑器与速查卡共用同一套词条样式）。

### 验证

- `npm run typecheck` 零错误；`npm test` **30 文件 / 295 例**全绿；`npm run build` 通过（仅前端，debug 二进制运行时读盘，无需重编 Rust）。
- Playwright 实机（2280 / 1600 / 1200 三档，`ca-guide-check.py`）：引导卡 900×449 且**卡片中心 = 正文列中心**、三步齐、两个入口齐、`.http-tasks-split` 与搜索框均不渲染、无横向滚动、`pageerror` 零条。
- 交互实测：展开折叠 900×623（**开合不再变宽**——`width: 100%` 修掉了 flex 列里 `margin: auto` 优先于 stretch、宽度被内容撑成 790→900 的跳动）；点「新建直连任务」→ 引导卡让位、编辑器出现、左列显示「暂无已保存任务」+ 保存提示；关闭编辑器回到引导卡。
- 有任务态用 route mock（`GET /api/tasks` 注入两条 `type: http` 条目，`ca-ref-check.py`）验证：两栏恢复（列表 296px、搜索框在）、右列「字段速查」984×225 / 14 个 chip、无溢出、零 `pageerror`。
- 截图 `docs/reports/ui-check/102-guide-empty.png`、`103-guide-fold-open.png`、`104-guide-after-new.png`、`105-ref-card.png`（本地，不提交）。

## 开发中（2026-09-23 任务页二级导航收进左侧主侧栏：「任务」分组展开一层子项）

### 背景

- 上一条「任务页版式重做」把五个面板的切换做成页内竖排二级导航卡后，用户实测仍判「太丑」，并给出方向：**在左边导航栏里再加一级子导航栏**（附一张「父项 ▾ + 缩进子项」的侧栏树截图）。
- 根因是那张卡本身：2280px 视口下它离左边缘 400px 有余、四周全是留白，像一张被丢在页面中间的卡片；页面整体（导航 + 正文）又被居中限宽，导致正文与顶栏页标题（贴左）不在同一列上，读成两列。卡片是被"搬到页面里"的导航，导航本该长在它归属的一级项旁边。

### 实现

- **侧栏二级导航（`components/common/AppSidebar.vue` + `styles/components/sidebar.css`）**：「任务」一级项现为一行 `nav-group`——**一级项 `button` 自身就是展开 / 收起开关**（整行可点，`aria-expanded` + `aria-controls` 挂在它上面），行尾 caret 是它的状态指示器（行内 `margin-left: auto` 的 `IconApp`，`flex-shrink: 0`，展开态旋转 -90°，不参与交互）。子项无图标、字号与内边距各降一档，左侧一根 1px 细轨表达层级；**展开且有子项激活时父行让出高亮**（`.nav-group--open .nav-item--group.active` 清掉渐变/描边/发光，只留文字提亮），否则"父行发光 + 子行浅底"两处同时高亮会看不出当前在哪一页。**展开态刻意不落 localStorage**：侧栏是本应用唯一通往这五个页面的入口，一次忘记的折叠若被持久化，冷启动后整个任务区会被收进一个 caret 后面（发现性陷阱）；组件随 App 常驻，会话内记忆已足够。从别的页面点进任务区自动展开，组内切换保持手动折叠态（初版的"自动展开"已在下方二轮返工中撤销）。
  - **一轮返工（用户实测反馈「只能点箭头展开，点主体点不开」）**：初版是"点主体 = 跳 `/tasks`（浏览器任务）+ 展开、点 caret = 展开/收起"的复合设计，后果是"想展开子项却得瞄准那个小箭头，点了主体页面反而跳走"。改为 folder 语义（对齐用户给的侧栏树参考）：**整行只负责开合、不导航，页面切换只由子项承担**；代价是从别处进入任务区需两次点击（先展开、再点子项），换来的是"点哪儿都开"与零意外跳页。`/tasks` 的 redirect、深链、`startsWith("tasks")` 高亮均不受影响。
  - **二轮返工（用户要求「默认不要展开任务页」）**：初版 `tasksExpanded = ref(true)`，且「从别的页面点进任务区自动展开」——结果是每次冷启动、每次进任务区，侧栏都被撑开五条子项。现改为 `ref(false)` 并**去掉按路由自动展开**：展开与否完全由用户点「任务」这一行决定，展开态仍在会话内记忆（组内切页不会自己收回去）。默认收起时父行保留激活高亮（`.nav-group--open` 才让出），所以"人在任务区、侧栏是收的"也能一眼看出当前位置。
- **导航数据单点（`utils/navTree.ts`，新增）**：`TASK_NAV_CHILDREN`（原 `TasksView.vue` 的 `TABS` 迁移至此）+ 纯函数 `activeChildId(children, routeName)`（按 `route.name` 精确匹配，返回 `null` 表示本组无激活项）+ `IconName` 类型来自 `IconApp`（图标改名时编译期即报错）。消费方两个：侧栏（宽屏展开渲染）与任务页（窄屏兜底），避免两处硬编码同一份路由表后各自漂移。**路由名/路径零改动**，深链、`editorGuard` 的 `/tasks` 前缀判定、侧栏 `startsWith("tasks")` 高亮均不受影响。
- **任务页只剩正文（`views/TasksView.vue` + `styles/pages/tasks.css`）**：删除页内导航卡与 `.tasks-content` 包裹层；`.tasks-page` 由「flex + 1360px 居中」改为**左对齐 + 1440px 上限**（该上限已在下面「铺满宽度」一条中撤销）——左对齐是为了与顶栏页标题同一起始列（居中会让标题贴左、正文居中），1440px 是"再宽就把地址/请求体输入框拉成 1500px 以上长条"的取值（直连任务两栏是 `296px 列表 + 编辑器`）。`.tasks-subnav*` 全部规则与 900px 降级块删除。
- **窄屏兜底（`TasksView.vue` + `styles/pages/tasks.css` + `styles/responsive.css`）**：≤768px 时主侧栏只剩 64px 图标、子项文字放不下，故侧栏的 `.nav-children` 与 `.nav-caret`（指示器没有指示对象了）整块隐藏，任务页内改由**同数据的横向 pill 行**（`.tasks-narrow-nav`，`display: none` → 768px 以下 `flex`）兜底，否则窄屏只能靠手输地址到达其余四个面板。
- **铺满宽度（一轮返工：用户实测「为啥右边空一块」）**：上一版给 `.tasks-page` 设了 1440px 上限（理由：直连任务表单的输入框不该拉到 1600px），代价是 2280px 视口右侧白扔 500px 以上，与本站其余页面（都不限宽）也不一致。现改为 `max-width: none`，正文铺满 `.content-wrapper`（2280px 视口下 1976px，左右各 32px 内边距对称）。**表单里的单行输入框确实变宽了**（直连编辑器：请求地址 1393px、认证地址与请求头 1608px），但换来的是零空白；真要收窄只能给控件单独加 `max-width`，那会在卡片里留出新的空白带，反而更像"没铺满"。
- **说明卡宽屏两列（`views/tasks/HttpTasksPanel.vue`）**：直连任务无草稿时，说明卡落在 `1fr` 整列上（列表列只有 296px），2280px 下宽 1622px——单列会让中文行宽拉到 1400px（一行百余字）、右半张卡全空。现按**卡片自身宽度**用容器查询（`.http-help-card { container-type: inline-size }` + `@container (min-width: 1000px)`）在 ≥1000px 时拆成「导语 1.35fr + 词表 1fr」两列，引导条横跨两列；窄屏仍竖排。**刻意不用视口断点**：卡片宽 = 视口 − 侧栏 240 − 页面内边距 64 − 列表列 296 − 卡片内边距 40，1440px 视口下只剩 782px（两列会把词表挤到 319px、chip 逐行成柱状），2280px 下才是 1622px，视口断点表达不了这层关系。`@container` 不被支持时整块规则被忽略，回落单列（渐进增强，与全站 `:has()` / `color-mix()` 同口径）。
- **文档同步**：`docs/guides/` 四处（`task-manual.md` 的「五个标签页」与「各 Tab 的导入」、`user-guide.md` 的「直连任务 Tab」「五个标签页」「定时任务标签页」、`custom-script-guide.md` 的「脚本标签页」、`http-login-guide.md` 三处「Tab」）改为「侧栏「任务 → X」子页」；`docs/plan-next.md` 同口径一处。
- **未同款收编**：「设置」的六个 Tab 仍留在设置页内的横向页签——它不产生"孤岛"，且窄屏下横排页签比侧栏展开更省纵向空间。

### 验证

- `npm run typecheck` 零错误；`npm test` **30 文件 / 295 例**全绿（新增 `utils/navTree.test.ts` 6 例：id/路由名唯一、`tasks-` 前缀、首项为 `tasks-browser`、`/tasks` 经 redirect 后命中浏览器任务、非本组返回 `null`）；`npm run build` 通过。
- 实机（真实二进制 + 真实前端 + Playwright，2280 / 700 两档）：`/tasks`、`/tasks/http`、`/tasks/scheduled` 三路由子项齐五条、激活项正确；侧栏右缘 240、正文左边 272、**页标题左边同为 272**（同列）；有子项激活时父行背景透明、`box-shadow: none`；旧页内导航零残留；700px 下侧栏子项与 caret 隐藏、页内 pill 行 `display: flex` 且「脚本」点亮。截图 `docs/reports/ui-check/96-nav-*.png`（本地，不提交）。
- 返工后按「整行开合」重新实测（`ca-group-click2.py`）：`/tasks/http` 内点标签 / 点行尾 caret 均只开合、**URL 不变**；`/profiles` 下点主体开合且仍停在 `/profiles`（不再被拽去浏览器任务）；点子项「定时任务」→ `/tasks/scheduled` 且激活项正确；焦点 + Enter 同样开合；`pageerror` 零条。
- 铺满宽度后实测（`ca-fill-check.py` / `ca-http-help-check.py` / `ca-overflow-scan.py`）：五面板（`/tasks`、`/tasks/http`、`/tasks/scripts`、`/tasks/scheduled`、`/tasks/ai`）在 2280px 下正文均 1976px、右侧余量 32px（与左内边距对称）；**五面板 × 五档宽度（2280 / 1600 / 1200 / 900 / 700）横向溢出全为 0**；说明卡容器查询按预期分档——卡片 1622px 时 `display: grid`（列 913 / 677），卡片 782px 与 622px 时回落 `display: block`（单列），即视口 2280 走两列、1440 与 1280 走单列。
- 验证脚本 `ca-nav-check.py` / `ca-group-click2.py` / `ca-fill-check.py` / `ca-http-help-check.py` / `ca-overflow-scan.py`（`%TEMP%`，本地）。控制台仅余既有的「字体加载失败 → 回退系统字体」（无网络环境下的预期回退，与本次改动无关）。

## 开发中（2026-09-23 任务页版式重做：竖排二级导航 + 直连任务改「列表 + 编辑器」两栏）

### 背景

- 直连任务页上线后用户实测反馈「太丑」，并列出具体观感问题：2280px 视口下正文拉满、空态是一整块大白框、列表与编辑器上下叠成一条长页、右上四个按钮平级拥挤、顶部还压着一整条蓝底提示横幅。
- 用户从三个候选版式（主从两栏 / 卡片网格 / 紧凑列表+抽屉）里选定 **A 主从两栏**，并同意叠加 **D 任务页改竖排二级导航**。设计稿留在 `docs/compose/http-task-ui-options.html`（本地，不提交）。

### 实现

- **任务页外壳（`views/TasksView.vue` + `styles/pages/tasks.css`）**：横向 `.tasks-tabs` 改为左侧竖排二级导航（190px，图标 + 短标签，AI 项用「AI 生成」、全称进 `title`）+ 右侧内容区；激活态是左竖条（`inset 2px 0 0 0 var(--accent)`）+ 8% 强调色浅底，刻意比主侧边栏轻，避免出现"两个一模一样的一级导航"。**路由名/路径零改动**（`tasks-browser` 仍是 `/tasks` 的空子路径），深链、`editorGuard` 的 `/tasks` 前缀判定、`AppSidebar` 高亮全不受影响。**正文限宽 1360px 居中**（导航与内容一起限宽，否则会出现"导航贴左、正文居中"的割裂）；≤900px 降级为横向可换行的 pill 行。`.tasks-notice` 三组规则随横幅退役一并删除，`responsive.css` 里针对 `.tasks-tabs` 的窄屏微调同样移除（附说明）。
- **直连任务面板（`views/tasks/HttpTasksPanel.vue`）**：改为 `296px 列表 + 编辑器` 两栏（≤1100px 堆叠）。左列 = 数量 + 搜索（按名称 / 任务 ID / 请求地址前端过滤）+ 行（名称、方法 chip、等宽地址摘要、**被哪些方案绑定**的 pills）；行尾 `⋯` 菜单承载复制 / 导出 / 删除，导入 / 仓库导入 / 分享适配 / ＋新建在标题行。空态只占左列（一句说明 + 新建 / 从仓库导入），右列照旧是帮助卡；"搜索无匹配"与"一个都没有"用不同文案。编辑器卡头部 = 名称 + 新建/已保存徽标 + 配置向导 / 发送测试请求 / 保存 / ✕（原卡片 footer 的取消-保存行随之移除，保存进标题行、关闭由 ✕ 承担，dirty 确认不变）。点"当前已打开的那一行"直接 no-op——否则脏草稿时会弹「放弃未保存的修改」，等于诱导误删。
- **列表行的信息面**：新增「被哪些方案绑定」（数据来自 `useProfiles().profiles`，`useUi.init` 已拉取，不新增请求）。方案列表尚未就绪时**整行不渲染绑定区**，而不是显示「未绑定」——空表与"没人用"是两回事，显示错了会让用户删掉正在被引用的任务。
- **后端摘要补两个展示字段**：`TaskSummary` 增加 `url`（浏览器=登录页、直连=请求地址、脚本=空）与 `http_method`（仅直连，非法/缺失为 `None`），由 `TaskKind::summary_url()` / `http_request_method()` 统一派生（列表与详情两条构造路径必须给出同一个答案）。列表读取本来就把整个 JSON 解析出来了，顺手取字段是免费的——**因此删掉了初版那套「每条任务再补一次详情请求」的 N+1 缓存**（`useHttpTasks` 的 `httpTaskRowMeta` 与三个辅助函数、面板的三参数调用）。`ProfileSummary` 同步补 `active_http_task`（绑定 pills 的数据源）。
- **浏览器任务 Tab 的归属提示**同样收进标题旁的 `?`（与直连 Tab 同口径：这是"首次配置读一次"的解释，不该常驻横幅把内容往下压）。
- 新增 `utils/httpTaskList.ts`（绑定索引 / 行摘要 / 搜索过滤三段纯函数）+ `utils/httpTaskList.test.ts` 17 例：前端无组件测试环境，派生逻辑放纯函数里才能被 vitest 直接盯住——绑定未就绪与"未绑定"的区分、缺字段不编造 GET、搜索跨字段不误命中，这三类都是会让用户看错事实的地方。

### 验证

- 前端：`npm run typecheck` 零报错、`npm run test` **29 文件 289 例全过**、`npm run build` 通过。
- Rust：`cargo fmt --all --check` 零差异、`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告、`cargo test --features no-embed` **1026 passed / 0 failed / 1 ignored**（lib 978 + 集成 48；新增 `test_list_summary_carries_url_and_http_method` 锁定"列表摘要自带方法与地址"这条契约，避免将来又把 N+1 补回来）。
- 实机（真实二进制 + 真实前端 + 真实配置，2280 / 1440 / 900 三档宽度）：页宽上限生效（2280 视口下正文 1146）、二级导航 190px 且 ≤900px 变横向 pill 行、任务页五个路由（`/tasks`、`/tasks/http`、`/tasks/scripts`、`/tasks/scheduled`、`/tasks/ai`）全部可进、激活项正确、控制台零错误；直连任务页在无任务时左列是紧凑空态 + 右列帮助卡，浏览器任务页在同样外壳下 8 条真实任务正常渲染。截图见 `docs/reports/ui-check/9x-*.png`（本地，不提交）。
- 过程中修掉的两处自身缺陷：旧版把「详情 N+1 补齐」当临时方案留着（后端摘要补齐字段后已删除）；`responsive.css` 与 `tasks.css` 里针对已退役横幅/Tab 的死规则。

## 开发中（2026-09-22 直连请求重构为独立「直连任务」：三类任务模型 / 配置迁移 v10 / 登录链路 / Web API）

### 背景

- 用户诉求两项：①「直连请求」的参数编辑从方案编辑器里挪到任务页、成为独立对象；② 直连任务能进仓库分享（和浏览器任务一样）。
- 此前直连参数是**方案的内联字段**（`ProfileData.http_*`），三个后果：同一门户的多个账号要各填一遍同一份请求配置；方案分享被迫连直连配置一起带（含会被执行的凭据变换脚本）；任务仓库只服务浏览器任务，直连配置无处可分享。
- 已定方向（用户三选一确认）：**独立具名任务、方案里选一个**；仓库里**只分享直连任务**（不含账号密码，它们本就属方案）；**复用现有任务仓库**（同一索引、多一类条目，老条目 `type` 缺省视为 browser）。

### 实现

#### 一、存储层：任务体系新增第三类（`type: "http"`）

- `src/tasks/models.rs`：新增 `HttpRequestMethod`（`GET`/`POST`，`UPPERCASE`）与 `HttpTaskConfig`（`common` 扁平嵌 `task_id`/`name`/`description` + `method`/`url`/`auth_url`/`headers`/`body`/`success_pattern`/`failure_pattern`/`crypto_script`/`ignore_https_errors`/`metadata`），`TaskKind::Http` 第三臂（`common()`/`common_mut()`/`type_name()` 与手写 `Deserialize` 同步，未知类型报错文案列出三类）。上限常量：`MAX_HTTP_SCRIPT_BYTES` 128 KiB（与 `login/http_login.rs` 的 `MAX_SCRIPT_BYTES` 同口径）、`MAX_HTTP_HEADERS_BYTES`/`MAX_HTTP_BODY_BYTES` 256 KiB。
- `src/utils/paths.rs`：新增 `tasks/http/` 目录并纳入 `ensure_runtime_dirs` 预建清单（目录布局单一事实源）。
- `src/tasks/loader.rs`：`TaskManager` 增 `http_dir`，把「按类型选桶 / 清其他桶残留 / 删 / 查 / 列表扫描」收敛成 `bucket_dir()`/`buckets()`/`other_bucket_paths()`/`scan_json_bucket()`（顺带删掉两段重复的目录遍历与私有 `read_summary()`）；`validate_task` 的 http 分支校验 `url` 非空、`auth_url`（可空，非空须 http/https 且带主机名）、脚本/请求头/请求体体积；`ensure_default_task` 仍只播浏览器默认任务。
- **保留 ID 规则（新增）**：三类任务共用同一个 `task_id` 命名空间，而 `default` 是浏览器渠道未绑定方案时的兜底任务——非浏览器任务占用它会导致保存时的残留清理删掉种子文件（浏览器渠道随即「当前无可用浏览器任务」）或两份定义按桶优先级互相遮蔽。故 `validate_task`（JSON 口径）与 `save_task`（入参口径，`PUT /api/tasks/{id}` 的 body 未必带正确 `task_id`）都拒绝非浏览器类型使用 `default`，文案指向「请换一个 ID」。
- `src/tasks/executor.rs`：两处穷尽匹配补占位臂——直连任务不经 Python Worker，`/api/tasks/{id}/execute` 与定时任务 task_id 对它是 `ValidationFailed`（400）；它的验证入口是「发送测试请求」。

#### 二、配置：绑定字段 + 迁移 v9 → v10

- `ProfileData` / `ProfileSnapshot`：删除全部 `http_*`，新增 `active_http_task: String`（空 = 未绑定）。`CURRENT_CONFIG_VERSION` 9 → 10；`crate::config::HttpLoginMethod` 删除（枚举唯一来源改为 `tasks::HttpRequestMethod`）。
- `src/config/migration.rs::migrate_v9_to_v10`（跨文件迁移）：
  - 判定「这份方案配过直连」按**取值而非键存在**——`ProfileData` 带 `#[serde(default)]`，任何方案文件都会写出全部 `http_*` 键（值是默认值）；口径是「任一项非空即算配过」，因为配到一半（先写好脚本、地址还没填）的方案若判为"没配过"，那些字段会被新结构忽略、下次保存时静默消失。
  - 每个配过直连的方案生成 `tasks/http/<id>.json`（名称「<方案名> 直连」）+ 回填 `active_http_task` + 清空内联字段 + 并入 `.order.json`。
  - ID 选取：优先方案 id；被**任一类型**的任务占用时换 `<id>-2`…`<id>-20`（`default` 撞名是最典型的真实场景：方案叫 default、内置浏览器任务也叫 default）；绝不覆盖用户文件。任务已存在且内容一致时直接复用（上次迁移写到一半退出的续跑不会重复建）。迁移产物刻意**不填** `auth_url`——方案的 `auth_url` 仍在原处，留空即回退用它，升级前后行为完全一致。
- 方案分享：导出清空 `active_http_task`（与 `active_task` 同口径）、不再输出任何 `http_*`；导入忽略老文件里残留的 `http_*` 键并在响应里回报 `legacy_http_config_dropped`（老 payload 含非空 `http_url` 时为 true），前端据此提示「该分享文件包含旧版直连配置，已忽略」。

#### 三、登录链路（`src/login/`）

- `HttpLoginRequest::from_task(task, username, password, auth_url, fetch_page, global_ignore_https_errors)` 取代 `from_profile`。
- 新增 `LoginOrchestrator::resolve_http_task`：**未绑定 / 任务不存在 / 类型不对都是明确的终态错误**、零回退（直连没有可内置的默认任务），文案直接指向「任务 · 直连任务」页；`validate_profile` 的直连分支相应从「url 为空」改为「未绑定直连任务」。
- 认证地址回退链：**任务的 `auth_url` 优先，留空才回退方案的**（该字段浏览器/直连两渠道共用，老配置不填照旧可用）；证书策略：任务级 `ignore_https_errors` 优先、缺省跟随全局 `browser.ignore_https_errors`。

#### 四、Web API

- 新增 `POST /api/http-tasks/test`（新模块 `src/web/routes/http_tasks.rs`）：请求体 `{ task_id?, task?, profile_id?, username, password, fetch_page }`——`task`（任务编辑器里未保存的草稿）优先于 `task_id`（方案编辑器）；账号必填、密码留空时按 `profile_id` 回退方案已保存密码；认证地址与证书策略与正式登录同源；响应字段与旧端点逐字一致。错误码区分「两者都没给」（400）、「任务不存在」（404）、「任务类型不对」（400）。
- 删除 `POST /api/profiles/http-login-test`（路由 + handler + `HttpLoginTestBody` 全删），`openapi.json` 同步（删旧路径、加新路径，`openapi_json_matches_route_table` 绿）。
- 方案读写：`ProfileCreateBody`/`ProfileUpdateBody`/`PATCH /api/config` 的方案域白名单全删 `http_*`、加 `active_http_task`；三条保存路径共用 `validate_http_task_binding`——`login_channel == http` 时绑定不能为空、且必须是存在的 `http` 任务（直连没有兜底任务，绑错必然登录失败，放过去只会把配置错误伪装成运行错误）；`PATCH /api/config` 为此加了 `State<Arc<dyn TaskApi>>`（只在请求真的带了方案域字段时才校验，纯全局设置保存不受影响）。
- `POST /api/tasks` 的 `kind` 分支支持 `"http"`（用于建最小骨架），未知类型文案改为 `支持 browser / script / http`。

#### 五、前端

见下方同日条目「任务页新增「直连任务」Tab，任务仓库按条目类型分流」；本条目侧的配套改动：`LoginChannelField.vue` 退化为「渠道卡 + 该渠道用哪个任务 + 直连测试」（未绑定/绑定任务已被删除时当场提示并给出口，保存时前端也拦「选了直连但没绑任务」）；抽出 `HttpTaskFields.vue`（四组字段 + `?` 说明，任务编辑器与向导共用）、`HttpTestResult.vue`（结果卡）、`useHttpTaskTest.ts`（发送 + 单飞 + toast 口径，两个宿主共用）；直连配置向导改为编辑直连任务草稿（入口在任务编辑器里）；方案页认证地址的一行 hint 改为讲清「任务优先、这里是回退来源」；方案导入预览改为点名「旧版分享文件里的直连配置（含脚本）不会被导入」；测试结果文案改成上下文中立（两个测试入口共用，不再说「点保存方案」）。

### 验证

- `cargo fmt --all --check` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed` **1025 passed / 0 failed / 1 ignored**（lib 977 + 集成 48；含真跑真通过的 `http_login_chain`——自带 `TcpListener` mock 门户 + 真实二进制 + `PATH=""`）。
- 前端：`npm run typecheck` 零报错、`npm run test` **28 文件 274 例全过**、`npm run build` 通过。
- **迁移实测（真实存量形态的隔离副本）**：拷贝真实 `settings.json` + 浏览器任务 + 一份 v9 形态方案（带全套 `http_*` 实际值），以新二进制启动 —— 产物 `tasks/http/default-2.json`（因方案 id 与内置浏览器任务 `default` 撞名而换后缀）、`tasks/browser/default.json` 原封不动、方案 `active_http_task = "default-2"` 且 `http_*` 全部清空、`config_version = 10`、`auth_url` 与密码密文未变。
- **实机全链路（真实二进制 + 真实前端 + Playwright）**：任务页出现「直连任务」Tab 并列出迁移生成的「宿舍移动 直连」（带迁移说明）；打开编辑器字段逐项还原（地址/请求头/请求内容/成败关键字/脚本/证书=严格校验，认证地址为空即回退方案）；填测试账号密码后「发送测试请求」对本地 mock 门户返回 `请求判定成功 | HTTP 200`（走通「任务草稿内联 → 新端点 → 结果卡」全链）；编辑器内「配置向导」可打开；方案页「登录方式」显示绑定的「宿舍移动 直连」+ 发送测试 + 使用文档。
- 过程中修掉的两处自身缺陷：迁移测试夹具把「没配过直连」写成了「只有地址为空」（与真实存量的全默认值形态不符，掩盖了判定口径），改为按 `http_url` 是否为空决定其余字段；撞名规则最初只看 `tasks/http/`，实测才发现会与内置浏览器任务抢 `default`。

## 开发中（2026-09-22 任务页新增「直连任务」Tab，任务仓库按条目类型分流）

### 背景

- 直连请求的登录参数已从「方案内联字段」重构为独立任务对象（`type: "http"`，落 `<base>/tasks/http/<id>.json`，方案用 `active_http_task` 引用一个任务），后端列表接口顺带返回（`task_type: "http"`）。但前端此前**只有方案编辑器**能挑一个直连任务，「任务」页没有对应 Tab：创建/编辑任务本身只能手改 JSON 文件，方案编辑器「配置直连任务」跳的 `tasks-http` 也是一个尚不存在的路由名。
- 同一个任务仓库（`Misyra/campus-auth-tasks`）同时承载浏览器任务与直连任务（索引条目 `RepoTask.type`，缺省视为 `browser`），而导入弹窗此前把索引**全量**列出、确认结果一律按浏览器任务落草稿——直连条目会被写进 JSON 编辑器，步骤为空、请求字段全丢。

### 实现

- **`frontend/src/composables/useHttpTasks.ts`（新增，单例）**：对齐 `useTasks` 的结构——单例草稿 `httpTaskDraft`（`HttpTaskDraft | null`）、`useDirtySnapshot` 的 dirty/丢弃确认三件套、`fetchHttpTasks`（委托 `useTaskDirectory` 的混合列表拉取，只取 `httpTasks` 视图）。新增 `showNewHttpTaskDraft` / `showHttpTaskEditor(id)` / `closeHttpTaskEditor` / `saveHttpTask` / `deleteHttpTask` / `duplicateHttpTask` / `exportHttpTask` / `importHttpTask`。与 `useTasks` 的差异（有意）：草稿是**强类型平铺字段**而非一段 JSON 文本（`HttpTaskFields` 原地修改），故没有 JSON 校验/格式化/模板加载/危险步骤检测；缺口校验走 `httpTaskDraftGaps`（ID 形态 + 请求地址）；没有「执行」入口（直连不经 Python Worker，验证路径是发一次测试请求）。`duplicateHttpTask` 的 `_copy`/`_copy_N` 去重与 `exportHttpTask` 的下载口径照抄 `useTasks`；`saveHttpTask` 后清草稿并 `fetchDirectory(true)`；保存前对**非空 `crypto_script`** 弹一次确认（文案讲清"沙箱内执行、无网络与文件访问"，与 `useTasks.saveTask` 对 `eval`/`custom_js` 危险步骤的确认同口径，用户拒绝即不保存）；删除正被编辑的任务时关掉编辑器（否则"删除"看起来没生效——再保存会以同一 ID 重建）。`importHttpTask` 先按条目 `type` 过滤（兼容磁盘文件顶层 `type`、导出详情 `config.type` 与 `data` 信封三种形态），非直连条目拒绝并提示，避免"提示导入成功却在列表里看不见"。
- **`frontend/src/views/tasks/HttpTasksPanel.vue`（新增）**：以 `BrowserTasksPanel` 为模板（`tasks-grid` 两列 = 列表卡 + 编辑器/帮助卡）。列表卡头部：导入（文件）/ 仓库导入（`showRepoImport("http")`）/ 分享适配（`TASK_REPO_URL`）/ 新建直连任务；**不放「调试」按钮**（直连任务不经 Worker，验证入口是测试）。编辑器卡：ID（仅新建可改）/ 名称 / 描述 + `<HttpTaskFields :model="draft" />` + 测试区 + footer 保存/取消，并挂 `<HttpLoginWizard :draft :open :test-username :test-password>`（向导编辑同一份草稿，凭据与测试区共用同一对 ref）。**测试区**：本页没有方案上下文且任务不含凭据，故手填一次账号密码——两者是组件本地 ref，不进草稿（进草稿会被写进任务文件，还会让 dirty 永远为真），用 `FieldHelp` 说明其来源；发送走 `runHttpTaskTest({ task: httpTaskPayload(draft), username, password, fetch_page: true })`（内联未落盘草稿，不用已保存任务），前置校验缺账号/缺密码/缺请求地址各自给出提示；按钮态用共享的 `running`（禁用 + 「正在发送…」+ `spin`）；草稿任何变化（deep watch）或草稿被换掉/关掉时 `clearTestResult()` 并收起向导。帮助卡用 4 段导语（是什么 / 与 `active_http_task` 的关系 / 凭据留在方案 / 怎么分享导入）+ 占位符与脚本函数速查（沿用 `loginChannel` 的词表常量，不复制第二份说明）+ 一句「字段详情见编辑器内 `?`」。**支持 `?task=<id>`**：`onMounted` 内先拉目录、再判定该任务存在后 `showHttpTaskEditor(id)`，只跑一次，故参数不清理也不会反复覆盖用户当前编辑。
- **路由与 Tab**：`frontend/src/router/index.ts` 在 `/tasks` 下新增子路由 `{ path: "http", name: "tasks-http", meta: { title: "任务 · 直连任务" } }`（默认落地仍是 `tasks-browser`，未动 `redirect`）；`frontend/src/views/TasksView.vue` 的 `TABS` 在「浏览器任务」之后插入 `http` 项，`activeTab` 同步按 `/tasks/http` 末段判定。
- **`frontend/src/router/editorGuard.ts`**：该守卫按 `from.path.startsWith("/tasks")` 定向判定，直连任务草稿也是同一区域内的页内编辑器卡片，故第三个纳入 `useHttpTasks().isDraftDirty`——脏则二选一确认后 `clearHttpTaskDraft()`，与浏览器任务/脚本草稿同口径。
- **仓库按类型分流**：`frontend/src/composables/useRepoImport.ts` 新增 `repoKind: "browser" | "http"`，`showRepoImport(kind)` 改为**必填参数**（给默认值会让"忘了传"变成静默导入错类型）；`filteredRepoTasks` 先按类型过滤再按关键词（`""`/缺失/`"browser"` → browser，`"http"` → http，其余如 `"script"` 两类都不进）；`acceptRepoDisclaimer` 按 `repoKind` 分派——browser 走既有 `setTaskDraft`，http 用 `httpTaskDraftFromConfig` 填进 `useHttpTasks` 的新建草稿（`_isNew = true`，名称优先取条目名，ID 归一化到 `[A-Za-z0-9_-]`），两侧均保留 dirty 确认与「请在右侧编辑器内确认后保存」提示；下载文件实际 `type` 与列表类型不一致时拒绝导入并提示。`frontend/src/components/RepoImportModals.vue` 按 `repoKind` 显示标题（「从云端仓库导入直连任务 / 浏览器任务」）、来源 hint 后补一句类型过滤提示（「当前只显示直连任务条目（浏览器任务请到「浏览器任务」Tab 导入）」），免责声明按类型措辞：直连任务里没有凭据（凭据属于方案）故改说"凭据提交到任务写明地址"，并补一段点明**条目可能带凭据变换脚本**（登录时会在沙箱内执行其中的 JavaScript、无网络与文件访问）。调用方 `BrowserTasksPanel.vue`、`views/settings/TaskEnvironmentSettings.vue` 改为 `showRepoImport("browser")`。
- **测试**：新增 `frontend/src/utils/httpTask.test.ts`（14 例：`emptyHttpTaskDraft`；`httpTaskDraftFromConfig` 的证书三态（`null` 跟随全局 / `true` / `false` 不得塌成 `null`）与缺字段兜底；`httpTaskPayload` 的 `type:"http"`、trim、名称全空白兜底、请求头/请求内容不 trim；`httpTaskDraftGaps` 的「ID 形态只在 `_isNew` 时校验」、空 url 报缺、合法草稿无缺口、判定关键字不强制）；新增 `frontend/src/composables/useHttpTasks.test.ts`（10 例：保存前的 ID 形态/地址/名称三类缺口拦截、非空 `crypto_script` 的确认与拒绝后保留草稿、无脚本时不打扰、落盘载荷 `type:"http"` 与 trim 后的地址、保存失败保留草稿、删除正在编辑的任务时关掉编辑器、取消确认不删除、删除别的任务不动当前草稿；`api`/`useToast`/`useConfirm` 全部 mock，不碰真实 HTTP）；`frontend/src/utils/loginChannelTask.test.ts` 补 `httpTaskOptions` 5 例（首项空值「未绑定（直连登录不可用）」、任务顺序保持传入顺序、无 name 回落 id、空任务仍保留未绑定项）；`frontend/src/composables/useRepoImport.test.ts` 补条目按类型分流 4 例（只列当前类型 / 搜索不越过类型边界 / http 条目落直连草稿 / 文件类型与列表不符时拒绝），并把既有调用改为 `showRepoImport("browser")`。
- 未改动（属其他改动面，签名已冻结）：`utils/httpTask.ts`、`components/common/HttpTaskFields.vue`、`HttpTestResult.vue`、`HttpLoginWizard.vue`、`composables/useHttpTaskTest.ts`、`api/types.ts`、`api/index.ts`、`components/common/LoginChannelField.vue`、`composables/useProfiles.ts`。

### 验证

- `npm run typecheck` 零报错；`npm run test` **28 文件 274 例全过**（本次新增 33 例）；`npm run build` 通过（`vue-tsc` + `vite build`，产物含 `HttpTasksPanel-*.js` 35.22 kB / gzip 12.07 kB）。

## 开发中（2026-09-22 直连请求面板：说明文字收进 `?` 气泡，气泡支持点击钉住）

### 背景

- 用户反馈直连请求面板「元素过多、说明性文字太多」。实测该面板 1086px 宽下正文共 8 段说明 + 1 个占位符速查块：每个步骤标题各带一句 `<small>`（4 处）、请求内容的占位符速查（头句 + 规则 + MAC 形态说明）、判定顺序提示、证书策略的常驻 `hint`、测试区的「不会保存方案」——7 个输入控件被挤到首屏之外。这些文字都是「首次配置才需要读一次」的解释，常驻正文的代价是每次打开都要重读。
- 信息不能删（占位符规则、判定顺序、脚本契约都是踩过坑才写下的），因此改为「字段留在界面上、解释收进 `?` 气泡」：标签、示例按钮、输入框 placeholder、真实风险提示全部保持可见，只有「为什么这么填」挪进气泡。

### 实现

- `frontend/src/components/common/FieldHelp.vue`（全站帮助气泡的唯一实现）：
  - **点击钉住**：新增 `pinned` 态。此前只有 `:hover` / `:focus-visible` 显示（纯 CSS），鼠标一移向输入框气泡即消失，多段说明等于看不完。现在点击切换钉住，再点一次 / 点别处 / Esc 关闭；全局 `pointerdown`、`keydown` 监听只在钉住期间挂载（本组件实例以数十计，常驻监听白占开销），`onBeforeUnmount` 兜底摘除。键盘 `Enter` / `Space` 同样可切换。
  - **自动翻转 + 宽度实测**：新增 `syncAutoFlip()`，在 `mouseenter` / `focus` / 点击时量触发点左右两侧实际剩余空间，右侧放不下就翻到左侧，两侧都放不下则把 `--tip-max` 压到较宽一侧的可用值（下限 200px）。此前 `.field-help--flip` 只能由作者静态指定，窄屏下靠右的 `?` 气泡会整块跑出视口——本面板把说明都挪进气泡后，这等于把文字弄丢，故必须实测。
  - **`wide` 修饰**：宽气泡 460px（翻转时 420px），用于脚本契约、占位符规则这类长文案；默认 320px 会把它们拉成极长的竖条。
- `frontend/src/styles/components/form.css`：气泡 `white-space` 由 `normal` 改为 `pre-line`（`data-tip` 里的 `\n` 按段落换行）、补 `overflow-wrap: break-word`（URL / 代码片段不撑破气泡）；新增 `.field-help--pinned`（展开态 + 限高 `min(60vh, 420px)` 可滚 + 放开指针事件，做到「边填边看」）与 `.field-help--wide`；四处 `max-width` 改为 `min(var(--tip-max, 默认值), calc(100vw - 80px))`，让组件实测值生效。
- `frontend/src/styles/responsive.css`：≤640px 的 `.field-help::after` 覆盖规则原本写死 `max-width: calc(100vw - 120px)`，会盖掉组件写入的 `--tip-max`，改为 `min(var(--tip-max, 320px), calc(100vw - 64px))`。
- `frontend/src/components/common/LoginChannelField.vue`：
  - 面板文案集中到脚本内 `HELP` 常量表（`channel` / `url` / `request` / `headers` / `body` / `placeholders` / `cert` / `verdict` / `success` / `failure` / `scriptWhen` / `scriptContract` / `test`），以 `\n` 分段；`certHelp` 把证书口径与「当前选择实际含义」（原常驻 `hint`）合成为一条气泡。
  - 删除：面板导语 `.http-channel-intro`（与渠道卡重复，「失败不切回浏览器」并入 `HELP.channel`）、4 处步骤 `<small>`、占位符速查的三段说明、`.http-judge-note`、证书常驻 `hint`、测试区说明句；浏览器渠道的 `.channel-note` 与重复的「按方案绑定」`hint` 一并去掉（该口径在任务 `?` 气泡里已有）。
  - 保留可见：全部标签与示例按钮、输入框 placeholder、占位符词表（要照着抄）、`passwordInUrl` 风险提示（它拦的是用户看不见的后果，藏进气泡就失去拦截力）、测试前置提示「需先填好账号与密码」。
  - 步骤 4 的折叠标题行改为 `.http-advanced-row`（标题按钮按内容收缩，气泡与「已配置」徽标同排）：气泡不能放进按钮内部，嵌套可聚焦元素会让点击语义打架。
  - 关联 CSS 清理：删除 `.http-channel-intro*`、`.http-step-head small`、`.http-judge-note*`、`.http-template-help`、`.http-template-head`、`.http-template-note`、`.http-help-block .http-help-title`、`.http-advanced-copy small`、`.channel-note`；新增 `.http-step-title`、`.http-template-row` / `.http-template-label`、`.http-chip-row--inline`、`.http-advanced-row`。

### 验证

- `npm run typecheck` 零报错；`npm run test` 241 例全过（26 文件）；`npm run build` 通过。
- 真机实拍（`--base-path` 隔离实例 + 重编译内嵌 dist，Playwright Chromium）：1440 / 1024 / 640 / 480 四档视口逐项悬停面板内全部 `?` 并截图，气泡与触发点均落在视口内、页面与面板横向溢出为 0、控制台无错误；钉住态实测「点击 + 鼠标移开仍展开（1）→ 点别处关闭（0）」；占位符规则这条 5 段长文案换行正常，`\n` 未被吞。
- 窄屏回归：480px 下面板宽 334px，最长的占位符气泡按实测可用宽度 255px 渲染并完整落在视口内，占位符词表自动折行。

## 开发中（2026-09-21 新增 `docs/promo/` 项目介绍演示页：纯 HTML/CSS/JS/SVG，15 页可放映）

### 背景

- README 里的三张静态截图不足以说明「控制平面 + Python 插件」这套架构与运行模式取舍，需要一个能直接双击打开、像 PPT 一样翻页/自动播放的介绍材料，且不引入构建步骤与第三方运行时。
- 硬约束：只用 HTML / CSS / JS / SVG，`file://` 双击即用（不依赖本地服务器）；1920×1080 到 1366×768 之间任意窗口下版面构图一致。

### 实现

- **目录**：`docs/promo/index.html`（放映页）+ `script.html`（分镜与旁白讲稿）+ `README.md`（用法/设计说明）+ `css/{tokens,stage,backdrop,slides,motion}.css` + `js/{deck,backdrop,fx}.js`，共 11 个文件，无构建。复用 `docs/assets/` 已有的 `logo.png` / `logo-dark.png` 与三张 `preview-*.webp`，不新增二进制资源。
- **不用 ES module**：全部走经典脚本（`IIFE` + `window.PROMO` 命名空间）挂载。原因是 `type="module"` 在 `file://` 下受 CORS 限制会被拦掉，双击打开即白屏。
- **固定舞台缩放**：内容按 1600×900 设计，`#stage` 用 `translate(-50%,-50%) scale(min(vw/1600, vh/900))` 整体等比缩放，标题/正文/图形的位置关系在任意分辨率下完全一致，版式不随窗口漂移。
- **背景非纯色**：7 层叠加（`#sheen` 斜向光晕 / `#bloom` 径向色斑 / `#grid` 线格 / `#arcs` 圆弧刻度 / `#mesh` canvas 网络点阵 / `#streams` 流线 / `#vignette` + `#grain` 噪点压边），逐页 `data-bg` 切换情绪（`hero` / `grid` / `blueprint` / `streams` / `bloom` / `glow` / `quiet`），0.9s 交叉淡入。`#mesh` 是 canvas 实时绘制：46 个漂移节点（每 11 个为枢纽）、252px 连线阈值、4 个游走数据包，密度函数随横坐标右重（`0.06 + 0.94 * pow(x/W,1.5)`）。
- **昼夜两套主题靠两个变量**（`--fg` / `--bg`）切换，所有描边、面板、线格用 `color-mix(in srgb, currentColor N%, transparent)` 派生，因此换主题不需要改任何组件样式。强调色只有一支 `#12CFBB`，仅用于表达「已连通 / 正在播放」。
- **动画原语**：`.wipe`（`clip-path` 遮罩打字式揭示，用于标题）/ `.rise`（面板上浮）/ `.pop`（标签弹出）/ `.draw`（`pathLength="1"` 描边生长，用于连线与图形）/ `.fade` / `.flow`（连接线上流动虚线）/ `.breath` / `.live` / `.caret`；单元素延迟用 `--d`，容器上 `data-stagger` 自动给子元素铺开延迟。`js/fx.js` 额外提供数字滚动（`[data-count]`）与密文打乱（`#cipher`）。
- **放映引擎**（`js/deck.js`）：单个 `requestAnimationFrame` 循环同时驱动时间轴、进度轨与 canvas；每页 `data-dur` 独立时长（合计约 134s），`data-theme` 决定昼夜，`data-title` 作进度轨悬浮提示；`history.replaceState` 写 `#n` 支持深链。键盘 `←/→`、`PageUp/PageDown`、`Space`/`k`、`Home`/`End`、`f` 全屏，点击任意处前进，进度轨刻度可跳页；标签页失焦暂停；自动播放到末页停住，播放键变「重播」。`prefers-reduced-motion` 下走静态降级（`still` 标记）。
- **内容口径**逐条对齐 `README.md` / `docs/updatelog.md` / `AGENTS.md`：v5.0.2、AES-256-GCM 加密配置、30 天登录历史、200 MiB 日志上限、实时日志缓冲约 79% 内存下降、12 类任务步骤、三条更新通道（stable/prerelease/all）、直连登录的能力边界（不绕过验证码与风控）、23:00–06:00 夜间暂停默认仅对新装生效。

### 验证

- **真机分页截图核对**（Playwright Chromium，`file://` 直开，1600×900）：15 页逐页截图 + `script.html`，每页等 4600ms 让入场动画走完后再取图；`getBoundingClientRect` 逐元素比对，15 页内容盒底部一律 754px（`#stage` 内容区下沿，末页 565px 为设计留白），横向溢出 0，无文本相互压盖；另按 `line/path` 采样 + canvas 实测字形墨迹盒核对 SVG 内文字与线是否相交、SVG 文字是否被自身 `viewBox` 裁掉，本机全为斜体矩形的倾斜检测亦为 0。
- **交互自检**（Playwright 逐项断言，23 项全过、控制台无错误）：起始页与夜间主题、进度轨刻度数与计数文本、`→`/`←`/按钮前进后退、`End`/`Home` 首末页与夜间/情绪切换、`Space` 暂停、点击舞台前进、刻度跳页与 `#n` 深链、末页停住不循环并出现重播按钮、1600×900 / 1280×720 / 3840×2160 / 1100×900 四种窗口下舞台等比缩放。
- **节拍实测**（`promo-perf.py` 读引擎时间轴）：01→02 发生在约 7.0s、02→03 约 14.6s，与 `data-dur` 一致，无丢帧导致的整体漂移；首屏首次内容绘制 1240ms，`load` 约 3.7s（两处 CDN 字体各约 950ms，无头软件渲染下约 34fps）。
- 过程中修掉的实现缺陷：`#stage.is-night` 选择器不匹配（实际类名只有 `is-night`，改为 `#stage.is-night`）；CSS 拆分时 `.hero-art` 丢了 `position:absolute` 落到左上角；`#grain` 的 SVG 无宽高回退成默认 300×150 导致噪点不覆盖（补 `width/height:100%` 与 `viewBox`，顺带把首屏 raster 成本降约 16×，此前在无头环境下第一次绘制卡顿约 4s）；`#vignette` 被 `#backdrop .bd` 优先级压住恒为 `opacity:0`（改为 `#backdrop #vignette`）；`#arcs .dial` 旋转原点缺 `transform-box:view-box` 导致圆心漂移。
- 未随本轮提交的备选（已记入 `docs/promo/README.md`）：导出 MP4 的两种做法（录屏自动播放，或 Playwright 按 `data-dur` 逐页截帧后交 ffmpeg 合成，裁掉底部 118px 即可隐藏进度轨与控制栏），以及把图片内联成 base64 使 `index.html` 单文件可移植。

### 修订（同日：13 → 15 页，拆开两种登录方式并补轻量实测）

- 用户反馈驱动的三处调整：①两种登录方式的优缺点要「全讲清楚」，一页带过不够；②要单独一页说内存占用，突出轻量；③对新手最关键的是「仓库里已经有内置任务，所以装完零配置」，这一点要重点写。
- **拆页**：原「两种登录方式」拆成 05「浏览器自动化」与 06「直连请求」，两页共用 `.login-split` 结构 —— 左半各自呈现（05 是认证页线框 + 五枚步骤标签，06 是深色终端里一次真实 POST 与 200 响应 + 两节点小图「Rust 控制平面 → 校园网认证门户」），右半是优点四条 / 缺点三条的双栏清单，页脚各留一句「什么时候用它」「建议顺序」。优点记号用 ASCII `+`、缺点用 `-`（避开 U+2212 的字形风险），青色只落在优点侧。
- **新增 07「轻量」**：24 小时时间轴上四根青色细柱 = 浏览器真正存在的那几秒（`viewBox="0 0 1200 262"`）；标题行右侧一张常态后台内存读数卡（`< 10 MB` + 任务管理器实测行 `campus-auth.exe / 5.8 MB 内存 / 0% CPU / 非常低 电源`）；页脚三段说明（常驻的只有控制平面 / 按需拉起、用完就还 / 日志不再堆在内存里）。读数取自 Windows 任务管理器实测，页面按上界口径写成 `< 10 MB`。卡片放在标题行右半（`.mem-top` 两列），竖向不额外占高。
- **重写 14「上手」为零配置**：标题改为「装完就能跑，零配置」，新增 `.zero` 区块（`0` 份配置要写 + 内置任务说明）与四步清单，终端日志补 `内置任务「通用登录」已就绪`、`Engine 待机中 · 点「启动检测」开始`，页脚点明「零配置不等于只有一条路：默认任务删不掉，删掉别的任务会自动回退到它」。口径依据 `src/tasks/seed_default.json` 被 `src/tasks/loader.rs` 以 `include_str!` 内置为 `DEFAULT_TASK_SEED`、`DEFAULT_TASK_ID = "default"`（不可删除）。
- **版面缺陷修正**（均来自用户截图）：①控制台页五枚数据片原先压住仪表盘截图，改为截图居中偏左（`width:900px`）、数据片整体移到右侧一列（`x=948 / 1216`），互不相交；②架构页的 S 形折线与竖向主轴交叠显乱，重写为单一正交折线（`http 直连登录 · 不经 Worker`），图例挪到右上；③截图不再加 `transform:rotate()`（用户明确要求「不要东倒西歪」）；④`.frame-shot` 改为 flex 列布局、`img` 用 `object-fit:cover`，修掉图片高度忽略窗口标题栏造成的越界假象；⑤问题页胶囊框与分隔线缩短，不再压到四段标签；⑥轻量页时间轴两端由 `x=30/1170` 改为 `x=0/1200`，与内容栏左右边界对齐。
- **修掉一类此前没人注意的裁剪缺陷**：SVG 根元素默认 `overflow:hidden`，文字一旦越过自身 `viewBox` 就被削掉——问题页「明天再来一遍」上缘被削约 1px、安全页更新通道的 `prerelease` 标签右端超框约 26px。核对脚本因此补了一项「svg 文字被裁」（canvas 实测墨迹盒 vs svg 自身盒，全四边），修完后 15 页 0 命中。
- `docs/promo/script.html`（分镜与旁白）与 `docs/promo/README.md`（用法 / 设计说明 / 录屏口径）同步更新到 15 页、约 2:14，并补记各项数据的出处（含内存读数实测来源）。

### 修订（2026-09-22：统一暖白宣传风格，15 → 9 页）

- 用户希望最终材料更像 1–3 分钟宣传片，而不是技术说明会：全片由 15 页 / 约 2:14 收束为 **9 页 / 62.4 秒**，叙事改为「问题与承诺 → 24 小时重连闭环 → 四个核心卖点 → 真实产品证据 → 开箱使用 → 收尾」。架构、运行模式、夜间暂停、安全与更新等说明性内容不再单独占页；两种登录方式合并为一页，只保留对观众有用的取舍。
- **不再切换白底 / 黑底**：`tokens.css` 把日间与夜间令牌统一映射为暖白纸张 `#F4EFE6` + 深棕黑正文，琥珀黄 `#D79346` 承担品牌强调，低饱和绿色只表示在线/成功。9 个 `<section>` 保持同一套颜色令牌，页面仅通过 `data-bg` 改变光晕、同心弧与连接线密度，不改变曝光基线。
- **开场回到真实产品**：采用「左侧品牌文案 + 右侧仪表盘实机截图」构图，与官网首屏一致；仪表盘 / 任务 / 方案三张真实截图统一装入有边界的浅色窗口。曾生成的夜间宿舍候选图因不符合用户指定的暖色调方向而弃用并从仓库移除。
- **产品窗口强化**：四处真实界面截图统一换成 macOS 风格三色窗口按钮与居中地址栏；首屏产品窗口由 710×426 放大到 800×500，并增加「常驻内存 `< 10 MB`」与「网络连接正常」两张轻量状态浮层，让实机界面成为画面主焦点，同时把低占用、持续守护直接放进首屏。
- **9 页全部改用对应实机页面**：启动隔离基准路径下的真实 `campus-auth.exe`，通过 Chromium 逐一访问 `/`、`/settings/monitor`、`/settings/system`、`/settings/network`、`/tasks`、`/profiles`、`/settings/browser`、`/settings/tasks` 与 `/about` 截图；另把方案页滚动到“认证设置”单独捕获“浏览器自动化 / 直连请求”选择区。02 用网络检测设置、03 用系统设置 + 网络与更新、05 用浏览器任务、06 用认证设置 + 浏览器设置、07 用任务与环境、08 用方案顶部、09 用关于 + 任务 + 方案矩阵，不再用同一张仪表盘图重复裁切。方案截图中的账号仅在截图 DOM 中替换为演示学号 `20260001`，不写回配置。
- **双引擎卡片留白修正**：第 6 页左右两张卡片的 `HTTP 直连` / `浏览器自动化` 标题区下移 24px，与上方实际界面截图和悬浮图标拉开距离，避免信息挤在截图底边。
- **仓库页采用用户指定截图**：第 5 页左侧产品窗口改用用户提供的“从云端仓库导入任务”实机截图，直接展示 GitHub / Gitee / 自定义来源、任务列表与预览区域；保留窗口化处理和“仓库导入”标注，不再使用普通浏览器任务列表截图代替仓库界面。
- **直连页采用用户指定截图并重做双窗口构图**：第 6 页左侧改用用户提供的“直接向校园网网关发送登录请求”配置截图，完整露出请求地址、请求头与成功判断配置；左右实机画面统一改成带三色按钮的 Mac 风格窗口，截图高度扩展到 290px，并移除遮住画面的渐变、悬浮标签和圆形图标。`HTTP 直连` / `浏览器自动化` 标题与说明固定排在窗口下方，产品画面成为卡片主体。
- **直连截图改用近景版本**：按用户最终指定图片覆盖第 6 页直连窗口素材，镜头集中展示 GET / POST 方法选择、登录请求地址和请求头输入区，避免此前全页截图中文字过小；超宽原图采用完整适配并关闭推近，不裁掉左右两侧内容。
- **移除背景方格**：用户反馈格线观感杂乱，`campaign.css` 对 `#backdrop #grid` 直接 `display:none`，不保留弱化版本；背景只剩暖色渐变、柔和蓝灰/琥珀光晕、同心弧和极淡的网络连接线。
- **卖点前置**：第 3 页直接落出「解压即用 / 共享任务仓库 / 24 小时自动重连 / 常态后台 `< 10 MB`」；第 5 页把任务仓库讲成「浏览 → 预览 → 导入」并明确 GitHub / Gitee 镜像与分享适配；第 7 页用 24 小时时间轴上的短柱说明浏览器 / OCR 只在真正登录的几秒出现；第 8 页把上手收为「解压 / 填一次账号 / 启动检测」三步。
- **动画节制**：新增 `css/campaign.css` 承载 9 页构图；实机截图只做 2%–7% 的镜头推近，循环动效只保留重连脉冲、双引擎状态点与内存短柱。`prefers-reduced-motion` 下全部关闭；首屏与第二页截图在 `<head>` 预加载，减少自动播放切页时的图片闪入。
- **背景气候修正**：`deck.js` 切页与首屏初始化时调用 `backdrop.setMood()`，此前只改了 `data-mood`，canvas 节点疏密的 `moodK` 实际没有同步。
- `docs/promo/README.md` 与 `script.html` 同步重写为 9 页 / 62.4 秒口径；旁白按镜头逐页给出，收尾预留 2–3 秒静帧。

### 验证（9 页宣传版）

- Playwright Chromium 以 `file://` + 1600×900 逐页取图核对：9 页均保持同一暖白背景，无明暗切换与方格残留；每页至少一张对应实机截图且全部加载成功、比例正确；第 6 页标题由浏览器随机断词改为主动两行断句。
- 自动检查覆盖 9 个进度刻度、`01 / 09` 计数、所有页 `data-theme=night`、图片 `naturalWidth`、可见文字舞台边界、控制台 / page error，以及 `Home` / `End` / `ArrowRight` 基础交互。

## 开发中（2026-09-21 AI 生成页：服务商卡片改名「自定义服务商」+ 切换服务商不再丢配置）

### 背景

- 卡片文案与实际含义对不上：`custom` 是"自己填地址的通用 OpenAI 兼容入口"（Ollama / LM Studio / 火山方舟这类都走它），却写着「其他兼容服务 / 需要知道接口地址」。改为标签「自定义服务商」，标识符 `custom` 与后端校验口径不动（自定义不校验域名，指向已下架渠道的老配置仍可保存）。
- 缺陷：`selectProvider` 每次切换都用该服务商的预设默认值覆盖表单，而 `custom` 的预设默认值就是空串 —— 于是"自定义 → 预设 → 切回自定义"把手填的地址/模型/Key 全部清空，已保存的自定义配置同样拿不回来（`loadConfig` 只在挂载时跑一次，切换不重新读盘）。

### 修复

- **每服务商一份草稿**（`drafts: Map<provider, {baseUrl, model, apiKey, modelList, customModel}>`）：切换前 `stashDraft()` 存当前表单，切回时原样恢复（含已拉取的模型列表与"手动/下拉"模式）；`loadConfig` 把已保存配置种进草稿，所以切回拿到的是"上次保存 / 上次编辑"的值而不是预设空值。**空表单不入草稿**（读配置失败或用户压根没填过时，不能把"空"记成该服务商的状态，否则切回拿到空值而非预设默认值）。
- **Key 状态按槽位判定**：新增 `savedKeySlot`（内置服务商＝标识本身；自定义＝`base_url` 的 origin，与后端 `LlmSettings::key_slot` 同一口径）与 `keySlotOf()`，`hasApiKey` 由可变标志改为 computed。原因是 `configured_providers` 只报内置服务商（自定义 Key 按 origin 隔离，没有单一槽位），切回自定义时前端无从判断 Key 是否还在，会把已保存的 Key 显示成"尚未保存"；现在只要地址没变就重新显示「已保存（留空保持不变）」，换成别的 origin 则不声称已保存。
- 同步更新指向该标签的注释与类型说明（`src/ai/mod.rs`、`src/ai/llm.rs`、`frontend/src/api/types.ts`、`AiTaskView.vue`）。

### 验证

- **真机验证**（对运行中的实例 UI `http://127.0.0.1:50721/tasks/ai`，Playwright 驱动；只点服务商卡片与输入框打字，不点「保存配置」，不触碰实例里的真实配置）：15 项断言全过 —— 卡片已改名且旧名消失；挂载即载入已保存配置（`custom` + `https://ark.cn-beijing.volces.com/api/coding/v3` + `ark-code-latest`，`has_api_key=true`）；切换 GLM 用 GLM 预设，切回自定义恢复出已保存的地址/模型且占位回到「已保存（留空保持不变）」；手填三字段后与 GLM/DeepSeek 来回切换四轮，自定义值与 GLM 值各自完整保留、互不串值；把自定义地址改成另一个 origin 后不再声称「已保存」；控制台零报错。
- 该轮验证同时暴露并修掉了草稿逻辑的一个边界：探测脚本曾在实例已退出（后端不可达）时运行，`loadConfig` 失败留下空表单，被 `stashDraft` 记成 GLM 的空草稿，导致之后切回 GLM 拿到空值 —— 即上面"空表单不入草稿"这条的由来。
- `npm run typecheck` 零错误；`npx vitest run` **241 passed / 26 files**。

## 开发中（2026-09-21 任务录制器：智能检测移到底部动作行 + 选中态强调色）

### 背景

- 「智能检测」是常用**模式**（打字/点击自动识别），却和一次性步骤类型挤在同一个网格里，位置随网格顺序漂；底部动作行只有右侧一个主按钮 + 关闭，左侧空着。
- 上一轮把选中态做成了中性灰（`rgba(0,0,0,.22)` 描边 + 灰底），选中与未选中的差别太弱。

### 修复

- 步骤类型新增 `bottomRow` 标记，`smart_detect` 打上；网格按 `primary !== false && !cfg.bottomRow` 过滤，底部行按 `cfg.bottomRow` 过滤后用同一个 `createStepBtn` 生成并插到最前。**保留 `.ca-step-btn` 类**：`selectStepType` 是按 `dataset.type` 统一切换高亮的，所以选中态、与网格卡片的互斥行为都不用改。
- 底部动作行改为 `.ca-bottom-row`（flex）：左侧常驻「🔍 智能检测」，`#ca-btn-copy-prompt` 用 `margin-left: auto` 推到右侧（原来在左），关闭按钮仍在最右；移除原先 `style="margin-left:auto"` 的内联样式。
- 网格少一张卡后剩 6 张 + 「更多」，让 `.ca-more-btn` 跨整行（`grid-column: 1 / -1`），避免第 4 行只挂一张卡、左边空一格。
- **选中态改用强调色**：新增令牌 `--ca-accent` / `--ca-accent-soft` / `--ca-accent-ink`（日间 `#5566e8` 靛蓝 —— 与徽标彩虹环的靛蓝端同一色系；夜间换 `#8b9cff` 的亮调），步骤卡与模式开关（多步录制 / 隐藏检测 / 显示隐藏）的选中态统一成「强调色描边 + 10% 淡底 + 强调色深调文字」，卡片 hover 描边与「更多」hover 也一并改用强调色。主按钮维持深底白字（单色主按钮 + 彩色选中态，避免整块面板被配色淹没）。

### 验证

- 真 Chromium：网格内 `smart_detect` 计数 0、底部行 1；点底部按钮 → 自身 active 且网格内 active 为 0（互斥保持）、状态条回执 `🔍` + hint；再点网格卡片 → 底部高亮自动取消。选中态计算样式为描边 `rgb(85,102,232)`、底 `rgba(85,102,232,0.1)`、文字 `rgb(63,79,208)`；模式开关选中态同色。样式/主题其余断言全过，控制台零报错。样张：`style-panel.png`、`style-bottom-row.png`、`probe-toggle.png`。

## 开发中（2026-09-21 任务录制器：修回 @namespace 身份 + 重复安装自检）

### 背景（实测定位）

- 现场：面板做了日间/夜间两套令牌后，点切换按钮状态条回执「已切换到日间模式」，但面板配色纹丝不动。
- 定位：截图里「复制 AI 提示词」是**靛紫 `#667eea`** —— 那是 **restyle 之前**的旧配色（日间应为近黑 `#16181d`、夜间近白 `#e9ebef`），说明**旧版样式表还在生效**。根因是当天早些时候"修仓库链接"那轮把 `@namespace` 从 `github.com/Misyra/Campus-Auth` 一并改成了 `.../Campus-Auth-rs`：**Tampermonkey 用「`@name` + `@namespace`」判定脚本身份**，改了 ns 等于换了个脚本，重新安装时 TM 会**另装一份**、旧那份不会被覆盖，两份同时在跑。旧样式表作用域是 `#ca-recorder-panel`（ID 选择器，`1,0,0`），而这次主题令牌放在了 `:root`（伪类，`0,1,0`）→ 旧令牌胜出，把配色压回旧版。
  - 也解释了前一轮现象：改深色那版时令牌仍在 ID 作用域，同优先级下后注入的样式表赢，所以能看到改后的深色版；令牌一挪到 `:root`，旧表立刻重新压住。
- 工装复现（真 Chromium）：只装新版 → 面板 `rgb(255,255,255)` / 主按钮 `rgb(22,24,29)`；**新版 + 旧样式表** → 面板 `rgb(26,26,46)`（旧 `#1a1a2e`）/ 主按钮 `rgb(102,126,234)`（旧 `#667eea`），与现场截图一致。

### 修复

- `@namespace` **改回历史值** `https://github.com/Misyra/Campus-Auth`：namespace 是机器标识（TM 拿它当脚本主键），不面向用户展示；**可见的仓库链接**（面板页脚、帮助页脚）保持指向 `Campus-Auth-rs`。同时在脚本里 `VERSION` 常量旁写明硬约束——不要再跟着仓库改这个字段，否则重装即分叉。
- 新增**重复安装自检**：面板挂载后若发现页面上存在多个 `#ca-recorder-panel`，先把自己移到 `body` 末尾（叠在最上层，保证用户看到的是本份面板而不是被旧样式表压着的旧界面），再 `console.warn` + 状态条提示「请在 Tampermonkey 里删除旧的那份」。不自动处理（删脚本只能由用户做），但把这类静默故障变成可见提示。
- 注意：`@namespace` 的两次翻转（先改成 `-rs`、又改回原值）各自造成一次 TM 分叉——每翻一次，重装就多出一份。现已冻结在原值，脚本里已写明硬约束。

### 验证

- `recorder_conflict_check.py` 三种场景：只装新版（配色正确）、新版 + 旧样式表（复现旧配色，判定"已复现"）、存在重复面板（自检提示触发，状态条命中"重复安装"）。
- `node --check` 通过；日/夜主题的 14 项断言在"只装新版"下仍全过。

## 开发中（2026-09-21 任务录制器：日间 / 夜间主题切换）

### 背景

- 面板改成白底后只剩一套配色：夜里或在暗色门户页上用它很刺眼，需要能切。

### 修复

- **令牌位置调整**：原来一组令牌挂在 `#ca-recorder-panel` 上，而 tooltip / 揭示面板 / 揭示弹窗都在面板作用域之外（挂在 `body` 上），拿不到主题；现在令牌提到 `:root`，主题类挂 `<html class="ca-dark">`，面板内外一起换。**样式规则一行未改** —— 夜间只是同一套令牌的另一组值。
- **两套取值**：日间维持白底（`#ffffff` / 卡片 `#f4f5f7` / 主按钮深底白字）；夜间 `#17191e` / 卡片 `#1f232b` / 输入 `#14161b`，主按钮翻成亮底深字、成功/危险/警示三色换成暗底可读的亮色调；投影分两套（`--ca-shadow-panel` / `--ca-shadow-pop`）。
- **切换入口**：头部右侧按钮组（与「?」并列）新增主题按钮，图标显示**目标**模式（日间显 🌙、夜间显 ☀️），点击即切换并在状态条回执。偏好走独立键 `ca_recorder_theme` + `GM_setValue` 持久化（与录制状态的 `ca_recorder_state` 互不影响），**默认日间**；`applyTheme()` 在样式注入前先跑一次，开面板之前 `<html>` 上已是正确主题，避免白闪。

### 验证

- 真 Chromium 14 项断言全过：默认日间（无 `ca-dark`、面板 `rgb(255,255,255)`、主按钮深底白字、图标 🌙）；点切换后 `html.ca-dark` 就位、面板 `rgb(23,25,30)`、卡片 `rgb(31,35,43)`、主按钮翻成亮底深字、图标 ☀️，且**面板之外的 tooltip / 揭示面板 / 揭示弹窗同步转深**（`rgb(29,32,39)`）；刷新后仍是夜间（偏好已存、且开面板前就已生效）；再点回日间；控制台零报错。
- 样张：`docs/reports/debug-panel-harness/theme-light.png`、`theme-dark.png`。

## 开发中（2026-09-21 任务录制器：面板改浅色现代简约 + 彩虹环律动）

### 背景

- 面板视觉停留在早期版本：紫色渐变头部 + 深蓝紫底 + 到处用饱和色（步骤卡 2px 透明边、开关紫色发光、状态条紫底），hover 还带 `translateY(-1px)` + `brightness(1.1)`；观感偏重、与主程序控制台的口径也不一致。第一版改成中性近黑，实测后按"要白的"改为 **白底浅色** —— 录制器是盖在门户页上的浮层，白色面板在绝大多数登录页上更轻、不压视线。

### 修复

- **只改 CSS 与令牌映射，DOM/JS 逻辑零改动**（替换逐条断言"恰命中一次"，未命中就不写回）：
  - 浅色表面：`--ca-bg/--ca-surface/--ca-input #ffffff`、卡片 `#f4f5f7`、hover `#eceef1`、active `#e4e7eb`；文字 `#16181d / #4a5058 / #7a838f`；描边统一为 `rgba(0,0,0,0.10)` 的 hairline；面板 `border-radius: 14px` + 阴影改成浅色下更轻的两层 `0 20px 48px rgba(15,18,24,.16) + 0 2px 6px rgba(15,18,24,.05)`。
  - **头部去紫渐变** → 白底 + 底部 1px 分隔线，标题 `15px/600`，副标题走 muted 色，"?" 帮助按钮改幽灵态（hover 才亮）。
  - **按钮体系重排**：主按钮＝深底白字（`--ca-primary #16181d` + `--ca-primary-ink #ffffff`，hover 再压到纯黑），次要按钮＝透明 + 描边，危险按钮＝幽灵态（hover 才转红）；删掉位移 + 提亮的 hover。
  - 其余同步收敛：小标题 `11px/600/0.08em`；开关改药丸形（active＝黑 6% 底）；状态条改虚线框（录制中＝红 8% 底 + 深红字）；步骤卡 1px hairline + 中性 hover/active（去掉紫色高亮）；已录制序号徽章改中性；输入框补 focus 描边；弹窗/遮罩改白底 + 轻遮罩 `rgba(15,18,24,.35)` + 轻微模糊。
  - 页面上的 tooltip / 揭示面板 / 揭示弹窗一并改浅色（这些在面板作用域外，用字面量；揭示语义的绿色收敛为 `#12855a`）；顺手删掉已无人引用的 `--ca-step-*` 与 `--ca-primary-grad`。
- **彩虹环律动**：彩虹层从元素自身背景挪到 `::after`（超采样圆盘 `inset: -30%`，靠按钮 `overflow: hidden` 裁成圆环带），`ca-rainbow-spin 6s linear infinite` 整体旋转；白底 `::before` 与 logo 是静态同级元素、**不跟着转**（否则 logo 会自转）。面板标题那枚徽标同样转、周期放到 9s 免得抢视线；`prefers-reduced-motion: reduce` 下停用动画。

### 验证

- 真 Chromium 实测：浮动按钮隔 1.2s 各拍一帧，PNG sha256 不同（确认在动而非静态渐变）；`::after` 计算样式为 `ca-rainbow-spin / 6s / infinite` + `conic-gradient(...)`、`::before` 为 `inset: 5px` 白底、logo 无滤镜；面板计算样式 `rgb(255,255,255)` / `14px` / `rgba(0,0,0,0.1)` 描边、头部 `background-image: none`；帮助弹窗白底 + 描边；控制台零报错；`node --check` 通过。实例接口复核：返回的脚本含 `--ca-bg: #ffffff` 与 `ca-rainbow-spin`、已无深色残留 `#17191e`。
- 样式样张：`docs/reports/debug-panel-harness/style-panel.png`、`style-help.png`、`style-more.png`（"更多"展开态）、`btn-t0.png` / `btn-t1.png`（律动两帧）。

## 开发中（2026-09-21 任务录制器：仓库链接改指 Rust 版 + 图标换成软件 logo、加彩虹环）

### 背景

- 录制器用户脚本（`resources/tools/task-recorder.user.js`）的仓库链接仍指向**已归档的 Python 版仓库** `Misyra/Campus-Auth`：`@namespace`、面板页脚 GitHub 链接（含显示文字）、帮助面板页脚链接共 3 处。主程序其余位置的仓库链接早在 `4ae78a7` 一代就统一为 `Misyra/Campus-Auth-rs`（`frontend/src/utils/constants.ts` 的 `APP_REPO_URL`、关于页、README、Docker 镜像名），只有录制器脚本漏改——它是从 `resources/` 原样分发、既不过前端构建也不受 CI 文案护栏覆盖的文件，所以一直没被发现。
- 图标：浮动入口按钮与面板标题用 emoji（🎬），Tampermonkey 安装列表里也没有 `@icon`（显示默认图标），录制器没有软件自身的视觉标识。

### 修复

- **3 处链接统一为 `https://github.com/Misyra/Campus-Auth-rs`**（`@namespace` / 面板页脚 / 帮助页脚），页脚显示文字同步为 `Misyra/Campus-Auth-rs`。任务分享仓库 `Misyra/campus-auth-tasks` 是有意保留的另一仓库，不动。
- **图标换成软件 logo（认证喵）+ 彩虹环**：
  - 浮动入口按钮、面板标题里的 🎬 换成 logo `<img>`（纯黑标，取自 `resources/icons/tray.png`），压在白底内圈上，外圈用 CSS `conic-gradient` 画彩虹环；环宽由 `--ca-ring` 收口（按钮 5px／标题 3px），两处共用同一套规则。
  - 彩虹环变量放在 `:root` 而不是面板变量块：浮动入口按钮挂在 `body` 上、不在 `#ca-recorder-panel` 作用域内，放面板里 `var(--ca-rainbow)` 取不到，环根本不会画出来（首版就栽在这，实测 `background-image: none`）。
  - `@icon` 换成 **96×96 合成图**（彩虹环 + 白底 + 黑标，透明背景）：Tampermonkey 列表/标签页只有一张图、画不进 CSS，环必须烤进图里；仍用 **data URI 而非 `raw.githubusercontent.com` 链接**，取图标不依赖联网/GitHub 可达，国内网络与离线场景都能显示。
  - 该文件没有构建步骤、`@icon` 只能是字面量，故 logo 分两处存在（`@icon` = 合成图，`LOGO_PNG` = 页面内两处用的纯黑标），注释里写明换图时三处（`tray.png` / `@icon` / `LOGO_PNG`）同步。

### 验证

- 真实 Chromium 实测（给 `GM_*` 打最小桩后加载用户脚本、点击浮动按钮展开面板）：浮动按钮 48×48、`background-image: conic-gradient(...)`、内圈 `inset: 5px` 白底、logo 26×26 无滤镜；面板标题徽标 26×26、内圈 3px、logo 15×15；面板页脚链接与文字均为 `Misyra/Campus-Auth-rs`；控制台零报错；`node --check` 语法通过。

## 开发中（2026-09-21 调试面板：单步/批量执行后补拍截图 + 弹窗放大）

### 背景（实测定位）

- 现场：任务调试面板逐步执行 `input` / `select` / `ocr` / `click` / `sleep` / `eval` 这类常规步骤时，右侧「实时截图」始终停在会话启动那一刻的画面，点「单步执行」预览不刷新。
- 根因：**整条调试链路里只有两处会推送 `screenshot` 事件**——`handle_debug_start` 的初始截图与显式 `screenshot` 步骤（`step_handlers.handle_screenshot`）。普通步骤的公共执行路径 `run_step_async` 只推 `step_progress`，一张图都不补，于是前端 `session.screenshot_url` 自始至终是初始帧。Rust 侧转发（`bridge/mod.rs` 事件白名单）、WebSocket 内联（`ws.rs::prepare_bridge_event`）与前端 `useWebSocket` → `handleScreenshot` 三段本身都是通的——缺的只是"源"。

### 修复

- **每步补拍一帧**（新增 `playwright_worker._capture_debug_frame`）：`handle_debug_step` 与 `handle_debug_run_all` 在记录步骤结果后调用，对当前活动页（`context.page`，弹窗/新标签页场景已是接管后的页）截图并 `emit("screenshot", {path, step_index})`。
  - 步骤帧用**视口截图**（`full_page=False`）：1280x720 视口体积小、无需整页拼接，既不拖慢单步响应，也避开长页 full_page 超过 Rust 侧内联上限（`DEBUG_SCREENSHOT_MAX_BYTES` = 8MiB）被静默丢弃的坑；会话初始帧仍取整页，保留"一眼看清门户全貌"。
  - 超时 `DEBUG_FRAME_TIMEOUT_MS = 5000`；失败（页面已关闭 / 超时 / 磁盘错误）只记 debug 日志并返回 False，**不影响步骤结果与命令响应**。文件名带步骤序号（`debug_{session}_{step}_{stamp}.png`），同一毫秒内重复截图不会互相覆盖；路径照旧登记进 `context.screenshots`，`debug_stop` 时统一清理。
  - `handle_debug_start` 的初始截图改为走同一实现（`full_page=True`），删掉那份重复的手写落盘逻辑。
- **预览标注归属步骤**：截图事件带 0 基 `step_index`（初始帧不带），前端 `useDebug` 新增 `screenshotStep`，面板标题右侧由固定「调试浏览器」改为「步骤 N 后」；`startDebug` / `stopDebug` / 裂图 `clearScreenshot` 时一并重置。
- **弹窗放大**：`Modal` 新增 `size="xxl"`（`max-width: 1200px; width: 96%; max-height: 92vh`），调试面板由 `lg` 改用 `xxl`；面板内部同步放大——截图列 `clamp(360px, 34%, 520px)`、步骤列表 `max-height: min(600px, 58vh)`、截图预览 `max-height: min(600px, 58vh)`、正文 `min-height: min(460px, 52vh)`；双栏折叠断点由 768px 提前到 900px（弹窗近乎全宽后该断点太晚，截图列 360px 的最小宽度会把步骤列挤到不可读）。
- **左右两栏重新分配宽度**：步骤列只需容纳「序号 + 状态 + 类型徽标 + 一句描述」（最长的一句约 150px，此前却占着约 900px），改为 `clamp(240px, 30%, 360px)`，剩余宽度全给截图。同时把两栏的高度都锁进同一份竖向预算（`calc(92vh - 295px)` = 92vh 减去弹窗页头页脚与体留白、面板信息条、截图头、余量），于是截图在够高时按列宽铺满、不够高时等比压高度，**两种情形都不会把面板顶出滚动条**（此前 1220x716 下溢出 58px）。步骤描述与结果文案补 `:title`，窄列下省略号截断仍可悬停看全。
- **截图点击放大**：预览区宽 390~520px，验证码/表单细节看不清，故截图整块改为可点按钮（`cursor: zoom-in` + 常驻「点击放大」角标 + `focus-visible` 描边，键盘也能开），点击弹出 `Modal size="xxl" preview` 大图：宽度铺满弹窗（1280 视口帧即接近 1:1），超出高度时可滚动看完整页。
  - 打开时**冻结** URL 与归属步骤：放大后再来新一帧会把画面换掉，冻住才能安心比对这一帧；标题显示「调试截图 · 会话启动」或「调试截图 · 步骤 N 执行后」。
  - 调试面板关闭（停止调试 / 会话结束）时放大窗一并关闭，不留悬空预览。

### 验证

- 真机链路实测（临时工装驱动真实 Worker + headless Chromium，`file://` 本地页三步 input → click → eval）：`debug_start` 得初始帧（整页 9836B），两次 `debug_step` 各补一帧（9972B / 8642B，均 1280x720 视口），三帧字节两两不同；第 2 帧可见 `alice` 已填入，第 3 帧页面已按点击结果变色（背景由绿转深蓝、标题变「登录中…」）——**预览确实随单步推进**；`debug_stop` 后截图文件全部清理。
- `python_worker/tests/test_worker_execution_contract.py` 新增 3 例（单步两帧且 `step_index` 递增、批量执行逐步补拍、补拍失败不影响步骤结果），`pytest` 全量 **200 passed**。
- `npx vitest run` **241 passed / 26 files**（`useDebug.test.ts` 新增 2 例：截图归属步骤 + `startDebug` 重置归属）；`npm run typecheck` 零错误。
- 弹窗尺寸工装实测（真实 CSS + Playwright 截图）：1512x950 视口下弹窗 **1200x705**（原 `lg` 上限 720px 宽）、960x760 下 922x640、860x820 下折为纵排且无横向溢出；放大窗按用户实际窗口尺寸（1220x716 CSS px）渲染为 1171x659、图片按原分辨率铺满。
- 两栏宽度/高度工装实测（真实 CSS，逐档量 bbox）：1220x716 下步骤列 336px、截图列 767px、图片 647x364、面板体溢出 **0px**；1220x780 下图片吃满列宽 751x422、溢出 0px；1512x950 下步骤列 345px / 截图列 787px / 图片 771x434、溢出 0px；1100x650 下图片 303 高、溢出 0px；≤900px 折为纵排（此时面板体正常滚动）。

## 开发中（2026-09-21 页面捕获：非 Chromium 渠道资源快照降级修复 + 离线还原副本）

### 背景（实测定位）

- 实测现场：浏览器渠道设为 `firefox`，对演示站（`http://127.0.0.1:8890/`）执行「捕获登录页面」并「保存页面文件」，zip 内只有 `meta.json` / `page.html` / `page_structure.json` / `screenshot.png`，无 `page.mhtml`、无 `resources/`；`page.html` 里的 `<link href="static/css/style.css">` 指向空气，离线打开无样式、无脚本。捕获结果 note 为 `资源快照失败: BrowserContext.new_cdp_session: CDP session is only available in Chromium`。
- 根因：**MHTML 完整快照（`Page.captureSnapshot`）与 CSS/JS 资源快照（`Page.getResourceTree` / `getResourceContent`）都只走 CDP**，而 CDP 是 Chromium 独有。firefox / webkit（含 custom 渠道配这两个引擎）下 `context.new_cdp_session` 在协议层就不存在——不是"失败"，是根本没有。同一份代码、同一站点、只换 channel 复跑对照：chromium 得到 `page.mhtml` + 2 个资源文件、note 为空；firefox 得到上面那份残缺产物。**"报错"与"快照很垃圾"是同一个原因。**
- 顺带暴露两个与渠道无关的缺陷：① `page_capture` 路径**从不改写** HTML 里的资源引用（只有 `feedback_capture` 改写），即 chromium 下 `resources/` 也只是躺在包里，仅因 MHTML 兜着才没暴露；② 资源只从 CDP 内存缓存取，已逐出或导航后才动态插入的取不到（逐项静默跳过）。

### 修复

- **引擎能力判定收口**（`_channel_supports_cdp`）：firefox / webkit（含 custom 配这两个引擎）判为不支持 CDP；`BrowserController._is_chromium_channel` 改为委托同一函数，避免启动参数过滤与捕获路径各写一份口径而漂移。
- **非 Chromium 渠道跳过 CDP 并给出可操作说明**：不再尝试必然失败的 MHTML / CDP 调用，note 改为中文（带具体渠道名 + 引导切到 Chromium / Chrome / Edge 后重新捕获），不再把 Playwright 的英文异常原样抛给用户。
- **引擎无关的资源兜底抓取**（`_http_resource_snapshot`）：页面内枚举已加载的 script / stylesheet（`script[src]`、`link[rel~=stylesheet]`、`link[rel=preload][as=style]`，加 `performance.getEntriesByType('resource')` 覆盖导航后动态插入与预加载的请求），再用 `context.request.get` 按 URL 回补正文（走浏览器网络栈，带 context 的 cookie / UA）。firefox 下这是唯一能拿到 CSS/JS 正文的路径；chromium 下作为补充，捞 CDP 内存缓存已逐出的资源。逐项容错，失败计数与首个原因随 note 上报（不再静默跳过）。
- **离线还原副本 `page.offline.html`**：有资源时把 HTML 里的资源引用改写成 `resources/<hash>.<ext>`，zip 一并分发，解压后双击即可脱网还原（无 MHTML 的渠道下这是唯一可离线查看的形态）。`page.html` 保持原始不改写——它是 Rust 侧喂给 LLM 的材质，URL 语义不该被污染。
  - **改写必须靠"原始书写形态"**：映射键是 CDP / 网络层的**绝对 URL**，而 HTML 文本里普遍是相对写法（`static/css/style.css`、`/css/a.css`），只按绝对 URL 做文本替换永远命中不了。初版 `page.offline.html` 与 `page.html` 字节完全相同（9463B）即栽在这里：资源确实落盘了，引用却一处没改。故枚举阶段额外采集属性的原始值作为别名（`_ResourceSink.alias`），且**无条件登记**——CDP 已拿到正文的资源不会再走回补，它的相对写法只能在枚举阶段补上（否则 chromium 下别名永远为空）。
  - 上限（200 文件 / 单文件 5 MiB / 总量 50 MiB）在 `_ResourceSink` 内统一判定，两条抓取路径共用，不会绕过预算撑爆捕获包。
- **`feedback_capture` 同步**（导出问题报告）：同样先判引擎再决定是否尝试 MHTML，page.html 的改写改用「绝对 URL + 原始形态」合并映射——它的相对 URL 改写此前同样命中不了。
- **Rust 侧**：捕获包 zip 白名单加入 `page.offline.html`（`GET /api/ai/capture/bundle`），`capture_bundle` 用例同步断言该文件随包分发。
- **文档**：`python_worker/README.md` 的 `page_capture` 命令行同步为产物的实际构成与渠道限制（MHTML 仅 Chromium、离线副本、非 Chromium 渠道的兜底抓取）。
- **前端**：AI 生成页第 2 步在渠道落在非 Chromium 引擎（`firefox` / `webkit` / custom 配这两者）时给静态预警——说明 MHTML 完整快照会缺、CSS/JS 由联网回补抓取、去「浏览器设置」切换渠道可拿回完整快照，文案与后端 note 同口径；「保存页面文件」按钮 title 同步为产物的实际构成。

### 验证

- 渠道对照实测（同一份代码、同一站点，仅换 channel）：chromium → `page.mhtml`(19.4KB) + `resources/*.css` + `resources/*.js` + `page.offline.html`，note 为空；firefox → 资源补齐为同样的 2 个文件 + `page.offline.html`，无 MHTML（协议层不可得），note 为上述中文说明。
- 离线副本 A/B 实测（abort 掉全部 http(s) 请求后以 `file://` 打开）：`page.offline.html` 样式生效（`.card-head` 底色 `rgb(43, 123, 211)`）、脚本执行成功（点击「忘记密码」弹出 `main.js` 绑定的 alert）、零网络请求被拦；同一目录的原始 `page.html` 两者皆无（底色透明、点击无反应）。文件名与引用路径与实际落盘的 `resources/af1562c10743.css` / `de91cec2f1eb.js` 一一对应。
- `python_worker/tests/test_resource_capture.py` 由 4 例增至 10 例（`pytest` 全量 **196 passed**）：CDP 过滤/逐出容错、超限跳过、逐出资源经 HTTP 回补、CDP 已落盘资源仍登记相对别名、firefox 不触碰 CDP（fake 会在被调用时断言失败）且 note 可操作、回补失败计数与成功项并存、枚举失败不抛异常、`_channel_supports_cdp` 口径表、`_resource_ext` 按种类兜底。
- `cargo test --lib` **939 passed / 0 failed / 1 ignored**；`cargo clippy --all-targets -- -D warnings` 零警告；`cargo fmt --check` 零差异；`npm run typecheck` 零错误；`npx vitest run` **239 passed / 26 files**。

## 开发中（2026-09-21 AI 生成页：OpenCode 渠道下架 + 模型下拉框 + 本地网关直连）

### 下架

- **OpenCode Zen 渠道整体下架**：前端预设卡片、后端 `infer_provider`（`opencode.ai` 分支）/ `validate_provider` / `configured_providers` 白名单、`openapi.json` 的 provider enum 与前端 `AiLlmConfig` 类型同步移除。指向 `opencode.ai` 的老配置在界面上回落到「其他兼容服务」（自定义服务不校验域名），因此**已有配置仍可保存**、不会被服务商标识校验拦下。
  - **实测依据（为何不是"修好"而是"删掉"）**：Zen 免费档的门控已从 `User-Agent` 收紧为「必须在 OpenCode 客户端内」。对同一模型发四组请求——不带原生头 / 带 `User-Agent: opencode/1.15.5` + `x-opencode-client: cli` + `x-opencode-session`（`ses_`）+ `x-opencode-request`（`msg_`）/ 各加与不加 HTTP 代理——**全部**返回 `403 {"type":"FreeTierError","message":"Error from provider (Console): OpenCode's free tier can only be used from within OpenCode"}`。逐模型探测结论一致：`mimo-v2.5-free`、`ling-3.0-flash-fin-free`、`muse-spark-1.2-contributor-free`、`nemotron-3.5-lightning-free`、`big-pickle` 均为此 403；`deepseek-v4-flash-free` 是上游 `Model is unavailable`；付费模型（如 `glm-5.2`）无真实 Key 时 `401 Missing API key`。`/models` 本身**无门禁**（不带任何头也能列出 74 个模型，含 `*-free`），门禁只作用在推理请求上——所以"能列出、调不动"。
  - 旁证：所参考的社区实现 `pgciq/pi-cn-free-model-providers` 也只保留「后台探测等官方放开」，其 README 明确声明当前没有已验证可供扩展匿名调用的免费模型。
  - 此前实现过的「伪装官方客户端」原生请求头（含 `ses_`/`msg_` 标识生成）、匿名 `public` Key 占位、以及为 Zen 403 单独收敛的中文提示（`AiError::OpenCodeFreeTierBlocked`）随渠道一并删除——留着一条必然 403 的路径不如删掉。**官方若放开，恢复该渠道需同时恢复请求头、预设与 Key 占位，本条目即为此留档**（含完整实测方法与结论）。

### 新增

- **模型下拉框 +「获取模型列表」**：AI 生成页的模型名不再只能手填。新增 `POST /api/ai/models`（body `{provider, base_url, api_key?}`）拉取服务商 `GET {base_url}/models`，前端把结果渲染成下拉框（`CustomSelect`，可滚动，上限 500 条）。`api_key` 缺省或为空时后端回退到该服务商**已保存**的 Key，因此「选服务商 → 拉列表 → 选模型 → 保存」这条顺序可用，不必为了看列表先存一次配置（GLM / DeepSeek 的 `/models` 需要鉴权）。
  - 不擅自改写当前模型：拉取结果命中列表才切到下拉模式，否则保持手动输入（私有模型、旧配置里的值都可能不在列表里）；下拉框恒带「自定义（手动输入）」项，拉取失败也能照旧手填。
  - 解析兼容三种响应形态：`{data:[{id}]}`（OpenAI 标准）、`{models:[{id}]}`、裸数组（字符串或 `{id}`/`{name}` 对象）；去重、保持服务端顺序。
  - 界面同时显示实际请求的 `/models` 地址（排障用）；`openapi.json` 同步补上该路径（路由表一致性用例 `openapi_json_matches_route_table` 强制）。

### 修复

- **「测试连接」不再把"回复被截断"当成连接失败**：该按钮原实现写死 `max_tokens = 8`，客户端拿到 `finish_reason=length` 后按 `AiError::Truncated` 判失败并回 503——对"先思考/话痨"型模型（GLM flash、各类推理模型、第三方聚合网关）必然误报，明明链路与凭据都正常。实测同一份保存配置（`cn:glm-5.3-flash`，第三方网关）：8 token 截断、64 token 仍截断、256 token 正常返回；即 8 这个预算是主要元凶。现在测试上限放宽到 256，并把「截断」与「HTTP 200 但无正文」按**连通**处理（响应附 `note: 模型回复被测试上限截断，连接与凭据正常`，前端 toast 一并显示），只有真正的传输/鉴权/协议错误才判失败。**生成路径的截断判定不变**（那里产物是 JSON，截断就是残缺）。
- **本地/私网 LLM 网关不再走系统代理**：reqwest 默认采用系统代理，但**不**套用系统代理的绕过列表——实测宿主机开着 `127.0.0.1:7890` 代理时，对 `127.0.0.1` 的请求会被转发给代理并回 `502 Bad Gateway`，本页「其他兼容服务」接 Ollama / LM Studio 这类本机网关（`base_url` 允许 http 回环/私网）直接不可用。该缺陷先于本次改动存在，做模型列表时被实测暴露。现在回环/私网基址强制直连，公网基址仍走系统代理——需要代理才能出网的场景不受影响。

### 验证

- `cargo test --features no-embed --lib` **937 passed / 0 failed / 1 ignored**（原 926，净增 11 例）；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo fmt --check` 零差异；`npm run typecheck` 零错误；`npm run test` **239 passed / 26 files**；`npm run build` 通过。
- 新增用例覆盖：`/models` 三种响应形态解析、空列表与非法 JSON 的错误、`api_key` 显式优先与已保存回退、不带伪客户端标识、回环/私网判定、非 2xx 原样透传、**测试连接对截断宽容而对 401 仍判失败**（均用本地 TCP 替身，不依赖外网）。
- 实机联调（本地实例 + 第三方聚合网关）：模型下拉框从 `https://buddy.amiya.cc/v1` 取回 **65** 个模型；「测试连接」在一次真实误报（503 截断）上定位并复现了上述 max_tokens 问题。
- 渠道移除后 `git grep opencode` 仅剩注释与"保证不再发该标识"的负向断言（`src/ai/llm.rs`、`src/ai/mod.rs`、`AiTaskView.vue`、`types.ts`）。
- 前端本次仅类型检查 + 单测 + 构建，未做浏览器截图核对（页面需要后端在线）——留待实机查看。

## 开发中（2026-09-20 更新弹窗：检查到新版本直接弹出并展示 GitHub 发布说明）

### 新增

- **更新弹窗**（新增 `frontend/src/components/UpdateDialog.vue` + `frontend/src/composables/useUpdateDialog.ts`，全局挂载于 `App.vue`）：检查到新版本时直接弹出，展示版本对比（最新 / 当前）、发布日期、包体积、本地包命中提示与**完整 GitHub 发布说明**（可滚动大区域 `max-height: min(52vh, 460px)`），底部为「立即更新（带下载进度条）/ 前往下载 / 在 GitHub 查看 / 稍后提醒」。关闭方式：右上角按钮 / ESC / **点击弹窗以外的任意区域**（`Modal` 的既有行为）。
- **三条触发路径统一到一个弹窗**：① 启动自动检查命中——`useUi.autoCheckUpdateOnStartup` 由"只发一条 toast"改为直接弹窗（原 toast 文案是"请前往设置页更新"，等于把入口又藏起来）；② 后端周期检查命中——`status.update_available` 由假转真时经 `initAutoOpen()` 注册的 watch 拉详情并弹窗，**此前这条路径前端完全没有提示**（只在设置页"上次检查"处留一句文字）；③ 设置页「立即检查」命中直接弹窗。
- **发布说明 Markdown 渲染器**（新增 `frontend/src/utils/releaseNotes.ts`）：零依赖，覆盖标题 / 无序与有序列表 / 引用 / 分隔线 / 围栏代码块 / 加粗 / 斜体 / 行内代码 / Markdown 链接与裸链接。标题整体下沉三级（`##` → h5、`###` → h6），避免与弹窗自身标题争层级；`stripTemplateTitle` 去掉发布流程写在正文开头的模板标题「## 更新日志」。
  - **安全**：所有文本在拼接进标签前一律转义（正文里的原生标签按纯文本显示），链接只放行 `http/https/mailto`，`javascript:` 等一律降级为纯文本，故调用方可安全 `v-html`。
  - 未引入 `marked` + `DOMPurify`：前端其余部分零运行时依赖（仅 vue / vue-router），为一页发布说明引两条依赖不划算。
- **「稍后提醒」记忆版本**：localStorage 记下忽略过的版本号，同一版本不再自动弹窗（手动入口不受影响），出现更高版本时照常弹。否则每次开机都弹会变成骚扰。
- **新增主程序仓库常量**（`frontend/src/utils/constants.ts`）：`APP_REPO_URL` / `APP_RELEASES_URL` / `releaseTagUrl(version)`。主程序仓库地址此前只在 `AboutView.vue` 模板里硬编码过一次，弹窗的「在 GitHub 查看」需要按版本拼 tag 发布页，再抄一份必然漂移。
- **设置页新增常驻「查看更新日志」入口**：不依赖"先点立即检查"即可打开同一弹窗看发布说明；已是最新时展示的是**当期发布**的说明（后端 `GET /api/check-update` 无论是否有更新都会带 `notes`），因此这个入口平时也有内容可看，不必非得等到有更新。

### 改进

- **设置页更新区瘦身**：移除 8em 高的 `update-notes` 纯文本小框（更新日志统一在弹窗里看），保留紧凑状态行（发现新版本 + 立即更新 / 前往下载 / 本地包提示）。
- `StatusSnapshot` 前端类型与 `mapBackendStatus` 补上 `update_available` 映射：后端早已在状态快照里推该字段，前端此前未消费，周期检查命中因此无提示。
- 启动检查命中不再额外发 toast：弹窗本身就是入口，避免同一件事两个提示。
- `useUpdateDialog` 对并发的检查请求做了去重（启动检查与周期检查可能同时命中，同一时刻只发一次 `GET /api/check-update`、只弹一次）。
- **修复 `openDialog` 的陈旧缓存**：原来只在"还没拉过数据"时才请求，于是弹窗一旦开过，即便后端周期检查已发现新版本，再点「查看更新日志」也永远显示旧结论「已是最新」。现在结果超过 1 分钟即重拉（1 分钟内复用缓存，避免连点重复请求），并补了对应用例。
- **实拍预览后修正三处**（用本地预览页逐状态核对时发现）：① 无更新时弹窗标题由「已是最新版本」改为「更新日志」——那句话是状态说明，当标题会读成"没更新还专门弹个窗通知你"；② 发布日期改按 **UTC** 显示，`published_at` 是 UTC，按本地时区（UTC+8）渲染会显示成次日，与发布说明标题里的日期对不上；③ 更新日志区加浅一档底色与描边，让"这块能滚动"的边界一眼可见；无更新时版本行只留「当前版本 vX」，不再与标题重复一句「当前已是最新版本」，「远程发布暂无当前平台的安装包」是唯一仍在无更新时额外说明的情况。

### 验证

- `npm run typecheck` 零错误；`npm run test` **238 passed / 26 files**（新增 `releaseNotes.test.ts` 16 例、`useUpdateDialog.test.ts` 8 例）；`npm run build` 通过。
- 渲染器用**线上真实发布正文**验证：拉取 `/releases/latest` 的 v5.0.2 `body` 过一遍渲染，输出为 `<h5>v5.0.2（2026-09-20）</h5><h6>修复</h6><ul><li>…</li></ul><h5>平台运行说明</h5><ul>…6 项…</ul>`，行内代码（`xattr -cr campus-auth` / `sudo apt install libgtk-3-0 …`）完整闭合、无原生标签泄漏；该正文原文已作为 fixture 固化进用例（发布流程改模板导致渲染退化会直接失败）。
- 后端、`openapi.json`、版本号均未变动（`notes` 本就是 `GET /api/check-update` 的既有字段，即 GitHub Release 的 `body`）。

## 开发中（2026-09-20 调试启动预检与首导航回落）

### 新增

- **调试启动新增"已联网且未配置登录网址"预检**（`src/web/routes/debug.rs`）：在浏览器渠道 + 认证地址留空 + 触发地址为空或等于内置默认值的形态下，先复用「重定向检测」同一份门户预检（`monitor::detect_portal`，各目标并行）判断当前是否已联网，命中直接返回 409 与提示「当前已联网且未配置登录网址，可能无法打开门户页面；请先退出校园网登录，或在「方案」页填入登录网址后重试」。
  - 动机：已联网时访问触发地址不会跳转，调试只会打开一页探测返回值（无登录表单），白拉一次浏览器且用户看不出所以然。
  - 前端**零改动**：`useDebug.startDebug` 已有 `extractApiError` → `toastOnly` 通道，409 的 `message` 直接成为 toast。
  - 只在"用默认触发地址"这一形态下探测（填了登录网址、或旧版方案自带触发地址时访问的是用户自己给的地址，联网时也可能真能打开门户，拦了即误报）；预检超时收敛为 `cfg.http_timeout.min(3s)`（新增常量 `DEBUG_PREFLIGHT_TIMEOUT`），它只是咨询性判定，不该让点击"调试"明显卡顿。
  - 放置位置在 JSON 对象校验与参数组装之后、Bridge 派发之前：非对象请求体（400）不触发任何网络探测。

### 修复

- **首导航取值收敛为三级**（`python_worker/playwright_worker.py` 新增 `_resolve_start_url` / `_profile_login_url`）：显式 `navigate_url`（登录强制去触发地址，逐字段等价于原实现）> 任务自身 `url` > Profile 有效登录地址（`trigger_url` 回落 `auth_url`，与登录同口径），三级皆空则跳过导航。
- **修复调试任务无起始地址时的两类失败**：原 `handle_debug_start` 只判 `if task.url:`（判的是字面量 `{{LOGIN_URL}}`，恒真）就 `page.goto` 解析后的空串——认证地址留空时以无效 URL 中断启动、会话根本建不起来；任务 `url` 为空时则停在新开空白页且无任何提示（真实登录此时会去 Profile 有效登录地址，两个入口口径不一致）。现回落 Profile 有效登录地址；两者皆空时记 warning 跳过首导航，任务自带的 `goto` 步骤仍可手动单步执行。
- **修复 `_run_task` 同形状缺陷**：原 `target = navigate_url or task_config.url` 的判空发生在 `resolve` 之前，`{{LOGIN_URL}}` 解析为空后仍会 `goto("")` 报无效 URL（直连渠道 + 认证地址留空 + 执行引用 `{{LOGIN_URL}}` 的任务即命中）。现判空在 `resolve` 之后；`handle_execute_browser_task` 传 `nav_fallback_url`，与调试共用同一规则。
- **变量未命中不再当地址用**：`resolve` 变量未命中会保留 `{{...}}` 字面量，`_resolve_start_url` 按空处理并回落，替代此前"把字面量交给浏览器报无效 URL"。
- 登录首导航计算收敛为 `_profile_login_url(params)`（原三行局部变量为重复实现，行为不变）。

### 验证

- `python_worker`：`uv run --frozen pytest -q` **191 passed**（新增 3 例：`test_start_url_prefers_task_then_falls_back_to_profile`、`test_debug_start_falls_back_to_profile_url_when_task_has_none`、`test_debug_start_skips_navigation_when_no_start_url`）。
- `cargo test --features no-embed --lib web::routes::debug` **13 passed**（新增 2 例：`start_blocked_when_online_without_login_url` 用本地 204 服务锁定"已联网 → 409 且不触达 Bridge"；`start_injects_default_trigger_when_login_url_not_configured` 锁定"未联网不误拦 + 注入默认触发地址"，顺带补上此前零覆盖的 `trigger_url` 注入断言）。
- `cargo fmt --check` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed --lib` 全绿：**926 passed / 0 failed / 1 ignored**。
- 接口（`openapi.json` 路径表与响应契约）、前端、版本号均未变动，故无版本提升；后端错误码复用既有 `CONFLICT`。

## 开发中（2026-09-20 修复：应用内更新下载完成后重启仍为旧版本）

### 修复

- **应用内更新「立即重启」后更新未生效（v5.0.0 → v5.0.1 实机复现）**：更新下载、校验、暂存、写 `pending.json`、唤醒更新助手全部正常，但前端「立即重启」确认框走的是通用重启接口 `POST /api/system/restart`——该接口先 `spawn_restart_successor()` 用**旧 exe** 生成 `--restarting` 后继进程，再优雅关闭主进程。后继进程锁住 `campus-auth.exe`（Windows 运行中的 exe 不可覆盖），助手替换必然失败（os error 32），回退同样失败，最终跑的仍是旧版本；且旧失败路径会清理 pending + staging，新包一并被删、下次更新需重新下载。实机日志证据：`helper.log` 两轮「替换失败 / 回退失败 os error 32」与 `app.log`「收到重启请求，生成后继进程」在同秒内先后出现。
- 修复口径：存在待应用更新（`update/pending.json`）时，重启入口**不生成后继进程**，走纯退出，由已等待的更新助手完成替换并用新 exe + `original_args` 重启（关机路径 `ensure_helper_for_shutdown` 会兜底补唤醒助手）。覆盖两个入口：
  - `POST /api/system/restart`（`src/web/routes/system.rs`）：分支日志措辞与实际行为一致，便于从日志区分两种重启路径；前端两条更新路径（「立即更新」与「手动选择安装包」）共用此接口，均被覆盖。
  - 定时自重启（`src/launcher.rs` `spawn_auto_restart_timer`）：覆盖「暂存后未立即重启、定时器到期自动重启」的同款场景。
- 配套：`UpdaterApi` trait 新增 `has_pending_update()`（`UpdaterService` 实现，`src/web/routes/system.rs` 两处测试 mock 补 `false`）。
- 纵深（助手失败保留现场）：助手替换失败时不再清理 `pending.json` 与 staging（`src/helper_main.rs`）。替换失败几乎必然是目标 exe 被占用（复制在打开目标阶段即失败，目标内容未被改动），保留现场供下次启动 `apply_pending_on_startup` → `self_replace` 重试（rename 语义不受目标占用影响）；包损坏场景到不了此路径（前序 SHA256 复核已拒绝并清理），`apply_pending_locked` 自身失败仍会清理，不会形成无限重试。

### 验证

- `cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed` 全绿：lib **924 passed / 0 failed / 1 ignored**（新增 `test_has_pending_update_reflects_pending_file`），集成测试各二进制全过（含 `updater_channels` 16 例）。

## 开发中（2026-09-19 README 优化）

### 文档

- 重构根目录 `README.md`：新增 CI / Release / License 徽章；「功能特性」按认证核心 / 自动化 / 界面与体验 / 运维分组；「使用说明」收敛为用户视角要点（移除 `ProfileSnapshot::uses_redirect_login`、`TaskKind` 等代码级引用，字段语义下沉至 `docs/guides/`）；新增「命令行速查」表（对照 `--help` 实际输出核对）与指南文档索引表（对照 `docs/guides/` 实际文件）；Docker CLI-only 用法折叠收纳；许可证章节补全为 AGPL-3.0-only（对齐 `Cargo.toml` 与 `LICENSE`）并保留使用场景声明。
- README 头部改为居中门面布局（标题 + 标语 + logo + 徽章）：新增 `docs/assets/logo.png`（由 `frontend/public/logo.png` 缩至 512×512）与 `docs/assets/logo-dark.png`（RGB 反相的白猫变体，GitHub 暗色主题下可见），经 `<picture>` + `prefers-color-scheme` 自动切换；原 logo 为纯黑剪影，暗色主题下不可见，故必须配暗色变体。
- 徽章区扩为两行并逐一实测：第一行 CI / Stars / Forks / Downloads / Issues / Contributors / License（shields.io 动态端点，实时取 GitHub API 数据，实测当前值 stars 12 / forks 2 / downloads 6.5k / issues 0 / contributors 2 / AGPL-3.0）；第二行为技术栈静态徽章（Rust / Tokio / Python / Playwright / Vue 3 / TypeScript / Vite / Docker，`logo=` 参数取 simple-icons slug，逐一验证存在性：axum 无 slug 故不上徽章、playwright 已从 simple-icons 移除故用纯色无 logo 版）。
- 新增「界面预览」区：本机隔离实例（`--base-path` 临时目录 + 独立端口，v5.0.1 debug 构建）实机截取三张 Web 控制台截图——仪表盘（初始化 Python 环境清除未就绪横幅、启动检测后截「公网连接正常 + 运行中」健康态）/ 方案编辑 / 任务管理；1280×800 圆角描边。
- **修预览图噪点（推后返工）**：首版为压体积对截图做 FASTOCTREE 256 色 + Floyd-Steinberg 抖动量化，浅色渐变区（侧边栏等）留下网纹状噪点、浏览器缩放后明显，用户反馈「怪怪的」；整体重截并改为 **WebP 无损**（`method=6`，374/259/314 KB，较 PNG 省 35%），无任何量化损伤；README 引用同步 `.png` → `.webp`（logo 与社交卡片仍为 PNG）。
- 新增「架构」区：mermaid flowchart（GitHub 原生渲染，无图片文件），表达使用入口 / Rust 控制平面（Axum、Engine、Scheduler、ConfigService、Updater、Bridge Supervisor）/ Python Worker（Playwright + ddddocr，按需拉起空闲回收）/ 认证门户的关系，与 AGENTS.md 架构口径一致。
- 开发与贡献末尾新增折叠的 Star History 图表（api.star-history.com SVG 外链）。
- 新增 `docs/assets/social-card.png`（1280×640，69 KB）：白猫 logo + 标题 + 标语 + 仓库地址的深色分享卡片；GitHub 社交预览图无上传 API，需仓库 Settings → General → Social preview 手动上传该文件。
- 「文档」索引表首行新增官网链接 `https://campus-auth.misyra.com`（下载入口 / 在线文档 / 更新日志）。附带发现官网部署缺陷（在官网仓库侧，非本仓库）：部分 JS 资源被构建为 `http://127.0.0.1:4173/assets/...` 绝对地址（vite preview 本地端口），访客浏览器必然加载失败，静态预渲染的文档正文不受影响但 SPA 交互失效；待官网仓库修正构建 base 后重新部署。
- 信息不丢失约束：README 精简掉的内容（重定向判定、API 路径、任务模型细节）均有既定去向（`AGENTS.md` / `docs/guides/` / `openapi.json`），未产生仅存于旧版 README 的信息。

## 开发中（2026-09-18 手动更新：`update/` 目录放置安装包 + 更新页手动选择安装包）

### 新功能

- **本地包复用**（`src/updater/local.rs` 新增）：把发布包放进 `<base_path>/update/` 根目录后，应用内检查更新发现新版本时若本地文件摘要与远程清单声明值一致，即复用该文件暂存，跳过网络下载。检查阶段仅回报 `UpdateInfo.local_package`（文件名/大小/摘要）供前端提示「已在 update/ 目录找到匹配的安装包 xxx，更新时将跳过下载」，按钮文案相应变为「使用本地包更新」。
- **手动选择安装包**（`POST /api/system/update-package` 新增 + 更新页「选择安装包」按钮）：浏览器选定本地压缩包上传，暂存后走与联网更新同样的替换流程。与本地包复用的**信任口径有意不同**：用户显式选定即采纳，**不比对远程摘要**——自编译包、他人重打包的镜像包摘要必然不匹配远程清单，按摘要拒绝会让该入口对最需要它的场景不可用。信任级别的下降是用户显式选择的结果，但硬约束保留：target 恒取 `current_exe()`（不接受上传方指定路径）、Worker 目录须为内置 `<base>/python_worker`（外置/Docker 布局拒绝）、版本须**严格高于**当前（同 helper `pending_version_allowed`，否则写下的 pending 会被 helper 拒绝、留下永远无法应用的待定更新）、每包 512 MB 上限、文件名净化（只取末段，防路径片段变成落盘名）。
- **两条路径共用同一落盘序列**：抽出 `UpdaterService::finalize_staged_package`（解压产物 → 实算 exe 摘要 → 写 `pending.json`），本地包/上传包/网络下载三路都必须经过它，任何一路绕过都会形成"helper 认可的更新但校验不完整"的旁路。exe 摘要始终由本进程从解压产物实算，不接受外部声明值。
- **上传不把整包读进内存**：压缩包可达数百 MB，`field.bytes()` 会同时持有 axum 缓冲与 Vec 两份副本。Web 层把 multipart **流式落到临时文件**（分块写盘 + 上限即拒），更新器收**路径**再流式复制到 staging，内存占用与包大小无关。
- **错误映射分层**：`PackageNotNewer` / `ExtractFailed` / `DownloadTooLarge` → 400（用户可纠正的输入错误，如"包太旧""不是有效压缩包"）；`LoginInProgress` / `UpdateInProgress` → 409（调用时序冲突）；其余 → 500。
- **更新页说明块**：自动更新卡片顶部新增默认折叠的说明条，讲清两条手动入口的区别，并显示实际 `update/` 目录绝对路径（取自 `GET /api/system/info` 的 `base_path`，新增 `systemApi.info` 绑定）与一键复制；取不到路径时降级为相对描述，不打断本页其他功能。
- **本地包复用仍不支持完全离线**：摘要来自远程清单，取不到清单即无从校验（fail-closed，不做"信任本地文件"降级）——离线场景由新增的「选择安装包」入口覆盖。

### 验证

- `cargo fmt --check` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed` 全绿（lib **923 passed / 0 failed / 1 ignored**；`tests/updater_channels.rs` 由 7 → **16 例**）；前端 `vue-tsc` 零错误、`npm run build` 通过、vitest 全绿（本条落地时为 212 例；随后同日「设置页按钮缺 type」修复新增 2 例守卫，终值 **214 passed / 24 files**）。
- **新增集成用例（真实实现，非 mock）** 5 例：正常路径 staging + `pending.json` 且 `pending.sha256` 等于解压后 exe 的实算摘要、版本不高于当前时拒绝（`PackageNotNewer`）、外置 Worker 布局拒绝（`UnsupportedSelfUpdateLayout`）、非压缩包拒绝、zip 内无可执行文件拒绝；后四例同时断言**不得写入 pending**。mock 回环新增 `/mirror/current.json`（清单版本 = 当前版本）作为"不高于当前"的受控来源。
- **新增路由级用例** 5 例（`web::routes::system`）：字节完整送达更新器（比对内容而非路径——上传用临时文件承载，句柄随请求销毁）、空文件 400、缺 `file` 字段 400、包问题 → 400、登录中 → 409。
- **新增 `updater::local` 用例** 4 例：文件名净化（末段/分隔符/前导点/超长回退）、上传包复制往返（跨多分块）、超限拒绝且清理半写入目标、大小声明不符仍按摘要命中。
- **变异验证**（强制重编后确认，均改回并复原）：① 去掉上传路径的版本闸门 → 「不高于当前应拒绝」失败；② `pending.sha256` 改为固定值（模拟写成压缩包摘要）→ 「sha256 应为解压后 exe 的摘要」失败。
- **两次"变异未生效"的排查教训**：用 `Copy-Item -Force` 还原被变异文件时其 mtime 被保留为旧值，cargo 判定产物新鲜而跳过重编，导致"变异后仍全绿"的假象。**结论：变异验证必须显式刷新被改文件的 mtime 再跑**，否则得到的是上一次的构建结果。
- 未做实机验证：本机 `agent-browser` 起不来（既有记录，见 v5.0.0 条目）。**该限制已于同日被绕过**：改用 `python_worker/.venv` 内的 Playwright 直连 CDP，完成了真实浏览器的取证与回归（见同日「设置页按钮缺 type」条目），本条目的更新页说明块渲染已由该轮脚本覆盖。

### 说明

- 本轮新增 `openapi.json` 的 `/api/system/update-package` 路径（multipart 请求体、含 409 响应）；`openapi_json_matches_route_table` 漂移护栏在本轮**实际拦住了**漏加该路径（先失败后补全），验证了护栏有效性。
- 用户可见操作步骤更新到 `docs/guides/user-guide.md` §9 的「手动更新（两条入口）」小节；`AGENTS.md` 的「Updater」要点补记两条入口的信任口径差异与共同落盘序列。

## 开发中（2026-09-19 修复：设置页「登录一次后退出」登录成功后程序不退出）

### 缺陷修复

- **用户报告**：设置页「启动后执行」选「登录一次后退出」，登录成功后主程序并不退出。核实属实：前端文案承诺「启动后执行一次登录，成功后自动退出程序」（`SystemSettings.vue` 的选项 label 与提示、`runMode.ts` 的差异提示均同口径），但后端 `apply_startup_action` 的 `LoginOnce` 分支只 spawn 后台任务提交登录并打结果日志，**没有任何退出逻辑**，程序照常以完整/轻量模式驻留——UI 承诺与实现相反。全局检索 `LoginSource::LoginOnce` 消费点确认无其他退出路径。
- 辨析：CLI `--mode login-once`（`launch_login_once`）的「登录一次后退出」实现正确（等终态 → `graceful_shutdown` → 进程退出，成功 exit 0 / 失败 exit 1），缺的只是 `startup_action=login_once` 这条入口。

### 修复

- `apply_startup_action` 的 `LoginOnce` 分支：登录**成功**后取消应用级关闭令牌（`state.shutdown_token.cancel()`），由既有的 `wait_for_shutdown` → `graceful_shutdown` 链路统一收尾退出（调度器 / Engine / Bridge / Axum 逆序有界关闭），与 CLI login-once 共用同一退出路径；成功日志补「按 startup_action 配置退出程序」，`apply_startup_action` 的 doc comment 补记该口径。
- 失败 / 取消不退出，保持驻留：用户可打开 Web 控制台查看失败原因并重试，与文案「成功后自动退出」的字面口径一致。
- 并发安全性复用既有机制，无新增原语：托盘 / Web 关闭若先于登录完成触发，登录后台任务被 `abort_background_tasks` 中止，在途会话经容器内 shutdown child token 以「应用关闭」取消终态收尾（A3 既有链路）；登录若先于启动编排后续步骤（更新检查 / 自重启计时 spawn、`wait_for_shutdown` 注册）完成，这些步骤创建后立即随令牌取消而退出，无竞态窗口。

### 验证

- `cargo fmt --check` 零差异；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo test --features no-embed --lib launcher` 通过。
- 未跑真机 E2E：改动为单点行为（成功 → cancel token），退出链路整段复用 CLI login-once 已验证路径，无新增并发原语与数据面改动。

### 说明

- updatelog 新增「尚未发布（开发中）」段落补入用户可见条目，随 5.0.1 发版冻结为 `v5.0.1` 章节（v5.0.0 tag 不含本修复，不得落在 v5.0.0 小节）。
- Docker 部署若配置 `startup_action=login_once`，登录成功即退出进程，容器按 restart 策略重启后再次登录——这是该选项语义的自然结果，与 CLI 模式一致。

## 开发中（2026-09-19 修复：设置页按钮缺 `type` 导致点击后整页刷新）

### 缺陷修复

- **用户报告**：「点击手动选择会刷新一下」。实测确认：点「选择安装包」会触发**原生表单提交**并整页重载，且提交时**token 查询串被覆盖掉**（`?token=...` → `?`），随之丢失鉴权上下文。

### 根因（Playwright 取证，非推断）

1. `SettingsView.vue` 用 `<form autocomplete="on">` 包裹 `<router-view />`（只为拿浏览器自动填充），且**没有** `@submit` 处理器；表单 `method=get`、`action` 为当前 URL。
2. HTML 规范：`<button>` 缺 `type` 时其 IDL `type` 默认为 `"submit"`。新增的「选择安装包」按钮漏写 `type="button"`，于是点击即触发表单提交 → 导航到当前 URL → SPA 重载。
3. **取证结论**（`docs/reports/verify-submit-bug.py`，用 `sessionStorage` 跨导航持久化记录——初版用 `window` 变量记录，整页重载后 init script 重置数组，等于把证据擦掉）：submit 事件确实触发、`event.submitter` 即该按钮、提交后 URL 变为 `?`。

### 为何同类按钮当时未暴雷（关键，决定修法）

审计发现表单内共有 **4 个** 按钮的 IDL `type` 为 `submit`（`选择安装包` / `立即检查` / `重新加载配置` / `初始化 Python 环境`），但**只有新增那个真的重载**。机制：浏览器在 click 派发完成后才执行默认动作，其间会检查 submitter 是否已 `disabled`。

- 「立即检查」处理器**首行**同步置 `updateChecking=true` → Vue 微任务刷 DOM → 按钮 disabled → 默认动作被取消；
- 「选择安装包」首行是 `await pickFile(...)`，置位发生在其后 → 默认动作窗口内仍启用 → 提交发生。

**这是偶然保护**：只要置位挪到任一 `await` 之后就立即失效。故不能依赖「先置 busy」，采用三层守卫（缺一不可）：按钮显式 `type="button"` + 表单 `@submit.prevent` 兜底 + 静态守卫测试。

### 修复

- **根因层**（`SettingsView.vue`）：`<form class="settings-form" @submit.prevent>`。该表单只为自动填充而存在，**没有**标签页提交语义，拦截 submit 后即使将来再漏写 `type` 也只会"不提交"而不会刷新页面。
- **直接原因层**：补齐 4 个按钮的 `type="button"`（`NetworkSettings.vue` 的 选择安装包/立即检查/立即更新/重新加载配置、`TaskEnvironmentSettings.vue` 的两个环境引导按钮）。
- **顺带收敛**：`AppearanceView.vue` 随机壁纸弹窗的两个按钮（其在 `Teleport to="body"` 的模态框内，**不在**表单里、本无实害）也补上显式 `type`，使守卫规则可以定为"设置页内所有按钮都必须显式声明 `type`"，无需为 Teleport 例外开口子。
- **新增静态守卫测试**（`frontend/src/views/settings/settingsForm.test.ts`）：断言 `form.settings-form` 含 `@submit.prevent`、且设置页各子组件所有 `<button>` 均显式声明 `type`。这类缺陷类型检查与构建都发现不了（属性可有可无、渲染正常），只能靠源码级断言拦住回归。

### 验证

- **真实浏览器取证 + 回归**（Playwright + 修复后二进制，`docs/reports/verify-fix.py`）：
  - 三个按钮真实点击均无 submit 事件、URL 保留 token（修复前「选择安装包」有 submit 且 token 丢失）；
  - **纵深防御实测**：向表单**注入**一个故意不写 `type` 的探针按钮（IDL `type='submit'`、`form.contains()` 为真），点击后**未发生整页重载**——证明漏写 `type` 时兜底生效；
  - 功能未破坏：点「选择安装包」仍正常打开文件选择器（`filechooser` 事件触发）；
  - 全 6 个设置 Tab 审计：表单内可由按钮触发的 submit **0 个**。
- **守卫测试变异验证**（强制重编/重跑后确认，均已复原）：① 去掉 `@submit.prevent` → 第一个用例失败；② 给「选择安装包」去掉 `type="button"` → 第二个用例失败并点名该文件与标签。
- 前端 `vue-tsc` 零错误、`npm run build` 通过、vitest **214 passed / 24 files**（新增 2 例）。
- 后端本轮无改动，`cargo test` 未重跑（上一轮全绿结论不受影响）。

### 说明

- 过程报告与取证脚本留在本地忽略目录 `docs/reports/`（`verify-submit-bug.py` / `verify-fix.py` / `probe-*.py` / `audit-submit-buttons.py` / `diag-settings-page.py`），不入仓。
- 遗留同类风险面（**未修，仅记录**）：`<form>` 之外若将来出现「包在 form 里的按钮」仍需靠守卫抓；守卫目前只覆盖 `views/settings/` 与其父 `SettingsView.vue`，其他页面若引入 `<form>` 需同步扩展守卫范围。

## 开发中（2026-09-18 登录网址单输入 + 重定向离线误判兜底）

### 交互简化

- 方案页移除“重定向模式”开关，将浏览器登录收敛为一条规则：**登录网址填写时直接使用，留空时由浏览器访问触发地址并跟随校园网重定向**。旧版 `trigger_url` 非空的方案继续按重定向运行；用户填写新的固定网址时会清除旧触发值，避免两个地址互相覆盖。
- 默认触发地址统一为 `http://www.msftconnecttest.com/connecttest.txt`，只在运行时补入，不写回空白 Profile；自定义入口收进默认折叠的“重定向高级设置”，并明确提示非特殊网络无需修改、触发地址必须使用明文 HTTP。
- 删除会把候选门户地址写入表单的“自动检测 / 检测并填入”：方案页、直连向导与 AI 捕获页均不再自动回填可能带一次性 token 的重定向地址。
- 方案页在“认证地址”上方新增“重定向检测”：先用 HTTP 预检识别当前已联网并提示退出校园网登录；其余情况强制启动可见浏览器，在独立临时会话中等待跳转并综合页面地址、表单结构及“校园网 / 登录 / 认证”等中英文语义判断。识别到认证页时提示认证地址无需填写，未识别时提示重试或手动填写，全程不修改方案草稿。
- 重定向检测使用独占 Bridge 会话，避免与登录、浏览器任务或调试会话互相关闭窗口；临时浏览器不复用正常登录的 Cookie / localStorage / 持久化上下文，检测结果不返回最终 URL 或页面正文，避免门户临时参数进入前端状态与日志。

### 重定向“没网”修复

- 修复严格模式下重定向门户把所有公网探测请求直接丢弃时，系统恒判 `Offline + WaitForNetwork`、永不启动浏览器的问题：当且仅当 Profile 使用重定向、探测未确认在线且本地网卡确实有可用地址时，将结论标记为低置信度 `RedirectLoginAssumed`，谨慎启动一次浏览器访问触发地址。
- 物理链路不可用、网卡检查失败或未启用任何有效公网探测时仍保持原结论；兜底使用 `AttemptLoginOnce`，同一门户事件与配置版本只放行一次，避免普通断网时反复拉起浏览器。
- `ProfileSnapshot` 新增统一的重定向判定与有效首导航地址解析；登录、普通浏览器任务和调试会话共用该口径，避免空地址在不同入口退化为 `about:blank`。

### 验证

- `cargo test --lib config::runtime::tests --features no-embed`：5 passed；`cargo test --lib monitor:: --features no-embed`：58 passed；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告。
- `frontend npm run build` 通过，Vitest 23 files / 212 tests 全通过；真实 Rust 本地实例 + Playwright/Chromium 验证登录网址留空、填写、重新清空、自定义触发地址展开四条交互均通过，且无页面脚本错误。

## 开发中（2026-09-18 修复：engine 测试因新增重定向兜底分支而挂起）

`5d6f962` 新增「重定向登录兜底」后，`engine::run_loop::tests::test_background_probe_keeps_commands_responsive_and_merges_reentry` 在 Windows / Linux / macOS 三平台**稳定失败**（CI run 35329391997 三平台失败点一致，均为 `run_loop.rs` 的 `等待条件超时（虚拟时钟 1.5s 内未满足）`）。

### 根因

测试 helper `make_engine_with_hanging_probe` 用默认 `ProfileData`（`auth_url` 与 `trigger_url` 均为空、渠道为 Browser），此时 `uses_redirect_login()` 返回 **true**。改动前 `want_local_link` 在严格模式下被 `!strict_login_mode` 短路为 false；改动后新增的 `redirect_fallback` 分支**不再被该短路覆盖**：

```rust
let redirect_fallback = purpose == CheckPurpose::AutoMonitor
    && cfg.strict_login_mode                       // true
    && rt.profile.uses_redirect_login()            // true（两地址皆空）
    && redirect_fallback_candidate(&assessment);   // true
// → want_local_link 变为 true，首次进入 probe_local_link()
```

而该 helper 注入的 `HangingDetect::list_interfaces()` 是永久 `pending()`，于是探测永不回传：实测推进 **40s 虚拟时钟仍失败**，证明并非超时预算不足。插桩确认卡点：`[C] want_local_link=true` → `[D] awaiting probe_local_link` → 永无 `[E]`。

**仅 1 个用例受影响**：另两个共用该 helper 的用例分别因全天暂停门控、以及走 `ManualDiagnostic` 路径（不经过 `redirect_fallback` 判定）而未进入该分支。

### 修复（仅改测试，不动生产逻辑）

`make_engine_with_hanging_probe` 显式写入「非重定向」形态的 Profile：`auth_url = "http://127.0.0.1:9/login"`（非空使 `uses_redirect_login()` 为 false），并把 `auth_url_timeout` 从默认 5s 收紧到 1s（对齐 `monitor_with_http_target` 既有范式）。选 `127.0.0.1:9` 是因为该端口无监听、连接被**立即拒绝**，`inspect_auth_endpoint` 直接返回 `Unreachable` 而不消耗超时预算。

- 新增**前提守卫**：在 helper 末尾断言 `!uses_redirect_login()`。若该前提再被配置默认值或判定逻辑变更打破，失败点会直接指向原因，而非只抛笼统的「等待条件超时」——本次排查耗费较多轮次正因缺少此断言。

### 验证

- 目标用例 + 另两个共用 helper 的用例：均通过
- 插桩确认**测试未被架空**：探测真实执行（`n=1`）、`local_link=NotChecked`、`auth=Unreachable`
- `cargo test --lib`：**908 passed / 0 failed / 1 ignored**（修复前 907 passed / 1 failed）
- `cargo fmt --check` 零差异；`cargo clippy --all-targets -- -D warnings` 零警告

### 说明

- 该失败**非本次性能改动引入**，而是 `5d6f962` 已存在于 master 的缺陷；推送时随 master 一并触发 CI。
- 生产行为**无缺陷**：真实用户配置了登录网址时 `uses_redirect_login()` 返回 false，不入该分支。副作用仅是「未填登录网址的 Browser 方案」每轮失败探测会多一次网卡枚举（`5d6f962` 的有意设计），非本项修复范围。

## 开发中（2026-09-18 性能：/ws/logs 收窄单连接缓冲，16 连接常驻内存降低约 2 MB）

内存专项分析（见 `docs/reports/perf-profile-2026-09-18.md` §7.1）实测每 WebSocket 连接私有内存 **0.154 MB**（16 连接合计 2.46 MB），是控制平面最大的可变内存项。

### 修复（内存占用）

- **收窄 `/ws/logs` 单连接读/写缓冲**（`src/web/ws.rs`）：tungstenite 默认读缓冲 **128 KiB**、写缓冲 128 KiB，且读缓冲是 **eager 分配**（见 `tungstenite/src/protocol/mod.rs` 的 `read_buffer_size` 文档：「This buffer is eagerly allocated」）。该默认值面向「高读负载」场景，而本端点入站仅有 ping 心跳与 `frontend_log`（后者已在 `record_frontend_log` 截断至 KB 级），128 KiB 明显过大。
  - 新增 `WS_LOGS_READ_BUFFER_SIZE = 4 KiB`：入站无高负载需求，4 KiB 足够；
  - 新增 `WS_LOGS_WRITE_BUFFER_SIZE = 16 KiB`：出站单帧最大为状态快照（~0.9 KB）与截断后的日志条目，16 KiB 可容纳单帧而非频繁扩容。
  - 读缓冲调小**不影响大消息**：缓冲只是分块读取粒度，消息总长仍由既有 `max_message_size`/`max_frame_size`（64 KiB）约束 —— 已实测 64 KiB 级消息在 4 KiB 读缓冲下正确接收且连接存活。

### 实测效果（同脚本 A/B，私有内存增量）

| 并发连接 | 修复前每连接 | 修复后每连接 | 修复前总增量 | 修复后总增量 |
|---|---|---|---|---|
| 4 | 0.136 MB | 0.016 MB | 0.54 MB | 0.06 MB |
| 8 | 0.144 MB | 0.017 MB | 1.15 MB | 0.13 MB |
| 12 | 0.149 MB | 0.021 MB | 1.79 MB | 0.25 MB |
| 16（上限） | 0.154 MB | **0.033 MB** | 2.46 MB | **0.53 MB** |

- 每连接内存**降低约 79%**，16 连接（当前上限）总增量 **2.46 MB → 0.53 MB**。
- 5 轮「16 连接建立→断开」循环峰值：**11.32 MB → 9.20 MB**，与节省量吻合；两版本均不累积（峰值逐轮稳定，属分配器保留空闲页而非泄漏）。

### 验证

- `cargo fmt --check` 零差异；`cargo clippy --all-targets -- -D warnings` 零警告
- `cargo test --lib`：**899 passed / 0 failed**（连续 3 次全量并行）；`cargo test --test login_chain`：5 passed
- 端到端功能验证：WS 连接建立后每秒 1 帧状态快照正常；4 KB 前端日志正确回显；64 KiB 级入站消息正确接收、连接与进程均存活

### 说明

- 本项与同日的 `/api/history` 修复（见上一条）是同一轮内存/性能分析的两项产出：前者消除随历史量增长的 CPU/延迟，本项降低多连接常驻内存。
- 内存分析的其余结论（Virtual Bytes 4.27 GB 为地址空间预留、无泄漏、7 MB 私有内存基线）见性能报告 §4 与 §7.1，均**无需改动**。

## 开发中（2026-09-18 性能：/api/history 改为按天倒序早停，消除随历史量线性增长的解析开销）

性能分析（见 `docs/reports/perf-profile-2026-09-18.md`）实测发现：`GET /api/history` 先读 30 天**全量**记录，再按 `limit` 截断，导致 `limit` 对 I/O 与解析量**完全没有节流作用**——前端只需 30 条，服务端却解析全部历史。

### 修复（性能缺陷）

- **`HistoryStore` 新增 `query_latest(from, to, limit)`**（`src/login/history.rs`）：语义等价于 `query` 后取末尾 `limit` 条，但实现**按天倒序**（最新在前）逐天读取，累计条数达到 `limit` 即停止，不再触碰更早的日期文件。成本由「最新一天的文件规模」决定，而非与 30 天历史总量相关。
  - **刻意不给默认实现**：该方法存在的唯一理由就是省掉全量读取，若提供「先全量再截断」的默认实现，实现方漏覆写即静默退化为原性能问题，故由编译器强制实现（仅 `LoginHistoryService` 与测试 `MockHistory` 两处实现）。
  - **必须显式按时间排序，不得依赖物理文件顺序**：`query` 末尾本就有 `sort_by_key`，即代码库的既有口径。`record` 每条新开 tokio `File`，而 tokio `File` 内含写缓冲、drop 时仅**尽力**异步 flush，紧密连续写入时落盘先后可能不同于调用顺序。天与时间的对应是可靠的（`record` 以 `entry.timestamp.date_naive()` 选文件），但**同一文件内不可假设物理顺序等于时间顺序**。
  - 空行与损坏行沿用 `query` 的静默跳过策略，且**不占用** `limit` 配额——否则损坏行会挤占返回条数，与「全量后截断」语义不一致。
- **handler 按 `limit` 是否存在分派**（`src/web/routes/history.rs:48`）：传 `limit` 走 `query_latest`，不传仍需全量（保持既有契约）。分页路径与响应格式**未改动**，对外契约不变（`openapi.json` 路由表未变，`openapi_json_matches_route_table` 用例仍通过）。
- **新增 9 个单元测试**覆盖：与「全量取尾」的等价性（核心不变式）、升序性、跨天取数、损坏行不吃配额、`limit=0`/超总量/空目录边界、单日大文件、**多天 × 多文件真实量级（30 天 × 167 条）**、**物理行序乱序**。

### 过程记录：首版实现不可靠，已由测试捕获并改正

首版实现为「从文件尾部按 64 KiB 分块倒读、凑够 `limit` 即停」，**该实现基于一个错误假设**：文件物理顺序等于时间顺序。

- **症状**：新增的 3 个用例在**全量并行**测试下约 1/5 概率失败（`结果必须按时间升序`），单独运行该模块却始终通过。非确定性使问题极易被误判为「偶发」。
- **根因**：`record` 每条记录新开一个 tokio `File` 写入后立即 drop，而 tokio `File`（`src/fs/file.rs`）内含 `max_buf_size` 写缓冲，drop 时是**尽力异步 flush**；紧密循环写入同一文件时，物理落盘顺序可能与调用顺序不同。倒读早停把「物理靠后」误当作「更新」，因而漏选或错选记录。
- **修正**：改为按天倒序 + 单日文件**整读后排序**。天级早停仍然有效（最新一天读满 `limit` 即停），因此性能收益保留；同时新增 `test_query_latest_handles_physically_unsorted_file`，以**刻意乱序写入**把该假设钉死为可确定复现的失败条件，不再依赖并发碰运气。
- **教训**：低概率非确定性失败必须查到根因，不能以「重跑通过」收尾——该问题只在并行全量下暴露，正是 CI 最易漏判的形态。

### 实测效果（A/B 同一脚本、同一数据，每档 300 次请求累加以规避 15.6 ms 量化误差）

| 累积历史条数 | 修复前 CPU | 修复后 CPU | 修复前延迟 | 修复后延迟 |
|---|---|---|---|---|
| 90 | 0.83 ms | 0.10 ms | 5.82 ms | 1.69 ms |
| 990 | 0.94 ms | 0.00 ms | 7.91 ms | 0.49 ms |
| 5 010 | 3.23 ms | 0.00 ms | 20.35 ms | 0.79 ms |
| 20 010 | **10.16 ms** | **0.26 ms** | **61.27 ms** | **1.84 ms** |

修复后 990 → 20 010 条之间 CPU 基本持平，**不再随历史总量增长**；20 010 条时延迟从 61 ms 降到 1.8 ms。正确性经逐条比对确认：20 010 条下全量返回与落盘一致、各 `limit` 均等于全量取尾且升序。

### 验证

- `cargo fmt --check` 零差异；`cargo clippy --all-targets -- -D warnings` 零警告
- `cargo test --lib`：**899 passed / 0 failed**（history 模块 23 例，含新增 9 例）；并**连续 12 次全量并行**运行确认非确定性已消除
- `cargo test --test login_chain`（读 `/api/history` 的真实链路）：**5 passed / 0 failed**

### 说明

- 性能报告早期版本中该缺陷的数字（10.05 ms / 60.88 ms）来自**字段写错的合成数据**（用了 `duration_ms`，而 `LoginHistoryEntry` 要求 `duration_secs` 且无 `serde(default)`，导致每行反序列化失败、接口返回空数组）。已用与结构体严格对齐的数据重新测量并替换，趋势与结论方向不变。报告中同时标注了该修正原因。

## 开发中（2026-09-18 直连请求渠道缺口修复：HTTPS 证书可配、UA 兜底、响应头回显、保存校验体积）

对直连请求渠道（`src/login/http_login.rs` + `ProfileData.http_*`）做缺口复核后修复四项；复核结论与剩余待办（Cookie/两步门户、判定只看响应体、方法仅 GET/POST 等）见 `docs/plan-next.md` 的「直连请求渠道待办」。

### 修复（可用性缺口）

- **HTTPS 证书策略与浏览器渠道口径不一致（P1）**：直连客户端此前用 reqwest 默认 rustls 严格校验，而浏览器渠道与监测客户端都读 `browser.ignore_https_errors`（默认 **true**）。校园网门户大量使用自签名证书，后果是**同一门户浏览器能登、直连必然 `NetworkError` 且无从配置**。现新增方案级三态字段 `ProfileData.http_ignore_https_errors`（`Option<bool>`）：`None` = 跟随全局 `browser.ignore_https_errors`，`Some(bool)` = 本方案显式覆盖（可收紧为严格校验，代价是自签门户登不上，UI 已写明）。
  - 解析点收口在 `HttpLoginRequest::from_profile(profile, global_ignore_https_errors)`；测试端点同源解析（未提交该键时读 `runtime_snapshot().browser.ignore_https_errors`），避免出现「测试报证书错误、正式登录成功」这类无从判断该信哪边的组合。
- **未发送 User-Agent**：reqwest 在未显式设置时**完全不发** `User-Agent`（已对照 reqwest 0.12.28 源码确认），部分门户/WAF 据此返回 403 或另一套页面，表现为"抓包看不出问题、直连就是失败"。现 `build_client` 统一补浏览器 UA 兜底；用户显式写了 `User-Agent` 请求头则以其为准（用例锁定不被覆盖）。
- **响应头不可见**：判定与排查此前只看响应体。`HttpAttemptReport` 新增 `response_headers`（逐行 `Key: Value`，上限 8 KiB），经既有凭据字典脱敏后进入测试结果面板与 IPC `data`——排查「中文乱码导致关键字命中不了」「302 跳转去向」这类问题的线索正是 `Content-Type`/`Location`。

### 修复（一致性）

- **保存路径不校验直连配置体积**：`HttpLoginRequest::validate()` 此前只在执行与测试端点调用，超限配置能静默落盘、直到登录执行才报「过长」，用户看到的是"保存成功"。现新增纯模板校验 `validate_templates`，由 `POST /api/profiles/{id}` 与 `PUT /api/profiles/{id}` 在落盘前调用（与执行路径同一口径：URL 8 KiB / 请求头 64 KiB / 请求体 256 KiB / 关键字各 8 KiB / 脚本 128 KiB）。指南 §8 相应从"保存不校验"改为"超限保存会被拒绝"。

### 连通面

- `ProfileData` / `ProfileSnapshot` / 创建与更新 DTO / `PATCH /api/config` 的 Profile 域白名单与扁平响应 / 方案导出导入（`Some(bool)` 显式解析，缺失即 `None`）/ 前端 `types.ts`（`Profile`、`CredentialsConfig`、测试请求与结果）/ `constants.ts` 默认值（`null`）/ `useProfiles`（创建载荷与测试参数：`null` 时不提交该键）/ `LoginChannelField.vue`（三态下拉 + 说明）/ `HttpLoginWizard.vue`（复用同一草稿传参）。
- 证书三态映射收敛在 `frontend/src/utils/loginChannel.ts`（`certPolicyFromValue` / `certPolicyToValue` / `certPolicyHint` / `HTTP_CERT_POLICY_OPTIONS`），纯函数单一事实源——`follow` 必须落回 `null` 而非 `false`，否则「未设置」会变成「显式严格校验」，自签门户从此登不上（单测锁定该回归）。

### 文档

- `docs/guides/http-login-guide.md`：新增 §3.2.1 HTTPS 证书三选项说明；§5 结果面板补响应头线索与「证书校验失败」归因；§8 边界补「只发一次请求、不共享 Cookie」「不跟随系统代理」，并把体积上限那段从"保存不校验"改为"超限被拒"。
- `docs/plan-next.md`：新增「直连请求渠道待办」节，记录 Cookie/两步门户的两条实现路径（先定方向再动手）、判定只看响应体、方法仅 GET/POST、测试端点与正式登录的 `local_ip` 来源不同、登录历史不记渠道。

### 验证

- Rust：`cargo check --features no-embed --all-targets` 通过（新字段全部构造点同步）；`cargo test --features no-embed --lib http_login` **20 例**通过（新增 5：默认 UA 兜底、显式 UA 覆盖兜底、响应头回显且脱敏、体积校验拒绝超限、证书策略三级回退）；`--lib web::routes` **191 例**通过（新增 2：保存超限 400 且不落盘、证书策略 true/false 往返落盘）。
- 前端：`vue-tsc --noEmit` 零错误；`vitest` **212 例**全绿（新增 5 例锁定证书三态映射与往返语义）。
- 未做实机门户验证：本轮无真实自签名 https 门户可测，证书策略由单测覆盖到"解析与传递"环节，**实际 TLS 握手是否放行未在真机确认**（`danger_accept_invalid_certs` 为 reqwest 既有能力，风险低）。

## 开发中（2026-09-18 直连登录「使用文档」入口改指在线文档）

- **两处入口由内置端点改为在线文档**（`frontend/src/components/common/LoginChannelField.vue`、`frontend/src/components/common/HttpLoginWizard.vue`）：`href` 从 `/api/docs/http-login-guide` 改为 `https://campus-auth.misyra.com/docs/profiles/http-login`，并补 `target="_blank" rel="noopener noreferrer"`（在应用内以新标签打开，不打断编辑草稿；与 `AboutView` / `SetupWizard` 既有外链口径一致）。
- 动机：内置端点返回 Markdown 原文（浏览器直接展示纯文本，无排版、无目录、无站内跳转），在线文档是站点渲染后的版本且与最新功能同步。
- **保留 `GET /api/docs/http-login-guide` 与 `docs/guides/http-login-guide.md`**：离线场景（便携版无网）仍可按地址访问，且该文档仍被 `docs/guides/README.md`、`docs/guides/user-guide.md` 引用；本轮只改前端入口，不动后端路由与 `openapi.json`。
- 验证：`npm run typecheck` 零错误；vitest 全量 207 例通过（23 文件，无链接断言，改动为纯模板 href 替换）。

## 开发中（2026-09-18 修复 login_chain 自动重登用例依赖挂钟导致夜间 CI 必红）

- **`setup_profile_and_task` 显式关闭定时暂停并断言落盘**（`tests/login_chain.rs`）：`PauseSettings` 默认 `enabled=true` 且窗口为 23:00–06:00（夜间不自动登录）。用例基座走 `tempdir` + 默认配置，不吃 `tests/fixtures` 里那些 `enabled:false` 的模板，而引擎定时器分支有 `is_any_pause_active` 门控（`src/engine/run_loop.rs:226`）——CI 在该窗口内运行时自动重登永远不触发，`login_chain_auto_relogin_after_kick` 必然在 150s 后超时失败。
- 根因定位依据：连续两次 CI（00:32Z / 01:04Z UTC）同用例失败、其余 4 例全绿，而 16:50Z 的 CI 全绿；单跑失败 job 复现一致，排除偶发。其余 4 例走手动 `POST /api/login`，不经引擎定时器，故不受影响。
- 修复用 `PATCH /api/config` 的 `pause.enabled=false`（该键在 `global_keys` 白名单内，`src/web/routes/config.rs:111-122`）并在 `GET /api/config` 回读断言，挡住「白名单拒收该键导致静默失效」这类回归。
- 与 2026-09-17 那轮「e2e 集成不受影响」的判断并不矛盾：当时是按**白天**运行 + `immediate_check_blocked_by_pause` 语义推得「自动重登由定时器路径驱动、不受影响」，本轮暴露的正是定时器路径自身在窗口内被门控（`docs/changelog.md` 该轮已把「恰在 23:00–06:00 跑 CI」列为极端情况，未处理，本轮补上）。
- 验证：`cargo fmt --check` 通过、`cargo clippy --test login_chain` 无警告、`--list` 5 例齐备。**本机缺 ddddocr/PIL，`preflight()` 会打印原因后跳过（`cargo test --test login_chain` 0.33s 全绿即跳过态），实跑以 CI 的 `e2e-login-chain` 为准。**

## 开发中（2026-09-17 Docker 部署核对整改：静态资源、关闭预算与运行口径）

静态核对 Docker 部署（本机无 Docker 环境，未做实机构建；`uv` 相关结论由本机 uv 0.11.21 等价实验得出）发现并修复的问题。

### 修复（Docker 运行时）

- **`resources/` 随镜像分发并同步到数据卷**（`Dockerfile` + `docker/entrypoint.sh`）：`GET /api/tools/task-recorder.user.js` 从 `<base_path>/resources/tools/` 读取，且**没有** rust-embed 嵌入副本（对比 `docs/guides` 有 `GuideAsset` 兜底，是这条链上唯一断的）。此前 `resources` 只在 rust-builder 阶段 COPY（供 `include_bytes!("../../resources/icons/tray.png")`），运行时镜像不含该目录，`resolve_script_path`（`src/web/routes/tools.rs:15-37`）三级查找（base_path → 父级链 → 编译期 `CARGO_MANIFEST_DIR=/build`）全部落空 → 前端「安装录制器」按钮 **404**。修法：运行时 `COPY --from=rust-builder /build/resources /opt/campus-auth/resources`，entrypoint 每次启动 `cp -rf` 覆盖同步到 `${DATA_DIR}/resources`（与便携版 helper 的 overlay 口径一致：覆盖同名、新增缺失，保证升级镜像后不残留旧脚本；失败仅告警，不阻断启动）。
- **`uv run` 两步补 `--no-dev`**（`Dockerfile`）：`uv sync` 带 `--no-dev`，但后续 `uv run` 不带 → uv 默认把 dev 组拉回 venv。本机实测（真实 `pyproject.toml` + `uv.lock`）：sync 后 5 个包，一次不带 `--no-dev` 的 `uv run` 就装回 `pytest/pluggy/iniconfig/packaging/pygments/colorama`。三步统一加 `--no-dev` 后实测 import 探针 `import playwright; import worker_main` 仍通过、site-packages 无 pytest。
- **compose 补 `stop_grace_period: 40s` 与 `init: true`**（`docker-compose.yml`）：优雅关闭预算约 26s（托盘 3s + 调度 5s + 引擎 5s + Bridge 8s + Axum 5s，见 `src/launcher.rs:947-1005`），Docker 默认 10s 后 SIGKILL，会在 Bridge 等待 Worker 退出前强杀并留下孤儿 chromium。`init` 负责回收浏览器子进程树的僵尸进程。

### 修复（文档口径）

- **根 `README.md` 的 `docker run` 补 `--restart unless-stopped`、`--stop-timeout 40`、端口改绑回环**：原写法 `-p 50721:50721`（发布到所有网卡）与 `docker/README.md:39`「不要把容器端口直接暴露到局域网或公网」自相矛盾；缺 `--restart` 时，`app.auto_restart_hours` 默认 24h 到期后主进程主动退出，容器会停在 exited 不再回来（compose 有 `restart: unless-stopped` 故无此问题）。`docker/README.md` 的单独命令示例同步补 `--stop-timeout 40`。
- **`docker/README.md` 明示「容器内不支持应用内自更新」**：镜像 Worker 位于 `/app/python_worker`、不在数据目录 `/data` 内，`self_update_worker_dir`（`src/updater/mod.rs:44-55`）按外置布局返回 `UnsupportedSelfUpdateLayout`，`POST /api/system/update` 必然失败——这是有意设计（由部署系统更新），但此前文档未写。同节补充定时自重启与容器重启策略的关系。
- **新增 `.gitattributes` 固定 `docker/*.sh` / `Dockerfile` / `.dockerignore` / `docker-compose*.yml` 为 LF**：仓库 `core.autocrlf=true`，当前 entrypoint.sh 工作区与 git blob 均为 LF，但换机 checkout 被转成 CRLF 会让容器报 `exec: /entrypoint.sh: no such file or directory`（shebang 行带 `\r`）。

### 其他

- `.dockerignore` 补 `python_worker/tests`、`captures`、`debug`、`build`、`dist`、`.mypy_cache`、`.ruff_cache`（此前随 `COPY python_worker` 进镜像）。
- 未改动项（已核对通过，记录备查）：`rust:1.98-bookworm` 与 `python:3.12-slim-bookworm` 均提供 amd64/arm64 且与 `rust-toolchain.toml`、`requires-python >=3.12,<3.13` 一致；运行时 GTK/ayatana/librsvg/libxdo 齐备；`worker_project_dir` 的 `/app/python_worker` 兜底命中镜像布局；`PLAYWRIGHT_BROWSERS_PATH=/ms-playwright` 构建与运行同源；`is_docker_env()` 经 `/.dockerenv` 生效（自动绑 `0.0.0.0` + 禁托盘）。

## 开发中（2026-09-17 修复 unix CI 的 dead_code 编译失败）

- **`unescape_csv_field` 加 `#[cfg(windows)]`**（`src/bridge/orphan.rs`，对应测试同步加 `#[cfg(windows)]`）：该函数只服务 Windows 分支的 `Get-CimInstance` + `ConvertTo-Csv` 路径（CSV 字段反转义），unix 分支读 `/proc/<pid>/cmdline` 无转义需要处理。缺 cfg 时 unix 下触发 `dead_code`，而 CI 的 `rust-tests-unix`（ubuntu-22.04 / macos-latest）以 `-D warnings` 运行 clippy，直接编译失败（exit 101）——最近两次 master CI 失败均源于此，Windows job 不受影响所以本地与 Windows CI 一直全绿。
- 验证：Windows 本地 `cargo clippy --all-targets -- -D warnings` 零警告、`cargo fmt --check` 通过、`cargo test --tests` 全绿（lib 885 例 + 集成）。unix 侧改动仅为平台门控（不新增/不删代码路径），本地无 unix 交叉编译器（`ring`/`gtk-sys` 需要 `x86_64-linux-gnu-gcc` 与 pkg-config），以 CI 的 `rust-tests-unix` 复核为准。

## 开发中（2026-09-17 任务页补充「任务需在方案中启用」提示）

- **任务页（浏览器任务 Tab）顶部新增归属提示条**（`frontend/src/views/tasks/BrowserTasksPanel.vue` + `styles/pages/tasks.css`）：文案「请前往侧边栏『方案』选择并启用任务，此处仅编辑调试」，其中「方案」为跳转 `/profiles` 的链接。此前本页只有编辑/调试/导入入口，不在本页也不在方案页的用户无从得知「在这里建的任务不会自动生效」——登录实际执行的是方案绑定的 `active_task`（未绑定时回退内置默认任务），该绑定关系只在方案页的「浏览器任务」选择器里可改。
- 归属选择：提示只放「浏览器任务」Tab，不放「任务」页容器。页内另有「脚本」「定时任务」「AI 生成浏览器任务」三个 Tab，均与方案绑定无关系（脚本/定时任务走独立调度，AI 生成只是任务的产出入口），套用同一文案会失真。
- 样式：新增 `.tasks-notice`（`grid-column: 1 / -1` 横跨两列，避免落入左列把任务列表挤到右列；`.tasks-grid--empty` 的单列布局下同样成立），配色沿用 `--primary` 信息态（非 `.ai-dev-notice` 的警告态——这不是功能缺陷，是操作路径说明）。
- 验证：前端 `vue-tsc` 零错误、`vitest` 207 例全绿、`vite build` 通过；另以 vite dev + 浏览器实测确认：`/tasks` 顶部渲染整条提示且横跨两列（`grid-column: 1 / -1` 生效，未挤占任务列表列），点「方案」跳转 `/profiles`，`/tasks/scripts`、`/tasks/scheduled`、`/tasks/ai` 三个 Tab 上均不渲染（`count = 0`），900px 宽视口下无横向溢出。

## 开发中（2026-09-17 实测反馈整改：首轮落盘配置、任务成功语义与浏览器提示）

依据全新安装实测与外部反馈复核结论（`docs/reports/fresh-install-acceptance.md` 第 7 节，不入仓）逐条整改。

> 说明：本轮曾把 `StartupAction` 默认值由 `None` 改为 `Monitor`（启动即监测），经确认该默认值是产品方**有意设定**（多数用户不希望在启动时自动拉起检测），已完整回退——`src/config/schema.rs`、`frontend/src/utils/constants.ts`、`tests/login_chain.rs` 均恢复原状，本节不含该项。

### 变更（行为）

- **首次运行落盘默认 `settings.json`**（`src/config/service.rs::load_or_init_settings`）：此前默认值仅存在于内存（首次配置变更才写文件），用户无法查看/编辑初始配置，排障时也无从核对实际生效值。写入失败仅降级为「沿用内存默认」并记 debug 日志，不阻断启动（只读介质场景不应起不来）。
- **`handle_ocr` 空识别结果改为失败**（`python_worker/step_handlers.py`）：识别器对空白/未渲染区域返回空串时，原先会 `fill('')` 清空输入框并继续提交，而 `run_steps` 只看步骤技术执行成功，最终任务报「执行成功」但门户实际拒绝登录。现显式抛 `UNKNOWN_ERROR`（文案「OCR 未识别出有效文本（验证码为空）」）且不发生回填。经真实空白图驱动验证：抛错、回填次数 0。（注：OCR 读到按钮文字等「非空乱码」不属本场景，是选择器配错，不在拦截范围内。）

### 修复（提示与状态一致性）

- **系统通道健康检查失败不再丢失可操作信息**（`python_worker/playwright_worker.py` + `src/bridge/mod.rs`）：`msedge`/`chrome`/`custom` 在 `_managed_engine` 上返回 `None`，原先上报空 `missing` 数组，Rust 侧只剩「未上报缺失组件明细」这一无指向性文案，排障方向从「配置的浏览器未安装」偏到「Worker 启动超时」。现系统通道给出可操作提示（如「未检测到 Microsoft Edge，请安装 Microsoft Edge 或改用 chromium」），并新增 `reinstallable` 标志：托管引擎（可由 `uv playwright install` 补齐）仍记「将尝试自动重装」，系统通道/custom 改记「配置的浏览器不可用且无法自动安装，请按提示处理后重试」——避免宣称一个不存在的自愈动作。
- **`GET /api/browsers` 的 custom 条目按真实路径判定**（`src/web/routes/system.rs::custom_channel_installed`）：此前硬编码 `installed: true`，`browser_custom_path` 指向不存在文件时仍报「已安装」，与同响应中托管引擎的真实探测自相矛盾。现与 `browser::is_channel_available` 的 custom 分支同口径（`Path::is_file()`）。判定抽为独立函数以便单测。
- **任务执行结果文案区分「已执行」与「认证成功」**（`frontend/src/composables/useTasks.ts`）：浏览器任务的成功语义是「步骤都执行完了」，后端明确不做登录后网络验证（`src/tasks/executor.rs`）。未声明 `success_condition` 的任务原先笼统提示「执行完成」，用户会把「脚本跑完」误读为「已认证上网」（实测验证码识别出错时任务报成功但门户实际拒绝登录）。现按是否声明成功条件分流：未声明提示「已执行完成（未校验结果）」，已声明提示「执行完成」。成功条件经 `tasksApi.get` 读取并按任务 id 缓存，读取失败保守按未声明处理。
- **任务页补充 `success_condition` 用法说明**（`frontend/src/views/tasks/BrowserTasksPanel.vue`）：前端此前完全无该字段的说明入口，用户无从得知存在业务结果判定能力。新增「如何判定任务真的成功」小节（含 `eval` + `store_as` 可复制示例）。
- **OCR 状态区分「依赖已装」与「运行时可用」**（`frontend/src/views/settings/TaskEnvironmentSettings.vue` + `api/types.ts`）：`/api/ocr/status` 的 `installed=true` 与 `runtime_ocr=null` 并存易被误读为「装了空壳」，而 `null` 只表示认证核心按需懒加载、当前未运行。现按三态给出说明（已加载 / 未加载需处理 / 首次使用时自动加载），并给 `OcrStatus` 契约类型补上 `runtime_ocr` 字段。

### 验证

- Rust：`cargo fmt --check` 通过、`clippy --all-targets -D warnings` 零警告、`cargo test --tests` 全绿（lib 885 例，含新增 `test_first_run_writes_default_settings_file` / `test_existing_settings_not_overwritten_on_load` / `custom_browser_installed_reflects_real_path`；集成 40 例）。首轮并行跑时 `instance_lifecycle` 曾因端口/锁竞争偶发失败，单独与复跑均通过，确认与本次改动无关。
- Python：`uv run pytest` 186 例通过（原 182 + 新增 4：系统通道提示与 `reinstallable` 标志、空 OCR 结果抛错、纯空白结果等价处理）。
- 前端：`vue-tsc` 零错误、`vitest` 207 例全绿、`vite build` 通过。
- 端到端（`E:\Test\VerifyFixes` 全新目录 + 更新后的 Worker 源码）：首轮生成 `settings.json`（`config_version=9`）；custom 路径不存在报 `installed:false`、指向真实文件报 `true`；健康检查失败时系统通道给出可操作提示且 `reinstallable=false`、托管通道 `reinstallable=true`；空白验证码图使 `handle_ocr` 抛错且回填次数为 0。

## 开发中（2026-09-17 全新安装实测修复：CLI 重定向输出丢失 + 远端字体被 CSP 拦截）

从便携包 → 全新目录解压 → 真实浏览器端到端实测（验收报告本地 `docs/reports/fresh-install-acceptance.md`，不入仓）中发现并修复两个缺陷。

- **修复 GUI 子系统 release 构建下 stdio 重定向/管道输出被静默丢弃**（`src/main.rs::attach_parent_console`）：实测 `campus-auth.exe --status > out.txt`、`--version > out.txt`、`cmd /c "… > f 2> e"`、`… | findstr .` 在真实控制台窗口中均得到 **0 字节输出且退出码为 0**（debug 控制台子系统构建全部正常）。根因是句柄操作顺序：**`AttachConsole(ATTACH_PARENT_PROCESS)` 自身就会重写本进程标准句柄表**，把调用方传入的重定向句柄替换为控制台句柄；而原实现先 `AttachConsole` 再判断"句柄是否缺失"，判定时看到的已是换上的控制台句柄，于是"齐全 → return"，重定向输出丢失——即原注释声称要避免的行为恰好发生（`Start-Process -RedirectStandardOutput` 能通过属 stderr 为 NULL 触发补写路径的偶然结果）。改为：附着**之前**用 `GetStdHandle` 记录原始 stdout/stderr，**两路均有效时完全不调用 `AttachConsole`**（不触碰句柄表）；确有一路缺失时才附着，随后逐路处理——原有效的重定向句柄还原、原缺失的补 `CONOUT$`。修复后实测：仅重定向 stdout、stdout+stderr 双重重定向、管道三种场景输出完整（27 字节版本号 / 25 字节状态文本），无重定向时终端可见性不变。
- **修复 CSP `style-src` 未放行 jsDelivr 导致远端字体从未生效**（`src/web/mod.rs::security_headers`）：控制台每次加载均报 `Loading the stylesheet 'https://cdn.jsdelivr.net/npm/@fontsource-variable/noto-sans-sc@5.3.0/index.css' violates … "style-src 'self' 'unsafe-inline'"`。该字体是**以 `<link rel="stylesheet">` 外链**的，请求受 `style-src` 管辖（其内部 `@font-face` 指向的字体文件才走 `font-src`）；原实现只放行了 `font-src`，故此前"全站字体改为远端 Noto Sans SC"一轮（见下方同日条目）**实际从未生效**，界面始终回落系统字体栈。修法：`style-src` 补 `https://cdn.jsdelivr.net`（`font-src` 白名单保留），`script-src` 仍限 `'self'`。真实浏览器复验：CSP 报错由每载 1 条降为 **0 条**，`document.styleSheets` 已含该外链、`document.fonts.size = 101`（与 index.html 注释声明的 101 条 `@font-face` 一致），字体分片按 `unicode-range` 请求 200。
- 验证：`cargo fmt --check` 通过、`clippy --all-targets -D warnings` 零警告、`cargo test --tests` 全绿（lib 881 例 + 集成 40 例，含 `login_chain` 5 例、`instance_lifecycle` 4 例、`smoke_test`），release 重建后 PE Subsystem 仍为 2（GUI）。CSP 策略串抽为 `CONTENT_SECURITY_POLICY` 常量并新增回归测试 `csp_allows_remote_font_stylesheet_and_files`（断言 `style-src`/`font-src` 均含 jsDelivr 且 `script-src` 仍只限 `'self'`），防止将来清理 CSP 时再次误删；lib 测试数 881 → 882 全绿。

## 开发中（2026-09-17 托盘菜单精简与状态行）

- **托盘菜单移除「检查更新」项**（`src/tray/mod.rs` + `src/launcher.rs`）：更新入口统一收敛到关于页与设置 · 网络与更新，托盘不再重复提供。连带删除 `TrayAction::CheckUpdate` 分支、`update_menu_label`、`menu_action_for` 的 `check_update` 映射，以及仅为该功能存在的 `TrayDeps.updater` 字段与 `UpdaterService` 导入（该字段全仓仅此一处消费）。测试补断言 `menu_action_for("check_update")` 返回 `None`，防止入口复活。
- **状态行改名并借启用态表达强调**：首行由「当前状态：运行中 · 在线」改为「**状态：运行中 · 在线**」。muda 的 `MenuItem` 无颜色/字重 API（仅 `set_text`/`set_enabled`/`set_accelerator`），故「运行中深色、未运行浅色」落到 `set_enabled`：引擎 `Running` → 启用（系统默认深色），`Stopped`/`Dead` → 禁用（系统灰化），即 Windows 原生菜单表达主次的方式。新增 `status_item_enabled()` 承载该映射并单测锁定；`update_tray` 与 `run_os_thread` 首帧均按状态复位启用态（仅在取值变化时调用 `set_enabled`，避免无谓重设）。
- 现行菜单结构：状态行 → 分隔线 → 启动/停止监测（随状态切换）→ 手动登录 → 打开控制台 → 退出。README 与 `docs/guides/user-guide.md` 同步。
- 验证：`cargo test --lib tray::` 8 例通过（新增 `test_status_item_enabled`）、`clippy --all-targets -D warnings` 零警告、`fmt --check` 通过；实例重启后日志确认「系统托盘已创建」且不再出现「托盘检查更新」条目。**菜单视觉呈现（分隔线、灰化）依赖 Windows 原生渲染，无法自动化断言，需人工目视确认。**

## 开发中（2026-09-17 指南文档端点统一为下载语义）

- **`/api/docs/*` 三端点（编写指南 / 任务手册 / 直连登录指南）`Content-Disposition` 由 `inline` 改为 `attachment`**（`src/web/routes/system.rs::markdown_response`）：浏览器访问 URL 即弹出保存对话框，三个端点行为统一。同步移除前端「导出编写指南」链接上冗余的 `download` 属性（响应头已带 filename），`target="_blank"` 在线阅读入口（任务页编写指南、设置页指南链接、直连面板与向导的文档入口）随响应头语义自然变为下载，`title`/提示文案同步改为「下载」口径；`openapi.json` 三个端点 200 描述注明 attachment。验证：`cargo test --lib web::routes::system` 18 例通过、clippy 零警告、fmt 通过；前端 `vue-tsc` 零错误、vitest 207 例全绿。

## 开发中（2026-09-17 全站字体改为远端 Noto Sans SC）

- **全站正文字体改走远端 Noto Sans SC 可变字重**（`frontend/index.html` + `styles/base.css` + `src/web/mod.rs`）：
  - 引入 `@fontsource-variable/noto-sans-sc@5.3.0/index.css`（jsDelivr，SIL OFL 1.1），CSS 内 101 条 `@font-face` 以 `unicode-range` 分片，浏览器只下载实际字形所在分片（实测界面 1343 个中文字命中 35 片 ≈ 1.6 MB）；`preconnect` 提前建连。
  - `--font-sans` 首选 `'Noto Sans SC Variable'`，逐级回落系统栈（PingFang SC / 微软雅黑 / Noto Sans SC / sans-serif）；断网时按 `font-display: swap` 用系统字体渲染，不阻塞首屏、不白屏。
  - CSP 的 `font-src` 放行 `https://cdn.jsdelivr.net`（`src/web/mod.rs::security_headers`）——此前为 `'self' data:`，不改则远端字体被浏览器直接拦掉；`style-src`/`script-src` 仍限 `'self'`。**[2026-09-17 更正]** 本条结论不完整：该 CSS 以外链样式表方式加载，受 `style-src` 管辖，仅放行 `font-src` 时字体**实际从未生效**；补 `style-src` 的修复见上方「全新安装实测修复」条目。
  - 选可变字重版而非静态多字重：静态 400 单字重 0.96 MB 但 500/600/700 需合成加粗（中文合成加粗发糊），静态 4 字重约 3.84 MB，可变版 1.63 MB 覆盖 100–900 全部真实字形。
- **品牌字改为跟随全站字体**（`styles/layout.css` 的 `.logo-text`）：侧栏「认证喵」不再用霞鹜文楷，改用 `--font-sans`；字号提到 `--text-2xl`、字重与 `.page-title` 对齐为 600（此前 500 是霞鹜文楷时期为取真实 Medium 字形而定，与 600 的页标题并置明显偏轻）。删除 `frontend/public/fonts/` 下的霞鹜文楷子集与 OFL 副本、`base.css` 中两条 `@font-face`，仓库不再存放字体文件。变更缘由：鸿蒙字体（HarmonyOS Sans）虽免费但许可明确「不得修改」，子集化属违约，故未采用；霞鹜文楷子集方案被本次远端方案取代。
- **README 新增「第三方资源」节**：声明 Noto Sans SC（Google Inc.，OFL-1.1）的授权与加载方式——字体不经仓库分发、不内嵌二进制，由 `frontend/index.html` 经 jsDelivr 按 `unicode-range` 分片加载，并说明该 CDN 已在 CSP `font-src` 放行。查证结论：OFL 1.1 的「再分发须附许可副本」义务因**未分发字体文件**而不触发，故无强制声明义务；本次声明属透明度考虑（浏览器会向 `cdn.jsdelivr.net` 发起请求），同时为将来可能改为自托管预留说明位置。
- 验证：`cargo build` 通过；实例重启后 `/api/health` 正常、CSP 响应头实测含 `font-src 'self' data: https://cdn.jsdelivr.net`、首页已嵌远端字体链接、`dist/fonts` 已消失；`vue-tsc` 零错误、vitest 207 例全绿。

## 开发中（2026-09-17 关于页新增赞助入口）

- **关于页新增「赞助」链接**（`frontend/src/views/AboutView.vue` + `styles/pages/about.css` + `components/common/IconApp.vue`）：页脚 about-links 在「使用文档 / GitHub」后追加指向 `https://blog.misyra.com/sponsor/` 的赞助入口，外观与既有两链接同款（`.sponsor-link` 并入同一 hover/边框样式组），heart 图标以 `--accent` 品牌色区分；`IconApp` 图标注册表新增 `heart`（Feather heart path，单点维护避免内联 SVG 拷贝漂移）。验证：`vue-tsc` 零错误、vitest 207 例全绿。

## 开发中（2026-09-17 软件中文名改为「认证喵」）

- **软件中文显示名由「校园网自动认证」改为「认证喵」**，英文标识 `Campus-Auth` / `campus-auth` 与全部链接、仓库名、二进制名、注册表键、LaunchAgent label 等机器标识**一律不动**。改动点：关于页 `<h1>`（`AboutView.vue`）、侧栏 logo 文字与 aria-label（`AppSidebar.vue`）、顶栏默认标题（`AppTopbar.vue`）、设置向导欢迎语与协议文案（`SetupWizard.vue`）、托盘 tooltip 两处（`src/tray/mod.rs` 初始 + 动态刷新）、`index.html` `<title>`（定为「认证喵 - Campus-Auth」）、`Cargo.toml` description、`README.md` / `AGENTS.md` / `python_worker/README.md` 标题与首段。关于页副标题「Campus Network Auth」、版本行与说明行「校园网自动认证工具」按用户要求保留原样（名字与说明性文字区分）。验证：`cargo check` 通过、前端类型检查零错误。

## 开发中（2026-09-17 文档全量对质：修正 20 处与代码不符的陈述）

以「文档声明 → 源码事实」逐条对质全部用户/开发文档（`README.md`、`AGENTS.md`、`docs/**`）与 `openapi.json`，发现并修正 20 处与当前实现不符的陈述；同时清理 `docs/archive/` 遗留引用并删除该已清空的目录。

### 与实现相反（P0）

- **`task-writing-guide.md` §10**：原称「`wait` 无 selector 保存新任务时被校验拒绝（`步骤[n] 需要 selector`）」——与代码相反。`src/tasks/loader.rs:548-561` 的实际规则是「`selector` 与 `duration` 不能同时为空」，只写 `duration` 的 `wait` **校验通过**（`test_validate_wait_accepts_duration_only` 已固化，有意设计以免 AI 生成的休眠步骤反复自纠失败）。改为如实描述校验口径与推荐写法。
- **`user-guide.md` 运行模式表漏列 `pause_enabled`**：`frontend/src/utils/runMode.ts:38-58` 的 `RunModeSettings` 有 7 个字段，文档只列 6 项。补「启用暂停时段（默认模式开启 / 调试模式关闭）」，与 `docs/updatelog.md:11` 对齐。
- **`task-manual.md` API 表重复 `POST /api/login` 两行**（`:62` / `:68`，一行还沿用旧术语「活跃任务」）、末行与 `## 8.` 标题间缺空行（表格会被标题截断）。重写该表：去重、补 `scheduler/jobs` 六个端点、路径参数按实现改 `{task_id}`、补空行。
- **`user-guide.md` §3 切换检测周期错误**：原称「每 60s 检测」，实际默认 **180s**（`src/engine/mod.rs:27`、`src/config/schema.rs:249`），可配 60–600（`:35-37`）。

### 事实性错误（P1）

- **`README.md`「增量更新」→ 全量分发**：`src/updater/download.rs` 整体下载归档解压，无差分包逻辑；`docs/changelog.md:1338/1349` 自身口径即「更新全量分发」。
- **`README.md`「优先级排序」→ 约束条件多者优先**：`src/config/` 无 `priority`/`order` 字段与端点，匹配顺序由 `profiles.rs:284/289` 的 `strength` 自动推导。
- **`README.md`「端口冲突自动 +1」→ 改绑端口 0**：`src/app.rs:128-130` 仅在回环地址遇 `AddrInUse`/Windows 10013 时改由内核随机分配，非回环直接报错。
- **`user-guide.md`「v8 schema」→ v9**：`src/config/mod.rs:14` 为 `CURRENT_CONFIG_VERSION = 9`。
- **`user-guide.md` 运行时目录树**：`.venv`、`captures/`、`debug/` 实际都在 `python_worker/` 下（`environment::PYTHON_EXE_RELATIVE`、`src/ai/mod.rs:268`、`src/web/routes/debug.rs:152`），Playwright 浏览器在平台默认缓存（`environment/browser_registry.rs:73-86`）；补 `logs/login_history/` 与各项状态文件。
- **`user-guide.md`「镜像目录 `~/.cache/campus-auth`」删除**：全仓无该路径任何写入点（`git grep` 仅命中该行文档本身）。
- **`user-guide.md`「开机自启写系统注册表」限定 Windows**：macOS 写 LaunchAgent plist、Linux 写 XDG desktop（`src/utils/platform.rs:14-150`），与本文件 §7 三端描述一致。

### 过时/失准（P2）

- **`custom-script-guide.md`「stdout/stderr 经 tracing 与 WebSocket 推送」不成立**：`src/tasks/executor.rs:431-450` 只把输出放进 `TaskResult.output`（截断 500）随响应返回；9 处 `tracing::` 调用无一携带子进程输出，日志面板/`GET /api/logs` 看不到脚本输出。
- **`custom-script-guide.md`「子进程带 `CREATE_NO_WINDOW`」不成立**：`executor.rs:381-393` 构造脚本子进程时未设 `creation_flags`（`:469-472` 那处只用于超时强杀的 `taskkill`）。改为中性描述并给出静默运行的规避方式。
- **`custom-script-guide.md`「`script_path` 可为绝对路径」**：`executor.rs:340-357` 对绝对路径同样 canonicalize 并要求落在 `tasks/scripts/` 内，越界报「script_path 越界」。
- **`http-login-guide.md`「体积超限在保存时被拒绝」**：`HttpLoginRequest::validate()` 仅在登录执行（`http_login.rs:106`）与测试端点（`profiles.rs:443`）调用，保存路径只校验 URL 合法性，超限配置可存盘。
- **`plan-next.md`「85 个路径 / 响应 schema 全为 `{}`」**：实为 88 个路径（`/api/*` 87 + `/ws/logs`、operations 103），且 `/api/ai/capture/status` 带真实 schema（`openapi.json:3490-3499`）。
- **`known-issues.md` 引用坐标修正**：#3 序列化点 `:360`（原 `:658` 是测试注释）、#15 排序示意移到 `:272-291`（原 `:138-157` 为摘要赋值与 slug 校验）、#19 校验位置改为 `src/web/ssrf.rs:26-69`（`repo.rs` 已无 IP 判定代码）。
- **`known-issues.md` 两条失效条目**：「`useTasks` ↔ `useScripts` 循环动态导入」与「本地遗留 `python_worker/.venv` 待清理」均不再成立（前者两模块互不引用、均经 `useTaskDirectory`；后者目录已不存在），「三、低危清理项」据此清空。
- **`known-issues.md` 核实口径**：删除「已逐项对照当前代码核实（2026-09-06）」这一与 `plan-next.md:42`（#22 注 21 项未复核）矛盾的表述，改为逐条注明核实时点。
- **`docs/changelog.md` 头部**、`docs/known-issues.md` 头部：`docs/archive/` 死引用改为如实指向。
- **删除 `docs/archive/`**：该目录自两轮清理后已成空壳（仅剩 README，无任何归档材料），且所有引用点均为「说明它已空」的元描述，无实际内容依赖。删目录同时收敛引用：`AGENTS.md` 文档分工表去掉该行、`docs/changelog.md` 头部与 `docs/known-issues.md` / `docs/plan-next.md` 改为「archive 已删除、历史归档材料不可追溯」。
- **`docs/archive/README.md` 台账失真**：删除已不存在的 `test-coverage-2026-08-30.md` 行（该路径还被 `.gitignore` 的 `/docs/**/*-coverage-*.md` 命中，无法入库）。
- **`src/ai/prompt.rs:3`**：自指行数改为「约 570 行」（实际 571）。

### 顺带更正（本次对质新增发现）

- `task-manual.md` 页面名 `设置 · 监测` → `设置 · 网络检测`、`设置 · 任务` → `设置 · 任务与环境`（以前端路由 `title` 为权威）。
- `custom-script-guide.md` / `task-manual.md` / `user-guide.md` 标题旧叫法「配置方案」统一为「方案」。
- `README.md` 单次登录改为「当前方案绑定的任务」、快速开始 OCR 入口改为「设置·任务与环境」、项目结构中 docs 清单补 `updatelog`/`guides`。

### 验证

- 文档内部相对链接全量扫描：**0 断链**；文档提及的 `/api/*` 路径与 `openapi.json` 逐一比对，除通配写法（`/api/autostart/*`、`/api/scripts`）与示例脚本 ID 外无悬空端点。
- `cargo fmt --check` 通过；`cargo clippy --all-targets --features no-embed -- -D warnings` 零警告；`cargo check --features no-embed` 通过。
- `cargo test --lib` **878 passed / 0 failed**（含 `tasks::loader::tests::test_validate_wait_*` 实跑确认 wait 双语义、`scheduler::tests::test_systemtime_to_iso_uses_local_offset` 实跑确认 #3 已修）；`cargo test --test '*'` 全部通过（`instance_lifecycle` 首轮出现一次并行时序偶发失败，单独与重跑均通过）。
- `web::tests::openapi_json_matches_route_table` 通过。
- 未运行前端构建/vitest 与 pytest：本轮仅改文档与一处 Rust doc comment，未触及前端与 Worker 代码。


## 开发中（2026-09-16 修复 engine 测试隐式依赖墙钟时间）

提交前全量验证时发现 `engine::run_loop` 两个测试在夜间时段失败：`make_engine_with_hanging_probe` 的非暂停分支隐式吃 `PauseSettings` 新默认值（enabled=true, 23:00–06:00），真实墙钟落入窗口时立即检测被 F4 门控拦下，`wait_for` 预算内 `probe_total` 永不满足而 panic。当日 19–21 点全量跑绿是因为恰在窗口外——隐式依赖默认值的测试正是这样漏网的。修复：非暂停分支显式 `pause.enabled = false`，并在两处注释写明「测试不得依赖墙钟时间」。全量 `cargo test --lib` **878 passed**。

## 开发中（2026-09-16 直连配置补充 {local_mac} 格式说明，防 MAC 形态误判）

### 背景

用户询问 `{local_mac}` 的格式——程序内部统一为**小写冒号分隔**（`aa:bb:cc:dd:ee:ff`，`normalize_mac` 收口三平台差异），而大量门户（Dr.COM 系常见）要求 12 位裸十六进制（`aabbccddeeff`）或大写/连字符形态。用户按直觉直接引用占位符就会提交错误格式，且无任何报错，属于典型的「格式误判」。原 UI 与文档只写「同接口的 MAC」，没有交代格式与转换方式。

### 改动

- **单一事实源**：`loginChannel.ts` 新增 `HTTP_MAC_FORMAT_NOTE`（MAC 固定形态说明 + 三种常用转换写法 + eportal 靠来源 IP 防串号、MAC 提交空值即可的提示），`LoginChannelField.vue`（方案编辑器直连面板的占位符说明区）与 `HttpLoginWizard.vue`（向导「请求地址」占位符卡 + 「脚本契约」卡两处）注入同一常量，避免多处文案漂移。
- **使用文档**（`http-login-guide.md` 第 4 节）：占位符表新增「替换后的值示例」列（每项给出真实形态示例），`{local_mac}` 行显式标注固定小写冒号分隔，并附转换代码块（`replace(/:/g,"")` / `toUpperCase()` / 连字符形态）与空值容忍提醒。

### 验证

- `vue-tsc` 零错误；`loginChannel.test.ts` 20 passed；`npm run build` 通过。
- `cargo build` 后重启实例，`/api/docs/http-login-guide` 嵌入文档已含新示例（嵌入走 rust-embed，文档改动需重编译才在 `/api/docs/*` 生效）。

## 开发中（2026-09-16 仓库整理：删除过时过程报告与本地缓存）

按 AGENTS.md「过程报告仅在本地使用，有效结论只保留在 known-issues / plan-next」的约定做例行清理。全部删除对象均已被 gitignore（未入 git），不影响任何提交内容。

### 已删除

- **docs/reports 过时报告**：`code-review-2026-09-12/`（13 part + FIX-PLAN + VERIFY）、`audit-2026-09-15.md`（结论已全部修复并归档）、`async-concurrency-review.md`、`http-login-channel-plan-2026-09-14.md`（直连渠道已上线）、`frontend-ui-audit-2026-05-14.md`、`p3-recheck-2026-09-13/`（摘要已在 known-issues #22 注）、`ui-review-2026-09-12/`、`run-mode-ui-demo.html`（三方案对比 demo，UI 已定案）。
- **docs/reports 过程截图**：根目录 14 张 PNG（直连渠道开发期的侦察/演示图，零引用）；`ia-verify/` 内 17 张验证截图与 5 个 `shot_*.py`、1 个 `inspect_*.py` 伴生脚本。
- **docs/compose/**：compose-next 会话 spec（repo-audit / repo-bug-scan-2026-09-11）。
- **本地缓存与临时**：`python_worker/.venv`（416 MB，known-issues 既有挂账项，运行时按需重建）、`.playwright-cli/`、根与各子目录的 `__pycache__` / `.pytest_cache` / `.ruff_cache`、`tests/mock-servers/*` 缓存、`config-backup-20260916-*/` × 2、`debug/`（日志导出产物）、`dist/`（便携包产物，`build.ps1` 可重建）、`.workbuddy/`、`.zcode/`、`.worktrees/`。
- **`.campus_network_auth/`**（v3 Python 旧版加密密钥，用户确认 v3 不再用）。

### 保留（盘点确认）

- `docs/reports/ia-verify/` 的 **11 个 `verify_*.py` 探针**——changelog 引用其中 5 个作为验证方式记录，且可复现重跑（依赖 `target/debug/config/.auth_token`，该目录保留）。
- 活跃文档（changelog / updatelog / known-issues / plan-next / guides 7 篇 / archive）、`resources/`、运行时目录（config / logs / tasks / environment / update）与 `target/`（未获明确指示，暂不动）。

## 开发中（2026-09-16 修复 ipconfig 网关续行解析；新增 eportal 变体门户 mock 验证直连渠道）

### 缺陷：双栈网关环境下 `ctx.local_ip` 密钥协商失效

用「加密算法换新」的 eportal 变体门户（`tests/mock-servers/eportal-fnv1a-b64/`）端到端验证直连渠道时，恒定失败于 `IP 不匹配`——客户端加密用的 IP 与门户看到的来源 IP 不一致。

**根因不在客户端选址逻辑，而在 `ipconfig /all` 的网关续行解析**：双栈（IPv6 + IPv4 默认网关并存）DHCP 环境下，ipconfig 把 IPv6 网关放在「默认网关」标签行、IPv4 网关放在**下一行续行**（无标签冒号，仅缩进+值）。`parse_adapter_block` 只解析标签行自身，续行上的 IPv4 网关被丢弃 → WLAN 接口 `gateway=None` → `select_primary_interface` 的「有网关优先」落空。本机同时存在 aTrust VPN 虚拟网卡等干扰，选址行为变得不可预期，脚本 `ctx.local_ip` 与 TCP 源地址不一致，密钥协商失败。症状是「认证失败」而非报错（与 `interfaces.rs:88-92` 注释预言的排障困境完全一致）。

**修复**：`parse_adapter_block` 引入「网关续行窗口」——网关标签行开启窗口，紧随的无冒号 value-only 行若能解析出 IPv4 即为 IPv4 网关；窗口在任意其它带标签行处关闭。DNS 服务器有同款续行（`223.5.5.5` 独占一行），靠「仅网关标签行开启窗口 + 命中后立即关闭」双重约束排除。

### 实现

- `src/network/detect.rs`：`parse_adapter_block` 重构为 continue 三分支（IPv4 / 物理地址 / 网关标签 + 续行窗口），网关语义不变（标签行直接给出 IPv4 时立即采用且关闭窗口）。
- `src/network/detect_tests.rs`：新增 2 例——`test_parse_ipconfig_ipv4_gateway_on_continuation_line`（真实双栈输出形态，含 DNS 续行干扰项）、`test_parse_ipconfig_gateway_continuation_closes_on_next_label`（无续行时不得误认）。
- `tests/mock-servers/eportal-fnv1a-b64/`：新变体门户 mock（协议框架同 `eportal-xor`，加密算法换为 FNV-1a 密钥 + 逐字节 XOR + Base64）+ 同算法自测客户端。`--accept-ip` 参数对齐环回测试的密钥协商视角（真实校园网入站 IP 即客户端 WAN IP，无需此参数）。
- `docs/guides/http-login-guide.md` 新增 6.2 节：Base64 加密结果进 GET 查询串必须 `url_encode()`（`+` 不编码会被服务端解析成空格）；32 位散列乘法必须用 `Math.imul` 而非 `*`（Number 精度在 2^53 处舍入，`>>>0` 救不回已丢失的低位——本次调试中两个连环踩坑的书面化）。

### 端到端验证（真实主程序实例 + 变体门户）

- 服务端 roundtrip：同算法客户端（等效 login.sh）→ `"result":1` 认证成功。
- 主程序直连渠道（凭据变换脚本 + 占位符渲染）：**认证通过**（`"result":1`，服务端日志确认 `account=',0,20230001@cmcc' pwd='secret123' wip='192.168.123.210'` 全部正确解密）。
- 负向用例：错误密码 → `invalid_credential`（失败关键字命中「账号或密码错误」），成功/失败关键字分流正确。
- `cargo test --lib network::` **71 passed**（含 2 新例）；clippy 零警告、fmt 通过。
- 调试过程自建 Rust 探针复现解析逻辑定位根因（临时目录，未入仓）；本地测试实例与 mock 已清理。

## 开发中（2026-09-16 运行模式纳入暂停时段开关；初始默认启用 23:00–06:00 夜间暂停）

### 需求

用户要求：默认模式启用暂停时间段（23:00–06:00），调试模式不启用；初始配置也启用。

### 实现

- **预设新增 `pause_enabled` 字段**（`utils/runMode.ts`）：默认模式 `true`、调试模式 `false`。只预设**启用与否**，不预设起止——起止是用户可调的具体值，覆盖它们会抹掉用户自定义的时段（见 `useRunMode.ts` PATCH 载荷：`pause: { ...config.pause, enabled: preset.settings.pause_enabled }`）。调试关闭的理由：调试要随时手动复现，若落在暂停窗口里，登录会被引擎拦住、看似“没反应”。
- **初始配置默认启用夜间暂停**：后端 `PauseSettings` 派生 Default 改为手写实现（`enabled=true, 23:00→06:00`），前端 `DEFAULT_CONFIG.pause` 同步镜像。`#[serde(default)]` 语义下**只影响全新安装与字段缺失回退，既有配置保留已落盘的值**——这与该文件既有的默认值调整口径一致（见下方“不影响既有配置”轮次）。
- 预设差异标签表（`RUN_MODE_FIELD_LABELS`）补「启用暂停时段」，确认弹窗的改动清单随之如实多列一项。
- UI 维持现状（仅「设置 · 系统」卡片）：此前一轮的三方案对比 demo（`docs/reports/run-mode-ui-demo.html`，过程产物不提交）结论为用户接受现有形态。

### 验证

- vitest **207 passed**（23 文件）：`runMode.test.ts` 新增/调整 4 例（预设字段集合锁定、默认/调试的 `pause_enabled` 取值、两组差异关键项断言纳入 `pause_enabled`、差异标签「启用暂停时段」）；`vue-tsc` 零错误、`npm run build` 通过。
- Rust：`config::` 模块 **116 passed**（新增 `pause_defaults_to_night_window_enabled` 锁定新默认值）；`engine::run_loop` 26 passed；`cargo clippy --all-targets -- -D warnings` 零警告、`cargo fmt --check` 通过。
- 既有测试适配 1 处：`test_patch_settings_reports_profile_load_failure` 原以 `pause.enabled=false` 为初始态反证落盘失败；默认值调整后 mock 初始态即为 `true`，断言改为盯 `save_calls=0`（真正承重的“不落盘”证据）。
- **全量回归**：`cargo test --lib` **876 passed**（0 failed）；`login_chain` 集成 **5/5**（kick 自动重登 / failonce 重试 / ban 窗口 / 跳转链 / 慢门户）。
- **连带影响排查（新默认值 `enabled=true` 的波及面）**：
  - **e2e 集成不受影响**：`login_chain` 基座经 tempdir 全新引导，`setup_profile_and_task` 不写 pause 段（吃新默认值），但用例白天运行不在 23:00–06:00 窗口内；且 `immediate_check_blocked_by_pause` 仅在窗口内跳过立即检测、`monitoring` 照常置位，自动重登由定时器路径驱动。极端情况（恰在 23:00–06:00 跑 CI）本就是仓库既有基座模板显式 `"enabled": false` 所防的场景（见下）。
  - **`tests/fixtures/runtime-envs/*` 四个隔离基座全部显式 `"enabled": false`**——字段已落盘则 `serde(default)` 不介入，新默认值不影响它们；这是防止测试落进暂停窗口的既有防线，确认保留不改。
  - `verify_run_mode.py` 探针补 `pause_enabled` 断言（snapshot 纳入字段、调试/默认两向校验、恢复段含 pause），探针与预设字段重新对齐；`verify_new_defaults.py` 不校验 pause，无需扩展。
- **updatelog 补记**（用户可感知变更）：「运行模式」条目补暂停时段行为（默认启用 / 调试关闭 / 时段起止不被预设覆盖）；「尚未发布 · 体验」新增「新安装默认启用夜间暂停时段（23:00–06:00）」，标注既有配置不受影响。

## 开发中（2026-09-16 「从云端仓库导入任务」弹窗 UI 重构 + 国内网络提示）

用户反馈该弹窗"有点丑"，并要求为国内用户补一条"访问慢请用 Gitee"的提示。重构中发现并修掉两个真实缺陷。

### 界面重构

原布局的问题不在于"配色不好看"，而是**信息关系没有表达出来**：

- 「加载索引」原先是独占一行的次级按钮，而它**作用的对象是这个弹窗的索引地址**——按钮与它要拉取的源被隔开，看不出点它会用哪个源。
- 索引地址只以输入框形式出现在「自定义」场景下，预设源不显示，用户无法确认"现在到底在从哪拉"。

改动：

- **工具条同行**（`.repo-import-toolbar`）：左侧「来源」标签 + `.segmented` 分段控件，右侧「加载索引」。复用全局 `.segmented`（与主题切换、更新通道同一控件），当前源高亮，一眼看出按钮会拉哪个源。
- **逐源提示**（本轮新增的其实只有这一条说明文案，来源选择本身此前已有）：来源选择器下方一行次级说明文字，内容由源本身决定（GitHub：`国内访问可能较慢或加载失败，卡住时请改用 Gitee 镜像`；Gitee：`国内访问更快，推荐国内用户使用`；自定义：无，整行不渲染）。
- **空态重做**：原先只有一行"暂无数据"式的文字，现在复用全局 `.empty-state--dashed`——虚线框表达"待填充"，配 `globe-grid` 图标、「尚未加载任务列表」标题、说明文案，以及一个「直接查看仓库」入口，让空窗期也有出路。
- **footer 按状态三态化**：未选中且无列表 → 「本页任务来自社区仓库，导入前请核对内容」；有列表未选中 → 「点击左侧任务查看详情后导入」；已选中 → 「导入此任务」按钮。
- 删掉 `styles/pages/tasks.css` 里的重复 `.repo-source-label` 规则（其 `font-size` / `color` 从未生效——scoped 规则编译后带 `[data-v-*]`，特异性更高；只有 `margin-right` 半生效）。现只保留组件内一份。

### 顺带修掉的两个缺陷

- **空态「直接查看仓库」原先指向 raw JSON 索引地址**：它绑的是 `repoImport.url`（给程序 GET 的 `index.json`），点开是一屏原始 JSON 而不是仓库页面。根因是**一个变量承担了两种语义**——"程序的抓取地址"与"人的浏览地址"本就不是一回事。现将源选项表显式拆成 `indexUrl` / `homeUrl` 两个字段，链接改指 `homeUrl`（自定义源回退为用户自填地址，因为手填的地址未必有对应主页）。
- **列表为空时 footer 仍提示「点击左侧任务查看详情后导入」**：左侧根本没有可点的任务，这句引导指向不存在的东西。加 `tasks.length` 条件。

### 实现要点

- 源选项收敛为 `TASK_REPO_SOURCES` 单一事实源（`utils/constants.ts`），新增 `TASK_REPO_URL_GITEE` 与 `TaskRepoSourceId`。分段控件由 `v-for` 遍历生成，增删源只改常量表，不再出现"同一 host 在多处各写一份"——这正是上一轮「分享适配」指向错仓库的成因。
- `selectRepoSource` 改为查表实现：不再硬编码任何 host。离开自定义源前先存 `customUrl`，切回时**仅在确实存过手输内容时**回填——否则会把输入框清空，比保留上一个源的地址更差。
- 自定义源的索引输入框保持"按需显示"（预设源下不出现），但**从按钮行里挪到工具条下方独立一行**：原先它嵌在来源按钮中间，选中「自定义」时按钮会被撑开、那一行的对齐随之跳动。

### 验证

- **真实浏览器 21/21**（新增 `docs/reports/ia-verify/verify_repo_import_ui.py`，覆盖两主题两宽度）：选择器与「加载索引」**同行**（y 中心差 0.0px）；分段控件渲染 3 源且默认高亮 GitHub；切换后 hint 文案随之变化、高亮跟随；自定义源 hint 整行不渲染且出现手输框、切回预设源后收起；**Gitee 下「直接查看仓库」href 为 `https://gitee.com/Misyra/campus-auth-tasks` 且不含 `raw`/不以 `.json` 结尾**；空列表 footer 无「点击左侧任务…」而含核对提示，加载后出现该文案、选中后变「导入此任务」；**真实拉取 Gitee 索引拿到 9 个任务**（证明国内推荐路径端到端可用）；640px 下工具条无横向溢出（`scrollWidth - clientWidth == 0`）。
- **单测 +14**：`useRepoImport.test.ts` 新增「来源切换」7 例（索引地址互换、自定义地址跨切换保留、`sourceHomeUrl` 断言、逐源 hint、未知源回退不产生 `undefined`），`taskRepo.test.ts` 新增「仓库来源选项表」7 例（id 顺序、预设 URL 与共享常量一致、自定义项两地址皆空、**hint 必须能把国内用户导向 Gitee 并说明原因**、`indexUrl !== homeUrl`、`homeUrl` 非 raw、Gitee 镜像同 owner/name）。
- **变异验证 4/4 全部被捕获**：① `sourceHomeUrl` 退回读 `repoImport.url` → 「预设源给仓库主页」失败；② 去掉 `else if (customUrl)` 前置判断（无条件回填）→ 「切到自定义保留已填 URL」失败；③ 自定义源 hint 改为非空 → 「自定义源无说明」失败；④ Gitee hint 去掉国内表述 → 两个文件各 1 例失败。已全部复原（复原后 `npm run build` 产物 hash 与变异前一致，确认无残留）。
- vitest **205 passed**（23 文件）、`vue-tsc` 零错误、`npm run build` 通过。

## 开发中（2026-09-16 修复「分享适配」指向错误的仓库）

### 缺陷

任务页「浏览器任务」卡头的**「分享适配」按钮指向主程序仓库** `Misyra/Campus-Auth-rs`，而它的用途是"把你的登录任务分享给社区"——**任务由独立仓库 `Misyra/campus-auth-tasks` 承载**。点过去只会看到 Rust 源码，找不到任何可分享或可导入的任务，该入口完全无效。

同一页的「仓库导入」读的正是 `campus-auth-tasks` 的 `index.json`（`useRepoImport.ts`），录制器脚本也引导用户把任务提交到该仓库的 Issues（`resources/tools/task-recorder.user.js:2573`）——即**同一个仓库，在同一屏里被写成了两个地址**。

### 修复

- 「分享适配」改指 `https://github.com/Misyra/campus-auth-tasks`，`title` 由「分享你的适配方案」改为「把你的登录任务分享到任务仓库，供他人一键导入」（说明点进去之后做什么）。
- **顺带收敛重复定义**：任务仓库坐标原本散在三处（「分享适配」硬编码的 hub 地址、「任务仓库 →」的硬编码、`useRepoImport` 的两条索引地址），这正是上面出错的原因——两处各写一份必然漂移。现集中到 `utils/constants.ts` 的 `TASK_REPO_OWNER` / `TASK_REPO_NAME` / `TASK_REPO_URL` / `TASK_REPO_INDEX_URL` / `TASK_REPO_INDEX_URL_GITEE`，三个消费点全部改为引用。
- 未改动 `AboutView` 的仓库主页与 LICENSE 链接：它们指向主程序仓库是**正确**的（那是关于本程序自身）。

### 验证

- **真实浏览器 4/4**（`docs/reports/ia-verify/verify_task_repo_links.py`）：「分享适配」href 为任务仓库；任务页**全量扫描 `a[href^='https://github.com']` 只有一条**且指向任务仓库（证明页面里不再有任何指向主程序仓库的外链）；「任务仓库 →」同为任务仓库。
- **单测 7 例**（`utils/taskRepo.test.ts`）：锁定任务仓库坐标不与主程序仓库重合、两个索引源都含任务仓库路径且不含 `/Campus-Auth-rs/`、索引是可直接 GET 的 raw 地址；并以源码断言锁住三个消费点都改用共享常量、不得再出现硬编码的 hub 地址。
- **变异验证**：把「分享适配」改回 `https://github.com/Misyra/Campus-Auth-rs` → 2 个用例失败（`expected ... to contain ':href="TASK_REPO_URL"'`、`expected ... not to contain 'https://github.com/Misyra/Campus-Auth-rs'`），确认护栏拦得住这次的原缺陷；已复原。
- vitest **191 passed**（23 文件）、`vue-tsc` 零错误、`npm run build` 通过；其余浏览器套件全绿（`verify_ui` 25/25、`verify_autoopen` 21/21、`verify_network_match_collapse` 15/15、`verify_run_mode` 23/23、`verify_confirm_dialog_regression` 9/9、`verify_clear_password_ui` 9/9）。

## 开发中（2026-09-16 确认框支持结构化改动清单）

- 原实现把改动拼成字符串塞进 `message`，靠 `white-space: pre-line` 换行：字段名与取值**同权重同颜色**、无对齐、无视觉指向，四五项时就是一团文字（实测效果如"浏览器后台运行：开启 → 关闭"逐行堆叠）。
- `useConfirm` 新增可选 `changes: ConfirmChange[]`（`{label, from, to}`）与 `confirm-message--tight` 间距修饰；`ConfirmDialog` 渲染为两列对齐列表：字段名在左，旧值浅灰、右侧箭头、**新值加粗**——一眼看出"哪一项会变成什么"。
- 走**可选字段**而非改 `message` 语义：既有几十处调用（删除方案、清空历史、退出应用、放弃草稿等）完全不传 `changes`，渲染路径不变，无需逐个改动。
- 新增 `arrow-right` 图标（`IconApp` 此前只有 `arrow-left`，补镜像版）。
- **两处自身修正**：① 旧值起初用 `line-through` 划线，放大实测发现短字面（`INFO`）被横线穿过主干后**比不划更难读**，改为纯低对比度弱化；② 间距修饰原本想用 `:has(+ .confirm-changes)`，改为显式类——与本文件既有约定一致（见 `.modal-overlay--confirm` 的说明：不依赖较新选择器）。
- **未引入 `--text-muted-rgb`**：核实该变量不存在（`--text-muted` 只有色值形式），避免了一个悄悄失效的样式声明。

### 验证

- **运行模式端到端 23/23**（探针同步适配：改动已从 `message` 文本移到结构化列表，改为读 `.confirm-change` 行并逐行打印）。
- **确认框回归 9/9**（新增 `verify_confirm_dialog_regression.py`，专门为"改的是全局共享组件"而做）：无 `changes` 时**不渲染列表**（既有用法零影响）、`danger` 样式在、**`--z-confirm` 层级仍为 500**（Earlier 轮次修过的"确认框被弹窗压住"未复发）、Esc 可取消且不误退出、打开时焦点落在按钮、Tab 在框内循环。
- 其余套件全绿：`verify_network_match_collapse` 15/15、`verify_autoopen` 21/21、`verify_ui` 25/25、`verify_clear_password_ui` 9/9。
- `verify_new_defaults` 改为**前置校验后明确跳过**：它只适用于"删空 `config/` 后新建"的全新安装场景，而当前运行目录是既有配置（`startup_action=monitor`）——既有配置保留自己的值正是正确行为，此时它的断言本不成立。此前会报 3 条假失败，现改为打印跳过原因与复现步骤。
- vitest **184 passed**、`vue-tsc` 零错误、`npm run build` 通过。

## 开发中（2026-09-16 「设置 · 系统」新增「运行模式」预设：默认 / 调试 / 自定义）

### 背景与调研结论

需求是「加几个默认选项，一键改一组值」。调研后确认三件事，它们决定了实现形态：

- **`developer_mode` 是死字段**：`src/config/schema.rs:356` 定义、默认 `false`，但后端无消费点、前端 `types.ts` 连类型都没有。故"调试模式"不能复用它，需另建概念。
- **预设涉及的三类生效机制不同**：多数项可经 `PATCH /api/config` 一次提交；`logging.level` 必须走 `PUT /api/config/log-level`（仅 PATCH 只落盘**不热更新 tracing filter**，会出现"界面显示 DEBUG、实际仍按 INFO 过滤"的静默假象）；`autostart_enabled` 不在 PATCH 白名单，只能走 `POST /api/autostart/*` 且**会真实写系统注册表**。故"一键切换"必然跨 3 个 API、**非原子**——这一点在设计上被正面处理（见下），而非掩盖。
- **跨 API 的中途失败会留下半套配置**：为此失败时明确提示"部分设置可能已生效，请检查后重试"，并尽力回读让界面反映真实状态，避免用户以为"什么都没变"而重复点击。

### 实现

- 新增 `utils/runMode.ts`（**纯函数**，可直测）：预设定义、模式判定 `detectRunMode`、差异计算 `diffRunMode`、值格式化。
- 新增 `composables/useRunMode.ts`：读取当前值 → 判定模式 → 应用预设（与 API 打交道的部分）。
- 「设置 · 系统」页顶部新增「运行模式」卡：两个可选预设 + 一个**只读**的「自定义」态，卡片头徽标实时显示当前模式。

**预设内容**（`low_resource_mode` 两组都关：调试要看得见验证码图，默认模式也没理由为省资源而牺牲兼容性）：

| | 默认模式 | 调试模式 |
|---|---|---|
| 浏览器后台运行 | 开启 | **关闭**（看得见浏览器） |
| 登录后保持浏览器进程 | 关闭 | **开启**（反复查看现场） |
| 启动后执行 | 开始检测 | **无操作**（手动跑一次才看得见过程） |
| 日志级别 | INFO | **DEBUG** |
| 开机自启动 | 开启 | **关闭** |
| 低资源模式 | 关闭 | 关闭（两组相同） |

**三个设计取舍（均有理由，非默认行为）**：

1. **不含 `strict_login_mode`**：它决定"何时触发登录"，属功能行为而非可观测性；放进调试模式会让登录在证据不足时也尝试，可能在用户没预期的时机弹出浏览器。那是修 bug 的手段，不是"方便排查"的开关。
2. **「自定义」不可点**：它是"手动改过设置"的自然回落，不是一个可选目标。任一字段与预设不符即判为自定义——不做"匹配度最高者胜"的模糊判定，那会让界面显示一个用户并没选过的模式名。
3. **切换前弹确认并逐项列出改动**：由 `diffRunMode` 生成，**只列实际有变化的项**（实测确认框里不出现"低资源模式"，因两组取值相同）。这个动作会写多处设置、还会真实注册/取消开机自启，用户有权在动手前看到究竟改了什么。
4. **未保存的修改先征得同意**：应用模式后必须回读才能让界面与磁盘一致，而回读会冲掉设置页草稿；故先确认，用户取消则整体放弃（不产生任何写入）。

### 验证

- **纯逻辑单测 26 例**（`utils/runMode.test.ts`）：模式判定（完全匹配 / 任一字段不符即自定义 / 两边各取一半）、大小写与 `WARNING`→`WARN` 归一化、差异计算只列变化项、值格式化（布尔转开启/关闭、未知枚举值原样返回不崩）、`custom` 不可作为目标。
- **变异验证**：把 `matchesPreset` 的逐字段比对改成"只比第一个字段" → 3 个用例失败（`expected 'debug' to be 'custom'`），确认"任一字段不符即自定义"的判定真的承重；已复原。
- **真实浏览器端到端 22/22**（`docs/reports/ia-verify/verify_run_mode.py`）：卡片与三个选项齐全、「自定义」非按钮不可点；切到调试模式后**服务端 5 项全部落实**（`headless=false`、`keep_alive=true`、`startup_action=none`、`log_level=DEBUG`、`autostart=false`）、徽标变「调试模式」；在「设置 · 浏览器」手动开一项并保存后**徽标自动回落「自定义」**；切回默认模式后 5 项回到默认值、徽标变「默认模式」；确认框只列实际会变的项；无 JS 错误；**测试后自动恢复到测试前状态**。
- **系统注册表核对**：`autostart` 关闭时 `HKCU\...\Run` 下无 `Campus-Auth` 项，与 `settings.json` 的 `autostart_enabled` 一致——确认预设对自启动的改动是真的落到系统而非只改配置。
- 我自己的探针修了两处**假设错误**（非产品缺陷）：① 硬断言"确认框必含开机自启动"，而该项当时已与目标一致、**正确地**未被列出——改为断言"只列实际会变的项"；② 「低资源模式」在「浏览器」Tab，探针却在「系统」Tab 找它。另把探针改成状态无关（重跑时若已是调试模式，点击会被正确忽略，不再误判为"确认框没弹出"）。
- `vue-tsc` 零错误、vitest **184 passed**（22 文件）、`npm run build` 通过。

## 开发中（2026-09-16 默认值调整：不自动监测、不自动切方案；方案编辑器「网络匹配」默认折叠）

### 「网络匹配」区改为可折叠、默认收起

- `ProfilesView` 的「网络匹配」（网关 IP / WiFi 名称 / 检测当前网络）由常驻展开改为可折叠区，头部即切换按钮（与「设置 · 任务与环境」的 Python 环境卡同一交互模式，含 `role="button"` / `tabindex` / `aria-expanded` / Enter / Space）。
- **已配过匹配规则的方案默认展开**：`watch(editingProfile)` 在换草稿时按内容决定初值——收起会让人看不到自己设过的规则，以为丢了。同一份草稿内手动折叠后不再重算（否则改任意字段都会弹回展开，手动折叠形同虚设）。
- 折叠态显示摘要（`网关 192.168.7.7 · SSID Dorm-9F`），避免"看不见就以为没配"。
- 折叠状态是组件局部 `ref`，不持久化——它只是当次的查看偏好。

### 默认值：启动不自动监测、不自动切换方案

两处 `impl Default` 与枚举的 `#[default]` 同步改动：

- `AppSettings::default().startup_action`：`Monitor` → **`None`**，并把 `StartupAction` 的 `#[default]` 从 `Monitor` 移到 `None`（两处必须同时改，否则 `AppSettings::default()` 与 `StartupAction::default()` 语义不一致）。效果：全新配置启动后引擎停在 `stopped`，需在控制台点「启动检测」。
- `SettingsData::default().auto_switch`：`true` → **`false`**。效果：单网络环境用户不再每 60s 空转方案匹配；副作用是方案卡片变为可点击切换（`ProfilesView` 的 `!autoSwitch && setActiveProfile` 分支）。
- 前端兜底同步：`DEFAULT_CONFIG.app_settings.startup_action` 改 `"none"`、`useProfiles` 的 `autoSwitch` 初值改 `false`（均为加载失败时的占位，正常以服务端下发为准）。
- **不影响既有配置**：`#[serde(default)]` 只在字段缺失时生效，磁盘上已写的值优先。已实测（见验证）：放入 `auto_switch: true` + `startup_action: monitor` 的旧配置，新版本读取后仍为原值、磁盘未被回写、引擎照常自动启动。

### 联动修复：受影响的两处测试

- `config::service::tests` 的 `test_settings_data_default_values` / `test_settings_data_partial_json_fills_defaults` 原断言 `auto_switch == true`，改为断言新默认 `false`（后者显式覆盖"字段缺失回退默认"）。
- `test_modify_settings_concurrent_switch_and_toggle_field_isolation` 的注释称"默认 auto_switch 恰为 true"，已改为说明"不依赖默认值"（该用例本就显式置起点，只是注释过期）。
- **集成测试 `login_chain_auto_relogin_after_kick` 真的失败了**（首次运行 301s 超时）：它依赖"启动即自动监测"，自身从不调 `POST /api/monitor/start`，其旧文档注释也自认这一点（"实例启动即开始检测"）。这正是本次默认值变更的正确后果——已改为用例内**显式启动监测**并断言启动成功，让前提不再隐式依赖默认值。修复后 5 个用例全通过。

### 验证

- **全新配置端到端 12/12**（`docs/reports/ia-verify/verify_new_defaults.py`，删空 `config/` 后启动真实二进制）：`startup_action=none`、`auto_switch=false`；启动后 `engine_state=stopped`、`probe_total=0`、侧栏显示「已停止」；设置页启动动作显示「无操作」；方案页「自动切换」开关为关闭且文案为「自动切换已关闭」。
- **反向验证（排除"引擎本来就起不来"的假通过）**：同一实例点「启动检测」后 `engine_state=running`、`probe_total=1`，再点「停止检测」回到 `stopped`。只断言"启动后是 stopped"会被"永远起不来"骗过，故两向都验。
- **老配置不受影响（决定性检查）**：把含 `auto_switch: true` + `startup_action: monitor` 的旧配置放入运行目录后启动 → API 回读仍为 `True`/`monitor`、**磁盘未被回写**、`engine_state=running`（旧行为保留）。
- 折叠区 15/15（`verify_network_match_collapse.py`）：无规则默认折叠且无摘要、有规则默认展开、点击/回车可切换、`aria-expanded` 正确、**改动其它字段不弹回展开**、折叠态摘要含网关与 SSID、测试方案已清理。
- 日志佐证：当日 13:30 之前的每次启动均有 `按 startup_action=monitor 已启动检测`，13:30:41 的全新配置启动**无该行**——默认值变更确实生效。
- `cargo fmt --check` / `cargo clippy --all-targets --features no-embed -- -D warnings` 零告警；`cargo test --features no-embed` **873 lib + 全集成 crate 通过**；`vue-tsc` 零错误、vitest 158 passed、`npm run build` 通过。

### 待确认的联动（未改动，留给决策）

「开机自启」注册的命令行**不带** `--startup-action`（`src/utils/platform.rs` 三平台均只注册 exe 路径），启动动作完全取自 `settings.json`。因此新默认下「开启开机自启 + 全新配置」= 开机后程序启动但**保持待机**，需手动点「启动检测」——这未必是自启用户想要的。若要贴合直觉，可让「启用自启」顺带把 `startup_action` 设为 `monitor`（或注册时带上 `--startup-action monitor`）；本次未动，因其属行为变更而非默认值调整。

## 开发中（2026-09-16 任务页 Tab 文案：AI 生成 → AI 生成浏览器任务）

- 任务页第四个 Tab 由「AI 生成」改为「**AI 生成浏览器任务**」，路由 meta 标题同步为「任务 · AI 生成浏览器任务」，侧栏「任务」的 `title` 悬停提示同步。
- 改动理由：该 Tab 与左侧「浏览器任务」并列时，只写「AI 生成」看不出生成的是**什么**（脚本？定时任务？），用户得点进去才知道。补全宾语后一眼可知这一栏产出的是浏览器任务——它也正是登录实际执行的那一类，与「浏览器任务」的产物同类。
- 布局：标签栏是 `width: fit-content` 自适应（`tasks.css:5-9`），桌面端 4 个页签总宽 527px、单行 60px 高，未折行，**主尺寸无需改 CSS**。
- **过程中发现并修掉一处自身引入的缺陷（窄屏折行）**：`responsive.css` 原有 `.tasks-tabs .settings-tab { flex: 1 1 30% }`（三 Tab 时代为均分而设），改到 22% 后 760px 下每栏仅 75px，四个页签**全部被压成两行**（实测「浏览器任 / 务」「AI 生成浏 / 览器任务」）。已改为 `flex: 0 1 auto` + 只收紧内边距，并给 `.tasks-tabs .settings-tab span` 加 `white-space: nowrap`，让长标签靠容器换行（`.settings-tabs` 本就有 `flex-wrap: wrap`）而不是把文案压断。
- 同类修正：`tasks.css` 头部注释写「本页只有 3 个 [页签]」、`responsive.css` 写「任务页三 Tab 窄屏均分」、`ai_task.css` 写「第三个 Tab / 其他两 Tab」——均为定时任务并入前的旧口径，已同步为四项。
- 验证：浏览器断言 **8/8**——文案数组精确匹配、标签栏宽度（527px）与高度（60px）、AI Tab 可点开且内容渲染、窄屏零溢出、**窄屏四项文案均单行**（按计算行高判定 `lines≈1`，未截断）、无 JS 错误；`vue-tsc` 零错误、`npm run build` 通过、vitest 158 passed。
- **探针自身的教训**：初版窄屏检查只比 `scrollWidth - clientWidth`，而**折行时两者相等**——该检查对折行是假通过，已改为按行高判定行数（`height / lineHeight`），才暴露出上面那处折行。
- 同步：`task-manual.md:17`（Tab 清单）、`user-guide.md:77,111`（数据归属表与任务段）、changelog/updatelog 自身对 Tab 名的引用。

## 开发中（2026-09-16 「方案」页进入即展示当前方案编辑器）

### 问题：账号唯一入口却先落列表，高频操作多两步

上一节把账号/认证/登录方式收敛到「方案」页后，进入该页最高频的用途变成**改当前方案的账号**，但页面默认展示列表，用户还得找到卡片再点「编辑」。方案数越多、卡片列表越长，这段路越没有意义。

### 处置

- 新增 `openActiveProfileForEdit()`（`useProfiles`），进入「方案」页时自动打开**活跃方案**的编辑器；`ProfilesView` 的 `onMounted` 改为 `await fetchProfiles()` 后再尝试打开。
- **关键防御：列表未就绪时绝不打开。** `showProfileEditor(id)` 对"id 不在已加载列表里"的既有语义是**打开空白新建表单**（else 分支），而 `activeProfileId` 初始值恒为 `"default"`、`profiles` 在首个响应到达前为空——若直接调用，用户进入页面会看到一个空白表单并以为配置丢了，比留在列表页更糟。故 `openActiveProfileForEdit` 显式前置校验 `profiles[id]` 存在，否则返回 false 留在列表页。
- **已有草稿直接复用，不重载**：用户上次在本页编到一半就切走（或点了侧栏再点回来），单例草稿仍在。若重载会走 `confirmDiscardIfDirty`，等于每次回页都被问「是否放弃未保存的修改」。
- 自动打开**只在挂载时执行一次**：回列表后不会被立刻重新打开，用户「返回方案列表」的意图被尊重。
- 编辑器顶栏改为「返回按钮 + 标题」在左、「方案切换器 + 当前使用徽标」在右：编辑中可直接换方案，不必退回列表再进来（此前顶栏第三个 div 只是占位）。切换复用 `openEditor` 既有路径，dirty 时先确认；用户取消则 `editingProfile` 保持原指向，下拉显示随之回退，不会出现「下拉已变、实际还在编旧方案」。
- 新建草稿不显示切换器（无既有方案可切）。

### 验证

- vitest **158 passed**（21 文件；新增 `useProfiles.test.ts` 5 例）；`vue-tsc` 零错误；`npm run build` 通过。
- **变异验证**：删掉 `openActiveProfileForEdit` 里的 `profiles[id]` 前置校验 → 「列表未加载时不打开」用例失败（`expected true to be false`），确认该防御真的承重而非冗余；已复原。
- **真实浏览器 21 项断言全过**（`docs/reports/ia-verify/verify_autoopen.py`）：建 A/B 两方案并把 **B 设为活跃**后进入 `/profiles`，直接落在编辑器且账号为 `user-b`（**证明打开的是活跃方案而非列表首项**）；顶栏「当前使用」徽标与切换器存在；编辑器内切到 A 生效且徽标消失；侧栏离开再回来**不弹放弃确认**且改动 `draft-keep-me` 仍在（单例复用）；**整页 F5 后回落展示活跃方案**（单例随刷新清空）；「返回方案列表」后停在列表、不被自动重开、约 1s 后仍未被重开；「新建方案」仍可进入新建表单（ID 可编辑、无切换器）；无 JS 错误。测试方案已清理、活跃方案还原 `default`。

## 开发中（2026-09-16 界面按「配置对象」重组：方案成为账号类的唯一入口）

### 问题：同一份 `ProfileData` 有三个可写入口

界面此前按"功能清单"分组，而数据模型是"方案中心"的，两者错位：

- `GlobalConfig` 里**根本没有账号字段**（`src/config/schema.rs:38-56` 只有 browser/monitor/pause/logging/retry_settings/worker/app/updater）。
- 「设置 · 账号」不是全局设置的一个切面，而是**活跃方案凭据的第二个视图**——`PATCH /api/config` 把 `username/password/auth_url/trigger_url/isp/active_task/login_channel/http_*` 共 13 个键映射回 Profile（`src/web/routes/config.rs:90-108`），`GET` 也只是把它们摊平到顶层。

于是账号/认证/登录方式有了「设置 · 账号」「配置方案」编辑器两处等价入口（第三处「任务 · 直连登录」是上一轮新增），用户无从判断"改哪边才生效"。由此还带出两个实缺陷：

- **`留空使用全局` 是假文案**：`resolve_profile`（`src/login/mod.rs:655-672`）只在方案之间回退，从不回退全局账号；留空即登录校验失败。更糟的是失败文案写「请在**设置页**填写账号」（`src/login/mod.rs:740`）——照它去设置页填，改的是活跃方案（可能是另一个方案）。
- **known-issues #23 E1**：账号混在全局保存栏里，`useConfig` 的保存载荷带着 `auth_url`，于是「自动检测」填入的未确认候选地址会被任意 Tab 的「立即保存」静默落盘。

### 处置：让 UI 追上数据归属，每类数据只有一个入口

导航由「仪表盘/设置/任务/关于/更多(配置方案·定时任务·外观)」改为五项，按**配置对象**划分：

| 导航 | 编辑对象 | 存储 / 接口 |
|------|----------|-------------|
| 仪表盘 | 状态总览与手动操作 | — |
| **方案** | 账号、密码、认证地址、匹配规则、登录方式、直连参数 | `/api/profiles/*` |
| **任务** | 浏览器任务 / 脚本 / 定时任务 / AI 生成浏览器任务 | `/api/tasks`、`/api/scripts`、`/api/scheduler/jobs` |
| **设置** | 检测 / 浏览器 / 任务与环境 / 系统 / 网络与更新 / 外观 | `/api/config` |
| 关于 | 版本、更新与卸载 | — |

- 删除「设置 · 账号」Tab（`AccountSettings.vue` 删除），`SETTINGS_TABS` 去掉「账号」项、加入「外观」项（6 → 6，仍为 6 Tab）；`/settings/account` 保留为 **redirect → `/profiles`**，旧深链与书签不 404。
- 删除上一轮新增的「任务 · 直连登录」Tab（`DirectLoginPanel.vue` + `useDirectLogin.ts` 删除，路由与 `editorGuard` 第三分支同步移除）：直连参数本就是方案字段，方案页是它的唯一入口，该 Tab 沦为冗余。
- 「定时任务」由侧栏独立页并入任务页 Tab（`/tasks/scheduled`）；「外观」由侧栏独立页并入设置页 Tab（`/settings/appearance`）；两者旧路径保留 redirect。侧栏「更多」次级菜单随之删除（不再有需要折叠的项），`sidebar.css` / `responsive.css` 的 `.nav-more*` 规则同步清理。
- **设置页保存栏不再触碰任何方案字段**：`useConfig` 删除 `credentials` 段、独立 `password` 实例、`validateConfig` 的 auth_url/trigger_url 检查，`SaveConfigPayload` 只余 8 个全局键。`refreshActiveProfileConfig()`（把活跃方案凭据同步进设置页副本）随之删除，`useProfiles` 不再 import `useConfig`——它存在的唯一理由就是维护那份副本。
- `usePasswordField.ts` 删除：它的唯一用途是 `PATCH /api/config` 的三态密码契约，该契约已无调用方。
- `CredentialsConfig` 类型保留为字段清单的单一说明来源（`LoginChannelField` / `HttpLoginWizard` 的草稿契约引用它），但不再挂在 `Config` 下。
- `DashboardView` 的直连渠道判定与 `TaskEnvironmentSettings` 的"当前任务"改读方案摘要（`ProfileSummary.login_channel` / `active_task`），不再从 `GET /api/config` 的凭据投影取。
- 外观 Tab 在设置页内**不显示保存栏**（`isAppearanceTab`）：它改动即时写 localStorage 并生效，显示「立即保存」会让用户点下去只得到「配置没有变更」。同时去掉 `AppearanceView` 自带的 `.page-content` 外壳，避免与设置页框架嵌套出重复入场动画。

### 后端：补齐方案页取代账号页所必需的两个字段能力

账号页原有的两个能力在方案页无对应实现，必须补上（否则是净能力回退）：

- **`PUT /api/profiles/{id}` 新增 `clear_password`**（`ProfileUpdateBody`）。`password` 的空串语义是「未修改，保留原密码」，**无法**表达清除；清除此前只能经 `PATCH /api/config` 对活跃方案完成，那正是账号页的路径。
- 同时把该意图穿透到服务层：`ProfileApi::update_profile` 增加 `clear_password: bool` 参数。不能只靠置空 `ProfileData::password`——`ProfileService::update_profile` 会把它再交给 `save_password`，而后者的空串契约正是「保留原密码」，清除会被静默撤销（`src/config/profiles.rs:171-178`）。
- **`GET /api/profiles/{id}` 新增 `has_password`**（口径复用 `effective_has_password`，由私有改 `pub(crate)`）。此前该响应只把 `password` 置空后回 settings，前端无法区分「没设密码」与「有密码但被抹掉」，占位文案只能猜。
- `openapi.json` **无需改动**：它没有字段级 schema（`/api/profiles/{id}` 条目内均为 `"schema": {}`，见 `openapi.json:617-654`），两个新字段不涉及路径增删。
- 方案编辑器的密码区随之接入：已保存时显示「已保存，留空保留；输入新密码则更新」+「清除已保存密码」按钮，请求态 `_clearPassword` 放在**草稿对象内**（放对象外则「只点了清除」不产生未保存标记，离开时静默丢失），并提供「撤销清除」；账号占位由「留空使用全局」改为「学号 / 上网账号」+「留空无法自动认证」。

### 验证

- **前端**：`vue-tsc -p tsconfig.app.json` 零错误；`npm run build` 通过；vitest **153 passed**（20 文件）。
- **回归护栏**（新增用例锁定新边界）：`useConfig.test.ts` 新增「保存载荷只含全局设置」——逐个断言 15 个方案域键（含 `clear_password`）不得出现在 `PATCH /api/config` 载荷里，并断言 `useConfig` 不再暴露 `password`/`clearPassword`、`config` 上无 `credentials`；「后端仍在扁平响应里回传凭据时本 composable 不接收」用例确认接收这些字段不会误标 dirty。`editorGuard.test.ts` 移除直连分支用例，改为覆盖定时任务 Tab 的内部切换与离开判定。
- **Rust**：`cargo fmt --check` / `cargo clippy --all-targets --features no-embed -- -D warnings` 零告警；`cargo test --features no-embed` **873 lib + 全集成 crate 通过**。
- **变异验证**（新增护栏必须能失败才算数，三处均已复原）：① 路由层 `update_profile` 的 `clear_password` 透传改为恒 `false` → `test_put_clear_password_empties_existing_password` 失败（`left: "ENC:old-secret"`）；② 服务层 `if clear_password` 改为 `&& false` → `test_update_profile_clear_password_empties_existing` 失败；③ 过程中发现**路由层 handler 内的 `profile.password.clear()` 是死代码**——删掉它测试仍全绿，因为真正的判定在服务层，故将其删除、只保留意图透传，测试随即能锁住透传（这正是变异验证的价值：避免留下"看起来在防护"的冗余分支）。

### 文档

- `user-guide.md`：第 2 节「Web 控制台」改为五处导航的数据归属表（并说明账号属方案、切方案即切账号）；第 3 节补 `has_password` / `clear_password`；第 4 节任务页四 Tab 重排、定时任务并入、录制器入口指向、「设置·环境」等旧称统一。
- `task-manual.md`：任务页 Tab 清单与「直连登录」段改为"方案页是唯一入口"；启用任务的选择位置指向方案编辑器。
- `http-login-guide.md`：第 1 节切换入口、第 7 节保存步骤、FAQ 向导入口三处去掉「设置 · 账号」。
- `known-issues.md`：#23 E1 标记为 2026-09-16 已修复（架构性消除）并注明回归用例位置。

## 开发中（2026-09-15 默认主题文字加深 + 直连登录 UI 重做、向导与使用文档）

### 默认主题（浅色）文字色整体压到黑色系

- `base.css` 的 `[data-theme="light"]` 四级文字 token 由 `#000000 / #4a4a4a / #666666 / #6b7280` 改为 `#000000 / #1a1a1a / #333333 / #4a4a4a`。原次级/弱化档是中性灰，在浅色玻璃卡片（`--bg-card: rgba(255,255,255,.45)`）上半透明合成后观感发灰，长段落（协议文本、字段说明）尤其明显；现按「正文纯黑 → 次级近黑 → 弱化深灰」递降，层级仍可辨。
- **动态实测对比度**（Playwright + WCAG 相对亮度，分别对 body 底色与「卡片色合成 body 底色」两处取值，取最差）：`--text-primary` 18.68:1、`--text-secondary` 15.48:1、`--text-muted` 11.24:1、`--text-tertiary` 7.88:1，四档均远超 AA 正文 4.5:1。深色主题（`:root`）与 `--text-on-accent` / `--on-accent` 双轨未动，实测深色下仍为 `#ffffff / #b3b3b3 / #999999`。

### 直连面板 UI 重做（`LoginChannelField.vue`）

- **渠道选择由分段控件改为两张说明卡片**：各自给出「怎么工作」与成本标签（浏览器自动化=「需要 Python 与浏览器」/直连请求=「免 Python 与浏览器」）。原分段控件只有两个词，用户无法从界面判断两者代价差异，而这正是选渠道时唯一的决策依据。
- **直连参数按因果顺序编号分组**（① 请求地址 → ② 请求头/请求内容 → ③ 成功/失败关键字 → ④ 凭据变换脚本 → ⑤ 测试），每组带一句「该做什么」。此前 7 个输入平铺成一片，用户常只填地址就点测试，拿到「未命中成功标识」后不知还差什么。
- 新增「填入示例」按钮（地址/请求头/请求内容/脚本骨架），占位符与内置函数改为 chip 速查；判定顺序（先失败后成功）与「响应恒 200 门户必须填关键字」在 ③ 组内显式说明。
- 凭据变换脚本默认收起（绝大多数门户不需要），已配置时自动展开并显示「已配置」徽标。
- **测试结果面板补「下一步该怎么办」**：新增 `httpTestOutcomeHint()`，按 outcome 给出针对性指引（如 `assertion_failed` 明确指向「核对成功关键字；响应恒 200 时必须填」）。原面板只有结论标签，用户不知道改哪个字段，容易反复重试同一错配置。请求/响应报文收进 `<details>` 折叠、脚本错误单独高亮。
- 向导入口挂在标题行右侧（胶囊按钮）；**该入口独立于标题渲染**——`AccountSettings` 传 `:title="null"`，若与标题同处一个 `v-if` 会导致设置页整块入口消失（本轮自查发现并修正，见验证）。

### 新增直连登录分步向导（`HttpLoginWizard.vue`）

- 四步：找到登录请求 → 填写请求 → 设定判定 → 发送测试。每步只暴露该步字段，步骤条可点回看（未到达步禁用）。
- **与宿主共用同一草稿对象**（`v-model` 原地改）：向导内改动立即写回方案编辑器 / 设置页表单，关闭不丢，宿主原有 dirty 判定照常生效；向导自身不持有第二份状态、不负责保存。
- **按步把关并在底部说明被拦原因**：第 1 步要求账号可用（该步本身就把账号行标红，仍放行会让人走到第 4 步才发现）、第 2/3 步要求请求地址、第 4 步要求无缺口。禁用按钮同时给出 `blockedReason` 文案——只禁用不说明等于把用户卡住。
- 认证地址检测结果经 `portalDetected` 事件交回宿主写回（该字段不在组件读写的草稿契约内，且两个宿主落点不同）。
- 向导内嵌 `ctx` / 内置函数清单与 eportal（按来源 IP 推导密钥）的完整示例脚本。

### 新增使用文档与文档端点

- `docs/guides/http-login-guide.md`：从「怎么判断门户适不适合直连」到抓包、占位符、成败判定、凭据变换脚本（含 eportal XOR 完整示例）、测试结果解读、边界与已知限制、FAQ。
- 新增 `GET /api/docs/http-login-guide`（`src/web/routes/system.rs::http_login_guide`），前端面板与向导的「使用文档」入口指向它；`openapi.json` / `auth.rs` 免鉴权白名单 / `static_files::GuideAsset` 的 `#[include]` 同步。
- **顺带收敛三份指南的样板代码**：`resolve_guide_path` / `embedded_guide` / `resolve_manual_path` / `embedded_manual` 四段逐字重复的实现合并为 `GuideFile` 描述表 + `serve_guide()`。原实现新增一份指南要再抄一遍「嵌入兜底 → 磁盘查找 → 读文件 → 组响应」，漏抄 `#[include]` 时开发机（磁盘有文件）一切正常、只有便携包在缺 `docs/` 的真实用户那里 404。

### 验证

- **前端**：`vue-tsc -p tsconfig.app.json` 零错误；`npm run build` 通过；vitest **129 passed**（`loginChannel.test.ts` 由 6 → 20 例，覆盖 `httpTestOutcomeHint` 六种结论互不相同且 `assertion_failed` 点名恒 200 陷阱、`httpConfigGaps` 的顺序与「已保存方案不算缺密码」、`isCredentialExposedViaGet` 三情形、内置函数清单锁定）。
- **Rust**：`cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` 零告警；`cargo test --features no-embed` **855 lib + 全集成 crate 通过**；默认特性下新增的 `every_guide_is_embedded` / `guide_path_falls_back_to_manifest_dir` / `all_guides_resolve_to_existing_files` 通过。
- **变异验证**（新增护栏必须能失败才算数）：从 `GuideAsset` 删 `#[include = "http-login-guide.md"]` → `every_guide_is_embedded` 失败并报「http-login-guide.md 未嵌入」；把 `HTTP_LOGIN_GUIDE.rel_path` 改成不存在路径 → `all_guides_resolve_to_existing_files` 失败并打印该路径。两处均已复原。
- **真实二进制 + Playwright 动态验证**：便携包语义（base 目录**不含** `docs/`）下 `GET /api/docs/http-login-guide` 返回 200 / `text/markdown` / `filename="http-login-guide.md"`，正文为 Markdown 原文 ← 证明嵌入兜底真的生效，而非只走开发机磁盘。
- **端到端**：Playwright 驱动真实二进制 + `tests/mock-servers/full-portal` 门户，从 UI 填 POST 表单与成败关键字后点「发送测试请求」——正确凭据得「请求判定成功」，错误密码得「门户拒绝凭据」。**两个方向都验证**才排除「关键字恒匹配」的假通过（首次探针因 mock 路径解析失败报「请求未送达」，修正后重跑；该现象也反向确认了 network_error 分支的文案确实可读）。会话脚本错误数 0；700px 窄屏无横向溢出。
- 过程中修正一处自身缺陷：向导入口原本与标题同处 `v-if="title !== null"`，而「设置 · 账号」传 `:title="null"`，入口在该页会整块消失——已拆出独立的标题行容器并用设置页回归探针锁定（入口可见、点击可开、浏览器渠道面板仍在）。

## 开发中（2026-09-16 任务页新增「直连登录」Tab + 修正浏览器任务下拉）

### 浏览器任务下拉：去掉与 `default` 重复的空值项

下拉首项曾是空值「使用内置默认任务」，而它指向的就是任务列表里的 `default`
（`DEFAULT_TASK_ID`，播种名「通用登录」）——**同一件事两个条目**，用户不知选哪个；
且 `default` 那条不带任何说明，看不出它就是"内置默认"。

- 选项改为只来自任务列表（`browserTaskOptions`），`default` 一条加「（内置默认）」后缀承袭原语义。
- 显示走 `taskBindingDisplay` + computed 代理：未绑定（空 `active_task`）时显示为内置默认任务，
  但**读取绝不写回草稿**——否则编辑器一打开就与服务端不一致，立刻显示「未保存」。
  用户真正下拉选择时才写回，未触碰的方案保持原有空值。
- 两者等价有据：后端 `resolve_task_choice`（`src/login/mod.rs:111`）在 `active_task` 为空与
  为 `default` 时都回退同一个内置任务，故显示为 default 不改变行为。
- 逻辑落在 `utils/loginChannel.ts` 并由单测直接覆盖（与组件共用同一实现，
  避免测试自己抄一份逻辑的假覆盖）。

### 任务页新增「直连登录」Tab

直连参数（`http_*`）与登录渠道属于**方案**，此前只能在「配置方案」编辑器或
「设置 · 账号」两处编辑；只配直连时得先打开整个方案编辑器。现单列一个 Tab
（`/tasks/direct`），字段与保存语义与那两处**完全一致**（同一 `LoginChannelField`
组件、同一 `PUT /api/profiles/{id}`），三处互不冲突。

- **页内先选方案（默认当前活跃方案）**：直连参数是方案级配置，必须明确"正在改谁"，
  否则会出现"改了没生效"——登录用的是活跃方案。非活跃方案额外提示需到「配置方案」页切换。
- 草稿状态放在 `useDirectLogin` 单例而非组件内：任务页以子路由切 Tab，组件会卸载重建，
  组件内 state 会在切走时丢失（与 `useTasks`/`useScripts` 同因）。
- **不共用 `useConfig` 的草稿**：设置页也编辑同一批字段，共用会让两页的未保存状态互相串扰，
  且从任务页保存会把无关的全局设置一并提交。本页走 `PUT /api/profiles/{id}`，只写方案。
- `password` 提交空串：`update_profile` 的空密码分支是「保留原值」，故不会清掉已存密码
  （已端到端验证：保存后密文逐字节未变）。
- 离开守卫（`editorGuard`）补上直连草稿：该草稿是方案级快照，离开后不会自动恢复
  （下次进入重新从服务端载入），脏时必须确认，否则改了一半的参数会被静默丢弃。
  Tab 间切换不拦截（草稿在单例里，切回来仍在）。

### 验证

- **前端**：`vue-tsc` 零错误；vitest **155 passed**（148 + 7）；`npm run build` 通过。
- **变异验证**（护栏必须能失败）：① 去掉下拉的「（内置默认）」标注 →
  `browserTaskOptions` 两例失败；② 把 `taskBindingDisplay` 改成直接返回入参（未绑定不再显示默认任务）
  → 两例失败；③ 在 `editorGuard` 里短路直连分支（`if (false && isDirectDirty())`）→
  新增的 3 个直连用例失败。三处均已复原。
- **端到端**（真实二进制 + 浏览器）：① 任务页出现 4 个 Tab，`/tasks/direct` 直连登录为激活态；
  ② 任务下拉**只有一个条目**「通用登录（内置默认）」且已选中，重复项已消失；
  ③ 打开即无「未保存」提示（代理读取不写回）；④ 切渠道后填写请求地址并保存 → toast
  「直连登录配置已保存」，落盘 `login_channel=http` 且 `http_url`/成功关键字写入，
  **原账号与 `ENC:` 密码逐字节未变**（关键：证明空密码语义没清凭据）；
  ⑤ 方案下拉正确列出两个方案并标注「（当前使用）」，切换到另一方案后渠道与字段随之刷新；
  ⑥ 脏草稿下切 Tab 放行且草稿保留，离开 `/tasks` 区域弹出「当前直连登录配置有未保存的修改，
  确定放弃吗？」——取消则留在原页且草稿完好，确认才离开。
- **顺带更正一条错误口径**（本轮复查发现）：我在前一轮的说明中称"改前端后需 `cargo build`
  重新嵌入"，这是**错的**。实测（向 `frontend/dist` 注入标记后不重编译，运行中的 exe 立即下发
  含标记的内容）证明 debug 构建是**运行时读磁盘**的 `frontend/dist`，改前端只需 `npm run build`，
  无需重编 Rust、无需重启后端；编译期嵌入仅发生在 release。本仓库 `changelog.md:700` 早已记录
  该结论（此前那次误判源于 curl 未加 `--noproxy` 被系统代理劫持），本轮重复了同一个错误。
  **仍然成立的是**：同一 target 目录下 `--features no-embed` 与默认特性会互相覆盖同一个 exe
  （实测 no-embed 构建后重启 → `GET /` 返回 404「前端未嵌入」），故跑过 `cargo test --features no-embed`
  后仍需 `cargo build` 才能得到带界面的 exe。

## 开发中（2026-09-16 新增配置方案导出/导入分享）

### 需求

「配置方案」页增加导出与导入，便于把配好的直连方案分享给同学（单方案，不做全量导出）。

### 关键约束：密码密文跨机器不可用

导出必须剔除 `username` 与 `password`。密码在磁盘上是 `ENC:` 密文，密钥存放于
`~/.campus_network_auth/.enc_key.rs` 而**不在 config 目录内**，因此密文脱离原机即不可解。
实测确认后果：把外部 `ENC:` 密文当密码提交，`ProfileApi::save_password`
（`src/config/profiles.rs:275`）因 `can_decrypt_password` 为假而走「明文」分支**再加密一次**——
落盘值由 `ENC:AAAA` 变为 `ENC:uJOHV1G1...`，即双重加密。导入方登录必然失败，且
`/api/init-status` 的 `password_decryption_failed` 仍为 `false`（解密"成功"了，只是解出的不是密码），
排障时毫无线索。故凭据一律留空由接收方自填。

### 后端

- 新增 `GET /api/profiles/{id}/export`（`export_profile`）与 `POST /api/profiles/import`（`import_profile`），
  注册进 `web/mod.rs` route_table 与 `openapi.json`（87 → 88 paths：新端点 2 个 + 补回 1 个，见下"过程失误"）。
  openapi 采用**纯文本插入**而非重写：该文件含手写紧凑数组，任何 `json.dumps` 往返都会重排 3000+ 行
  （实测 `indent=1` + CRLF 的往返也在 `"enum": [...]` 处即分叉，故脚本先用基线断言守住再插入）。
  端点默认受鉴权保护（`auth.rs` 默认拒绝，导出含方案元信息故不进白名单）。
- **过程失误（已修复）**：为撤销一次 openapi 格式重排，我执行了 `git checkout -- openapi.json`，
  把**上一轮尚未提交**的 `/api/docs/http-login-guide` 端点一并回退了（HEAD 里本就没有该端点，
  checkout 等于丢弃工作区改动）。由既有护栏 `openapi_json_matches_route_table` 捕获
  （报"路由表存在但 openapi.json 未声明: ["GET /api/docs/http-login-guide"]"），
  已按同组 `task-manual` 的既有形状纯文本插回，并逐字段比对确认与兄弟端点完全一致。
  **教训：对含未提交改动的文件不要用 `git checkout --` 回退**，应定向编辑。
- **分享载荷带格式标记**：`{ campus_auth_profile: 1, suggested_id, exported_at, app_version, profile: {...} }`。
  `parse_share_payload` 强制要求该标记存在，缺失即 400 而不做「猜字段」宽松解析——错误猜测会导入
  一个看似成功却少了判定关键字的方案，用户要到下次登录失败才发现。更高版本号明确提示"请先升级"。
- **`suggested_id` 解决中文名方案的 ID 退化**（端到端实测发现）：最初按方案名 slug 生成建议 ID，
  而中文方案名（本应用主流用法，如「宿舍移动」）slug 后为空，一律退化成 `imported-profile`，
  同名分享给多个同学还会各自变成 `-2`/`-3`，接收方看到一串无意义编号。改为导出时携带源方案 id
  （恒为 ASCII slug，如 `dorm`），导入后即 `dorm` / `dorm-2`，可辨识。`suggested_id` 仍经
  `slugify_id` 规范化，恶意值（`../../evil`）无法穿越——已加测试覆盖。
- 导出同时清空 `active_task`：那是方案绑定的浏览器任务 ID，接收方通常没有同名任务，保留会被
  `LoginInorchestrator::resolve_active_task`（`src/login/mod.rs:703`）静默回退到默认任务。
- 枚举字段（`login_channel`/`http_method`）显式解析并报错，不静默退回默认（否则"导入成功但渠道被改"）。
- 直连地址在导入时即校验（`HttpLoginRequest::validate_url`），不留到每次登录才失败。
- **ID 冲突自动改名**（`name` → `name-2` → `name-3`），复用 `create_profile` 内部的
  `slugify_id`（提升为 `pub(crate)`）而非另写一份规则：否则冲突探测算 `dorm_2`、实际落盘 `dorm-2`，
  不一致会漏判冲突。绝不覆盖既有方案（覆盖会连带清掉对方凭据）。

### 顺带修复：方案 ID 字符集前后端不一致（P1，既有缺陷）

发现于导出 ID 设计阶段：后端 `create_profile` 的 `slugify_id` 把 `_` 归一为 `-`（落盘恒为连字符形态），
而前端 `useProfiles.saveProfile` 的校验是 `^[a-zA-Z0-9_]+$`（**拒绝连字符**）。
后果：任何含下划线/空格/中文名的新建方案都是「创建后再也改不动」——ID 输入框本身是 `disabled`，
改个名字保存会被前端拦下。

实测复现（真实二进制）：新建 `my_profile` → 落盘 `my-profile.json` → 编辑器内改名后点保存 →
toast「方案 ID 只能包含字母、数字和下划线」，保存失败，编辑器保持打开，**该方案永久不可编辑**。

修复：前端放宽为 `^[a-zA-Z0-9_-]+$`（与后端 `is_valid_profile_id` 一致），ID 输入框提示同步为
「字母、数字、下划线、连字符」。`useTasks.ts:126` 本就是 `[a-zA-Z0-9_-]{1,64}`，此修复也消除了两处不一致。

### 前端

- `ProfilesView` 新增标题栏「导入方案」按钮与每张方案卡片的「导出」按钮；导入经
  `Modal` 二次确认，展示文件名、方案名、渠道与匹配规则，并**脚本告警**：`http_crypto_script`
  是会被执行的代码，导入他人方案等于执行他人 JS，故用警示色区块摊开脚本原文，说明沙箱边界
  （无网络/无文件，仅纯计算，见 `run_script_in_sandbox`）后才允许确认。
- `useProfiles` 新增 `exportProfile` / `importProfile` / `readShareFile`，含 `profileImporting`
  单飞门（防连点重复导入同一文件）。
- 文件名生成 `shareFileName`：方案名是自由文本（含中文/空格/路径分隔符），剔除 Windows 非法
  字符、折叠空白、截断 60 字符，保留中文便于用户区分文件。
- 形状判定与信封展开抽为 `isProfileSharePayload` / `unwrapSharePayload`（`utils/file.ts`），
  与后端同口径，避免视图内联重复实现。

### 验证

- **Rust**：`web::routes::profiles` **27 例通过**（原 12 + 新增 15）。新增覆盖：导出剔除凭据与绑定、
  导出不存在方案 404、导出→导入往返逐字保留直连参数、ID 冲突两次导入互不冲突且既有方案不被覆盖、
  建议 ID 与 slugify 逐字一致（摘掉 `suggested_id` 以测"按名称推导"分支）、中文名方案沿用源 id、
  `suggested_id` 路径穿越净化、前端字符集与 slugify 兼容、非本应用载荷三种形态拒绝、
  未来格式版本提示升级、非法直连地址拒绝、非法枚举拒绝、无名称拒绝、`{data:...}` 信封兼容。
- **变异验证**（护栏必须能失败）：① 导出时改回 `profile.username` / `profile.password` →
  `test_export_strips_credentials_and_binding` 失败；② 把建议 ID 规则换成 `.replace([' ','-'], "_")`
  → `test_import_suggested_id_matches_slugify` 失败并打印 `left: "my_dorm_net" / right: "my-dorm-net"`，
  正是设计要防的偏差。两处均已复原。
- **前端**：`vue-tsc` 零错误；vitest **144 passed**（133 + 11）：`file.test.ts` 3 → 14 例
  （`shareFileName` 的中文保留/非法字符剔除/空白折叠/超长截断/控制字符，`isProfileSharePayload` 与
  `unwrapSharePayload` 的顶层/envelope/缺标记/非对象/数组拒绝）。
- **全量回归**：`cargo fmt --check` 零差异；`cargo clippy --all-targets -- -D warnings` 零告警；
  `cargo test`（默认特性）**11 个测试二进制全绿**（lib 871 + helper 10 + 集成 22，零失败）；
  `cargo test --features no-embed` lib 869 通过（差异为 3 个指南嵌入用例按特性条件编译）。
  期间一次 `instance_lifecycle` 失败经查为上一轮 E2E 遗留的 Python mock 进程占用端口所致，
  清理后全绿（非本次改动引入）。
- **端到端**（真实二进制 + 浏览器，CDP 驱动，非仅接口调用）：
  ① 建含脚本的直连方案 → 点卡片「导出」→ 捕获下载 Blob：文件名 `campus-auth-profile-宿舍移动.json`，
    内容中 `username`/`password` 均为空串，`ENC:`/账号/明文密码**一处都不出现**，
    直连 URL/请求体/成败关键字/脚本原文逐字保留；
  ② 同一文件走「导入方案」→ 确认弹窗正确渲染文件名、方案名、「直连请求（免 Python 与浏览器）」+
    WiFi/网关匹配规则，并**摊开脚本原文**警告；
  ③ 确认导入 → toast「方案已导入：dorm-2（请补充账号与密码后再使用）」，列表出现新卡片；
    磁盘 `dorm-2.json` 凭据为空、直连参数与脚本完整，**原 `dorm.json` 的账号与 `ENC:` 密码未被触碰**；
  ④ 用导入的方案配置对 `tests/mock-servers/full-portal` 发真实请求做双向验证：
     正确凭据 → `success`，错误密码 → `invalid_credential`（两向都验，排除"关键字恒匹配"的假通过）；
  ⑤ 导入件在未填密码时点测试 → 后端明确报「请输入密码；编辑已有方案时也可留空以使用已保存密码」，
     与 UI 提示一致。

## 开发中（2026-09-16 修复弹窗内确认框被触发弹窗压住）

### 缺陷

「关于」页点「卸载」→ 弹窗内点「开始清理」→ 确认框出现在卸载弹窗**下面**，必须先叉掉卸载弹窗才能点到确认。用户报告的是交互受阻，实测还多一层：焦点已落在被遮挡的确认按钮上。

`ConfirmDialog` 与所有 `Modal` 的遮罩层都取 `--z-top(400)`，**同层级按 DOM 顺序决胜**。而 `ConfirmDialog` 是 `App.vue` 里的常驻单例，其 Teleport 锚点在任何路由组件挂载前就已插入 `body`；页面内的 Modal 锚点是路由进入后才插入，于是 DOM 顺序恒为「确认框在前、触发它的弹窗在后」——只要在弹窗内发起 `confirm()`，确认框必被压住。

动态实测（修复前，真实二进制 + CDP）：

```
overlay[0] z=400  .confirm-dialog   「确认卸载清理」
overlay[1] z=400  .modal-container  「卸载程序」
elementFromPoint(确认按钮中心) = .uninstall-item-path   ← 点击被下层弹窗吞掉
document.activeElement = 「开始清理」(遮挡层之下的按钮)  ← Enter 可在不可见状态下执行
```

第二行是关键：这不只是「多点一次」的体验问题——确认按钮持有着焦点，此时按 Enter 会**在看不到弹窗的情况下**触发不可恢复的清理（关自启动、删用户数据、清浏览器缓存）。

### 修复

- `base.css` 新增 `--z-confirm: 500`，插在 `--z-top(400)` 与 `--z-max(9999)` 之间，使「确认」严格大于任意普通弹窗，不再靠 DOM 顺序决胜。
- `modal.css` 新增 `.modal-overlay--confirm` 修饰类映射到该 token，`ConfirmDialog.vue` 挂上它。
- **用显式类名而非 `.modal-overlay:has(.confirm-dialog)`**：本应用在用户默认浏览器中打开，版本不可控，而 `:has()` 失效的后果是危险操作按钮在不可见状态下可被点击，属正确性问题不应依赖选择器支持度；同类修饰已有 `.modal-overlay--preview` 先例（`custom-select.css` 里的 `:has()` 只管下拉层级润色，性质不同）。

### 验证

- **新增 `frontend/src/styles/zIndex.test.ts`**（4 例）：层级阶梯（base→dropdown→sticky→sidebar→overlay→modal→toast→top→confirm→max）严格递增、`confirm > top > toast`、`modal.css` 把修饰类映射到 `--z-confirm`、`ConfirmDialog.vue` 挂载该类。层级是纯声明式的，类型检查与构建都不会发现被压平，必须有护栏。
- **变异验证**（护栏必须能失败）：把 `.modal-overlay--confirm` 的 `var(--z-confirm)` 改回 `var(--z-top)` → 对应用例失败；把 `ConfirmDialog.vue` 的类名去掉 → 对应用例失败。两处均已复原。
- **真实二进制端到端**（`npm run build` + `cargo build` 重新嵌入前端后重启，CDP 驱动）：改为 `confirm z=500 / modal z=400`；确认框两个按钮 `elementFromPoint` 均命中自身（此前命中下层 `uninstall-item-path`）；点「取消」确认框消失且卸载弹窗保留（此前点不到）；按 ESC 只关确认框、卸载弹窗仍在且 `body.overflow=hidden`（滚动锁计数 2→1 正确）；再关卸载弹窗后 `body.overflow` 复位（无锁泄漏）。截图确认确认框浮在卸载弹窗之上、卸载弹窗内容在其后变暗。
- 前端 `npm run build` 通过、vitest **133 passed**（129 + 新增 4）。

## 开发中（2026-09-15 修复 OCR 卸载摧毁并发浏览器任务）

上一轮动态验证 P1-4 时顺带发现：`recycle_if_running`（`src/bridge/mod.rs`）是**无条件**回收，而 OCR 路由两处调用它。本次修掉其中的卸载路径。

### 缺陷

`POST /api/ocr/uninstall` 的流程是「暂停并取消在途 OCR 识别 → 回收 Worker → 移除依赖」。它确实有保护——`operations.pause_and_drain()`——但那个登记表**只装 OCR 识别请求**，定时浏览器任务 / 登录 / 调试都不在其中，所以拦不住。紧接着的 `bridge.recycle_if_running()` 是无条件强杀：

```rust
async fn recycle_if_running(&self) {
    if self.has_live_worker() {
        self.force_recycle().await;   // 谁在跑都杀
    }
}
```

于是「定时任务在途 + 用户点卸载 OCR」重叠时，任务的在途请求被 `trigger_all` 连带取消，**以 `Cancelled` 结束**（用户可见「执行错误: Bridge 错误: 请求已取消」——像被主动取消，实际与用户操作无关）。实测复现：并发任务在途时调 `recycle_if_running()` → 任务 `Err(Cancelled)`。

触发概率低（要求两件事时间重叠），但卸载**不能**简单跳过回收——Windows 不允许删除已加载的 onnxruntime DLL，必须先把持有模型的 Worker 收掉。

### 修法

「先检查、有冲突就拒绝」，而不是照样回收：

- `BridgeApi` 新增 `session_busy()`（`src/bridge/mod.rs`）：槽位非空 `current_cancel_id.is_some() || debug_session_open` 即视为有在途会话；默认实现返回 `false` 供内存 mock 复用。
- `ocr_uninstall` 前置该检查，命中返回 `409 CONFLICT` +「有任务正在执行，请稍后再试」（`src/web/routes/ocr.rs`）。
- 前端 `TaskEnvironmentSettings.vue` 的 `uninstallOcr` 改为 `extractApiError(e, 兜底文案)`，把后端具体原因透出（原为写死的「卸载失败，请查看后端日志后重试」，用户看不到真实原因）。
- `openapi.json` 的 `/api/ocr/uninstall` 补 409 响应文档。

### 未修（有意保留）

**OCR 安装**路径（`ocr_install` 的后台任务）同样调用 `recycle_if_running()` 且**未加**本检查。判断为不修：安装实为一生一次（新用户初次配置期，通常还没有定时任务在跑），且它是 `tokio::spawn` 到后台的——真正回收发生在点击后几十秒，用户已离开页面，感知不到因果。若后续要收敛，同一 `session_busy` 谓词可直接复用；已在 `session_busy` 的文档注释里写明此边界，避免后人误以为安装也受保护。

### 测试（均经证伪检验）

- `test_ocr_uninstall_rejects_when_session_busy`：有会话在途 → 409、错误码/文案正确，且**断言 `recycled == 0`、`removed == false`**（拒绝时不得有任何破坏性动作）。
- `test_ocr_uninstall_proceeds_when_idle`：空闲时照常卸载（新检查不得误伤正常路径）。
- `supervisor_session_busy_反映真实槽位占用`（`tests/bridge_supervisor.rs`，集成）：用**真实在途请求**验证谓词——无请求 false → 预热后 false → 在途时 true → 结束后回到 false。理由同上一轮教训：`session_busy` 是拒绝闸门的唯一依据，若它只被 mock 覆盖，真实槽位上失灵不会被发现。
- **变异验证**：把前置检查短路为 `if false && ...` → `test_ocr_uninstall_rejects_when_session_busy` 在状态码断言处失败（200 ≠ 409）；把 `session_busy` 改为恒 `false` → `supervisor_session_busy_反映真实槽位占用` 在 562 行失败。两者均已复原。

`cargo test` **892 passed**（854 lib + 38 集成），fmt/clippy 零告警；前端 `vue-tsc` 零错误、vitest 118 passed。

## 开发中（2026-09-15 动态验证 P1-4：修正错误变体记录 `WorkerCrashed` → `Cancelled`）

承接上条教训（测试数据须取自真实观测），对 **P1-4 做了同等级别的动态验证**——此前它只有单元测试（归属判定三情形，用 `register_test_cancel_id` 注入槽位）与代码链路推理，从未在真实 IPC 调用序列下跑过。

### 验证方法

新增临时集成探针（`tests/zz_p14_probe*.rs`，验证后删除），在真实 `BridgeSupervisor` + 真实 Python 子进程（假 Worker）上驱动真实在途请求，覆盖 9 个场景：

| 探针 | 场景 | 结果 |
|---|---|---|
| probe1 | 真实在途请求占槽（`cancel_id=task-1`），携他人 id 回收 | 跳过 ✓，在途请求正常完成（未被干扰） |
| probe2 | 携槽位持有者自己的 id 回收 | 执行回收 ✓ |
| probe3 | `owner=None` + 槽位被旧会话自己的 `close_browser` 占用 | 跳过 ✓（符合设计：宁可少回收） |
| probe4 | 槽位空闲 + `owner=None` | 回收 ✓（谓词退化为原语义） |
| probe5 | 兜底跳过后新会话能否拿到可用 Worker | 能（93ms 内成功；Bridge 内部自愈在 ~15s 释放槽位，早于 18s 抢占预算，无竞态） |
| probe6 | 对照组：无条件回收时新会话结果 | 亦成功（该时序下两者无差异——兜底跳过的价值在 probe7/8） |
| probe7 | **无条件回收 + 并发定时任务** | 任务被摧毁 ✓（症状真实存在） |
| probe7b | 上者重复 5 轮 | 变体**确定**（见下） |
| probe8 | **归属感知回收 + 并发定时任务** | 任务存活并正常完成 ✓ |
| probe9 | 登录仍是槽位持有者时回收 | 照常生效 ✓（证明修复未退化为"永不回收"） |

### 修正：并发任务被摧毁时的错误变体不是 `WorkerCrashed`

本次提交（`feeb6bd` 后的注释与 `docs/changelog.md`）把症状记为「对方以 `WorkerCrashed` 中途失败」。**probe7/probe7b 证伪**：实际恒为 `Cancelled`，5 轮重复观测无例外。

机制：`force_recycle` 先 `cancel_registry.trigger_all()` 取消会话区 token，**再** `kill_worker_now` 的 `drain_pending_requests`。在途请求的转发 task 用 `biased` `select!` 且 token 分支在前，故 token 分支必胜出，请求以 `Cancelled` 结算；pending drain 只是无人接收的兜底。

**用户可见差异（实测转换链，非推断）**：

| 变体 | 调度器历史消息 | Web API |
|---|---|---|
| `Cancelled`（实际） | `执行错误: Bridge 错误: 请求已取消` | 500 `INTERNAL_ERROR` |
| `WorkerCrashed`（原记录，错误） | `执行错误: Bridge 错误: Worker 进程崩溃: …` | 500 `INTERNAL_ERROR` |

即"任务自己失败"的真实表现是**「请求已取消」**——比"崩溃"更易被误判为"用户自己取消的"或"系统在清理"，排查难度比原记录描述的更高（这一点反而强化了 P1-4 修复的必要性，但原记录的机制描述是错的）。

已修正 4 处：`src/bridge/mod.rs`（trait 文档 + 实现文档）、`src/login/session.rs`（`try_retry` 注释）、`docs/changelog.md` 的 P1-4 条目。

### 回归防护（补上缺失的验证强度）

原 P1-4 只有单测，且其槽位是用 `register_test_cancel_id` **注入**的（实现者构造状态）——正是把孤儿清理漏判放进产物的那类盲区：注入式用例只能证明谓词逻辑对，无法发现"真实调用链根本没把槽位设成预期值"。

故在 `tests/bridge_supervisor.rs` 新增永久集成用例 `supervisor_归属回收_真实在途请求占槽时跳过`：槽位由真实 `execute_with_timeout`（真实 IPC + 真实 Python 子进程）写入，覆盖三步——并发任务占槽时携他人 id 跳过且**在途任务不受干扰地跑完**、持有者自己调用时允许回收、槽位空闲时允许回收。

**该用例经证伪检验**：临时把 `force_recycle_if_unowned` 的归属判定短路（`if false && !unowned`）后，用例在第 475 行断言处失败——证明它真的能拦住回归，而不是恒过的空壳。

### 结论

P1-4 的**修复本身正确且必要**（probe7 复现破坏、probe8 证明修复有效、probe9 证明未过度修复、probe1/4 证明谓词在真实槽位上按设计工作）；被修正的只是**破坏表现的记载**。`docs/updatelog.md` 的面向用户描述需同步（原文"以'任务自己失败'告终，且日志中看不出真实原因"未指名变体，但「请求已取消」比原文更误导，故改为如实说明）。

`cargo test` **890 passed**（852 lib + 38 集成），fmt/clippy 零告警。

## 开发中（2026-09-15 修正孤儿清理漏判：CSV 未反转义 + 含空格路径）

提交后做了一轮**动态验证**（此前只做了单元测试与计数核对），发现 P2-14 的基名匹配改动引入了真实回归：Windows 孤儿浏览器清理**一个都清不掉**。

**两个叠加的漏洞**（旧的全命令行子串匹配对二者天然免疫，故是收紧为基名匹配后新引入的）：

1. **CSV 未反转义**：`ConvertTo-Csv` 按 RFC4180 把字段内 `"` 转义为 `""` 并总是加外层引号，故真实命令行
   `"C:\Program Files\…\chrome.exe" --headless=new` 在 CSV 里是 `"""C:\Program Files\…\chrome.exe"" --headless=new"`。字符串以 `"""` 开头，原来取首个 token 再 `trim_matches('"')` 得到空串 → 基名为 `None` → 所有候选落空。
2. **含空格路径**：Windows 浏览器安装路径普遍含空格且被引号包裹，按空白分割取首 token 得到 `"c:\program`，基名成了 `program`。

**修法**：新增 `unescape_csv_field`（解析层还原 CSV 字段：剥外层引号 + `""` → `"`）与 `program_basename`（处理引号包裹与裸路径两种写法）。`still_orphan_chromium` 复核读的是 `QueryFullProcessImageNameW` 的映像路径（无 CSV/引号问题），故只修判定入口。

**验证方式（这次是真的端到端）**：`feeb6bd` 提交时我只跑了单元测试与进程计数，那不足以发现问题——单元测试的命令行是我自己构造的 `C:\pw\chromium-1228\…`（无空格、无引号），恰好绕过了真实形态。本轮改为：用 `subprocess.Popen(..., DETACHED_PROCESS)` 起一个**真实 Chrome headless 实例**并让父进程退出，造出真孤儿（PID 40472，PPID 34376 双确认父已退出），再对真实进程表跑判据。

| 阶段 | is_chromium 候选 | 定位到该孤儿 | kill 前复核 |
|---|---|---|---|
| 修复前 | **0**（全部漏判） | ✗ | — |
| 修复后 | 5（孤儿 1 + 其子进程 4） | ✓ | 孤儿 `true`，4 个子进程 `false` |

同时确认：11 个真实 Chrome 子进程因**父进程存活**被正确排除（这正是 `/T` 递归终止进程树所依赖的前提）；用户日常 Chrome（无 headless/调试特征）不命中。

**测试**：新增 2 个用例——`test_unescape_csv_field`（三引号转义、普通字段、空串）、`test_is_chromium_handles_quoted_path_with_spaces`（实测命令行原文、Edge 变体、headless_shell、反例）。

**教训（已并入本文件的方法论）**：**测试数据必须取自真实观测，不能只由实现者按预期构造**。本次两个漏洞都能被"用真实命令行原文做用例"提前拦下。后续凡涉及外部工具输出格式（CSV / JSON / 命令行 / 注册表）的解析，用例字符串一律从实际输出复制，不手写。

`cargo test` **889 passed**（852 lib + 37 集成），fmt/clippy 零告警。

## 开发中（2026-09-15 全面文档对质：修正 9 处与实现不符的陈述）

对全部 19 个 tracked Markdown 与 `openapi.json` 做了一轮与代码的对质。**每条陈述都以源码/路由表/脚本实际内容为准核对**，不依赖文档间的交叉引用。修法遵循一条原则：**能用「查询命令」表达的规模数字就不再写死绝对值**（那类数字必然腐化），能用代码定位替代行号的就写符号名。

### 接口与类型描述纠错（用户会照着做，影响最大）

- **`GET /api/shells` 与 `type=shell` 根本不存在**（`README.md`）：原文称「三类 `type`（browser/script/shell）……API 统一为 `GET /api/shells`」。实际 `TaskKind` 只有 `Browser` / `Script` 两臂，`shell` 类型在反序列化处被**明确拒绝**并提示改用 `script`（`src/tasks/models.rs:259-262`，有用例锁定），`/api/shells` 无任何注册（`routes/` 下也没有 `shells.rs`）。改为两类，并换掉失效端点。
- **定时任务不是「浏览器任务的 cron 调度视图」**（`README.md`、`user-guide.md`、`task-manual.md` 三处）：实际 `TaskExecutor::execute_with_timeout_override` 对 `Browser`/`Script` 两臂都有实现（`src/tasks/executor.rs:109-121`），`SchedulerService::task_type_of` 按 `target_id` 推导类型，前端定时任务页也提供「浏览器任务 / 自定义脚本」两个目标选项（`ScheduledTasksView.vue:22-23`）。三处均改为「两类（浏览器/脚本）」。
- **`GET /api/scripts` 不存在**（`task-manual.md:63`、`user-guide.md:91`）：实际只有 `/api/scripts/run`、`/api/scripts/binaries`、`/api/scripts/{id}`（GET/PUT/DELETE）。脚本列表复用 `GET /api/tasks`。原文所谓「脚本任务过滤视图」在前端也是本地按类型过滤，并无该端点。
- **`POST /api/ai/generate` 不存在**（`README.md:17`）：实际端点是 `POST /api/ai/generate/stream`（流式，`src/web/mod.rs:300`）。
- **`POST /api/debug/capture` 不存在**（`user-guide.md:113`）：实际反馈包端点为 `POST /api/debug/feedback-bundle`（`src/web/mod.rs:167`）。
- **`GET /api/profiles/active` 不存在**（`user-guide.md:75`）：活跃方案由 `GET /api/profiles` 响应的 `active_profile` 字段携带，切换走 `POST /api/profiles/switch`（前端 `profilesApi.setActive` 亦用后者）。
- **`POST /api/scripts/run` 的语义写窄了**（`task-manual.md`）：它按 body 分派——传 `task_id` 执行已落盘任务，传 `script` 执行临时内容（`task_id` 记为 `adhoc_script`，不落盘），二者皆缺返回 400（`src/web/routes/scripts.rs:65-79`）。原文只写了 `task_id` 一种。
- **前端 typegen 链路早已移除**（`frontend/README.md`、`openapi.json` 的 `description`）：原文称 `types.generated.ts` 是 `npm run typegen` 的产物、以 `openapi.json` 为源。实际 `typegen` 脚本与 `openapi-typescript` 依赖已在 `487e078`（C6）删除，`types.generated.ts` 也不存在。改为说明「手写 `types.ts` 是唯一权威，无生成链路」，并在 `openapi.json` 的 `description` 里如实交代其边界（只有 method+path，无 `components.schemas`，字段契约不在其中，漂移保护仅由 `openapi_json_matches_route_table` 覆盖路径集合）。

### 结构与计数纠错

- **任务页已是三个 Tab**（`user-guide.md`、`task-manual.md` 两处称「两个」）：`TasksView.vue:22-24` 与路由 `/tasks/{browser,scripts,ai}` 均为三项，「AI 生成」随 `5880401` 并入后文档未跟上。
- **`web/routes/` 的域清单含不存在的 `shells`、漏了 `tools`**（`AGENTS.md:111`）：实际目录有 `tools.rs`（`/api/tools/task-recorder.user.js`）而无 `shells.rs`。
- **`mock_portal/` 根目录重定向不存在**（`AGENTS.md`、`tests/README.md`）：该目录不存在、`git ls-files` 也无命中，原文「根保留 README 重定向」为凭空陈述。
- **`docs/known-issues.md` 两处行号失效**（`:15`、`:16`）：`next_fire_at` 的实际序列化点在 `scheduler/mod.rs:658`（原写 `:326-328`，该处是 `save_task`）；Profile 切换的 pause 门控在 `run_loop.rs:234-241`（原写 `:127-130`，该处是 `EngineInner::new` 的字段初始化）。改为按符号定位，避免行号再次漂移。
- **`docs/guides/user-guide.md` 版本号停在 `alpha.8`**：改为当前 `alpha.10`。

### 仍有效但表述可改进

- **`python_worker/README.md` 首段句子被打断**：原文「……由 Rust 主进程（控制平面）通过」后被 blockquote 截断，且第二条 blockquote 里嵌了「项目 `app.*` 模块……」的残句——系早前编辑事故。已重写该段。
- **`python_worker/README.md` 的「支持的命令」表只列了 9 项**：实际 `COMMANDS` 注册 **14** 项。补上 `worker_health_check` / `close_browser` / `debug_run_all` / `debug_status` / `feedback_capture`，并补一节说明会话互斥与轻量旁路（`ocr_recognize` / `feedback_capture` 与任意会话并发且不参与 Worker 回收时的全量取消），与 Rust 侧 `CancelRegistry` 的双区语义对齐。

### 未改动的部分（已核实准确）

`docs/guides/README.md`、`docs/archive/README.md`、`docs/plans/`、`docker/README.md`、根 `README.md` 的 Docker/更新/贡献各节、`tests/README.md` 的目录树与 E2E 环境变量说明、`docs/guides/task-writing-guide.md` 的 18 项 `VALID_STEP_TYPES`（与 `src/tasks/models.rs:40-61` 逐项一致）、`docs/guides/custom-script-guide.md`（上一轮已按 `shell` 移除重写）。

### 本轮验证

- **端点对质**：从 `src/web/mod.rs` 的 `route_table` 提取全部 84 条 `(method, path)`，与各文档正文提及的 `/api/*` 逐一比对（路径参数归一化），确保无「文档提到但未注册」的端点。
- **`updatelog.md` 章节提取回归**：`release.yml:197` 用 awk 按 `## <tag>` 前缀提取发布说明。本轮在该文件新增了 `## 尚未发布（开发中）` 小节，故按同一逻辑（PowerShell 复刻）验证：对 `tag=v5.0.0-alpha.10` 仅提取到 alpha.10 那一节（19 行），**未把新增小节并入**，前缀边界判定正确。
- `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` 零告警；`cargo test` **887 passed**（含 `openapi_json_matches_route_table` 与 `test_openapi_asset_embedded`，两例在 `openapi.json` 的 `description` 改写后仍通过）。
- `openapi.json` 改动后仍为合法 JSON（`info.version` = 5.0.0-alpha.10，85 个 paths）。

## 开发中（2026-09-15 全面审查后的缺陷修复：4 项 P1 + 8 项 P2 + 3 项 P3）

对全仓做了一轮审查与测试，逐项修复确认缺陷。所有结论均先经可执行实验或真实二进制验证，静态推理得出的判断一律以实验为准。

### P1（用户可感知的正确性缺陷）

- **P1-1 宽松触发吞掉「谨慎单次」降级**（`src/monitor/decision.rs`）：`lenient_trigger_candidate` 原先只排除 `FixConfiguration` / `NoProbeEvidence`，未排除 `AttemptLogin` / `AttemptLoginOnce`。后果有二：① 真实捕获的门户劫持（`High` / `CaptiveDetected`）在关闭严格模式后被**无条件**改写成 `Low` / `LinkUpLoginAssumed`，排障时无法区分「确实检测到劫持」与「只是按链路猜」；② 有意的「同一配置版本仅尝试一次」节流被升级为无差别 `AttemptLogin`，绕过 Engine 的 `cautious_attempted_config_version` 去重（仍受 `auto_login_in_flight`、连续失败 3 次进 300s 冷却两道闸门约束，故为「300s 内最多 3 次」而非无限）。语义上宽松触发是**兜底**（严格口径什么都没给出时才升级），已有登录建议时它无任何可补充之处。原测试矩阵恰好绕过了唯一会出问题的组合（7 个纯函数用例只覆盖 `WaitForNetwork` / `WaitForMoreEvidence` / `NoAction` / `FixConfiguration` / `NoProbeEvidence`；5 个接线级用例全部构造为 `Offline`）。新增 3 个回归用例（`Captive`+`Reachable` 的 `High` 结论不变、`Captive`+`Unreachable` 的 `AttemptLoginOnce` 不变、`Inconclusive`+`Reachable` 的 `AttemptLoginOnce` 不变）。
- **P1-2 纯净模式开关与配置双源，保存时静默回退**（`frontend/src/composables/useConfig.ts`）：`togglePureMode` 成功后只更新独立 ref `pureMode`，**从不回写** `config.browser.pure_mode`；而 `saveConfig` 的载荷携带整个 `config.browser`，后端 `browser` 属 `global_keys` 走 `json_merge` 递归覆盖。症状：用户关掉纯净模式后，在任意设置页点一次「立即保存」就会把它静默翻回开启，且 UI 开关仍显示"已关闭"（与落盘值分叉）、`dirty` 不置位、无任何提示。修法：切换成功与 `fetchPureMode` 拉取到权威值后都同步 `config.browser.pure_mode`，并沿用 `setLogLevel` 同款处理（`suppressDirty` 抑制 dirty 比对 + 仅在无未保存编辑时刷新 `savedSnapshot`，避免把其他未保存改动误判为已保存）。新增 2 个用例，其中一条直接断言 PATCH 载荷里 `browser.pure_mode` 等于开关当前值。
- **P1-3 Engine 崩溃重启窗口内命令返回 HTTP 500**（`src/web/error.rs`）：`From<EngineError> for ApiError` 原先只把 `ChannelFull` 映射 503，`ChannelClosed` 落入 `_ => Internal`。Engine 崩溃后 `watch_engine` 在 `cancel_auto_pending().await` 与重建之间有一个窗口，期间 slot 仍持已死句柄，用户点「开始/停止监测」「测试网络」会看到 **500 服务故障**，语义误导（前端与用户会以为是服务端 bug）。修法：`ChannelClosed` 与 `ChannelFull` 一并映射 503——两者都是「引擎暂时不可用，稍后重试即可」。同步更新 `src/web/routes/monitor.rs` 那条断言 500 的既有用例，并在 `web/error.rs` 补 `EngineError` 全变体映射用例（原先该文件的状态码用例不含任何 `EngineError` 变体）。
- **P1-4 `force_recycle` 无归属校验，会摧毁并发定时浏览器任务的 Worker**（`src/bridge/mod.rs`、`src/login/session.rs`、`src/login/mod.rs`）：定时浏览器任务与登录在 Bridge 层**共享同一个会话槽位且互不排斥**（`check_session_compat` 对 `Some(Login)` 放行 `execute_browser_task`，并有测试固化），而 `force_recycle` 是无条件强杀。登录可重试失败后的回收、抢占等待超时后的兜底都会调用它 → 时间重叠时定时任务在途请求被 `trigger_all` 取消，以**「请求已取消」（`Cancelled`）**中断（**注**：此处原记为 `WorkerCrashed`，经 2026-09-15 动态验证证伪并修正，见本文件顶部条目），症状是"任务自己失败"，根因却在另一条路径上。注意：审查报告原本归因于「登录收尾的 `close_browser` 关掉了任务浏览器」，经核对**不成立**——Python 侧 `_serve` 是严格串行的命令循环，`close_browser` 必须排队等待在途任务结束，真正的破坏点是 `force_recycle`。修法：新增归属感知的 `force_recycle_if_unowned(owner_cancel_id)`，判定口径与既有的 `grace_wait_slot_release` / `wait_cancel_ack_or_kill` 一致（仅当槽位空闲或属调用方时才放行）；`try_retry` 与 `wait_old_session_finished` 改走该入口。新增 1 个用例覆盖「槽位被他人占用时跳过」「冒名他人 id 被拒」「真正的持有者可回收」三种情形。
- 说明：`force_recycle` 是 `BridgeApi` trait 方法，`force_recycle_if_unowned` 以**默认实现**加入 trait（内存 mock 无需改动即视为无冲突），实际归属判定在 `BridgeSupervisor` 上实现。

### P2（健壮性缺口与验证能力缺口）

- **P2-5 探测/自动登录任务 panic 后在途标记永不复位**（`src/engine/run_loop.rs`）：`probe_in_flight` 与 `auto_login_in_flight` 的唯一复位点都是「结果回传」，任务若 panic 则 `tx` 随任务 drop、回传永不发生 → 此后每轮检测被防重入吞掉，**自动监测/自动登录静默停摆**（症状是"卡死"而非崩溃，极难排障）。修法：新增 `ProbeReturnGuard` / `AutoLoginReturnGuard` 两个 RAII 守卫，Drop（含 panic 展开）时经 `try_send` 补发一条失败消息复位标记；正常路径显式置 `sent = true` 避免重复回传。新增 3 个用例：panic 后守卫确实补发、正常路径不重复补发、自动登录守卫同样生效。
- **P2-6 `force_recycle` 的 `trigger_all` 波及并发 OCR**（`src/bridge/session.rs`、`src/bridge/mod.rs`）：`CancelRegistry` 原为单区，轻量旁路（`ocr_recognize` / `feedback_capture`）与会话类命令注册在一起，`trigger_all`（Worker 回收时的兜底取消）会把与本次回收毫不相干的 OCR 请求判为 `Cancelled`。修法：注册表改为**双区**，`trigger_all` 只作用于会话区；`trigger` / `remove` / `clear` 仍覆盖两区（精确取消与防泄漏语义不变）。新增 3 个用例（`trigger_all` 不波及轻量 token、轻量仍可被精确取消、`clear` 清空两区）。
- **P2-7 `spawn_environment_probe` 无取消令牌**（`src/container.rs`）：与同文件 `spawn_pending_update_check` 的不一致（后者有 `select! shutdown_token`）。无令牌时停机后该任务仍持 `Arc<EnvironmentManager>` 继续写状态/日志、可能 spawn uv 子进程，与重启后的继任进程争缓存锁。修法：改为与 `spawn_pending_update_check` 同构接受关闭令牌。
- **P2-9 `client.ts` 的超时与取消不覆盖响应体解析**（`frontend/src/api/client.ts`）：`finally` 块在**收到响应头后**就 `clearTimeout` 并移除 abort 监听，而 `await res.json()` 在其后才执行 → 若响应头已回但 body 卡住/半途断流，`res.json()` 无超时、不可取消而**永久 pending**（永久转圈、无 toast、`busy.save` 永不复位），与该文件「后端卡死时快速报错而非永久转圈」的修复意图直接冲突。修法：把清理移入覆盖 body 消费的 `finally`，并把 body 阶段抛出的 `AbortError` 同样归一为「请求超时」/「请求已取消」。该文件原先只测 `isNoBrowserMessage`，对请求层零覆盖；新增 5 个用例（body 挂起时超时生效、body 阶段取消带 `aborted` 标记、`{data}` 信封解包、错误信封的 code/message、401 仅重试一次）。
- **P2-10 `fetchRepoIndex` 无 epoch 守卫**（`frontend/src/composables/useRepoImport.ts`）：进入即 `loading=true`、`tasks=[]`，`await` 后**无条件**写状态。交错场景：慢索引 A 在途 → 关弹窗重开（`showRepoImport` 复位 loading）→ 为 URL B 再点 → A 迟到覆盖 B 的列表并提前清 loading，用户可能把 A 源任务当 B 源导入。修法：加 `fetchIndexSeq`，回包时 `if (seq !== fetchIndexSeq) return;`，且仅最新请求负责复位 `loading`（与 `useConfig` 的 `saveSeq` / `fetchConfigEpoch` 同口径）。新增 3 个用例。**变异验证**：去掉守卫后这 3 个用例全部失败，确认其鉴别力。
- **P2-12 抢占等待预算小于最坏路径**（`src/login/mod.rs`、`src/login/session.rs`、`src/bridge/mod.rs`）：原 `PREEMPT_WAIT_BUDGET = 13s`，注释按「5s 等终态 + 8s close_browser」推导，但 `session.rs` 实际传 12s，且命令超时后 Bridge 还有 `grace_wait_slot_release` 的 10s 宽限 → 最坏 ≈18~22s > 13s，超时兜底的 `force_recycle` 可能在旧会话收尾仍处宽限期时触发，「run() 返回 = close_browser 完成」的等待语义不成立。修法：`close_browser` 超时降到 8s（与 Python 侧 `_WAIT_TIMEOUT_SECS = 8` 对齐，两端同值时以 Python 自愈为主），并让 `PREEMPT_WAIT_BUDGET` **由各段常量推导**（`CLOSE_BROWSER_TIMEOUT + GRACE_WAIT_DURATION` = 8+10 = 18s）。新增不变量用例 `preempt_budget_covers_close_and_grace` 锁定该推导关系（此前该不变量只存在于注释里，因此会漂移）。
- **P2-13 `std::time::Instant` 使冷却恢复路径零覆盖**（`src/engine/run_loop.rs`）：冷却判定用 `std::time::Instant`，而测试统一用 tokio 虚拟时钟 + `advance`，两者**完全解耦**——`advance(600s)` 后 tokio 时钟走 600s 而 std 时钟仅走 108µs。因此「冷却期满重置 `consecutive_failures`」这条恢复路径**从未被任何测试执行过**，回归保护形同虚设。修法：改用 `tokio::time::Instant`（与 `check_timer` 同源），并把该逻辑提取为 `clear_expired_cooling_down()` 以便直接测试；新增 `test_cooling_down_expiry_resets_failure_count`（`start_paused` + `advance`）。**变异验证**：另跑一个临时测试确认 `tokio_expired=true std_expired=false`，即该用例确实能鉴别两种时钟。
- **P2-14 Windows 孤儿清理缺少 kill 前复核 + `is_chromium` 子串过宽**（`src/bridge/orphan.rs`）：unix 分支有 `still_orphan_chromium` 三重复核，Windows 分支直接 `taskkill /F /T`；且 `is_chromium` 用全命令行子串匹配（`含 "chrom" 且含 "--headless"`），实测 `cmd.exe /c echo chromium --headless` 即命中，脚本参数、含 `chrom` 的路径、甚至审查该模块的搜索命令都会成为候选（Windows 上「父进程不存在」是常态：实测 430 进程中 11 个父进程已消失，而 `Get-CimInstance` 枚举 460 进程需 ~710ms，这段窗口内无任何复核）。修法（两项都做）：① `is_chromium` 改为**先匹配可执行文件基名白名单**（`chrome.exe` / `chromium` / `headless_shell.exe` / `msedge*` 等）再看 headless/调试特征；② Windows 分支新增 `still_orphan_chromium`，kill 前用 `OpenProcess` + `QueryFullProcessImageNameW` 重读映像路径确认仍是浏览器、用 `CreateToolhelp32Snapshot` 重读父 PID 确认未变且仍不存在。`Cargo.toml` 补 `Win32_System_Diagnostics_ToolHelp` feature。新增 4 个用例（关键词出现在参数中不得命中、映像基名矩阵、`parent_pid_of` 对真实进程的验证、非浏览器 PID 被复核拒绝）。
- **P2-1 / P2-2 / P2-3 文档过时**：`tests/README.md` 的测试矩阵四项数字（73 处 / 5 crate / 127 用例 / 49 用例）全部过时，改为**不维护绝对数字**（附现取命令）——这类数字必然腐化；`docs/known-issues.md` 的 #2（`mapBackendStatus` 混入 `raw`，已由 P17 修为逐字段映射）、#7（`UV_SYNC_MAX_RETRIES` 未使用、`uv sync` 无重试，均已有 3 次重试）、W13（linux-arm64 无产物，已有 `ubuntu-24.04-arm` 原生构建）三条已修项标注为已修，并移除 `E3` 的过时数字；`tests/README.md` 与 `AGENTS.md` 中「根保留 `mock_portal/README.md` 重定向」的陈述与事实不符（该目录不存在），已删。

### P3（清理项）

- **任务执行失败弹绿色成功 toast**（`frontend/src/composables/useTasks.ts`）：任务失败同样以 HTTP 200 返回（`execute_task` 直接 `Ok`），成败由业务字段 `success` 表达；原实现硬编码 `toastOnly(true, ...)` 并让 `extractApiError` 对一个 object 取不到 `.message` 而固定回落"执行完成"。修法：按 `data.success` 分流，失败时以 `error` 为准并以 warn 级记日志。同时把 `tasksApi.execute` 的返回类型由 `MutationResult` 改为新增的 `TaskExecuteResult`（与后端 `TaskResult` 逐字段对应），使 `success` 在编译期可见。
- **`armStatusPoll` 的退避是死代码**（`frontend/src/composables/useUi.ts`、`frontend/src/composables/useStatus.ts`）：退避挂在 `fetchStatus().catch()`，但 `fetchStatus` 内部 `try/catch` 全量吞异常、永远 resolve → `.catch` 不可达、`statusFailStreak` 恒 0、300s 慢间隔永不生效，而后端挂死时前端会每 30s 持续发注定失败的请求并刷日志。修法：`fetchStatus` 改为返回**本次请求是否成功**（区分「请求失败」与「响应因过期被丢弃」——后者服务端可达，返回 `true`），`useUi` 据此驱动退避与恢复。
- **`replaceLogs` 在 `preserveAfterSeq=0` 时丢弃在途实时日志**（`frontend/src/composables/useLogs.ts`）：`realtimeDuringFetch` 仅在 `preserveAfterSeq > 0` 时非空，但 `pendingLogs` 被**无条件**清空；而 `fetchStartedSeq` 只从 `logs` 取最大 seq，用户点过「清空」后退化为 0 → 「清空后点刷新、期间到达的实时日志」被整批丢弃，`pendingNotAtBottom` 也被清零（"N 条新消息"不显示）。修法：`fetchStartedSeq` 纳入 `pendingLogs`（微任务缓冲里的日志同样属「请求发起后到达」）；seq 为 0 时改用「请求发起前已存在条目的内容键差集」判定；仅在无日志需保留时才清零新消息计数。新增 2 个用例。**变异验证**：还原守卫后这 2 个用例失败。
- **`docs/changelog.md` 分项与总数矛盾**：同一行既称「新增 14 个用例」又称「纯函数 5 个 + 接线级 5 个 + Web 往返 2 个」（合计 12），且称纯函数覆盖「`FixConfiguration` 与 `NoProbeEvidence` 保护」为 1 个用例。实测为纯函数 **7** 个 + 接线级 5 个 + Web 往返 2 个 = 14，已改正分项与用例计数。

### 本轮验证

- `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` 零告警。
- `cargo test`：**887 passed**（850 lib + 10 helper_main + 1 bridge_ipc + 5 bridge_supervisor + 1 http_login_chain + 4 instance_lifecycle + 5 login_chain + 3 scheduled_tasks + 1 smoke_test + 7 updater_channels），0 failed，1 ignored。
- `uv run pytest`：**182 passed**。前端 `vitest`：**118 passed**（18 文件）；`vue-tsc --noEmit` 与 `npm run build` 通过。
- 新增用例 **29** 个（本轮新增，不含工作树中既有的在研改动）。由基线差值核对：Rust lib 833 → 850 = +17，前端 106 → 118 = +12。分布——Rust：`monitor/decision.rs` +3（P1-1）、`bridge/session.rs` +3（P2-6）、`login/mod.rs` +2（P1-4 归属判定、P2-12 预算不变量）、`engine/run_loop.rs` +4（P2-13 冷却恢复、P2-5 两守卫与不重复回传）、`bridge/orphan.rs` +4（P2-14 关键词误判、映像基名、真实父 PID、非浏览器拒绝）、`web/error.rs` +1（P1-3 变体映射）；前端：`client.test.ts` +5（P2-9）、`useRepoImport.test.ts` +3（P2-10）、`useConfig.test.ts` +2（P1-2）、`useLogs.test.ts` +2（P3）。
  > 注：`monitor/decision.rs` 现共 22 个用例（相对 HEAD 为 +10），其中 7 个属本工作树既有的「登录严格模式」在研改动，本轮只新增 3 个；上表按**本轮实际新增**口径统计。
- **变异验证**（改回后确认用例失败、再复原）：P2-10 去掉 `seq` 守卫 → 3 个用例失败；P3 `replaceLogs` 还原旧判定 → 2 个用例失败；P2-13 另跑临时测试确认 `tokio_expired=true std_expired=false`（两种时钟确实解耦）。
- 本轮未改 `config_version`、未改版本号、未改 IPC 契约与 `openapi.json`。

## 开发中（2026-09-15 新增「登录严格模式」开关，默认开启）

用户实测：其校园网需**先登录学校门户、再进入校园网认证选择运营商**（学校门户决定账号）。该网络在严格口径下永不触发自动登录。实测日志（`debug/campus-auth-logs-20260914063057/logs/app.log.2026-09-14`）显示 13:39:38 起 `Online → Offline（原因=AllProbesFailed, 204门户=Fail）`，此后 13:39–13:45 反复手动测试均为 `Offline`，**全程无任何**「检测结论建议恢复认证，触发自动登录」或「谨慎尝试一次自动登录」记录，直到用户手动点登录才恢复。根因：`Offline` 在 `apply_auth_endpoint` 中仅当认证入口 TCP 可达（或非重定向模式）才升级为门户，否则落 `WaitForNetwork`；该网络的认证入口预检失败，故这条路径恒不触发。

- **配置新增 `monitor.strict_login_mode`（默认开启）**：开启＝严格口径，仅在探测给出明确门户结论（Captive 命中，或外网全失败且认证入口可达）时才自动登录；**关闭**后退化为宽松口径——只要**本地网卡已连接**且探测**未确认在线**即升级为门户并建议登录，不再要求先拿到门户劫持证据，面向「学校门户 + 校园网认证」两级认证等严格口径覆盖不到的组网方式。开关按正向语义命名（默认值即历史行为），用户勾选框默认选中，关闭才是行为变化。
- **判定实现为独立纯函数** `monitor::decision::apply_lenient_trigger`（`src/monitor/decision.rs`），只依赖「本地链路可用 + 未确认在线」，由「严格模式关闭」触发。三条边界刻意保留，避免退化成无差别登录：① `Online` 一律不动——网关放行了探测就是真能上网，不该被打扰；② `FixConfiguration` 不动——配置错误须用户先修正，拉浏览器只会产生误导性失败；③ `NoProbeEvidence` 不动——一个探测都没启用属「测量缺失」，升级会与既有「禁止自动恢复」告警语义冲突且每轮都尝试登录。另有 `local_link != Available`（未检查/无接口/检查失败）不升级：网卡没连上时登录没有意义。
- **「是否值得采集网卡」与「是否升级」共用同一谓词** `lenient_trigger_candidate`（同文件导出，只判断定是否还有升级空间、不含开关状态）：两处若各写一份条件，改一处忘另一处会导致白采集或证据缺失。`check_once` 的采集分支与 `apply_lenient_trigger` 的前置判断同源。
- **新增判定原因 `AssessmentReason::LinkUpLoginAssumed`**（置信度 `Low`）：界面可区分「明确检测到劫持」与「按链路连接推断需要登录」，不把推断伪装成检测结论。前端 `useStatus.ts` / `useUi.ts` 补文案。
- **网卡证据改为按需串行采集**（`MonitorService::check_once`）：原先仅手动诊断按 `local_check_enabled` 采集、自动监测恒为 `NotChecked`。现提取 `probe_local_link()`，仅在「手动诊断开关开启」或「严格模式关闭**且** `lenient_trigger_candidate` 成立」时调用。**由并行改为串行**（公网探测完成后再查）：并行虽能让判定少等一次网卡枚举，但代价是在线稳态下每轮**白跑**一次（结果随即被丢弃），收益只是失败路径省去 ≤3s 而失败路径紧接着就要拉起浏览器，延迟可忽略。已确认在线、配置错误、无有效探测三类情况一律不采集。
- **语义边界写进代码注释**：`probe_local_link` 只判**链路层是否连着**（非虚拟网卡持有非链路本地 IPv4），**不判「是否有网」**——插着网线但对端未通、连着 WiFi 但网关不响应 DHCP，同样报 `Available`。判断「是否有网」的仍是公网探测；网卡证据只在宽松口径下作为**比「有网」更弱**的兜底信号，不得用于替代探测。
- **应用顺序**：宽松判定在严格判定（含 `apply_auth_endpoint`）**之后**执行——它是对「未确认在线」的兜底升级而非替换严格证据，顺序颠倒会让 `FixConfiguration` 被覆盖。
- **认证地址预检不参与判定**（用户明确选择）：预检失败不构成阻止尝试的理由，风险由用户显式选择承担。（预检失败的真实成因更可能是网关对新连接限速/丢首包，而非「https 握手被拦」——TCP 握手属 L4，与 https 无关；成因描述已在注释中订正。）
- 前端：设置·检测页「诊断与恢复辅助」列新增**「登录严格模式」**开关（`badge--info`「推荐开启」），默认选中；**置于该列首位**（它决定「断网后会不会自动登录」这一最高频疑问，用户最需要先看到）；说明按「勾选＝严格 / 关闭＝宽松」两侧语义组织，并把**故障自诊指引写进文案**——「若遇到断网后未自动登录，请关闭本开关」。此指引方向经核对：该症状的成因正是严格口径把「网关放行探测判 Online」这类网络挡在门外，修复动作是**取消勾选**而非勾选。相邻的「手动测试时检查网卡」说明中的相对位置引用同步由「下方」改为「上方」；`frontend/src/api/types.ts`、`utils/constants.ts` 同步。
- 涉及文件：`src/config/schema.rs`（字段 + 默认值 + 默认值断言）、`src/monitor/model.rs`（新增 reason）、`src/monitor/decision.rs`、`src/monitor/mod.rs`（`MonitorConfig` 透传 + `probe_local_link` + 串行采集 + 应用顺序）、`src/web/routes/config.rs`（前后端字段映射 + 往返用例）、`frontend/src/views/settings/MonitorSettings.vue`（开关 + 文案 + 次序）、前端 `types.ts` / `constants.ts` / `useStatus.ts` / `useUi.ts`。
- 测试：新增 14 个用例。纯函数 7 个（Offline 升级 / Unknown+Inconclusive 升级 / Online 边界 / `FixConfiguration` 与 `NoProbeEvidence` 保护 / 链路不可用不升级 / 不调用即严格语义不变回归锚点）；`monitor::mod` 接线级 5 个（**严格模式关闭时确实采集网卡证据并升级为 `AttemptLogin`** 且恰好调用一次、**已在线时即便严格模式关闭也零次枚举网卡**、默认严格模式保持 `WaitForNetwork` 且零次、网卡不可用时不升级、配置错误时零次）；Web 往返 2 个（字段往返 + **PATCH 未提供该字段时不得把它写进 patch**，否则前端任意一次局部保存都会把用户配置悄悄切成宽松口径）；另 2 个既有默认值/映射断言扩展。`WiredDetect` 带调用计数以锁定「不白跑」。接线级用例首轮跑出真实交互：夹具 `auth_url` 为空会走 `Missing → FixConfiguration` 而使宽松判定不覆盖，改为配成不可达地址（与用户真实场景一致）后通过。**变异验证**（四处，均改回并确认复原）：① `apply_lenient_trigger` 短路为 no-op → 接线级用例失败；② `lenient_trigger_candidate` 改为恒真 → 「已在线不采集」「配置错误不采集」两用例失败；③ 采集分支 `!cfg.strict_login_mode` 翻转 → 3 个用例失败；④ 应用分支 `!cfg.strict_login_mode` 翻转 → 1 个用例失败（另一用例不失败，因严格模式下 `local_link` 恒为 `NotChecked`、`apply_lenient_trigger` 自身即 no-op——两道 guard 属纵深防御，各自独立生效）。
- 端到端：构建产物 `MonitorSettings` chunk 断言含「登录严格模式」「请关闭本开关」，且不含旧的「下方」引用、含新的「上方」引用；真实二进制上验证全新配置默认 `strict_login_mode=true`、显式关闭后为 `false`、随后一次不含该字段的局部保存不会把它翻回 `true`。
- 说明：本轮未改 `config_version`，字段为**本次尚未发布的新增项**（同一轮开发内即完成命名反转，从未随版本发布），故不涉及兼容负担。`strict_login_mode` 缺省由 `#[serde(default)]` 补齐为 `true`＝严格口径＝历史行为，存量配置行为不变，无须迁移。唯一的边界情况：若曾用本轮中间构建在配置里写过旧名 `lenient_login_trigger`，该键会被 serde 静默忽略（未知字段）、回到默认严格口径——因旧名从未发布，不视为兼容性问题。

## 开发中（2026-09-14 任务页三处视觉缺陷修复）

用户反馈「UI 有点丑」。因为本轮起可以看图，先按 `getBoundingClientRect`/`getComputedStyle`/CDP `getPlatformFontsForNode` 采集度量定位**可判定**的问题（而非凭感觉重画），再逐张截图复核。以下 7 项均为实测缺陷，改完逐项复测数值。

- **按钮与表单控件未继承应用字体栈（影响面最大）**：浏览器 UA 样式表给 `button/input/select/textarea` 预设 `font: 400 13.33px Arial`，而项目样式只覆盖字号、不覆盖字族。结果这些控件中的拉丁字符/数字走 Arial，与正文的 Segoe UI 不一致（中文两侧都回退到雅黑，故只在英文数字上显现，容易被忽略）。CDP 实测 Tab 标签的平台字体为 `Arial(3) + Microsoft YaHei(2)`，而卡片标题是纯 `Microsoft YaHei`。修法：在 `base.css` 为这四类元素声明 `font-family: inherit`，字号仍由各组件自行声明。
- **列表行图标按钮的紧凑尺寸从未生效**：`tasks.css` 声明 `.btn-sm.btn-icon-only { width:28px; height:28px }`，但 `btn.css` 的 `.btn-icon-only` 与 `.btn-sm` 各自声明了 `min-width/min-height`（44px/36px），而 **min-\* 压过 width/height** —— 实测 5 个操作按钮全部渲染 44×44、按钮组占 244px 行宽。修法：该规则移入 `btn.css`（组合还被 ProfilesView 的检测结果面板使用，本属通用修饰），并补 `min-width/min-height: 28px`。复测按钮 28×28、按钮组 164px、行高 78→73。28px 仍高于 WCAG 2.5.8 的 24px 最小目标。
- **卡头在窄屏把标题压成竖排且按钮组溢出**：`.card-header` 是 `nowrap` 的 space-between flex，右侧按钮组宽度固定，剩余空间不足时 flex 先把标题压扁——实测 460px 视口下标题宽只剩 16px、高 105px（中文逐字竖排），而按钮组仍溢出卡片 102px。修法：`.card-header` 加 `flex-wrap: wrap` + `row-gap`，标题 `min-width: fit-content`（声明最小需求宽度，参与收缩协商），新增 `.card-actions` 承载按钮组并允许自身换行；两个任务面板的 `flex-row gap-sm` 改用该类。复测 460/520/560px 及 320px 极窄下标题均单行、无溢出。
- **列表描述行在窄屏溢出卡片且省略号失效**：≤980px 时 `.task-item` 改纵向排列 + `align-items: flex-start`，子项按 max-content 定宽，而 `.task-desc` 是 nowrap，于是 `.task-info` 撑到 412px、连带整行溢出卡片（520px 视口溢出 52px）；省略号只在容器宽度受限时才生效，故同时失效。修法：该断点下 `.task-info { align-self: stretch }`。
- **AI 页 `.form-row--wide` 单子元素时右侧空 733px**：该变体是「窄列 + 宽列」两列模板，而 Base URL 那一行只有 1 个字段，被压在 359px、右侧 733px 空白。修法：`.form-row--wide > :only-child { grid-column: 1 / -1 }`。复测输入框 359→1092px（占满）。
- **AI 页三个字段挤在两列网格里**：`.form-row` 只有两列，而「模型名 / API Key / 最长输出」三个字段同处一行，第三个被挤到次行首列、右侧空着且与 API Key 的说明文字错位。修法：拆成「模型名 + 最长输出」「API Key」两行。
- **同行主/次按钮高度差 1px**：`.btn-primary` 是 `border: none`，`.btn-secondary` 有 1px 边框，同处一行的按钮组因此 40px vs 41px（实测 AI 页保存配置 40、测试连接 41）。修法：主按钮改 `border: 1px solid transparent`，与 `.btn` 保持同一盒模型。复测四个按钮均 41px。
- **任务网格两列等高拉伸产生大片空框**：`.tasks-grid` 默认 `align-items: stretch`，会把「任务列表」卡片拉到与右列等高——实测列表只有 1 行时左卡高 904px 而内容仅 283px，空出 621px 的带边框空白框，视觉上像「列表坏了」。右列长是常态（帮助说明/编辑器都长），故改 `align-items: start` 让左卡收到内容高；复测帮助面板态左卡 354px（body 283px）、编辑器态 354px/989px，两列各自独立、空白回到页面背景。
- 顺带清理：任务页 Tab 栏由「满宽卡片 + 内容宽页签」改为 `width: fit-content`（3 个页签原本只占 1136px 卡片的 32%，右侧空 753px——此前靠 `margin-left:auto` 把 AI 入口推到最右，入口移入页签后空白失去依托）；`tasks.css` 中重复的 `.icon-xs`（与 `misc.css` 同名不同值）补注释说明当前生效值，未合并。

## 开发中（2026-09-14 AI 生成并入「任务」页第三个 Tab）

- AI 生成任务从独立导航项改为「任务」页第三个 Tab（`/tasks/ai`），与「浏览器任务」「脚本」并列。此前它在任务页是右上角一个按钮（`tasks-ai-link`），跳走后再保存又得自己找回任务列表；并入同一页后「生成 → 保存 → 看到成果」不再跨页，且 Tab 由子路由表达（刷新、深链直达，与另两个 Tab 同一套语义）。
- 路由：`/tasks` 子路由新增 `tasks-ai`；旧 `/ai-task` 改为 `redirect: { name: "tasks-ai" }`（与 `/scripts → tasks-scripts` 同口径），书签与旧深链不失效。`editorGuard` 的 `/tasks` 前缀判定无需改动即已覆盖新路径。
- `TasksView`：Tab 判定改为按子路由**末段**匹配（`browser`/`scripts`/`ai`）——原先的 `startsWith("/tasks/scripts")` 写法在 `/tasks/ai` 上会误判为浏览器任务；Tab 栏补 `role="tablist"` / `role="tab"` / `aria-selected`，屏幕阅读器可识别选中态。
- `AiTaskView`：外层去掉 `.page-content`（改由容器持有，避免同一页出现两个入场动画容器），保存成功后跳到「浏览器任务」Tab 并把提示改为「已在「浏览器任务」中显示」，不再停在原处让用户看不到刚保存的成果；`.ai-task-page` 自行补入场动画，切 Tab 时与另两个 Tab 观感一致。
- 导航一致性：侧栏本就无「AI 任务」项（上一轮已移除），此处同步删掉 `TasksView` 里的 AI 按钮与 `.tasks-ai-link` 样式（含 `responsive.css` 窄屏规则）；`BrowserTasksPanel` 帮助栏的「用 AI 生成」链路由 `ai-task` 改指 `tasks-ai`，窄屏下三个 Tab `flex: 1 1 30%` 均分。

## 开发中（2026-09-14 直连脚本 ctx 暴露本机 IP 与 MAC）

- **补齐上一轮记录的缺口**：eportal / Dr.COM 类门户的字段加密密钥由**来源 IP** 推导（密钥 = 来源 IP 各字符 ASCII 的 XOR 累积），而脚本 `ctx` 只有 `username`/`password`/`auth_url`/`page`，此前只能绕道「门户首页把来源 IP 渲染进 HTML → 从 `ctx.page` 正则提取」，对不回显 IP 的门户直接无法复现。现 `ctx` 新增 `local_ip`（本机主用接口 IPv4）与 `local_mac`（同接口 MAC），二者在 `POST /api/profiles/http-login-test`（发送测试请求）与正式自动登录**同源生效**。
- 网卡地址采集：`network::InterfaceInfo` 新增 `mac: Option<String>`，三个平台解析器各自补齐 MAC 提取（Windows `ipconfig` 的「物理地址」/`Physical Address`、Linux `ip addr` 的 `link/ether`、macOS `ifconfig` 的 `ether`）。MAC 值本身含冒号，不能沿用取值到最后一个 `:` 的通用写法，故按关键字定位（`extract_mac_after_keyword`）或按标签定位（`extract_mac_after_colon`）；`normalize_mac()` 只接受 12 位十六进制（允许 `:`/`-`/`.`/空格分隔）并统一输出小写冒号形式，`link/ether` 这类非定长文本会被拒掉。
- 主用接口选择：`network::select_primary_interface()` 按 `有网关 → 非 Wi-Fi → 列表首个` 排序，`local_address_from()` 据此产出 `LocalAddress { ipv4, mac }`（取不到均为空串）。**不是**简单取 `interfaces.first()`——多网卡机器（有线 + 无线 + 虚拟网卡）取首个常常命中虚拟网卡，得到的 IP 与 TCP 源地址不一致，脚本据此算出的密钥必然错误。
- 查询时机：`LoginOrchestrator` 仅在 `HttpLoginRequest::uses_crypto_script()` 为真时才调 `MonitorService::local_address()`（`list_interfaces` 要 spawn `ipconfig`/`ip addr` 子进程，无脚本则无人读取这两字段）；测试端点无 `MonitorService` 注入，每次自建检测器（与 `detect_profile` 同口径）。查询失败/超时一律降级为 `LocalAddress::default()`（空串）而**不阻断登录**——地址只是脚本入参的一部分，脚本自身应容忍空值。
- **写进日志脱敏白名单之外**：`local_ip`/`local_mac` 同时作为占位符注入 `vars`（`{local_ip}` 可用），若按默认规则视作秘密，日志会把本机 IP 打成 `***`，恰好抹掉排查「IP 不匹配」所需的唯一信息（实测：修复前显示 `IP 不匹配(...!=***)`）。故 `collect_secrets` 显式排除这两个键（与 `auth_url` 同理：调用方自己的机器地址，非用户秘密），并由用例 `collect_secrets_excludes_local_address_and_auth_url` 锁定。
- 顺带说明**超出既定范围的一处**：本轮把 `local_ip`/`local_mac` 同时加进了模板变量表，故 `{local_ip}` 占位符在请求地址/请求头/请求体里也可用（原定范围只有脚本 `ctx`）。二者共用同一取值路径，去掉占位符侧不会更安全（值一样会进脚本），保留则少一条"为什么 ctx 有而占位符没有"的困惑；如需严格收敛回 `ctx`，删除 `run_once` 里的两行 `vars.insert` 即可。
- 前端直连面板的脚本提示同步补上 `local_ip` / `local_mac`（并注明取不到时为空串、脚本须容忍），否则用户从界面上无从得知这两个字段存在。
- 验证：**真机 + 真实 eportal mock**（mock 侧从 TCP 源 IP 推导密钥并解密校验明文，故成功了才证明发出的密文正确，而非只看 HTTP 200）。场景 A 用 `ctx.local_ip` 推导密钥 → `直连请求成功（HTTP 200 OK）` / `dr1003({"result":1,"msg":"认证成功"})`；场景 B 故意喂错值（用 `ctx.auth_url`）→ `invalid_credential` / `IP 不匹配(...!=192.168.123.210)`，**B 失败才使 A 的通过有意义**。目标取本机 LAN IP（非回环）以保证 TCP 源地址与 `ctx.local_ip` 一致。
- 变异验证：去掉 MAC 长度校验 → 2 个用例失败；`select_primary_interface` 改回 `interfaces.first()` → 3 个失败；把 `local_ip`/`local_mac` 移出脱敏白名单排除项 → 1 个失败。均改回并逐字节确认复原。新增 21 个用例（MAC 归一化 5、关键字/标签提取 4、三平台解析各 1、主用接口选择 3、`local_address_from` 3、`ctx` 暴露与空值 2、脱敏排除 1）。

## 开发中（2026-09-14 修复 E2E 全链路用例仍调用已删除的全局启用任务接口）

- **缺陷**：`bfe7f52` 移除了 `GET/POST /api/tasks/active*` 两个路由（启用任务改为按方案绑定），但 `tests/login_chain.rs::setup_profile_and_task` 仍在 `POST /api/tasks/active/mock-login`——CI 的 `E2E Login Chain` job 因此 5 个用例全挂：`API POST /api/tasks/active/mock-login 返回 404 Not Found：{"error":{"code":"NOT_FOUND","message":"接口不存在"}}`。
- **为何本地漏检**：`login_chain` 的 `preflight()` 在缺少 `PIL`/`ddddocr` 时**打印一行原因后 `return`（跳过而非失败）**，本机 venv 未装 OCR 依赖，故本地 `cargo test` 该用例恒为"通过"，路由删除没有任何提示。这类"环境缺失即静默跳过"是既有设计（缺环境不挂 CI），代价是**改动接口契约时它不会告警**——排查时需以「用例是否真的执行过」为准，不能只看 `test result: ok`。
- 修复：改走方案级绑定——先把任务 `PUT /api/tasks/mock-login` 建好，再 `PUT /api/profiles/default` 带上 `active_task: "mock-login"`（`ProfileUpdateBody` 已支持该字段，与 `PUT` 同语义），与 `resolve_active_task` 的"方案绑定"解析路径一致，等价于真实用户在账号页为方案选择任务。
- 验证：本机装齐 OCR 依赖后 `cargo test --test login_chain` **5 passed / 0 failed（127s 真实执行，非跳过）**；`cargo test --features no-embed` 全绿（797 lib + 各集成 crate）。验证完恢复 `python_worker/pyproject.toml` 与 `uv.lock`（用例会把 worker 项目复制进临时 base，仓库副本须保持干净）。

## 开发中（2026-09-14 修正前端类型检查从未生效）

- **缺陷**：`frontend/tsconfig.json` 是 TypeScript「解决方案」文件（`"files": []` + `references`），而裸跑 `vue-tsc --noEmit` 不会遍历 `references`，因此**不检查任何文件、恒返回 0**。`npm run build` 里的类型检查步骤、以及 CI 依赖的 `npm run build` 长期是空检查：本轮一度出现「类型检查通过、但代码调用了已删除方法」（`TasksView` 调 `useTasks().setActiveTask`）才暴露。
- 修复：`build` 脚本与新增的 `typecheck` 脚本改用 `vue-tsc --noEmit -p tsconfig.app.json`（`app` project 含 `src/**/*.vue`）。已验证该调用能捕获真实错误（注入未定义标识符即报 TS2304、退出码 2），裸跑则漏报。CI 的 `frontend-python` job 增加独立的「前端类型检查（vue-tsc）」步骤（并把 `npm ci` 提为前置步骤，避免同 job 内重复安装）：此前检查只隐含在 `npm run build` 中，失败会混在构建日志里难以辨认——这正是它长期失效却无人察觉的原因之一。
- 顺带修复该检查暴露的既有错误：`AccountSettings.vue` 把 `Ref<string>` 直接绑定给期望 `string` 的 `password` 属性（改为解构出 `value` 传入）；`useConfig.test.ts` 的 `patchMock` 声明为零参签名导致 `calls[0][0]` 越界，以及 `patch` 包装里 `as []` 断言抹掉实参。

## 开发中（2026-09-14 启用任务改为按方案绑定）

- **「用哪个浏览器任务登录」从全局唯一改为按方案绑定**：旧实现把选择存在 `tasks/.order.json` 的 `active` 字段（全局一份，任务页「使用」按钮写），而 `ProfileData.active_task` 虽存在却**全仓无读取方**——文档却教用户「为各 Profile 分别绑定 active_task」，照做无效（实测：方案级设为 9 步任务、全局为 4 步任务，登录执行的是全局那个）。现改为方案级优先：解析顺序 `显式 task_id → profile.active_task → 内置 default`（`src/login/mod.rs::resolve_active_task`），实现「切方案即切任务」。
- 方案绑定会**同时校验任务类型**（`is_usable_browser_task`）：绑定的任务被删除或改成脚本时回落内置 default 并告警。仅查存在性不够——脚本"存在"但 Worker 只拿到空 `task_config`，表现为"浏览器打开却什么都没填"的假登录。
- 移除全局启用任务：`OrderData` 去掉 `active` 字段（只保留排序）、`TaskManager::get/set/load_active_task` 与 `TaskApi` 对应方法删除、`GET/POST /api/tasks/active*` 两个路由及 openapi 条目移除；`TaskManager::ensure_default_task` 不再写 active，`load_task` 于 debug 反馈包的"活动任务"改取当前方案绑定。
- 前端：「浏览器任务」选择内聚进 `LoginChannelField`（浏览器渠道下显示，直连渠道不显示），账号页与配置方案编辑器两处入口同时获得该能力；任务页移除每行「使用」按钮与行高亮（任务页只负责编辑）；`useTasks` 的 `activeTaskId`/`setActiveTask`/`fetchActiveTask` 与 `tasksApi.active/setActive` 一并删除；设置页「任务概览」的"当前任务"改显示当前方案绑定（未绑定显示"内置默认任务"）。
- 前端表单状态收敛：`active_task` 由 `Config` 顶层的隐形往返字段（有读写、无 UI）移入 `credentials`，与 `login_channel` / 直连参数同域——设置页可编辑、随保存载荷提交（后端仍按扁平键写入活跃 Profile）。
- **配置迁移 v8 → v9**（`migration::migrate_v8_to_v9`）：把旧全局 `active` 搬给当前活跃方案，避免升级后用户既有选择被静默丢弃（否则回退内置 default，表现为"升级后登录用了别的任务"）。方案已有显式绑定则不覆盖（幂等）；`.order.json` 的残留 `active` 由 serde 忽略（`OrderData` 已无该字段），并有单测锁定该兼容性。
- 测试：新增 4 个迁移用例（搬迁/不覆盖/无旧值/缺文件）与 6 个取值决策用例（显式优先、方案绑定、绑定不可用回退、未绑定不告警、空白等同于未绑定、全不可用返回空）；两处硬编码 `config_version = 8` 的旧断言改为比对 `CURRENT_CONFIG_VERSION`（避免每次升版都要改测试）。关键断言均以变异验证（改坏实现即失败）。
- 真机验证：v8 数据实跑迁移（`active=hust` → 活跃方案 `active_task=hust`）；同一实例下 `dorm`（绑 7 步 test2）登录日志为「步骤 1/7: 选择运营商（按钮组）」、切到 `lab`（绑 9 步 hust）后为「步骤 1/9: 输入账号」——**用 Worker 实际执行的步骤证明"切方案即切任务"**，而非只看接口回显。

## 开发中（2026-09-14 eportal XOR 门户直连复现验证）

- 用真实门户脚本（用户提供的 `login.sh`，eportal / Dr.COM 加密登录）验证直连渠道的复现能力：新增 `tests/mock-servers/eportal-xor/`，严格复刻该门户的字段加密协议（密钥 = 来源 IP 各字符 ASCII 的 XOR 累积；每字段逐字符 `^key` 后 `%02x`；`GET /eportal/portal/login?...&encrypt=1&v=2576`；JSONP 回调 `dr1003`），mock 侧解密后再校验明文，可**直接证明**发出去的密文是否正确（而非只看 HTTP 200）。
- 算法保真已交叉验证：把 `login.sh` 的 `get_key`/`enc` 用 Python 复刻并与真实 `sh` 执行结果逐项比对（KEY/ACCOUNT/PWD/IP 四项全等），确保 mock 与结论不建立在误读之上。
- **复现结论**：把 shell 算法逐句翻译为沙箱 `transform(ctx)` 后，经真实 `POST /api/profiles/http-login-test` 跑通——mock 解密出 `,0,{账号}@cmcc` 与正确密码并返回 `认证成功`；错误密码返回 `账号或密码错误`。即该类门户**可以**用直连渠道复现，无需 Python / 浏览器。
- **发现两处待解决项（本次仅记录，未改实现）**：
  1. 脚本 `ctx` 不含本机 IP（仅 `username`/`password`/`auth_url`/`page`）。本例的密钥必须由 IP 推导，只能绕道「门户首页把来源 IP 渲染进 HTML → 从 `ctx.page` 正则提取」；对不回显 IP 的门户将无法复现。需评估是否在 `ctx` 暴露本机 IP（如 `ctx.local_ip`）。
  2. 判定口径：eportal 响应恒为 HTTP 200（JSONP），`success_pattern` 留空时的「2xx 即成功」口径会把**凭据错误也判为成功**；`login.sh` 自带的 `grep "success\|dr1003"` 同样恒真（`dr1003` 是回调名前缀）。正确写法是 `success_pattern='"result":1'` + `failure_pattern='"result":0'`（实测可准确区分成败）。前端直连面板与文档应提示这类「HTTP 恒 200」门户必须填成功/失败关键字。

## 开发中（2026-09-14 直连渠道抑制环境横幅与文档修正）

- 仪表盘「Python 环境未就绪」横幅按登录渠道抑制：直连请求在 Rust 进程内完成登录，不拉起 Python Worker 与 Playwright，环境缺失对它无影响；此前横幅无条件显示，免 Python/浏览器的用户会被无意义提示长期打扰。判定收敛为 `utils/loginChannel.ts::channelNeedsRuntimeEnvironment`（未知/缺失值按"需要环境"处理，与默认渠道 browser 一致，宁多提示不静默漏提示），并在 `loginChannel.test.ts` 补 3 个用例（含变异验证：改坏判定即失败）。
- **文档修正（`type=shell` 已移除但多处文档仍按其存在描述）**：`custom-script-guide.md` 通篇重写——原文以「`script` / `shell` 两类任务」为骨架，含 `shell_path`、`type=shell` 示例与 `TaskKind::Script/Shell` 引用，而 `ShellTaskConfig` 在源码中已完全不存在；`user-guide.md` 的「三类任务」改为「两类」并删除 `/api/shells` 端点与 `ShellTaskConfig`（端点已不存在，openapi.json 亦无此路由）；`task-manual.md` 同步。均补注历史 `type=shell` 遇到时报错并提示改用 `script`（`src/tasks/models.rs`）。
- 顺带修正同源错误：`user-guide.md` 称 `settings.json` 为 v6 schema，实际 `CURRENT_CONFIG_VERSION = 8`；`task-manual.md` 与 `custom-script-guide.md` 称脚本可用 `{{USERNAME}}` 等模板，实际脚本路径**不做模板替换**（`build_script_command` 只把 `content` 原样写入临时文件、`args` 原样传参，`variable_resolver.py` 仅服务浏览器任务步骤层）——已限定为浏览器任务特性，并说明子进程环境变量中的 `USERNAME` 是操作系统登录名而非方案账号。
- `plan-next.md` 移除已解决的挂账项「`shell` 类型静默失效」。

## 开发中（2026-09-14 任务页合并与脚本定位修正）

- 「任务管理」与「自定义脚本」两页合并为「任务」页双 Tab：`TasksView` 改为容器（子路由 + 页签），原内容拆为 `views/tasks/BrowserTasksPanel.vue` 与 `views/tasks/ScriptsPanel.vue`。两页本就同源（`GET /api/tasks` 与 `/api/scripts` 返回同一混合列表，前端按 `task_type` 拆分）并共享拖拽排序与活跃任务语义，拆成两个导航项是历史包袱。
- 路由：`/tasks` 增加子路由 `tasks-browser`（默认）与 `tasks-scripts`，旧 `/scripts` 保留重定向到 `tasks-scripts`（书签与深链不失效）；`editorGuard` 的离开判定由「离开 /tasks 或 /scripts」改为「离开 /tasks 区域」，两个 Tab 之间切换不再拦截（两块草稿各自持有于独立 composable 单例，切回仍在，拦截反而会误清草稿）。
- 侧栏移除「自定义脚本」与「AI 任务」两个导航项：脚本已并入「任务」页，AI 生成任务作为「任务」页右上角入口（原为独立导航项，实为任务生成器而非任务类型）。`MORE_PAGES` 同步收敛。
- **修正错误文案（事实性缺陷）**：删除脚本页「脚本设为活动任务后，自动检测会使用脚本登录」等宣称脚本可参与登录的表述。实测链路：`LoginOrchestrator::submit` → `build_worker_config` → `embed_task_config` 对 `TaskKind::Script` 明确不注入 `task_config`（`src/tasks/loader.rs:269`「非浏览器任务属可预期分支」），Worker 据此走 `run_steps` 空步骤分支直接返回 `任务未包含任何步骤，无法执行`（`python_worker/playwright_worker.py:257`，且 `ensure_browser` 已先拉起浏览器）——即脚本被设为活跃任务后，自动登录必定拉起浏览器再失败。脚本页改为定位「定时执行的辅助动作（打卡/签到）」，并显式指向「直连请求」作为免代码登录方式；`scriptTemplates.ts` 模板同步改为签到示例。
- 移除脚本列表的「使用」（设为活动任务）按钮与 `useScripts.setActiveScript`：该入口只能把用户带入上述必然失败的路径；脚本的真实用途是「定时任务」的执行目标，不参与登录认证。`useScripts` 因此不再依赖 `useTasks`（`useTasks` 中对脚本类型的提示改为指向脚本 Tab）。
- 任务页 Tab 栏复用 `settings-tabs` 外观（含窄屏折行规则）；`ScheduledTasksView` 脚本提示与任务手册、用户指南中的页面指引同步更新。
- 测试：`scriptTemplates.test.ts` 增补「模板不再以登录脚本定位」断言；`editorGuard.test.ts` 重写为 Tab 语义（Tab 切换不拦截且不清草稿、两块草稿同时脏时逐块判定）。

## 开发中（2026-09-14 登录方式 UI 提取为可复用组件）

- 新增 `components/common/LoginChannelField.vue`：把「登录方式切换 + 直连请求参数（方法/地址/请求头/请求体/成功失败关键字/凭据变换脚本）+ 发送测试请求与结果面板」从方案编辑器抽出为可复用组件，以 `v-model` 绑定宿主草稿对象（原地修改，宿主脏检测按全量序列化比对可直接感知）。此前该 UI 硬编码在 `ProfilesView` 编辑器内，设置页等其他入口无法切换登录方式。
- 测试结果改为组件自持局部 ref：`useProfiles.testHttpLogin` 由「读写单例 ref」改为无状态函数（接收参数、返回报告），同一页面多实例各自展示结果、互不覆盖；`httpTestRunning` 保留为全局单飞门（后端本就单飞）。
- 新增 `utils/loginChannel.ts`：渠道标签与直连 outcome 文案的单一事实源（含 `HTTP_METHOD_OPTIONS`），避免同一枚举在多处各写一套中文标签而漂移；补纯函数单测。
- 设置页「账号」Tab 新增「登录方式」卡（复用同一组件），与方案编辑器两处均可切换；配套扩展 `GET/PATCH /api/config` 扁平响应与 Profile 域白名单，携带并接收 8 个直连字段（`http_method/http_url/http_headers/http_body/http_success_pattern/http_failure_pattern/http_crypto_script`），`http_url` 走与方案接口同一校验口径（空串=未配置放行，非空须为合法 http/https），枚举字段非法值显式 400。
- 前端 `CredentialsConfig` / `ConfigResponse` / `SaveConfigPayload` 同步直连字段；`useConfig` 据此纳入表单状态（设置页可编辑，故随保存载荷提交），取代上一轮的只读 `activeLoginChannel` 方案。
- 样式随组件迁移：`pages/profiles.css` 中 `.http-*` / `.channel-note` / `.profile-channel-switch` 等规则移入组件 scoped 块（该组件已跨页面复用，页面级 CSS 无法覆盖另一页面）。

## 开发中（2026-09-14 设置接口暴露登录渠道）

- `GET /api/config` 扁平响应新增 `login_channel`（活跃方案的登录执行渠道）：前端判定登录方式与「按渠道抑制 Python 环境未就绪提示」的数据源，无需为一次判定再拉整个方案列表；完整直连参数（`http_url` 等）仍按需读 `GET /api/profiles/{id}`。
- `PATCH /api/config` 将 `login_channel` 纳入 Profile 域白名单并做枚举校验（非法值显式 400，不静默保留旧渠道）。此前该键既不在 Profile 也不在全局白名单，客户端原样回传会落入 `other_patch` 经 `json_merge` 合并到 settings 顶层。
- 前端 `ConfigResponse` 新增 `login_channel`；`useConfig` 以独立只读 ref `activeLoginChannel` 暴露，**有意不并入 `config` 表单状态**——该字段由「配置方案」编辑器按方案写入，若并入表单会进入 dirty 快照与保存载荷，用户在别处改完渠道后回到设置页点「立即保存」即把界面上不可见的旧值静默写回，翻转刚改的渠道。
- 补齐测试：GET 回传 http 渠道、PATCH 落回 Profile 且不改动全局设置、非法值 400；前端新增「只读派生值暴露且不进入保存载荷」用例。三处均以变异验证断言有效（改坏实现即失败）。

## 开发中（2026-09-14 直连 HTTP 登录渠道）

- Profile 新增浏览器/直连登录渠道及 GET/POST、URL、请求头、请求体、成功/失败关键字、凭据变换脚本配置；运行时快照完整携带，存量 Profile 通过 serde 默认值继续使用浏览器渠道，无需迁移。
- 新增 Rust 原生直连执行器：模板占位符、登录页抓取、boa JavaScript 凭据变换与散列/编码内置函数、失败优先的结果判定、登录后网络验证、重试/取消/历史状态机复用；直连路径不初始化或调用 Python Worker，也不失败回落浏览器。
- 直连网络边界：禁用系统代理、请求 20 秒超时、最多 5 次重定向、响应体流式限读 64 KiB；URL/模板/脚本设体积上限，测试请求单飞；报文、响应和错误按凭据与脚本产出脱敏，长凭据先替换避免前缀残留，Base64 非法输入显式报错。
- Profile Web API 创建/更新支持全部直连字段；新增 `POST /api/profiles/http-login-test`，可用编辑器未保存值测试，已有方案空密码安全回退本机已保存凭据，回显仅包含脱敏后的请求与响应摘要；OpenAPI 路由清单同步。
- 配置方案编辑器新增登录方式切换、直连参数与脚本输入、GET 凭据暴露提示、占位符/内置函数帮助、无保存测试及报文式结果面板；前端类型、默认值、保存载荷和 API 测试同步。
- full-portal mock 的 `/login` 增加 GET 查询串与 POST 表单直连模式，保留原 JSON+验证码浏览器模式；新增执行器单测、Web 路由脱敏/密码回退测试，以及清空 PATH 且无 Worker 目录的真实二进制直连登录集成测试。

## 开发中（2026-09-13 P3 评审项批量修复）

2026-09-12 评审报告 71 条 P3 + NEW-1：剔除 P2 批次已覆盖项后逐条复核（6 组并行子代理对照 HEAD 核实），4 条已修复/证伪（FE1-3 已修、FE1-4/TSK-5 证伪、FE2-7 清单过时）、43 个落地点按用户拍板分 5 批修复，18 项挂账 known-issues #22。拍板口径：MON-4 只拦环回+链路本地（解析后 IP 判定 + DNS 钉扎，不拦 RFC1918 内网门户）；FE2-9 全局路由守卫（定向判定 + 显式清草稿）；日志脱敏/签名体系/控制台非阻塞/PATCH 白名单收紧/IPC 行上限统一/run_script 复检/解压 canonicalize/TSK-6/8 判定强化均维持挂账。

**构建脚本（2026-09-13 实测收尾）**
- build.ps1 `-OutDir` 支持绝对路径（`IsPathRooted` 分支，此前 `Join-Path $Root $OutDir` 会把 `E:\Test\...` 拼成非法路径）；同日 E:\Test 便携版从 0 全功能实测（新 mock 门户 v2：302 跳转链/5 位验证码/环回跳转陷阱/失败注入），自动登录、failonce 重试、MON-4 三场景、FE2-9、C1 启动字段全过，发现 3 项低危 UI 问题（known-issues #23，测试资产固化见同批 E2E 固化提交）

**E2E 固化（2026-09-13 实测过程入仓）**
- 新增 `tests/mock-servers/portal-v2/`：实测用复杂 mock 门户进仓（form POST + 三级 302 跳转链、5 位数字验证码、sid 会话、failonce/failntimes/slowlogin/ban/kick 注入、MON-4 环回与通配跳转陷阱、/debug 面板），选择器与 full-portal 兼容
- `tests/login_chain.rs` 扩展 4 用例（用例级 tokio 串行锁）：302 跳转链登录、slowlogin 慢响应、/ban 限时封禁后重试跨窗、kick 掉线 → 监测发现 captive → 引擎自动重登（实测「断线自动恢复」端到端闭环）；原有 failonce 用例保留
- `tests/common/mod.rs` `spawn_instance` 将 worker 项目复制进 base（`copy_worker_project`，排除 .venv/缓存，清单同 build.ps1）：实例经 `worker_project_dir` 路径兜底曾共用仓库 `python_worker/`，bootstrap 的 `uv sync` 与 OCR 偏好对齐会改写其 pyproject/uv.lock 与 .venv——测试间互拆环境（preflight 假绿跳过）且弄脏工作区；副本化后实例依赖操作全部隔离在 TempDir，仓库不再被触碰。OCR 偏好按用例决定：`preset_ocr_preference` 仅供依赖 OCR 的用例在 spawn 前调用（当前仅 login_chain），其余用例保持偏好缺失的默认态——即覆盖大部分不使用验证码识别用户的真实引导路径（无 ddddocr、无额外下载）
- 新增 `tests/scheduled_tasks.rs` 3 用例（无 Python 门槛，普通 test job 全平台跑）：cron/startup 创建全字段落盘、切回 cron 由 `normalize_for_save` 权威清空启动字段（服务端互斥口径）、list 的 id 回填/task_type/startup_runs_today、重复 id 409/非法 trigger 400/更新不存在 404/toggle 往返与删除
- 验证：login_chain 5/5（真实执行 ~113-170s，两轮确认幂等）+ scheduled_tasks 3/3（0.7s，无 ddddocr 下载）；跑后 `python_worker/pyproject.toml`/`uv.lock` 无改动（ddddocr 由 CI e2e job 运行时 `uv add`，不入库）；实测发现 3 项 UI 问题挂账 known-issues #23

**批 1 配置/登录/监测（d2663df）**
- CFG-4 删除 settings 隔离态死代码（poisoned 标志与 4 处拒存守卫不可达：new_sync 起缓存恒为 Some 无置空写点）；CFG-5 删 is_windows_reserved_name 的 split('.') 死逻辑；CFG-6 decrypt_core 返回 Zeroizing<String>（can_decrypt 校验即弃明文不再留未清零副本，UTF-8 失败路径字节同样清零）；CFG-7 抽 set_key_permissions 复用（Python 密钥继承路径补权限收紧）；CFG-8 DecryptFailed 透传 profile_id
- LOG-3 重试耗尽文案统一总尝试次数；LOG-4 删死 match 臂；LOG-5 会话 panic 补齐 M4 终态协议（失败指标 + StatusManager 广播 + 历史记录，锁内取数不跨 await）；LOG-6 渠道自愈 warn 降 info
- MON-3 route print 网关保序去重；MON-5 ipconfig 真机测试改 #[ignore]；MON-6 块首标题行由 parse_ipconfig 传入删除块内重检；MON-1 删 ConflictingEvidence 生产不可达分支（变体保留供序列化兼容，debug_assert 锁定 tcp==Pass 不变量）
- MON-4 门户重定向跨主机跳转最小目的地址校验：解析后 IP 仅拒环回（127/8、::1、v4-mapped）与链路本地（169.254/16、fe80::/10），域名解析全部候选判定并钉扎首个已校验地址（ClientBuilder::resolve 防 reqwest 二次解析 TOCTOU）；同主机相对跳转免校验（mock 环境与真实场景均依赖）；不拦 RFC1918 内网门户；新增 4 测试

**批 2 bridge/tasks（62aad3c）**
- BRG-2 Rust 对 id=0 关闭哨兵回执特判降 debug（消除每次优雅退出的虚假"未知响应"告警）；BRG-4 _dispatch 响应写出收敛为可注入 emit + 单次回包守卫（handler 恰在超时瞬间完成的窄窗口不再同 id 双回包）；BRG-6 补 3 个容错测试（超长行后继续读、EOF 排空队列、超时竞态单回包）
- TSK-3 extract_zip 符号链接条目返回 Err（对齐 tar 口径）；TSK-7 wait duration 改 as_f64（浮点 1.5s 不再误判未配置）

**批 3 web（7141451）**
- WEB-9 PATCH active_profile_id 合并前校验 Profile 存在（悬空 id 此前落盘后静默回退空凭据），拒绝/放行两测试；WEB-7 profile 字段类型错误显式 400（不再静默跳过）；WEB-4 debug_screenshot 改 symlink_metadata + is_symlink 拒绝（对齐 WS 口径）；WEB-8 ?browser= 空串回退 chromium；WEB-1 导出 toast 显式告知本机产物已清理；WE2-5 七处 body_json 收敛到共享 test_support + profiles 内联 Mock 的 unreachable!() 改回退默认（保留 load_profile 真实查找语义）

**批 4 更新器/骨架/环境（69de14a）**
- UPD-5 格式 1 清单校验 sha256 非空（空串改报 ChecksumUnavailable）+ 修过期注释；UPD-6 超时字面量提常量（updater 60s 轮询、launcher 锁等待 30s、helper 退出轮询与二次探活）；UPD-7 UpdateInfo.size 交叉核对（与 Content-Length 及实际字节不一致 warn，SHA256 仍是完整性权威）；UPD-9 LauncherState 登记 background_tasks 关闭统一 abort（watchdog 有意除外），start_background_check 改返回 JoinHandle
- COR-2 删 setup_test_env 死代码；COR-5 --status 区分残留锁非零退出；COR-7 mock 服务器守卫托管（置停 + 回连唤醒 + join，不再泄漏监听线程）；COR-8 二次启动显式 --mode full；ENV-7 tag_name semver 白名单（非法跳过镜像）；ENV-10 check_uv_on_path 加 5s 超时 + kill_on_drop

**批 5 前端 + NEW-1**
- FE2-9 全局路由离开守卫（src/router/editorGuard.ts）：离开 /tasks、/scripts 且草稿脏时弹「放弃未保存的修改」二选一确认，按 from 路由定向判定，确认后显式 clearTaskDraft/clearScriptDraft（导出既有 clear 函数），取消/被抢占均阻断且保留现状；动态导入注册避免求值期循环依赖；6 个 vitest 用例
- FE2-4 CustomSelect 改 useId()（实例 id 不再恒为 cs-1）；FE2-10 通知自增 id 替代 time+message key；FE2-8 Dashboard 5s 复查定时器卸载清理；FE1-2 saveScheduledTask 补入口 in-flight 守卫
- NEW-1 LoginSource::Browser 注释口径修正（活来源而非历史遗留，API 契约与历史反序列化保留）

**复核收口（73e5c4d，A 组 7 条 + C1 + B2 + B5）**
- MON-4 补漏通配地址：判定谓词改 `is_blocked_redirect_ip` 并拦 `0.0.0.0`/`::`（含 v4-mapped），与 `web/ssrf` 口径对齐；放行决策抽纯函数 `all_addrs_allowed`、地址构造抽 `build_pinned_client`，新增 3 个离线用例锁定放行分支（含「钉扎后确实按已校验地址连接」，手工喂解析结果绕开真实 DNS）；残余风险 B1/B3/B4 改挂 known-issues #22 ⑱⑲⑳（挂账由 18 项增至 21 项）
- LOG-5 收口：panic 补偿路径去 `.expect`（改 `match &g.active_session` 直接取引用），补偿自身不再有 panic 点（原先若触发会跳过 notify_one 与清槽位）
- ENV-10 收口：`check_uv_on_path` 补 `.kill_on_drop(true)`，兑现注释承诺并与 `uv_executable_works` 同口径
- 注释/断言失实修正：Browser 抢占优先级 3 为次高（LoginOnce=4 最高）、detect-portal handler 去「无 SSRF 面」过时口径、mock 泄漏注释 8→6、editorGuard 补守卫覆盖范围（不含刷新/关窗）、隔离性断言改 `not.toHaveBeenCalled()`
- 前端类型精度：`ScheduledTaskPayload` 由 `Omit` 改显式 interface（`ScheduledTask` 的索引签名使 `Omit` 不剔除任何键，必填字段全丢）

**挂账与验证**
- known-issues.md 新增 #22：18 项 P3 挂账逐项理由（WEB-2、WEB-5、COR-4、UPD-4/ENV-8、COR-3、BRG-3、BRG-5、ENV-9、WE2-7、TSK-4/6/8、FE1-5、ENG-4/6、UPD-8、ENV-6）
- 验证：每批定向测试（config/login/monitor/engine/bridge/web routes/updater/environment + pytest 182 + vitest 91）+ fmt + clippy 全目标 -D warnings；收尾全量 cargo test / npm run build / vue-tsc 0 错

## 开发中（2026-09-13 前端类型检查清零）

- `npx vue-tsc -p tsconfig.app.json --noEmit` 存量 28 错全部清零（`npm run build` 内置的裸 vue-tsc 不检查任何文件，该命令才是真口径）
- CustomSelect 真正导出 `SelectOption`：interface 原先声明在 `<script setup>` 内（setup 块不支持 export 语句），7 个视图的 `import type { SelectOption }` 全部解析失败（TS2614）；改经普通 `<script lang="ts">` 块 `export interface` 供给使用方
- AiTaskView 生成终判重构：done 事件在 onEvent 闭包内写入的暂存变量 `pendingDone` 不参与外层函数的控制流收窄（外层视角被初始值钉死成 null，可选链非空分支随之成 never，TS2339×3）；改为 await 返回后经 `unref(generateResult)` 终判——直接读 `.value` 同样不行（此前置空动作的属性路径收窄会横跨 await 与闭包写入存活），unref 的返回类型来自签名、不带流收窄；`pendingDone` 本就是 `generateResult` 的重复暂存，一并删除
- scheduledTasksApi `create`/`update` 参数从完整 `ScheduledTask` 收敛为新增的 `ScheduledTaskPayload`（`Omit<ScheduledTask, "id" | "task_type"> & { id?: string }`，对齐后端 JobCreateBody：id 创建时前端生成、task_type 由后端按 target 推导，此前 TS2345×2）
- BrowserSettings 浏览器列表换用 `BrowserInfo`：本地手写类型多写了后端响应中不存在的 `engine` 必填字段（TS2322×2，模板实际未消费该字段）
- DashboardView 刷新按钮 `@click="fetchLoginHistory"` 改显式 `fetchLoginHistory(true)`（此前 PointerEvent 充当 force 实参，行为等价保持：绕过 5s 历史去重守卫）
- ScriptsView 新建脚本 `showScriptEditor(null)` 改 `showScriptEditor()`（签名 `taskId?: string`）
- NetworkSettings `checkFrequencyOptions` 去 `as const` 显式标注 `SelectOption[]`（只读元组无法绑定 CustomSelect 可变 options prop，TS4104）
- 清死导入/死变量（TS6133/6196×6）：useCustomColors 的 useToast、useProfiles 的 ProfileListResponse、useUi 的 BrowserInfo/UpdateInfo/browsersApi、drag.ts 从未读的 dragging ref（连带移除 ref 值导入）
- 测试桩去 spread（TS2556×5）：useStatus.test / useWebSocket.test 的 mock 包装改无参转发——裸 `vi.fn()` 的重载签名无法摊开 `unknown[]`，且被测代码本就无参调用
- 验证：vue-tsc 0 错、vitest 85 用例全绿（含改动过的两个测试文件）、`npm run build` 通过

## 开发中（2026-09-13 P2 评审项批量修复）

- FE1-1 配置加载失败复位 loadingConfig：`fetchConfig` catch 分支补复位——「并发取代后接管的新请求又失败」会让 loadingConfig 永久停留 true，dirty deep watch 被永久抑制导致设置页保存按钮失效
- FE2-2/3 弹窗滚动锁收口：新增 `useBodyScrollLock`（模块级计数），Modal / ConfirmDialog / SetupWizard 三处 5 个直接写 `body.overflow` 的位置全部收口——此前计数每实例独立 + ConfirmDialog 无条件清空，嵌套弹窗（AboutView 卸载、RepoImportModals 三层叠加）内层关闭会提前解锁外层
- FE2-5/6 浏览器设置页错误态：浏览器列表加载失败新增错误文案与重试按钮（对齐同页环境卡/OCR 卡模式）；「填入内置脚本」空 catch 补失败 toast
- WE2-1 `/ws/logs` 入站限制：升级前增加并发连接上限（16，满员 503 拒绝升级，RAII 计数守卫防泄漏）与单帧上限 64KiB（入站仅 ping 心跳与已截断的 frontend_log，对齐 axum 默认 64MiB 过宽的口子）
- WE2-6 OCR 并发钳制：Web OCR 登记器从无上限（capacity=None）收敛为并发 1——每个 OCR 请求派生 Python/ddddocr 子进程，无限并发会耗尽本地资源；409 文案改「已有 OCR 请求进行中」，`concurrent()` 无调用方后移除
- WEB-6 捕获包总量上限：`GET /api/ai/capture/bundle` 打 zip 前按 50MiB 累计预算预检（对齐 export_logs / feedback_bundle 口径），超限文件跳过并随 zip 附 `_skipped_by_quota.txt` 说明，消除异常产物全量读入内存撑爆内存的风险
- ENV-2 uv 校验超时：`download_uv` 第 6 步 `uv --version` 从裸 `.output()` 改用现成 `command_output_with_cancel`（5s 超时 + 响应取消 + kill_on_drop）——该步骤全程持有 BootstrapGate，挂起会永久占住引导互斥门导致整个环境子系统假死
- ENV-3 下载响应取消：`utils::io::download_streaming_with_stall` 新增可选取消令牌，chunk 循环以 `select!` 监听、命中即清理临时文件返回 `Cancelled`（uv 下载链透传既有 token）——此前取消仅在镜像尝试边界生效，传输中不响应，取消后最长仍占住引导门 300s
- ENV-5 SHA256 文件下载上限：`download_text` 增加 1MiB 响应体上限（content_length 预判 + 分块累计），修复镜像/劫持响应无上限全量读入内存的风险
- BRG-1 命令级超时预算下发：`execute_with_timeout` 向 params 注入 `rust_timeout_ms`，Python `_command_timeout` 改为 `min(0.9 × 预算, max(270s, 单步×20))`——此前 Python 固定地板与 Rust 600s 任务钳制不同源，长任务会被 Python 侧提前中断判失败；字段缺省回退原公式，双向向后兼容；IPC 契约文档同步
- ENG-1 Engine 崩溃重启窗口消除：`watch_engine` 重启流程改为「先 spawn 新 Engine 并原子换入 slot、再做 5s 冷却等待」——原顺序在冷却窗口内 slot 持已死句柄，Web/托盘全部命令派发返回 ChannelClosed；新 Engine 处于 Stopped 态即可正常接单，冷却仅延后恢复监测时机，崩溃循环限速语义保留
- ENG-2 探测失败刷新最近检测时间：`ProbeMessage::Failed` 分支在补发优先级检测后合并最小 Engine 快照（仅刷新 last_check，网络结论沿用上次、evidence 置 None），monitor 系统性故障期间前端「最近检测」不再冻结在最后一次成功值；探测定时器维持原周期不重建的有意设计不变
- UPD-1 更新检查进度闸门：后台/手动/启动检查在 `update_in_progress`（下载/应用进行中）时跳过 `PartialSnapshot::Update` 合并——此前四处 merge 的 `progress: None` 会把下载任务每 500ms 推送的实时进度清零、`available: false` 会把"更新中"误报回退；`update_in_progress` 字段改 `Arc<AtomicBool>` 以便后台 task 跨 'static 读取；`record_last_check` 不受影响
- UPD-2 下载失败清理半写入 .tmp：停滞超时、chunk 读取失败、写盘失败、flush 失败四条错误路径统一经 `cleanup_tmp` 删除残留（对齐既有超限/校验失败分支），不再等到 3 天 stale 清理
- UPD-3 `--restarting` 剥离口径统一：新增 `launcher::collect_args_without_restarting`（args_os 采集 + retain 剥离），更新 pending 的 `original_args` 与重启后继共用——此前更新捕获用 `env::args()` 原样含 `--restarting`，重启中进程执行更新会让更新后的新进程错误继承重启语义；顺带消除非 Unicode 参数 panic 隐患
- LOG-1 Auto 来源补环境自动初始化：引擎自动登录（`LoginSource::Auto`）此前被 `prepare_browser` 的自动初始化条件遗漏——`execute_login_attempt` 同样占用浏览器会话槽位，环境未就绪时占槽后立即失败并按重试策略反复空耗；现与 Manual/LoginOnce/Browser 一样触发 `ensure_capability`（与取消令牌竞速）
- TSK-1 任务文件 BOM 兼容：新增 `strip_bom` 统一剥离 UTF-8 BOM 前缀，覆盖 load_task、列表摘要（read_summary/read_summary_typed）、`.order.json`、`.meta.json` 全部五个读取入口——Windows 记事本默认带 BOM 保存此前会导致 serde_json 解析失败，任务从列表静默消失；新增带 BOM 文件的加载与列表回归测试
- CFG-1 迁移结果写回：`run_migrations` 结束时把 `config_version` 写回 `CURRENT`——此前仅改返回值，落盘的 settings.json 永远停留在迁移前版本，每次启动重跑迁移链并原子重写盘（提交点契约失效）；v5→v6 内硬编码的中间 checkpoint（=6）保留供中途失败续跑定位。修正固化 version=6 的测试断言并新增写回回归用例
- CFG-3 配置写盘后 reload：`update_profile` / `create_profile` / `modify_settings_and_profile_tx` 落盘成功后统一触发 `reload_with_signal`，消除 ArcSwap 快照滞后（运行中 Engine/Monitor 持旧凭据）；信号按语义区分——更新发 `ProfileSwitched{id}`（与 switch_profile 对齐，调度器增量处理不重载任务表）、新建与事务发 `GlobalChanged`；事务函数的 reload 置于 profiles/settings 两锁释放之后，避免与 `reload_lock` 形成新锁序；三处均 best-effort（失败仅告警，重试 reload 可恢复一致）
- COR-1 挂账：`ServiceContainer` 无 `Drop`（启动半失败路径的常驻任务不会被显式取消）记入 `docs/known-issues.md` #21，记录复核口径——startup 实际不可失败、失败即 exit(1) 由 OS 收尸，仅当启动流程变为部分失败进程存活时才值得引入回滚编排
- LOG-2 登录历史保留策略：`LoginHistoryService` 新增 `clear_older_than`（按文件名日期判定，非日期文件名不清理），挂入每日 housekeeping 任务，固定保留 30 天（与 `/api/history` 查询窗口对齐，更早文件无消费路径）；不与 `logging.retention_days` 共用避免语义混淆
- COR-9 日志总配额兜底（软上限）：`cleanup_old_logs` 重构为一次目录扫描同时服务保留天数与配额两条路径，`app.log*` 总量超 200MiB 时从最旧轮转文件删起、删除失败跳过继续；当日活跃 `app.log` 因 Windows 句柄锁不参与删除（单日爆盘不在此兜底范围，隔天轮转后回收）
- WE2-4 收敛 Profile 创建语义：`POST /api/profiles/{id}` 删除「load_profile 既有档案后合并覆盖」的死代码（Service 层原子 create 的 ProfileIdConflict 检查使该分支永不落盘，实际请求以 409 拒绝），收敛为纯新建构造——消除未来放宽冲突检查时空密码分支静默清空既有加密密码的陷阱；不引入 Web 层 exists 预检，保持 Service 层原子 create 为唯一冲突裁判（防 TOCTOU）。新增回归测试：重复 POST 409 且原数据不变、新建空密码保持空串、PUT 空密码保留既有密码
- TSK-2 取消传播补全：Bridge 转发 task 新增监听 `response_tx.closed()`——调用方 future 被 abort/drop（任务取消、调度器停止等）时，此前无人发送 Cancel、Worker 会继续跑到自然结束或命令级超时；现在进入与超时/显式取消相同的「发 Cancel → 等 Worker ACK → 归属校验 → 必要时强杀」链路。原「取消后等待确认、超时强杀」收尾逻辑抽成 `wait_cancel_ack_or_kill` 共用 helper，三条取消路径（请求超时、显式 cancel、调用方中止）语义单一防漂移
- 集成测试：`supervisor_调用方中止_取消传播到worker并释放槽位` 覆盖 abort 调用方 → Worker 收到取消 → 强杀回收 → 槽位释放 → 后续请求正常完成的完整链路
- MON-2 探测语义修复：HTTP/URL 探测遇非预期状态码（1xx/4xx/5xx）从判 `Pass` 改判新增的 `ProbeOutcome::Inconclusive`（证据不足）——拦截型网关（未认证返回 403/404）不再被误判成 Online 且 Online 态强制 NoAction 截死补救路径，也不与全 Fail 混同落 Offline；多目标汇总优先级调整为 Captive > Pass > Inconclusive > Fail
- decision 层配套分级：新增 `AssessmentReason::InconclusiveEvidence`，Inconclusive 证据 + auth_url 可达走 `RecoveryAdvice::AttemptLoginOnce`（Engine 按 `cautious_attempted_config_version` 谨慎单次去重），防止探测目标自身短暂 5xx 周期性误触发自动登录；明确 Offline（全 Fail）+ auth_url 可达仍保持原 `AttemptLogin` 升级路径
- 前端同步：`ProbeOutcome`/`AssessmentReason` 类型新增 `inconclusive` / `inconclusive_evidence`，状态详情文案区分「谨慎尝试一次」与「等待下一轮确认」
- 测试：probes 新增 403/404 → Inconclusive 与 summarize 排位用例（mock server 需先读请求再回响应，避免 Windows 下 close 携带未读数据触发 RST 丢弃响应），decision 新增 Inconclusive 组合与 Offline 回归锚点用例

## 开发中（2026-09-13 v5 迁移映射与端口校验修复）

- 修正 v5→v6 配置迁移的 `enable_local_check` 误映射：经 v5（Python 版）源码核实，该字段是登录前物理网卡连接检查开关（`check_login_prerequisites`），现正确改名到 `local_check_enabled`；URL 内容检测在 v5 无独立开关（`url_check_urls` 列表非空即生效），`url_enabled` 改为按拆分后目标列表是否为空派生（与 Web 层旧客户端"非空即启用"派生口径一致），不再吞掉 v5 的本地检查开关值。存量迁移用户（config_version 6-8）不做 v9 回写（`url_enabled=true` 无法与用户主动开启区分），影响与补救口径记入 `docs/known-issues.md` #20
- Web API 端口硬校验（对齐 v5 Pydantic `ge=1 le=65535` 口径）：`PATCH /api/config` 在合并前校验 `app.port ∈ 1..=65535`，0/负数/越界返回 400 且不落盘——此前 serde u16 仅保证类型，port=0 落盘后重启控制台将永远无法按配置端口监听（Linux 非 root/Docker 直接绑定失败起不来）；<1024 特权端口不强制拒绝（Windows 无特权概念、Docker 可能绑 80）
- 前端端口校验修复 0/NaN 穿透：`validateConfig` 原真值判断 `if (port && ...)` 恰好放过需要拦截的 port=0，改为显式校验 `Number.isInteger` 与 1-65535 范围；启动链路 `launcher.rs` 的 `.max(1)` 保留作最后兜底
- 测试：迁移测试断言新映射语义并新增派生独立性用例（关本地检查+非空列表→url_enabled 开、开本地检查+空列表→url_enabled 关）；路由层新增端口非法 400 不落盘与合法值保存用例

## 开发中（2026-09-12 Python Worker 功能审计修复）

- 统一 Rust 任务校验、AI 生成提示、任务指南与 Python Worker 的步骤契约：`click_select` 明确必填 `selector + value`，`option_selector` 仅作可选搜索范围；`assert_text` 允许省略 selector 并默认检查页面正文
- 修复 `select/click_select` 的可选步骤失败被处理器吞掉、正式结果不列失败且调试面板误显示成功的问题；取消不再被可选步骤降级为成功，失败摘要与调试结果恢复真实语义
- 单步执行新增统一 deadline 与取消轮询，覆盖截图、OCR 模型获取、元素等待、识别和回填；按 name/URL 的动态 iframe 在同一预算内等待，显式 Playwright selector（如 `text=Last, First`）不再被逗号错误拆分
- 调试 `debug_step/debug_run_all` 改为逐命令绑定 Rust 注入的 `cancel_id`；Rust Bridge 取消后保留会话槽位等待 Worker 回包确认，超时强制回收，避免旧命令仍运行时新任务误入串行 Worker
- OCR 缓存移除跨请求 FIFO 预算，改为每次识别显式传递独立剩余预算，避免元素失败、取消或冷加载超时留下旧预算污染下次识别
- 普通任务截图改为延迟清理，为 Rust WebSocket 异步读盘内联留出窗口；截图/弹窗事件补齐 `session_type`，避免登录事件污染调试面板
- BrowserContext 统一监听新页面并接管 popup/新标签页，所有页面绑定 dialog 自动处理；持久化上下文保留 localStorage 登录态，仅隔离 sessionStorage，自定义浏览器数据目录按引擎与路径隔离
- 自定义浏览器健康检查开始验证实际可执行文件；stdin 使用有界 `readline` 分块丢弃超长 NDJSON；MHTML 捕获保证释放 CDP 会话，frame 树变化不再使页面结构捕获崩溃，无效 `wait_until` 回退口径统一为 `domcontentloaded`
- JavaScript 步骤模板变量按所处字符串字面量转义，阻断引号、反斜杠、换行和模板插值破坏脚本；内置默认任务不再裁剪账号密码首尾字符

## 开发中（2026-09-12 定时任务启动触发）

- 定时任务新增「启动后执行」触发方式（与原「定时执行」并列二选一）：软件每次启动后延迟执行目标任务，解决 cron 定时触发在未开机时段漏跑的问题；典型场景如网站签到，配合开机自启实现"每天开机后自动签到一次"
- 启动触发支持三项参数：每日成功次数上限（默认 1，达到后当日自动跳过，跨天自动重置）、失败重试次数（默认 2，轮内固定间隔 1 分钟，0-10）、延迟执行秒数（默认 30，0-86400，等待网络/校园网登录就绪）
- 执行计数口径为"仅成功计入"：失败不计入每日额度（下一轮重试或下次启动可再试）；手动执行不受额度限制、失败不计数，但手动执行成功同样计入当日额度，避免延迟窗口内/后再重复触发
- 调度器新增 `TaskTrigger`/`DailySuccessCount` 模型字段（`startup_success` 簿记随任务文件持久化，跨重启去重）、login_once 模式标志（由 Launcher 在模式分发前设置，单次登录模式启动触发整体跳过，防止执行被关闭取消）；启动触发任务不进 cron 调度表，由 cron_loop 初始加载后的一次性扫描派发（reload 不重跑），执行轮延迟结束前以内存最新定义重验删除/禁用/额度状态
- 执行轮与 cron/手动共用并发信号量（4）、同任务防重叠标记与 RunningGuard；执行历史 `HistoryRecord` 新增 `trigger` 来源字段（cron/startup/manual，存量记录容错为空），一轮重试只在终态通知一次；重试尝试在 `last_result` 与历史消息尾部标注"（第 n/N 次尝试）"
- `POST /api/scheduler/jobs` 的 `cron` 放宽为可选（启动触发可缺省），创建/更新请求体新增 `trigger`/`max_runs_per_day`/`max_retries`/`startup_delay_secs`（非法触发方式返回 400）；`save_task` 按触发方式归一化（切回定时执行清空启动字段，启动字段钳制 1-99/0-10/0-86400），cron 校验仅对定时触发执行；列表接口对启动触发任务回填 `startup_runs_today`
- 前端任务弹窗"执行设置"改为触发方式驱动：定时执行保留时间选择；启动后执行展示每日成功上限/失败重试/延迟秒数三输入（钳制口径与后端一致，`clampStartupForm` 纯函数含回归测试）；任务列表启动触发任务显示"启动后执行 · 今日成功 x/N"替代"每天 HH:MM"，"表达式无效"标记仅对定时任务生效；编辑启动触发任务不再误报非每日表达式覆盖警告
- 存量任务文件零迁移兼容：无 `trigger` 字段按 cron 处理；测试新增模型序列化/成功计数窗口/保存归一化/路由请求体等 10 例，Rust 55 例与前端 85 例全绿，clippy 零警告

## 开发中（2026-09-12 Web UI 审查建议落地）

- 设置·浏览器页在当前浏览器未安装时提供“切换到 Chromium”显式操作，未安装项统一弱化；Python 环境未就绪时锁定 Playwright 浏览器安装入口并就地说明原因，避免点击后才报错
- 设置·环境页把仅展示正向徽标的概览改为 uv、Python、认证核心、浏览器、OCR 五项常驻状态清单，未就绪与可选状态均可直接识别
- 仪表盘登录历史、自定义脚本和定时任务空态补齐下一步操作；任务列表为空时改为单列顺排，帮助内容不再占据右侧长栏
- 外观页补充玻璃模糊度依赖毛玻璃效果的提示；关于页为 Python 状态增加成功/警告语义与环境设置入口；AI 任务页明确解释“测试连接”的禁用原因
- 顶栏运行中的“停止检测”降为次要按钮；204 门户检测目标改用多行输入，并继续兼容英文逗号分隔
- 仪表盘网络横幅改为按状态分级展示说明依据：公网连接正常时收纳进标题旁的 `?` 悬浮气泡（复用 FieldHelp 的 data-tip 样式，新增 `networkAllGood` 判定），门户劫持 / 离线 / 冷却 / 暂停等异常态说明保持原地直显
- 设置·检测页 204 门户检测去掉「（主要）」后缀改为「推荐开启」徽标（badge--info），检测目标下的可见提示行删除、内容并入 `?` 悬浮说明；网卡诊断与认证地址预检两个开关的 `?` 说明补明「自动监测与自动登录均不做此项」及设计原因
- 设置页「任务」「环境」两 Tab 合并为「任务与环境」（Tab 回到 6 个）：任务概览与任务录制器宽屏同排在上，Python 环境状态清单、OCR 依赖与验证码识别依次在下；新建 TaskEnvironmentSettings.vue 承载两页内容（均为动作/状态卡、无保存栏，合并无冲突），删除 TasksSettings.vue 与 EnvironmentSettings.vue；`/settings/environment` 深链重定向到合并 Tab，仪表盘环境横幅、浏览器页未就绪提示与关于页修复入口的跳转与文案同步更新
- 「任务与环境」页 Python 环境卡移到页面最上方并改为可折叠：头部常显「已就绪 / 未就绪」徽标与初始化阶段进度，就绪自动折叠、异常或初始化中自动展开（watch `capability_ready` 跟随状态变化），头部可点击 / 回车 / 空格手动切换；修复折叠箭头未限定尺寸导致 SVG 以默认 300×150 渲染成巨箭头遮挡内容的问题（显式 16px）
- 保留审查中已判定无需调整的账号输入样式、侧栏“更多”、WARN 日志色条和卸载确认流程
- IconApp 图标注册表补齐 `terminal` 图标（环境页新状态清单在用），消除 vue-tsc 类型错误与运行时图标不渲染；`.gitignore` 补充忽略 `.playwright-cli/`（Playwright CLI 浏览器会话产物，仅本地使用）

## 开发中（2026-09-12 环境页文案口语化）

- 设置·环境页 Python 环境卡片的说明文字改为用户视角表述：去掉“Worker 核心”“真实验证”“缺包”等开发者术语（“真实验证”存在“真·实验·证”歧义、“缺包会自动修复”主语不通），改为“自动登录需要 Python 环境、认证核心和可用浏览器三项就绪。每次启动前都会自动检测，缺失的组件会先自动安装再继续；验证码识别（OCR）是可选功能，按需安装即可”
- 环境页「Python 环境」「OCR 依赖」卡片的操作按钮从标题区移到卡片正文右下角（`.env-card-actions` 右对齐行），宽屏通栏下不再孤悬卡片右上角；“未安装，请点击右上按钮安装”指引改为状态行内“未安装”、按钮就近可见
- 更正 rust-embed 嵌入口径：debug 构建为运行时实时读取 `frontend/dist`（静态查证 exe 内不含任何资源字节；向 dist 注入标记后，运行中的 exe 下发内容立即包含标记），改前端只需 `npm run build`，无需重编 Rust、无需重启后端；编译时嵌入仅发生在 release 构建，便携版打包前须先 `npm run build` 再 `cargo build --release`。此前“debug 也必须重嵌”的结论系本机 curl 未加 `--noproxy` 被系统代理劫持、返回旧内容造成的误判

## 开发中（2026-09-12 低分辨率样式适配）

- 修复任务管理页帮助栏在 720p 窄栏下被按钮组挤成一字一行：`.help-tip` 允许换行且文字 `flex-basis` 保底 160px，窄栏时按钮组自然折行到下一行
- 修复矮视口（720p/1366×768 扣浏览器框）下展开侧栏“更多”子菜单后底部状态与退出按钮被推出屏外：`.nav-links` 补 `min-height: 0` + `overflow-y: auto`，导航区内部滚动、footer 常驻可见
- 修复外观页滑块标签“背景可见度/玻璃模糊度”在 56px 列宽下折行：列宽放宽至 72px 并对标签禁用换行
- 以 1280×720 视口实测全页面（仪表盘/设置七 Tab/任务/AI/定时/脚本/外观/关于/弹窗/通知）与 1280×600 极限高度，确认无其他结构破版

## 开发中（2026-09-12 AI 任务生成增强）

- 页面捕获新增结构化上下文：按主页面/iframe 提取表单、控件、label、placeholder、name/id/autocomplete、下拉选项、可见性、提交地址、验证码候选、Shadow DOM 信号及稳定 selector 候选；同时保留脱敏局部 HTML，原始 HTML 仅作兜底
- LLM 配置改为面向新手的服务商卡片向导，新增“测试连接”并显示实际请求地址与耗时；DeepSeek、GLM、OpenCode 与自定义服务使用独立加密 Key 槽位，切换服务商同步切换 Key，不再跨服务误用；开放 16K/32K/服务商默认三档输出长度，默认提高至 16384
- AI 页重做三段状态与结构扫描摘要，补充响应式布局、键盘焦点和减少动画适配；未保存的模型配置不再允许直接生成，SSE 未收到终态即明确失败
- 收紧 AI 生成安全边界：仅允许新建 browser 任务，强制 `{{LOGIN_URL}}`，拒绝模型指定任务 ID、脚本任务及 `upload_file`；保存前重新生成唯一 ID，执行 JavaScript 步骤必须二次确认
- 修复流式 UTF-8 跨 chunk 损坏、无尾换行事件丢失、部分输出后传输重试串文、全流程超时可叠加至多轮 10 分钟和输出缓冲无上限；无效 JSON 也会进入一次自动纠错
- Base URL 拒绝 query/fragment，补充提示词注入隔离、补充说明长度限制、超大截图降级及结构化材料/任务安全校验回归覆盖

## 开发中（2026-09-12 文档清理与过程报告忽略）

- 删除已归档的 `docs/test-coverage-2026-08-30.md` 重定向桩（内容已在 `docs/archive/test-coverage-2026-08-30.md`）
- 删除过时过程报告：`docs/defect-recheck-2026-09-06.md`、`docs/updater-audit-2026-09-05.md`、`docs/updater-review-2026-09-09.md`、`docs/code-audit-2026-09-09.md`、`docs/monitor-flow-unify-2026-09-10.md` 及对应 proposal HTML；有效结论此前已收敛进 `docs/known-issues.md` 与 `docs/plan-next.md`
- 完成 `docs/archive/step_screenshot_after_*.png` 的删除（此前仅工作区移除，索引仍跟踪）
- `.gitignore` 补强过程报告规则：新增 `/docs/compose/`、`*bug-report*`、`*-bug-scan-*`、`*-coverage-*`
- 同步更新 `docs/plan-next.md`、`docs/known-issues.md`、`docs/archive/README.md`：去掉对已删报告路径的依赖，摘要保留为权威口径
- `AGENTS.md` 补充文档分工表与过程报告 ignore 策略（含 `git rm --cached` 处置步骤）

## 开发中（2026-09-12 账号页交互修复）

- 修复账号设置与方案编辑器的自定义运营商状态：下拉选择和关键字输入改为稳定的双向映射，删除全部自定义文字后仍保持“自定义”选项与输入框，不再意外回落到“不选择”；补充组合式状态回归测试
- 项目协作规则新增按风险分级的验证策略：低风险且已有定向覆盖的小修默认不再追加细致浏览器或全量 E2E，高风险边界与缺少有效覆盖的交互仍按需升级验证

## 开发中（2026-09-11 发布流水线后续修复）

- Release 创建时根据 tag 是否包含预发布后缀自动传入 `--prerelease`，避免后续 alpha/beta/rc 版本被 GitHub 错标为正式版；已发布的 `v5.0.0-alpha.10` 同步修正为预发布
- 修复 Windows WinNAT / Hyper-V 保留端口导致 Web 控制台以 10013 启动失败：本地回环首选端口真实绑定失败且属于占用/保留时，不再扫描相邻端口，而由内核原子分配可用端口；Docker/LAN 非回环监听保持固定端口失败语义；完整模式提前持有监听器再初始化后台服务，实际端口继续同步到 `.runtime_port`、`.instance` 与浏览器地址；启动失败日志改在文件日志 guard 释放前写入并 flush
- 修复调试页导出问题报告遗失 HTML、MHTML、截图与 CSS/JS 资源：Worker 落盘目录从与 Rust 安全守卫不一致的 `<base>/logs/feedback-*` 统一到 `<base>/python_worker/debug/feedback-*`，并新增路径契约回归测试
- 重构登录终态语义：`LoginResult` 以 `LoginTerminal::{Success,Failed,Cancelled}` 作为唯一权威，状态、历史、指标与浏览器保活均由终态派生；登录后网络验证改为 Online/可重试失败/取消三态，用户取消、抢占与应用关闭不再被误报为“重试耗尽”，Engine 也只将 Auto 的真实失败计入连续失败冷却
- 重构 Python 环境就绪管道：虚拟环境层显式返回“本轮已完成 uv sync”证据，Worker 层按 Current/Missing/Stale、重同步标记与 force 生成动作计划；状态缺失的既有 venv 强制同步，刚同步的新 venv 可安全认领，已知需同步时跳过无效的前置 import 探针，OCR 无变更与引导完成路径不再重复加载 Worker
- 修复更新 helper 在 overlay 后比较依赖清单导致重同步标记正常路径永不写入：改为覆盖前比较并预写 `.venv-resync`，仅在主程序完成 sync、Worker 探针与指纹记录后清除，覆盖 helper 中途退出恢复场景；补齐登录取消、冷却预算、环境动作矩阵和 helper 标记顺序回归测试
- 收敛 Web 长操作生命周期：AI 生成与 OCR 识别改用 RAII 操作登记器，任务正常结束、取消、panic 或 abort 都会释放占用并传播取消；OCR 支持追踪并取消全部并发请求，卸载依赖期间暂停新请求、回收 Worker，并为单次识别增加 120 秒总超时
- 统一 Python Worker 调试会话清理：停止、关闭浏览器、会话退出和强制中断共用幂等 teardown，完整注销取消 ID、关闭页面并删除截图；启动清理同时覆盖 `.png`、`.jpg`、`.jpeg` 过期截图
- 修复前端更新应用成功后错误调用关机接口：改为请求重启；仪表盘登录结果图标改为互斥渲染，避免失败时成功与失败图标同时出现，并补齐重启 API 单测
- 将 monitor 配置 PATCH 改为 `Option<T>` 类型化局部 DTO：仅映射显式提交字段，未知字段与类型错误返回 400，省略字段不再被旧硬编码默认值覆盖；补齐转换与 handler 回归测试
- 复核 `repo-bug-scan-2026-09-11.md` 的高优先级缺陷并修复进程与环境并发边界：uv 子进程在 stdout/stderr 提前关闭后仍受取消和总超时约束，项目文件回滚会删除本轮新建文件；环境刷新统一串行化且不会把“无错误但仍未就绪”误判成功，Bridge 健康监视避免向已满命令队列自锁，关闭超时会回收健康任务
- 收紧配置、调度与 AI 操作的原子性：全局设置和当前 Profile 使用补偿式跨文件事务，手动定时任务重复触发返回冲突而非静默成功，AI 上下文采集与生成共用互斥登记；日志级别写入前校验并统一大写，旧版 URL 监测配置迁移时同步拆分预期响应映射
- 修复网络监测与状态反馈细节：URL 响应体读取中断立即判失败，完整 URL 正确解析 userinfo 与 IPv6，Linux 网卡只接受精确 `UP` 标志，Windows IPv6/空网关行不再清除已解析 IPv4 网关；首次真实故障不再被通知器吞掉，重试耗尽消息按“首次 + 重试”报告总尝试次数
- 强化 Worker 生命周期：反馈采集按轻量方法执行且不覆盖调试会话，停止/强杀同步清除遗留截图地址；Playwright 导航、页面准备和步骤执行统一进入 finally 清理，异常时清 Cookie，登录空闲释放始终重新布防，取消调试/截图/OCR 返回取消语义；`wait_url` 使用所选 frame，`assert_text` 严格限定 selector，OCR capabilities 改为共享动态状态，并清理过期 `feedback-*` 目录
- 修复前端状态一致性：保存密码后不再立即回到“未保存”，重载配置会先确认脏数据并回读后端，浏览器环境各接口独立加载；手动登录读取结构化 `success` 字段，关于页单接口失败不再隐藏其余版本信息，登录冷却倒计时在后端快照不变时仍按本地时钟递减
- 明确更新与超时策略：应用内自更新只覆盖当前程序随附的 `python_worker`，外置 Worker 与 Docker 部署拒绝 overlay 并交由部署系统更新；helper 显式校验 Worker 目标、区分锁占用与锁错误，并在覆盖多个文件时汇总首个错误。LLM 继续保留 10 分钟硬总上限，仅澄清前端提示和代码注释中的“空闲等待/总时限”区别
- 验证：项目 `uv` 环境的 Playwright/Chromium 真实启动并通过前端容错冒烟；Python pytest 154 项、前端 Vitest 76 项及生产构建通过；Rust Clippy `-D warnings` 通过，库测试 718 项与全部集成测试通过（沿用本机 Windows 已知的 3 个进程探测挂起用例排除口径）

## v5.0.0-alpha.10（2026-09-11 发布与运行环境修复）

### Python Worker 与 OCR

- Python Worker 版本从主程序发布版本解耦，`pyproject.toml` / `uv.lock` / 健康回包 / Worker README 统一固定为 `1.0.0`
- 修复 OCR 安装和卸载命令漏传 `add/remove` 子命令：实际执行现为 `uv add ddddocr>=1.6.1` / `uv remove ddddocr`，并新增参数顺序回归测试
- `ddddocr` 继续作为按需能力，源码 `python_worker/pyproject.toml` 不默认声明；CI 全链路也改用 `uv add` 安装，不再绕过锁文件使用 `uv pip install`
- Python 环境初始化拆为“解释器可运行 / Worker 核心可导入 / 依赖指纹已验证 / 浏览器可用”四层状态：`python --version` 不再被当作 Worker 就绪，系统 Edge/Chrome 也只能替代 Chromium 下载，不能掩盖 Playwright Python 包缺失
- 新增真实 Worker import + `1.0.0` 版本探针以及 `python-runtime-state.json` 清单指纹；pyproject/uv.lock 或版本变化、`.venv-resync` 标记、import 失败任一命中即自动 `uv sync`，同步后必须复验成功才放行
- 修复 uv 托管 Python 被清理后 `uv sync` 仍复用损坏 pyvenv.cfg 的深层故障：解释器实启失败时先将旧 `.venv` 原地隔离，重建并验证成功后清理备份；同步或验证失败则恢复旧目录，避免留下更差的半成品
- Bridge 每次新建 Worker 前重新探测环境；首次 spawn/健康检查失败会在当前请求内强制同步并重试一次，仍失败才累计熔断，环境修复成功自动解除旧熔断
- OCR 用户偏好迁移到独立的 `environment/python-preferences.json`：启用/禁用先持久化意图，再经 `uv add/remove` 对齐部署副本；失败保留偏好供后续重试，且 OCR 修复失败不阻断非 OCR 登录
- OCR 初始化改走 Worker-only 门禁，不再为了验证码识别下载 Chromium；设置页新增 Python、Worker、托管 Chromium/系统浏览器的分层状态展示，“重新同步”按钮现在会真正强制同步而非就绪时空操作
- 新增依赖指纹变化、OCR 偏好迁移、声明识别和“系统浏览器不得遮蔽 Worker 缺包”回归测试；`GET /api/init-status` 与日志导出环境摘要补齐 Worker、指纹、系统浏览器和 OCR 状态
- 验证：Rust Clippy `-D warnings` 通过；库测试 694 项通过（3 个本机 Windows 进程探测挂起用例排除后），Bridge/单实例/登录链/更新通道集成测试单独通过；前端 73 项 Vitest 与生产构建通过；重建后的真实 venv 完成 Worker import + `1.0.0` 探针，Python pytest 150 项通过

### Docker 部署

- Rust 构建镜像对齐 `rust-toolchain.toml` 的 1.98，Cargo 拉取与 release 构建均强制 `--locked`，不再吞掉依赖拉取失败
- Rust 构建阶段补入 `docs/guides/`，修复 `rust-embed` 在 Docker 构建上下文内找不到指南目录的硬失败；运行镜像显式补齐 Rust 二进制动态链接所需的 GTK / AppIndicator / librsvg / libxdo 库
- uv 镜像从漂移的 `latest` 固定为 `0.12.6`；Rust 二进制实启、Worker 同步、Chromium 安装和 Python 导入校验任一失败即终止构建，不再降级为系统 pip 或吞错生成半成品镜像
- 修正 Docker 文档代码块、环境变量表格和持久化说明，`.dockerignore` 补充 Python 与过程报告缓存排除
- `docker-compose.yml` 默认拉取 `ghcr.io/misyra/campus-auth-rs:prerelease`，普通部署不再执行本地源码编译；新增 `docker-compose.build.yml` 保留显式自构建路径
- Release 工作流使用 GitHub 原生 x64/ARM64 runner 并行构建架构镜像，在 GHCR 合并为版本 tag；预发布同步更新 `prerelease`，正式版同步更新 `latest`

### 发布与仓库维护

- GitHub Release 说明改为按 tag 自动读取 `docs/updatelog.md` 对应章节；缺失版本章节时直接阻止发布，避免静默发出空更新说明
- 本地与 CI 便携包补入 `updatelog.md` / `changelog.md` / `known-issues.md` 和 LICENSE，保证包内 README 链接与许可证完整；同时携带 `src/` / `frontend/` / Cargo 清单 / `openapi.json` 最小构建上下文，修复便携包内 Dockerfile 无源码可构建的问题
- 发布并发创建仅容忍“已存在”竞态，其他 GitHub Release 创建失败不再被 `|| true` 吞掉
- 删除已跟踪的 `docs/archive/feedback-bilibili-final.zip`；BugReporter、审计与方案报告继续只保留在本地忽略目录，递归规则同时覆盖 `docs/` 下的嵌套报告目录
- 发布版本提升至 `v5.0.0-alpha.10`，同步 `Cargo.toml`、`Cargo.lock`、前端包清单、OpenAPI 契约与项目说明；用户更新日志由“开发中”冻结为 2026-09-11 测试版

## v5.0.0-alpha.10（2026-09-10 网络监测完整重构）

### 检测与判定

- 网络探测改为“原始证据 → 连通性解释 → 恢复建议”三层模型：HTTP 204 与 URL 内容属于强公网证据，TCP 只作补充；明确门户证据优先，单个补充探测失败不再覆盖已经确认的公网成功
- 默认仅开启 HTTP 204 门户检测；TCP、URL 内容与本地链路诊断默认关闭，按需作为补充
- 新增 `Unknown` 网络状态；所有探测关闭、只有 TCP 弱证据或证据冲突时不再伪装成 `Offline`。暂停改为独立的 Engine 行为状态，保留最后一次真实网络结论
- 自动监测、手动网络测试、登录后验证拆成独立入口：只有自动监测会补充认证入口证据并给出恢复建议；手动测试在停止或暂停期间仍可运行，且不会触发登录；登录后验证只确认公网是否恢复
- 本地网卡检查改为手动诊断的并行证据，不再位于自动监测关键路径，也不会单独触发或阻止登录

### 自动恢复与可观测性

- Engine 只消费类型化恢复建议，并统一执行停止、暂停、过期配置、冷却期及登录在途门控；明确门户但认证入口 TCP 预检失败时，同一配置版本最多谨慎尝试一次，避免永久假阴性与反复拉起浏览器
- 状态快照新增连通性置信度、判定原因、认证入口状态、恢复建议与最近一次原始探测证据；仪表盘直接说明“为什么这样判断、接下来会做什么”
- 手动网络测试返回结构化诊断摘要与本地链路结果，前端请求超时由 5 秒调整为 30 秒，避免后端仍在检测时客户端提前报错
- 设置页将 HTTP 204 标为主要探测，TCP 与 URL 标为补充；登录前认证地址预检保持默认关闭并明确只影响手动/单次登录

### 文档与仓库维护

- 将 `docs/changelog.md` 明确为逐项记录开发变动的“更改日志”，历史内容原样保留；新增 `docs/updatelog.md`，只在形成可发布版本时汇总用户可感知的更新
- BugReporter、审计、复核与方案预览等过程报告统一视为本地产物，通过 `.gitignore` 阻止新报告进入提交；此前已经跟踪的历史报告保持不变

## v5.0.0-alpha.8 后补丁轮（2026-09-08 日志系统整治）

> 全面检查日志子系统后的来源精简与缺陷修复；含一处 P0（Worker INFO 日志三路丢失）。

### 日志来源精简（19 → 5）

- 后端 `normalize_source` 改为五大域映射：系统 `app`（app/launcher/container/tray/config/updater/web）、认证 `auth`（engine/login/monitor/network）、任务 `task`（scheduler/tasks/notification/ai）、执行器 `worker`（bridge/environment/python_worker）、界面 `frontend`；未识别 target（第三方库）保留首段回退
- 文件日志 JSON 仍保留完整 target，信息无损；前端 `LOG_SOURCE_LABELS` 同步精简为 5 项，仪表盘来源筛选下拉与徽标随动

### 缺陷修复

- Worker 的 INFO 日志不再被静默丢弃：动态 filter 白名单补上 `python_worker` target（此前仅 `campus_auth`/`frontend` 生效，Worker INFO 及以下在控制台/文件/WS 三路同时丢失）；启动与热更新共用 `build_targets` 保证规则一致
- 设置页"启用文件日志"开关真正生效：`file_enabled=false` 时 `init_logging` 跳过文件层（此前为死配置，关闭后仍照常写盘）
- DEBUG 级别下任务步骤不再双显：Bridge 事件原始 dump 降为 `trace!`（白名单转发 + 前端合成条目已覆盖）
- `/api/logs` 历史条目补齐结构化字段（` key=value` 拼接对齐 WS 实时条目，此前仅 message）
- 接管 panic hook：崩溃信息进入日志流（subscriber 就绪前仅落 stderr，避免无声丢失）
- 前端 logger WS 上行加同 `scope+message` 5 秒节流（错误风暴不再等量回流刷屏）；`logging.level` 注释移除实际不支持的 OFF 档；main.rs 过时注释修正

### 日志系统二轮（同日追加）

- 日志子系统初始化前移到实例锁之前：锁冲突、强杀残留等早期 warn 不再丢在 subscriber 就绪前；重复启动（转开已有实例控制台）路径真正落一条 info
- 每日兜底清理过期日志：`cleanup_old_logs` 此前仅启动执行一次，关闭"自动重启"的长驻进程会无限累积轮转文件；保留天数每次从配置快照读取，跟随热更新
- 任务步骤/弹窗日志改由后端任务域留痕（source=task，获得统一 seq/时间戳并落盘、可导出），前端删除本地合成条目
- Worker stderr 的 traceback 续行继承前一行级别（此前一律降 WARN，按 ERROR 过滤时堆栈断尾）；stderr 级别解析改严格匹配，堆栈行不再被误切出伪级别

## v5.0.0-alpha.8 后补丁轮（2026-09-07 全项目评审修复）

> 2026-09-07 四路评审（AI 流式 / Rust 核心 / 前端 / Python Worker）新发现修复，另验证确认 76 条清单的 P0×9 与 P1×12 已随 fix/p0-p1-batch（09d7f66）全部落地。

### 认证门户地址自动检测（新增）

- 新增 `POST /api/monitor/detect-portal`：未认证时请求监测配置中的明文探测地址并手动跟随 302（最多 5 跳，相对 Location 按当前 URL 拼接），返回候选门户地址；已在线/门户直吐登录页/断网分别给出对应提示
- 前端三处输入框旁加“自动检测”按钮：方案编辑器认证地址、账号设置认证地址、AI 任务捕获地址；共用 `usePortalDetect` composable，检测只读填入、保存仍由用户手动完成
- 使用前提：需先退出校园网登录再检测（已在线时探测直通 204，无跳转可抓）；检测目标固定为服务端内置地址、不接受客户端传参，无 SSRF 面

### AI 流式生成（未提交功能收尾）

- 空闲超时真正生效：前端仅对真实数据帧重置空闲计时，后端 15s keepalive 注释帧不再续命（此前 2/5 分钟档永远打不到，只有写死的 10 分钟兜底生效）
- 生成取消与防重入：`/api/ai/generate/stream` 加全局在途互斥（重复发起 409）+ CancellationToken；客户端断连/离开页面即中止后端 LLM 调用，不再照常烧 token；前端卸载时 abort
- 流式重试不再拼接多轮输出：`attempt_start` 时清空预览文本；可重试判定由 `contains("5")` 字符串匹配改为类型化 `StreamError { retryable }`（截断/取消/预算耗尽不重试）
- max_tokens 可配置：`llm.json` 新增 `max_tokens`（缺省 8192 兼容旧行为，`null` 表示不携带交由服务商默认），PUT `/api/ai/llm-config` 支持设置
- 页面刷新后恢复捕获状态：新增 `GET /api/ai/capture/status`，产物仍在时无需强制重捕；底部进度条 done/error 后 6s 自动收起；SSE 事件枚举补进 openapi

### 正确性修复

- OCR/AI 捕获改每请求唯一 cancel_id：固定 id 会在 CancelRegistry 留下 60s pending 记录，误伤窗口期内下一次请求（卸载 OCR 后 60s 内的识别被静默取消）；`/api/ocr/uninstall` 经在途注册表精准取消真实在途请求
- 修复“安装 Chromium”在引导因系统浏览器存在而跳过下载时静默 no-op 却报成功：判据由 `capability_ready` 改为引导前后 `playwright_ready` 对比
- `/api/history` 分页参数钳制（page/page_size 上限 + saturating 乘法），debug 构建下超大参数不再溢出 panic
- `next_fire_at` 展示改本地时区偏移（原 UTC `Z` 后缀与本地触发语义差一个时区）；清理死常量 `UV_SYNC_MAX_RETRIES`、死变体 `GitHubApiFallback`，移除零引用依赖 `tokio-stream`，`bootstrap.rs` 截断助手复用 `tail_chars`
- Worker evaluate/assert_text 支持 `frame`（此前静默在主 frame 执行，iframe 门户必败）；JS 超时/取消只中断调用不再关共享页（此前后续步骤含失败截图全灭）；`click_select` 空 value 与 select 同语义显式报错
- Worker 命令级超时的注销改 try/finally（CancelledError 绕过 except Exception 导致 cancel_registry 注册项泄漏）；`force_interrupt_pending` 补齐 on_page_lost 语义（僵尸调试会话不再占住单会话槽位）；OCR 模型构造失败清理加载标记（残留会让超时误判为“首次加载中”）
- `ocr_recognize` 注册 cancel_id：卸载 OCR 可真实取消在途识别（Windows 上避免 onnxruntime DLL 占用导致 uv remove 失败）
- wheel 打包白名单补齐 `ocr_runtime.py`/`debug_session.py`（此前非 editable 构建产物 import 即崩）；playwright 依赖加 `<2` 上界；debug/captures 目录锚定与 Rust 读盘侧一致的运行时工程目录

### 前端修复

- 创建配置方案不再丢字段：`POST /api/profiles/{id}` 扩展可选设置字段（网关/SSID/认证地址/触发地址/运营商/活动任务），创建即完整落盘；创建定时任务补发描述与超时（此前被静默丢弃）
- 定时任务类型切换清空目标选择 + 保存前死引用校验；启停开关加 busy 守卫；加载失败首败提示（不再误显示“暂无定时任务”）
- 方案编辑密码占位修正：后端对空串按“保留原密码”处理，占位由 `_isNew` 推导（原 `startsWith('•')` 永假）；脚本示例模板覆盖前经 dirty 确认；导入过滤器移除 .exe
- ConfirmDialog 键盘可达（Esc=取消 / Enter=确认 / 焦点管理）；CustomSelect 下拉内按 Esc 不再连带关闭外层弹窗；`theme=auto` 监听系统深浅色切换
- BrowserSettings 类型对齐后端字段；错误文案链接提取改为首个 URL 匹配；OCR 识别错误统一 `extractApiError`、预览 objectURL 卸载兜底 revoke；logo 渐变改用 accent token（跟随自定义强调色）
- 修复本地调试脚本 `启动模拟测试.ps1` 路径中的字面换页符字节（mock 门户此前必然启动失败）

### 文档

- 任务编写指南同步：eval/assert_text 的 frame 规格限制、click_select value 必填语义、eval 超时不再关页

## v5.0.0-alpha.8（2026-09-05 AI 任务生成实测 + 执行结果修复）

### AI 任务生成

- 服务商预设对照官方文档核对收敛：智谱预设换用 `glm-5.3-flash`（原生多模态、1M 上下文），保留 OpenCode Zen 免费默认与 DeepSeek 实验视觉模型，移除硅基流动；自定义项标注 OpenAI 兼容格式
- 模型配置支持收起：已保存完整配置自动折叠为“模型配置”摘要行，点击展开；保存成功后自动收起，未配置时默认展开
- 页面顶部新增使用步骤提示与开发中提示：生成不稳定时可前往「设置 → 任务」手动安装录制器按视频教程操作
- mock 门户全链路实测：MiMo 生成任务 1 轮通过校验，真实执行登录成功（验证码 OCR 两轮均识别正确）

### 前端修复

- 补齐浏览器高级设置缺失的启动参数输入框：纯净模式关闭后可填写 Playwright 启动参数（每行一个，`#` 注释；Worker 侧自动过滤安全敏感参数），此前仅后端/Worker 支持而 UI 缺失

### 任务执行

- 修复手动执行浏览器任务失败仍报成功：步骤异常经 Worker 结构化回包（信封 success=true），任务页执行结果误按信封判定——现以结构化 `outcome` 为准，失败任务正确落入任务历史

## v5.0.0-alpha.7（2026-09-05 重定向登录 + 开箱即用）

### 重定向型门户登录

- Profile 新增 `trigger_url`：非空即重定向模式（`auth_url` 可留空），Worker 首导航到明文 http 触发地址并跟随 302 到真门户，`{{LOGIN_URL}}` 同步；监测跳过 auth 探测、登录跳过 TCP 预检，劫持判定优先于断网
- 账号页与方案页新增重定向模式开关：打开自动填入默认触发地址（`http://www.msftconnecttest.com/connecttest.txt`，与 Windows NCSI 同源），关闭清空回直连

### 开箱即用

- 内置默认登录任务（通用登录，经 `include_str!` 编进二进制）：首启缺失则写入并自动启用，已有启用/已改动的不碰；删默认任务、删除回退、`active.txt` 迁移缺省收敛常量
- 登录缺配置与相关失败全部改中文指引：无启用任务→“当前无启用任务，请手动启用一个任务”；空账号/密码/地址、认证地址不可达、自动登录跳过均带下一步动作

### 前端修复

- 修复白屏：`useUi` 补缺失的 vue 导入（附入口求值回归单测）；首屏主题脚本外置过 `script-src 'self'`
- 修复表单内开关滑块叠字：`form-group label` 权重覆盖 `toggle` 的 flex，显式赢回并禁滑块压缩（监测页既有开关同病）

### 监测与登录

- 外网探测失败但认证地址可达判为认证门户劫持，自动登录得以触发；门户二次判定签名简化
- 浏览器自动选择、按需下载与无浏览器预检；非法代理 URL 转错误与配置合并去 unwrap；启动装配收敛与运行时目录单一事实源

### 环境与发布

- 环境下载链切 npmmirror 并改用 uv 管理 OCR（含锁文件）；流式尾行覆盖改纯函数单测去 flake
- 文档接口改 markdown 直返与任务导入兼容；测试目录整理与共享脚手架；路由单测与登录全链路覆盖补齐

## 未发布（安全加固 + 测试体系）

### 安全加固

- 更新器：更新 URL 严格校验（拒 userinfo/前缀绕过）+ 重定向终点复核；SHA256 缺失三处拒绝（下载入口/helper/启动 pending），无伴随文件的平台包跳过；staging 应用加目录穿越与摘要复核
- 登录：指定 Profile 失败不再回退全局；切换失败仅告警（磁盘为权威，Engine 经 reload 收敛）；创建 Profile 检查与写入同锁原子化
- 任务调度：成功条件脱敏（只记变量名，不拼值）；类型切换删旧目录残留；脚本路径解析失败直接拒绝；调度器文件写入串行化；自定义请求头黑名单 + 换行拦截
- 系统边界：配置 reload 锁外通知；JoinError 走缓存沿用；探测子进程 kill_on_drop；监测代理/证书热更新；自启动先注册后落盘（失败回滚）；--force 杀进程前校验可执行文件名（防 PID 复用误杀）
- 反馈与杂项：反馈包路径收敛 worker/debug + 50MiB 总量上限；OCR 目录统计不跟随 symlink；前端路径段编码 + window.open noopener；动态导入改静态；package-lock 对齐 alpha.7

### 测试体系

- 目录规范：散落 `target/` 的 mock/基座归档至 `tests/mock-servers/` + `tests/fixtures/`；`mock_portal/` 搬迁 `tests/mock-servers/full-portal/`；`tests/common/` 共享脚手架接线；fixture 剥离运行时状态、端口收敛 18765
- 覆盖补齐：debug/background/autostart/scripts/tools 路由 oneshot 单测 33 个；`download_and_verify` 本地服务真实下载 3 个；`tests/login_chain.rs` 全链路（mock→二进制→Playwright→success＋failonce 重试）；手动 E2E 脚本环境变量 + 预检 SKIP；CI 新增 `e2e-login-chain`（ddddocr pin 1.6.1）

## v5.0.0-alpha.6（2026-09-03 统一更新通道 + 迁移过渡）

### 更新逻辑

- 统一预发布与正式版到同一版本比较通道：`compare_versions` 改为仅按 semver 大小 `remote > current` 判定，不再按 `alpha`/`beta` 前缀隔离；`5.0.0-alpha.5` 此后可正常收到 `5.0.0` 正式版与后续 `alpha.6+` 的更新
- 存量 `alpha.5` 用户通过本版过渡，后续正式版将直接可达

### Docker 部署

- 新增 `Dockerfile`（多阶段：Node 前端 → Rust 构建 → Python 3.12-slim 运行时）+ `docker-compose.yml`（命名卷 `campus-auth-data:/data` + 健康检查）+ `docker/entrypoint.sh` + `.dockerignore` + `docker/README.md`
- 运行时预装 `python_worker` 依赖与 Playwright Chromium（含 OS 依赖），加速首次启动；`CAMPUS_AUTH_HOST=0.0.0.0` / `CAMPUS_AUTH_PORT` / `CAMPUS_AUTH_BASE_PATH` 环境变量 + CLI `--host` / `--port` / `--base-path` 支持容器化配置
- 代码适配：`src/app.rs` 绑定地址可配置（`parse_bind_addr` + `is_docker_env` 自动 0.0.0.0，`start_axum` 新增 `host` 参数）；`src/launcher.rs` 新增 `--host`（`env=CAMPUS_AUTH_HOST`）、`--base-path`/`--port` 环境变量、Docker 环境自动禁用托盘与 `AppConfig.host` 透传；`src/tray/mod.rs` `TrayDeps.host` 随按需 Axum 启动透传；`src/environment/mod.rs` `resolve_worker_project_path` 新增 `/app/python_worker` 与 `CAMPUS_AUTH_WORKER_DIR` 回退，便于镜像内预装 venv 命中

### 验证

- `cargo check` / `cargo check --features no-embed` / `cargo clippy --all-targets -- -D warnings` 零警告；`cargo run -- --help` 确认新增 `--host` / 环境变量生效；`docker-compose.yml` YAML 解析通过（宿主机无 Docker，静态校验）

## v5.0.0（2026-09-02 正式版：全面检查修复 + 正式发布）

### 全面检查与安全加固

- **更新器校验收紧**：`fetch_manifest` 清单拉取新增 `https` 强制校验（仅放行 `127.0.0.1` 回环用于 e2e），`download_and_verify` 已有同口径校验；此前旧仓库地址在用迁移已落地（v6→v7）
- **脚本执行加固**：`executor::resolve_script_source` 拦截 `..` 穿越写法 + `canonicalize` 前缀校验防 symlink 绕过；`loader::validate_task` 补 `powershell/pwsh/.ps1` 白名单与 `script_path`/`work_dir` 穿越校验，避免经 `POST /api/tasks` 绕过 `PUT /api/scripts/{id}` 的拦截；`resolve_work_dir` 防御性二次校验
- **Web 层修正**：`app::build_router` 移除与 `web::build_router` 重叠的外层 `CompressionLayer`（避免双重 gzip 判定）；`autostart` 三端点 `enable/disable/mode` 改 `modify_settings_tx` 原子锁消除与 `PATCH /api/config` 的丢更新并发；`auth::load_or_create_token` 补 `0600`/`icacls` 权限收紧与 `monitor/mod.rs:check_auth_url` 内网直连注释（Captive 态代理不可达）
- **登录可取消**：`session::verify_network_after_login` 的 `sleep(post_login_delay)` 与 `check_once` 均监听 `cancel_token`/`shutdown_token`，点取消不再阻塞 60s
- **文档区分**：`GET /api/shells` 补 Script/Shell 域注释（Shell 任务支持 `powershell`，Script 任务禁止，二者正交）

### 版本与发布

- 四端版本统一到 `5.0.0`（`Cargo.toml` / `frontend/package.json` / `openapi.json` / `python_worker/pyproject.toml` + `WORKER_VERSION`），`uv.lock` 同步；`AGENTS.md` 更新

### 验证

- `cargo fmt --check` / `clippy --all-targets -- -D warnings` / `cargo test`（532 passed）/ `cargo check --features no-embed` 全绿；`frontend` vitest 49 passed；`python_worker` pytest 127 passed

## v5.0.0-alpha.5（2026-09-02 第二十轮：定时自重启 + 显式代理 + 日志体系全面优化）

### 定时自重启与显式代理

- **定时自重启**：系统设置新增选项（`app.auto_restart_hours`），按本次运行总时长周期性优雅自重启回收长期运行累积的内存，运行时修改即生效；重启后继进程生成收敛为 `launcher::spawn_restart_successor`，与手动重启共用（避免争锁导致"重启变退出"）
- **显式更新代理**：`updater.proxy_port` 端口制改为 `updater.proxy_url` 完整地址制（支持非本机代理，如局域网代理机）；旧配置仅填端口时由 `resolved_proxy_url` 兼容派生；仓库任务下载共用；前端输入框同步替换并按后端口径校验
- **设置 dirty 快照比对**：设置表单由"动过即置位"的单向闩锁改为与最近保存快照比对，值改回原样未保存标记自动消失；日志级别独立保存期间抑制比对；补 useConfig 单元测试

### 稳定性

- Bridge 取消注册表 pending 项加 60s TTL：会话结束后迟到的取消不再无界累积（内存泄露回归），remove 时连同 pending 一并清除

### 日志体系全面优化（约 170 处）

- **补缺失**：helper 日志落盘 `logs/helper.log`（GUI 子系统下 stdout 不可见，更新失败自此可诊断）、5xx 服务端留痕、任务执行器全链路、登录抢占/去重/槽位取代、Engine 首次崩溃、损坏配置隔离备份、启动序列版本/模式留痕、卸载/自启动等破坏性与生命周期操作审计
- **级别修正**：monitor 每轮探测、引擎冷却/暂停跳过、uv 镜像枚举等高频轮询 info 降 debug；终态失败升 error；断链/丢弃类升 warn
- **去冗余与措辞**：登录失败相邻双 warn 合并、Worker 崩溃双 warn 合并、Profile 幂等未切换不再误报"已切换"等
- **脱敏**：proxy_url 凭据仅留 scheme+host、IPC 原始行截断 200 字符、配置保存审计只记字段名不记值、success_condition 不再打印变量值
- Python Worker 补浏览器启停/重建、OCR 识别与会话淘汰、点击/输入强制降级路径日志；前端静默 catch 接入 frontendLogger

### 界面

- 全局样式 token 档位对齐（字号/间距/圆角），index.html 预置主题脚本消除首帧闪烁，新增 badge 组件样式
- 卸载弹窗状态标签区分：存在（黑体加重）/ 无（浅黑弱化）

### 验证

- `cargo fmt --check` / `clippy -D warnings` / `cargo test` 全绿；前端 vitest 49 通过；Python pytest 127 通过；CI 全绿

## v5.0.0-alpha.4（2026-08-31 第十九轮：代理体系 + 更新全量分发 + 状态一致性）

### 代理体系（三路解耦）

- **更新/仓库任务代理**：系统设置新增“更新与代理”卡片（`updater.use_proxy` + `updater.proxy_port`，如 Clash 7890）；启用后更新检查/下载走 `127.0.0.1:{port}` 显式代理，`/api/repo/*` 仓库任务下载共用同一配置（国内访问 GitHub/raw 常需代理）；未启用时跟随系统代理
- **网络检测默认不走代理**：监测设置新增“禁用代理”开关（`monitor.disable_proxy`，默认开启直连，避免代理故障误判 Offline；关闭后 HTTP/URL 探测跟随系统代理，重启生效）；MonitorService 构造时按配置固定客户端策略
- **仓库下载 SSRF 兼容**：`secure_get` 拆出代理版本 `secure_get_proxied`，DNS 钉扎 + 逐跳重定向校验流程不变；本地回环关机请求保留 `.no_proxy()` 防劫持

### 更新机制（承 a8d8648）

- 更新器移除全局 `.no_proxy()`（国内直连 GitHub 极慢是更新“等好久”主因），reqwest 启用 `system-proxy` 特性
- **helper 全量同步分发内容**：替换 exe 的同时 overlay 同步 `python_worker/`、`resources/`、`docs/`（覆盖同名、新增缺失、跳过 `.venv`/`__pycache__`、不删用户数据），Python 侧修复此后可随应用内更新到达用户；helper 自身支持自更新（rename-then-copy 避开 Windows 文件锁）

### 状态与配置一致性（同轮合入）

- **快照原子发布**：`StatusManager.merge` 改 `watch::send_modify`（修改+唤醒在 watch 内部锁内原子完成），消除“锁内修改、释放锁后 send 旧快照”的回退窗口；新增 `snapshot_version` 单调递增，前端 `useStatus` 据此做新鲜度比较（优先于 `uptime_seconds`，可区分同秒多次变化）
- **Engine 配置版本订阅**：Engine 订阅 ConfigService 版本广播，配置变更（保存/切 Profile）后派生状态即时重建；探测结果携带发起时配置版本，版本失配的结果不用于自动登录决策
- **登录准备期可取消**：`LoginOrchestrator` 登记准备阶段（环境初始化/auth_url 预检）的取消令牌（PendingGuard 守卫自动注销），准备期点取消不再“假成功”
- **卸载清理 Web 化**：新增 `GET /api/uninstall/detect` + `POST /api/uninstall`，清理 `~/.campus_network_auth` 用户数据、开机自启注册与 Playwright 浏览器缓存（env 指定目录只删浏览器前缀子目录）；关于页提供卸载向导
- ConfigService 设置修改统一走 `modify_settings_tx` 提交事务（读-改-写在同一临界区，消除并发丢更新）

### 验证

- `cargo test --lib`、前端 `vue-tsc` + vite 构建、`build.ps1` 四步全通过

## v5.0.0-alpha.3（2026-08-31 第十八轮：调试体验 + 反馈包资源快照 + 环境就绪一致性）

### 调试面板

- **关闭通道收紧**：点击遮罩空白/ESC 不再终止调试会话（误触代价高），右上角 X 与右下角"停止调试"按钮保留且同语义（均执行 `debug_stop`）；`Modal` 新增 `closeOnEsc` 属性供同类场景复用
- **调试入口环境门槛**：`POST /api/debug/start` 前置 `ensure_capability`，与登录/任务执行对齐——环境缺失自动引导（同手动登录语义），失败返回 503；此前调试直接走 Bridge，绕过就绪检查

### 问题报告导出

- **CSS/JS 资源快照**：Chromium 的 MHTML 序列化按设计不保存 JS（CSS 也只嵌内存缓存命中部分），导出时经 CDP `Page.getResourceTree`/`getResourceContent` 额外抓取主框架已加载的脚本与样式表，落盘 `debug/resources/`（SHA1 短名，上限 200 文件/单文件 5MB），并生成引用改写后的 `debug/page.html`（绝对/协议相对/`&amp;` 转义三种 URL 形态均改写）；zip 内 MHTML 与 page.html 并存——前者供视觉离线还原，后者配合 resources 供源码级还原
- 会话兼容表放行 `feedback_capture`（无副作用查询，调试会话存续期可随时导出）

### 环境就绪一致性

- **启动即探测**：程序启动时后台执行 `check_environment`（只读探测，不触发下载）刷新 `EnvironmentStatus`——此前状态初始全 false，磁盘环境完好时重启程序 `/api/init-status` 仍报"未就绪"，直到首次登录/任务才纠正
- Dashboard 首次取到"未就绪"时 5s 后自动复查一次，接住后台探测结果，避免误挂未就绪横幅

### 验证

- e2e 实测（bilibili）：调试会话 → 导出反馈包 2.37MB，含 MHTML 1.7MB + 整页截图 + 12 个资源文件（10 JS + 1 CSS）+ 引用改写后的 page.html；`cargo test --lib web::` 112 项、python_worker 127 项、build.ps1 四步全通过

## v5.0.0-alpha.2（2026-08-31 第十七轮：环境自举 + 前端一致性 + 文档收敛）

> 本轮前 `tasks/browser/hidden_input.json` 等旧任务的 `{{username}}` 裸模板经变量桥接虽可执行，但裸写法已改为带引号示例；`wait` 无 selector 的遗留语义改为仅执行兼容、保存拦截；前端任务 ID 校验与后端 `TASK_ID_PATTERN` 对齐。

### 环境自举

- 登录链路自动初始化：`Browser` 定时任务与 `Manual`/`LoginOnce` 在 `capability_ready=false` 时经 `BootstrapGate` 同步 `ensure_capability`（uv sync + Chromium），失败回失败终态并携带 `last_error`；`src/tasks/executor.rs` 浏览器任务同路径
- 新增 `POST /api/environment/bootstrap`（幂等、同步等待完成）与前端卡片：`SystemSettings` 的 Python 环境状态/进度/重试按钮 + `Dashboard` 未就绪横幅 + `BrowserSettings` 未就绪提示；`openapi.json`/`route_table` 已补契约

### 前端与文档

- 任务 ID 前端校验改为 `^[a-zA-Z0-9_-]{1,64}$` 与后端一致（此前 `^[a-zA-Z][a-zA-Z0-9_]*$` 误拒 `e2e-smoke` 等带连字符任务）；`hidden_input.json` 的 eval 脚本 `({{username}})` 改 `('{{username}}')` 加引号防 `ReferenceError`
- `docs/guides/task-writing-guide.md` 10 节补"新任务固定等待必须用 `sleep`，`wait` 无 selector 仅为历史兼容、保存时被拒绝"

### 验证

- 白名单 16 项在 Rust/Python/文档三端一致；7 个存量浏览器任务复刻保存校验全 `OK`；Python `ocr_runtime/step_handlers/variable_resolver` 无阻塞性逻辑错误；`cargo test --lib tasks` 80 项、`python_worker` 123 项、`cargo test` 522 项、`clippy -D warnings` 零警告

## 开发中（2026-08-30 第十六轮：三端兼容性修复）

> 修复第十五轮审计发现的 16 项三端问题中的 12 项（known-issues 第四节 W 编号），剩余 4 项为有降级方案的低中危遗留。

### 环境引导链（W1–W3，mac / Linux 高危）

- **W2 venv 路径**：`PYTHON_EXE_RELATIVE` 按 cfg 分支——Windows `.venv/Scripts/python.exe`，unix `.venv/bin/python`；此前硬编码 Windows 布局使 mac / Linux 的 venv 检测、引导、Bridge Worker spawn 全链路误判"未安装"，浏览器登录整体不可用
- **W1 uv 资产格式**：下载 URL 按平台拼 `.{UV_ASSET_EXT}`（Windows zip / unix tar.gz，官方 unix release 只发 tar.gz，实测资产列表确认含 `.sha256`）；`extract_uv_from_zip` 泛化为 `extract_uv_from_archive`，unix 解压后显式补 0755 兜底
- **W3 权限位**：`extract_zip` 恢复 zip entry 的 unix_mode；新增 `extract_tar_gz`（tar slip 防护 + 链接条目拒绝 + 大小上限 + mode 恢复）；统一分派入口 `extract_archive` 按扩展名选 zip / tar.gz

### 更新器（W4 + W15，mac / Linux 高危）

- 下载落盘文件名跟随资产 URL（截断 query），解压经 `extract_archive` 按扩展名分派——unix 产 tar.gz 不再被按 zip 硬解；`collect_zip_assets` 更名 `collect_package_assets`
- helper 替换后在 unix 上对目标 exe 显式 chmod 0755（防解压链路丢 +x）；备份名统一 `<原名>.bak`（unix 上不再产出 `campus-auth.exe.bak` 怪名）

### 进程治理（W5 / W11 / W12）

- **W5**：`force_kill` unix 实现改为 `kill(pid, SIGKILL)`，`--force` 抢锁可用
- **W11**：`wait_for_shutdown` 新增 SIGTERM / SIGHUP 监听，外部 `kill` / launchd / systemd 停止走优雅关闭，Worker + chromium 不再变孤儿
- **W12**：unix 任务子进程以独立进程组启动（`process_group(0)`），超时 `killpg` 连带回收整棵 shell 子树，对标 Windows Job Object + taskkill /T

### 托盘与卸载（W6 / W7）

- **W6 Linux**：托盘线程改为 gtk::init（构建前）+ glib 主循环（50ms 轮询命令通道兼顾刷新与退出）——tray-icon 要求 gtk 循环与托盘构建同线程，此前仅阻塞 recv 事件永不分发；gtk 0.18 与 tray-icon 内部依赖同版本共享状态（cargo tree 验证单版本）
- **W6 macOS**：按用户决策**禁用 macOS 托盘**——tray-icon 要求主线程 NSApplication 事件循环，主线程运行 tokio runtime 的架构无法满足，非主线程构建有崩溃风险；拦截收敛在 `TrayManager::spawn` 内部单点（`cfg!` 运行时判断提前返回空句柄，后续任何调用路径都开不起来，且避免 macOS CI 的 dead_code / unreachable 告警）；轻量模式在 macOS 自动降级为完整模式（保住 Web 入口）
- **W7 卸载**：unix 生成可执行 `uninstall.sh`（/tmp 自复制 exec + pkill -x 精确按可执行名杀进程 + comm 截断名兜底），Windows 保留 uninstall.bat；unix 增补 shell 元字符注入校验

### 发布配套（W10 / W13 / W14 / W8 / W9）

- **W10 CI**：新增 `rust-tests-unix` job（macos-latest + ubuntu-22.04：clippy -D warnings + cargo test，Linux 装 GTK -dev），unix 分支编译错误不再首发于 tag 发布时
- **W13**：release 矩阵补 windows-arm64 产物（MSVC 自带 ARM64 工具链交叉编译）；linux-arm64 因 aarch64 GTK 交叉链接暂缺（workflow 注释说明，平台键保留）
- **W14**：build.ps1 打包排除 `__pycache__`，与 release.yml 口径对齐
- **W8 / W9**：Release 发布说明写入平台运行前提——Linux GTK 运行时库 apt 命令、macOS `xattr -cr` quarantine 解除指引
- zip 依赖 features 裁剪为 `deflate`（默认 features 拉入 lzma/bzip2/zstd 三个 C 依赖，构建与交叉编译都受累）

### 新增依赖

- `tar 0.4` + `flate2 1`（unix 资产解包）；`gtk 0.18`（仅 linux target，托盘事件循环）

### 发布流水线首跑修复（tag 触发 Action 后暴露）

- **更新源指向错误仓库**：updater 默认 URL 与设置默认值指向旧仓库 `Misyra/Campus-Auth`，前端两处仓库链接同源——统一修正为 `Campus-Auth-rs`，否则首个发布版的更新检查会查到旧 Python 项目
- **unix 存量编译错误**（CI 新增 unix job 首次暴露）：`bridge/orphan.rs` unix 分支 `debug!` 宏未导入、`environment/git.rs` unix 分支 `mgr` 未使用、孤儿清理嵌套 if（clippy collapsible_if）、`utils/io.rs` 权限测试缺 `PermissionsExt` 导入
- **Linux 链接缺库**：tray-icon→muda 在 Linux 链接 `-lxdo`，CI 与 release 的 Linux 构建依赖补 `libxdo-dev`（clippy/check 不链接故此前未暴露）
- **测试平台假设**：虚拟网卡特征表断言（VMware/Npcap 等仅 Windows 表）、ipconfig 实调、Windows 路径绝对性/反斜杠断言——按 `#[cfg(windows)]` 圈定，跨平台断言保留；uv 解包过滤放宽为同时接受 `uv`/`uv.exe`
- **--stop 兼容 unix 僵尸进程**：实例作为测试/脚本子进程退出后若父进程未回收，`kill(pid,0)` 仍判定存活导致 20s 空等——`stop_instance` 增加"监听端口已关闭"第二判据（shutdown POST 已送达时端口关闭即停止）

### 验证

- cargo test 522/522（含新增 tar.gz 解压布局 / 权限恢复 / tar slip 跳过 / extract_archive 分派 / uv tar.gz 提取 5 用例，其中 zip 权限位用例仅 unix 编译运行）；clippy `--all-targets -D warnings` 零警告；release 冒烟复测（HTTP 200 / 重复启动 / --stop 优雅退出）通过
- unix 平台无法本地编译（ring 等依赖需目标平台 C 工具链），已做 API 级自查（tokio `process_group`、glib `ControlFlow`/`timeout_add_local`、cfg 语句属性均对照源码验证）并以新增的 CI unix job 兜底，推送后首次生效
- tar header size 必须与数据长度一致（tar 按大小寻址）——测试初版 set_size 与数据不等导致 4 例失败，已改为由内容长度推导

## 开发中（2026-08-30 第十五轮：启动 GUI 化 + 三端兼容审计）

> Windows release 双击启动不再弹控制台并直接打开 Web 界面；完成三平台兼容性全面审计，问题清单登记至 known-issues 待圈定修复范围。

### 启动方式 GUI 化

- **隐藏控制台窗口**：`main.rs` 增加 `windows_subsystem = "windows"`（仅 release 生效，debug 保持控制台子系统以兼容 cargo run / 集成测试的 stdout 捕获），PE 头实测 Subsystem=2，双击 exe 不再弹出命令行窗口
- **终端输出兜底**：新增 `attach_parent_console`——release 从 cmd / PowerShell 启动时附着父进程控制台并只为缺失的标准句柄补 CONOUT$，`--status` / `--stop` 等子命令输出可见；重定向句柄（> 文件 / 管道）原样保留不覆写；双击启动（父进程 explorer 无控制台）附着失败静默返回——`Start-Process` 实证 Rust std 对 NULL 标准句柄按静默丢弃处理（退出码 0 无 panic）
- **ctrl_c 秒退防护**：无控制台环境下 `tokio::signal::ctrl_c()` 注册可能立即返回 Err，select 分支会瞬间完成导致进程秒退；注册失败时退化为永久挂起，退出路径交由关闭令牌 / Web API / 托盘
- **重复启动直接打开界面**：实例锁获取失败且运行中实例记录了 Web 端口时，改为在浏览器打开该实例的 Web 控制台后正常退出（受 `--no-browser` / settings `auto_start_browser` 约束），双击 exe 不再"静默无响应"；轻量模式（端口 0）维持原报错
- `read_startup_settings` 轻读补 `auto_start_browser`（兼容迁移前旧字段名 `auto_open_browser`）；`AppConfig.no_browser` 收敛为 `auto_open_browser` 单一字段，同时约束"启动后打开"与"重复启动打开"两条路径

### 三端兼容性审计（Windows / macOS / Linux）

- 全量排查 cfg 分支、隐含平台假设、进程与信号、打包分发、前端平台逻辑：自启动 / 网络检测 / 孤儿清理 / 平台键等已有健康的三分支实现，但环境引导链与更新器在 mac / Linux 上成套失效
- 16 项分级问题（高 7 / 中 5 / 低 4）登记至 `docs/known-issues.md` 第四节，高危集中在：uv 下载 URL 对 unix 目标拼 `.zip`（官方仅发 tar.gz）、`PYTHON_EXE_RELATIVE` 硬编码 `.venv/Scripts/python.exe`、解压 / 复制不恢复 unix 可执行位、自更新 tar.gz 被按 zip 解压、`--force` 的 kill 在非 Windows 为 stub、tray-icon 平台线程约束未满足、卸载仅产 `.bat`

### 验证

- cargo test 518/518（0 失败）；clippy `--all-targets -D warnings` 零警告；release 实测：PE Subsystem=2、后台启动 Web 200、重复启动报错路径与 `--stop` 退出正常、无标准句柄启动退出码 0

## 开发中（2026-08-30 第十四轮：钉底误报 + 调试面板修复 + 发布流水线）

> 发布前 e2e 验收发现的用户可见缺陷修复 + 三平台发布工作流。

### 仪表盘日志"N 条新消息"钉底误报（探针实测定位）

- **根因一（启动窗口翻转）**：`.log-viewer { scroll-behavior: smooth }`（00f9ea8 批次引入）把 scrollToBottom 的跳底变成 ~800ms 逐帧动画，动画中间帧让 onLogScroll 判定"不在底部"把 autoScroll 翻 false（实测 87 帧/约 670ms），窗口内到达的日志全被计入新消息计数；动画进入 40px 容差后又翻回 true
- **根因二（残留计数）**：newLogCount 仅按钮点击与 clearLogs 清零，手动滚回底部后残留
- **修复**：删除 smooth（程序化滚动仅自动跟随与回底按钮两处，瞬时跳底是日志面板标准行为；scrollToBottom 留防回归注释）；onLogScroll 判定在底部时调 useLogs::markAtBottom 清零；补回底清零单测

### 调试面板"该任务没有可执行的步骤"（两层根因）

- **主因（后端）**：debug 路由 start/step/stop/run_all 把整个 IpcResponse 信封 `{id, result:{data}}` 序列化进响应，前端 request() 只解一层 data，syncSession 拿到包装结构后 steps/running/task_id 全丢——步骤列表从未渲染，3/5 计数是 WS step_progress 撑的假象。四路由改 `Ok(data(resp.result.data))`，与 debug_status 的正确写法对齐
- **辅因（前端）**：刷新恢复时"执行全部"占死 Worker 命令队列，status 详情查询 5s 超时只剩骨架——新增 refillSessionDetails 退避补全（5/10/20/30/30s，拿到步骤或会话结束即停），骨架文案改"会话详情恢复中，当前执行结束后自动补全"
- **裂图**：last_screenshot_url 跨会话残留指向已被停止流程删除的截图文件——startDebug 清空残留 URL + 截图 img @error 兜底回退占位
- useDebug.test.ts 新增 4 用例（fake timers 覆盖骨架→退避补全→外部结束清理→停止取消）

### Release 流水线

- 新增 `.github/workflows/release.yml`：推送 `v*` 标签触发，四产物直传 GitHub Release——Windows x64、macOS arm64、macOS x64（Apple Silicon 主机交叉编译，规避 Intel 云主机退役）、Linux x64（ubuntu-22.04 旧 glibc 兼容）；各产物附 SHA256 校验文件（更新器校验的前置数据）
- 全部第三方 action 沿用 ci.yml 已锁 SHA，发布走预装 gh CLI 不新增依赖；tag 与 Cargo.toml 版本一致性校验；产物口径与 build.ps1 一致（主程序 + helper + resources + python_worker，额外排除 `__pycache__`）

### 验证

- vitest 45/45（+5 新用例）；cargo build / clippy --all-targets 零警告；真实 UI 实测：调试面板 5 步渲染、单步执行状态 success/current/pending 正确；仪表盘钉底不再出现误报计数、回底即清零

## 开发中（2026-08-26 第十三轮：打包验证 + 端到端冒烟）

> 针对第十一~十二轮约 8000 行改动的发布前验证。

### 打包链路（478d1b9）

- **build.ps1 修复 UTF-8 BOM 缺失**：Windows PowerShell 5.1 按 ANSI 解析无 BOM 的 UTF-8 脚本，中文注释乱码并触发语法解析错误，完整打包流程在默认环境下不可用。补 BOM 后 release 构建（2m06s）+ 便携包组装（14.5 MB）+ 新产物启动/`--status`/`--stop` 冒烟全通过
- AGENTS.md 模块清单补 logging.rs 与路由按域拆分说明；review 挂账核实修订：M1 config/tasks 大域已在早前轮次完成、「Pinia store 收敛」不适用（前端从未引入 Pinia）、utoipa 缓办

### 端到端冒烟（a640afa）

以真实二进制 + 本地登录测试页走通 API 全链路（axum → executor → bridge → Python Worker → Playwright）：

- **浏览器任务执行**：创建任务 → 填表/点击/断言四步全部成功
- **抓到真 bug 并修复**：assert_text 的 `wait_for_function("() => ...includes(arg)")` 箭头函数未声明形参，Playwright 抛 `ReferenceError: arg is not defined`——真实页面断言步骤全部失败（单测的 mock 未暴露该语义）。改为 `arg => ...` 并补回归测试
- **自动登录全链路**：Profile 配置后 `/api/login` 走完状态机 + 浏览器登录 + 登录后网络验证，返回「登录成功」（12.3s）
- **B3 互斥实测**：调试会话存活期内 `/api/login` 即时失败（0.14ms，互斥生效）；debug_stop 后登录恢复正常

### 验证

- pytest 83 全过（+assert_text 回归）；clippy/test 双 feature 全过；前端构建通过；release 产物冒烟通过

## 开发中（2026-08-26 第十二轮：C 组收尾 — A-4/A-5/B3 根治 + 小尾巴五件）

> `docs/review-2026-08-24.md` C 组计划四个批次全部落地（`af249e1` / `11c2233` / `207d9cd` / `5505faa`）。

### A-4 孤儿清理降频 + 防护（af249e1）

- 决策：Toolhelp32 替换 PowerShell 的原设想**否决**——现清理依赖 CommandLine 匹配区分 Playwright Chromium 与用户自装 Chrome，快照拿不到命令行，纯替换有误杀风险
- 落地：清理从「每次 Worker spawn/退出都跑」降频为「Supervisor 首次 spawn 前 + 崩溃路径」；PowerShell 枚举统一包 5s 超时防 CIM 卡死拖住 spawn/恢复流程

### 批次六：小尾巴五件（11c2233）

- `PerProbeDetail::new` 构造器收敛 probes.rs 内联构造 ×8
- scripts 目录扫描单次读盘（原 read_type + read_summary 各完整解析一次）
- 删除无调用方的 `SchedulerApi::history_dir` trait 方法
- `parse_host_port` 拒绝裸 IPv6 无端口输入（原 `"::1"` 被误拆为 `(":" , 1)` 还能通过校验）
- 前端新增 IconApp 组件收敛高频重复 SVG 图标（42 处 ×14 文件）；壁纸下载弹窗迁公共 Modal 并清理手搓遮罩样式

### A-5 system.rs 按域拆分（207d9cd）

- 背景图域迁 `routes/background.rs`、卸载域迁 `routes/uninstall.rs`（1103→627 行）
- 最后一个 container 旁路关闭：`task_writing_guide` 改 `State<Arc<dyn ConfigApi>>`，全路由层 `state.container` 触达归零

### B3 根治：调试会话存活期纳入槽位（5505faa）

- `debug_start` 成功后会话槽位保持 Debug 态直至 stop/失败/进程退出：命令间隙自动登录不再能插入与调试共用 `_page`（此前 Rust 槽位仅覆盖命令在途窗口）；空闲计时器存活期不启动，调试静置不再被回收
- 逃逸路径保留：登录抢占超预算后 force_recycle 兜底可打断僵死调试会话；Worker 崩溃/优雅关闭同步清除断开标记
- 实现：转发 task 在结果已知后经 `execute_debug_settle` 纯函数结算开合，守卫按开合语义分流（`debug_guard_cleanup`），均可无浏览器直测（+7 测试）

### 验证

- clippy `-D warnings` 双 feature 零警告；cargo test 479 lib + 14 集成全过；pytest 82 全过；前端构建通过

## 开发中（2026-08-26 第十一轮：全库审查修复 — R/F/G/A 四组 + B 组二轮增补）

> 来源：`docs/review-2026-08-24.md` 执行计划（7 路并行模块审查 → 逐条核实，47 条确认 40 / 部分成立 6 / 不成立 1）
> 加上二轮三域深挖的 6 条缺陷级新发现（B1-B6）。按域分波次施工：波次 1（Web 层 / 调度任务域 / 前端）、
> 波次 2（引擎配置监测 / Bridge 登录 Worker / 更新器环境）、波次 3（logging 抽取 / LoginSession trait 化 / WorkerCore 拆分）、
> 波次 4（实例生命周期集成测试）。共 10 个域级 commit（`04ae875`..`b7f1678`）。

### 安全（R 组 + G13/G25/G26）

- **R1 SSRF**：V6 分支先 `to_ipv4_mapped()` 解包按 V4 规则判定（堵 `::ffff:127.0.0.1`、`::ffff:169.254.169.254` 等全部映射形式）；V4 补 CGNAT `100.64.0.0/10` 与 `198.18.0.0/15`
- **R2** API token 改手写 XOR 累积式常量时间比较；**R5** repo 代理响应体流式累积 8 MiB 上限；**G16** PATCH config 中 Profile 加载失败显式 400（不再静默丢凭证）；**B4** PUT /api/config 复用扁平映射（原嵌套反序列化 + serde default 会把整份配置清成默认值）；**G14** WS epoch 改 AtomicU64 fetch_add 原子取号消除并发接入竞态；ApiError 六个零构造变体删除
- **R3** MinGit 下载补 sha256 校验（对照 uv 既有流程）；**G11** 更新资产平台键加架构区分；**G12** .sha256 缺失重试一次；**G13** 更新助手复核 staging SHA256 / 路径归属 base_path / 新版存活 5s 后才删 .bak；**G25** ProfileSnapshot 手动 Debug 输出 `[REDACTED]`；**G26** 换钥分支补告警并删除死字段 password_reinput_needed

### 并发与状态一致性（F 组）

- **F1** EnvironmentManager 引导经 BootstrapGate 串行化（双检 + 失败结果复用），并发 bootstrap 不再踩踏 .venv；**F2** Bridge 超时宽限循环校验 stuck_request_id 归属，不再误杀新会话；**F3** idle 回收让位在途请求（OCR 轻量旁路不再被回收打断）；**F9** 后台待应用更新与手动更新统一 update_in_progress 标记
- **F4** Start/Resume/ApplyProfile 立即检测纳入暂停门控；**F5** 引擎探测后台化（mpsc 回传，Shutdown/Stop 不再被探测阻塞，在途合并不积压）；**F6** 抢占等待旧会话完全收尾（预算 + force_recycle 兜底）；**F7** settings 读-改-写持锁（modify_settings）；**F8** tmp 清理按 `.tmp_` 前缀覆盖 profiles_dir；**F10** 解密失败标志按 profile 作用域 + can_decrypt 纯查询；**F11** uv copy 回退原子化 + 就绪实测启动校验；**F12** 调度器睡眠 ≤60s 分片墙钟重估 + 5 分钟外部删除兜底扫描

### 缺陷修复（G 组 + B 组）

- **B1** 任务/脚本两处拖拽排序互传残缺载荷互相清空对方顺序 → 互传全量；**B2** 步骤 `frame`（iframe）字段全链路断链接通（context.frame 从未被赋值）；**B5** 步骤 required 默认对齐 true（省略该字段的登录步骤此前被当可选静默吞掉假成功）+ 手写 fixture 跨语言契约测试锁 Rust↔Python schema 漂移；**B6** Worker 命令超时改 Rust 的 0.9 倍让轻量自愈先于强杀生效；**B3** 调试会话进行中拒绝登录/浏览器任务插入
- **G1+同类四处** input 降级 wait_for、ocr 截图、screenshot、wait_url 裸 Playwright 调用包 `_safe_op` 分类（瞬时失败不再升格 UNKNOWN_ERROR 不可重试终态）；should_force_recycle 的 UnknownError 不可达语义在注释/测试中改齐
- 其余：G3 last_check 不被登录结果污染、G4 迁移先校验后提交、G5 删除活跃 Profile 回落 default、G6/G7/G8/G9/G10 任务类型与加载校验系列、G15 轻量模式哨兵端口 0、G18 超长响应行结算在途请求、G23 快照新增 probe_total/login_total 真实计数、G24 备份目录冲突改名重试

### 重构（A 组 + B 组重构项）

- **A-1** 日志子系统抽出 `src/logging.rs`（launcher 减重 343 行）：广播层由「fmt 格式化文本→正则反解析」改为真实 Layer 直接从 metadata 构造 LogEntry，消除时间戳伪造与非标准行降级 INFO
- **A-2** LoginSession 依赖 BridgeApi trait（扩孔 execute_with_timeout/force_recycle/has_live_worker）+ SessionParams/SessionDeps 拆分（18 参数收敛为 7），新增脚本化 mock 驱动的状态机单测（重试耗尽 / UnknownError 终态语义回归）
- **WorkerCore 拆分**：`ocr_runtime.py` 归拢 ddddocr 实例缓存/图片预处理，`debug_session.py` 抽出调试会话纯状态机（无 Playwright 直测）
- 其余：TaskKind 访问器收敛 6 文件 match 样板、cron_loop 三臂 timeout 塌缩、job_history 内聚 SchedulerApi、TrayDeps 镜像收敛、auth_url 三份解析器单点化、设置扁平响应三连去重、前端任务/脚本单次拉取（useTaskDirectory）+ 循环依赖消解 + 横切样板收敛（guards.ts）+ 全局 errorHandler、tokio 裁剪 feature/libc 移 unix target

### 附带发现的真实缺陷（测试驱动挖出）

- **Windows is_process_alive 误判**：父进程持有未关闭的子进程句柄时（更新助手、测试框架），已退出进程对象仍可 OpenProcess 成功 → `--stop` 空转超时。补 GetExitCodeProcess 退出码判定
- 启动早期错误（如实例锁冲突）发生在日志初始化前，tracing 无 subscriber 导致静默退出 → 同步落 stderr
- create_task 路由 kind 字段未知值静默回退 browser → 400；死配置开关五件套处置（task_notification/auto_start_browser 接线，auto_update/task_script_timeout/monitor.enabled 删除）

### 验证

- `cargo clippy --all-targets --features no-embed -D warnings` 零警告；cargo test 双 feature 全过（474 lib + 12 集成，含新增 SSRF/常量时间比较/gate 并发/mock 状态机/生命周期等 60+ 测试）；pytest 82 项全过（含跨语言契约测试）；前端 vue-tsc 构建通过
- 审计证据文件 `debug/review-findings.md`、`debug/review-verification.md` 已随本轮落地删除

## 开发中（2026-08-23 第十轮：OCR 链路修复 + 安全加固 + 服务生命周期收口）

> 三个主题分批提交（`c99a64e` / `18c948b` / `3a5a7a9` / `3dcae6e` / `8fcaab9` / `5be21ea` / `4f8188e`）。
> 另清理排查期临时产物（worker_main_diag/probe.py、test_ocr/、volar.tgz）。

### OCR 链路（c99a64e）

- **根因修复：模型加载卡死**——ddddocr 链式加载 numpy C 扩展若发生在 Worker 后台线程（`asyncio.to_thread`），Windows loader lock + import lock 会让加载卡住约 100s，前端报「模型加载超时」。已实测主线程加载仅 ~0.1s 且命中 `sys.modules` 缓存后后台线程不再重新加载 DLL。新增 `_preload_ocr_deps()`：主线程、事件循环启动前 best-effort 预加载（缺依赖/失败均静默跳过，不拖垮 Worker）
- **识别/模型加载超时兜底**：`ocr_recognize` 的模型构造与 classification 均 `asyncio.wait_for(…, OCR_TIMEOUT_SECS)` 丢线程池执行，超时转一句话错误（含「uv add ddddocr」指引）；RGBA 截图先规整为 RGB 提升识别率
- **OCR 不再被 Chromium 阻断**：Bridge `ensure_worker` 增加 worker-only 健康检查分支——`ocr_recognize` 只验证 Worker IPC，浏览器任务才探测 Playwright/Chromium
- **Web 层信封修复**：`/api/ocr/recognize` 此前把 Worker 的 `IpcResponse { id, result }` 原样返回，前端契约只认 `{data}`/`{error}`——错误被埋在 200 响应里。现提取 `result.data` 或转 HTTP 错误；新增 15 MiB 请求体上限（对齐 Worker stdin 16 MiB 单行上限）
- **OCR 可用性权威判定**：`ocr_declared` 解析 `python_worker/pyproject.toml` 依赖块，前端据此展示安装/卸载入口（`declared` 字段）；任务设置页进入即检测、安装后轮询至就绪（最长 5 分钟）、检测失败可重试
- Worker IPC 加固：stdin 单行超限时从有界前缀提取请求 id 返回明确错误（此前静默丢弃）；stderr loguru 行解析去重前缀后按级别映射 tracing target `python_worker`
- venv 损坏自愈：解释器存在不代表可用，`python_executable_works` 实际启动 `--version` 验证，失败自动 `uv sync` 修复

### 安全加固（18c948b）

- **Profile ID 路径穿越封堵**：`is_valid_profile_id` 仅接受 1..=64 个 ASCII 字母/数字/`_`/`-`，get/update/delete/create 及 active_id 读取/重载全路径校验（非法 active_id 回退 default 并告警）
- **背景图验证**：按真实文件签名（magic bytes）判定位图格式并拒绝 SVG（同源脚本执行风险），统一 10 MiB 上限；multipart 请求体限制 = 上限 + 64 KiB 边界预留
- **下载/解压限流**：`download_streaming` 按 content-length 预检 + 流式累计超限即删档报错（uv/git 环境包 256/512 MiB）；zip 解压加条目数（8192）/单条目（512 MiB）/总量（1 GiB）三重上限
- `/api/auth/token` 响应补 `Cache-Control: no-store`；`ConfigError::InvalidProfileId` 映射 400

### 服务生命周期（3a5a7a9）

- **`ServiceHandle::stop_with_timeout`**：持有 JoinHandle 的限时停止——超时 abort 并 await 回收，不会丢弃句柄让后台 task 游离；关闭序列按 bridge(3s) → scheduler(5s) → engine(8s) 分级超时
- **Scheduler 任务收口**：`TaskTracker` + `CancellationToken` 追踪所有定时/手动执行，关闭时统一取消并等待清理；执行等待并发 permit 与取消 select 竞争
- **任务子进程隔离**：脚本执行 `env_clear`（不继承主进程 token/代理密码/调试变量）+ Windows Job Object（KILL_ON_JOB_CLOSE）——超时/关闭/取消时内核回收整棵进程树；stdout/stderr 持续排空并截断
- 定时任务缺省超时 clamp 到 1..=3600s；登录活跃任务判定统一走 TaskManager 的全局 `.order.json.active`（手动/自动/CLI 登录共用，定时任务独立 task_id 不受影响）

### 前端稳定性（3dcae6e / 4f8188e）

- **日志不丢不倒退**：HTTP 历史替换保留请求期间到达的实时日志（seq 基准 + 内容键去重回放）；历史接口失败仍开实时流不停留在空白；级别筛选交前端（后端保留全部级别）
- **WebSocket 防重入**：`connecting` 标志消除并发 connect 各自新建连接导致的「连接风暴」；构造同步抛错降级常规重连不永久卡死
- HTTP 客户端支持 timeout/signal 组合（AbortController 桥接，超时与取消区分报错）；FormData 不手工设 Content-Type
- 退出序列：仅后端确认收到关闭请求后才清定时器/销毁 WS/显示遮罩，失败保留可恢复会话
- 前端回流日志（ws.rs）scope/message/meta 按 128/4096/2048 字符截断，防异常堆栈撑大日志文件

### CI / 杂项（8fcaab9 / 5be21ea）

- CI 增加 Python 3.12 + uv（含 `uv.lock` 缓存）+ `uv run pytest`
- gitignore 增补 `/.campus_network_auth/` 运行时目录；移除前端 openapi-typescript 开发依赖

### 验证

- `cargo clippy --all-targets --features no-embed -D warnings` 零警告；`cargo test --features no-embed` 385 项全过；pytest 62 项全过；`npm run build`（vue-tsc）通过

## 开发中（2026-08-17 第九轮：Engine 引用收口 — 可替换句柄 + 崩溃恢复监测）

> todo 7.3 中期方案落地。修复两个问题：① Engine 崩溃自愈后以 monitoring=false
> 空转，监测静默失效；② Web/托盘/关闭流程持有启动时的初始 Engine 引用，
> 崩溃重启后向已死通道发命令、开关失效。

### 落地内容

- **EngineSlot**（新增 `src/engine/slot.rs`）：`Arc<ArcSwapOption<EngineHandle>>` 无锁可替换句柄槽。`replace`（重启后原子换入）/ `current_engine` / `current_handle` / `clear`（重启耗尽）/ `dispatch` / `try_dispatch`（无活跃 Engine → ChannelClosed）
- **container.engine_handle → container.engine: EngineSlot**：唯一权威入口，Web monitor 路由、托盘（TrayDeps.engine）、`apply_startup_action`、`graceful_shutdown` 全部经 slot 取「当前活跃」Engine；删除 LauncherState 的 `latest_engine_cmd_tx`（被 slot 取代）
- **崩溃恢复状态重放**：watch_engine 重启前捕获 `engine_state == Running`，新 Engine 换入后按原状态重发 `Start`，消除「崩溃自愈后监测静默失效」
- **附带缺陷修复：panic 检测失效**：原 `completed.notify_one()` 位于 run_loop `.await` 之后，panic 展开会跳过它——初始 Engine panic 从未被检测到；且 Notify 单 permit 在 watch_engine 与 graceful_shutdown 并发等待时会丢失唤醒。改为 `CancellationToken` + `CompletionGuard`（Drop 触发，unwind 中仍执行），panic 与正常退出均触发，任意数量等待者全体唤醒
- panic/正常退出不再区分：Engine 正常退出唯一路径是收到 Shutdown（仅在应用关闭令牌取消后发送，biased select 先命中 cancelled），token 未取消时的任何退出均按崩溃处理；`EngineHandle::into_result` 与 `Engine::cmd_sender` 删除

### 验证

- `cargo clippy --all-targets -D warnings` 零警告；`cargo test` 双 feature 全过（+5：slot 4 项 + CompletionGuard panic/正常退出语义 1 项）；`build.ps1` 完整构建通过

## 开发中（2026-08-17 第八轮：M1 上帝容器渐进 trait 化 — 细粒度 state 试点两域）

> 延续第七轮 P3 挂起项。模式：领域 trait + `AppState` 直字段 + `FromRef` 委派提取，
> handler 声明 `State<Arc<dyn Trait>>` 细粒度依赖，测试注入内存实现构造 mini Router
> 做 handler 级单测——无需装配完整 ServiceContainer（此前项目零 handler 测试）。

### M1 落地（两域试点 + status 直字段）

- **HistoryStore trait**（`login/history.rs`）：`query`/`clear` 两方法；history 路由改 `State<Arc<dyn HistoryStore>>`；新增 3 个 mock handler 测试（limit 截断保留最新、分页 total 语义、clear 恰好调用一次）
- **LoginApi trait**（`login/mod.rs`）：`submit`/`cancel_current`；login 路由改细粒度提取；新增公开构造器 `LoginHandle::immediate`（立即终态句柄，`immediate_handle` 内部逻辑复用，亦供测试 mock 构造）；新增 5 个 mock handler 测试（source 缺省映射、失败 200 语义、取消调用、状态快照序列化、once 语义）
- **status 直字段**：AppState 直接持有 `Arc<StatusManager>`（内存实现，免 trait），login/monitor 路由与 ws.rs 改 `state.status`，消除 3 处 container 触达
- AppState 细粒度字段：`history: Arc<dyn HistoryStore>`、`login: Arc<dyn LoginApi>`、`status: Arc<StatusManager>` + 三个 `FromRef` 委派实现

### M1 挂起（下一批）

- scheduler 域：`spawn_manual_run` 为 `&Arc<Self>` 接收者（spawn 需要所有权 Arc 传入 `execute_scheduled_task`），trait 化需先重构服务内部结构（Weak 自引用或依赖拆分）
- config/tasks 等大域（45/19 处访问）与 Pinia 前端 store 收敛

### 验证

- `cargo clippy --all-targets -D warnings` 零警告；`cargo test` 双 feature **331 项全过**（+8 个 handler mock 测试）

## 开发中（2026-08-17 第七轮：全面审计修复 — A1-A14 紧急项 + P2 性能包 + P3 架构批次）

> 来源：2026-08-17 全面审计（紧急 14 项 / 性能 17 项 / 模块重构 M1-M8 / 架构演进）。按路线图 P0→P1→P2→P3 分批落地。

### A 组：紧急修复（A1-A14，全部完成）

- **A1 select_ok panic**：TcpProbe 残留 future 不再 `unwrap_err`，按 detail 自带 success 字段收集
- **A2 auto_login_in_flight 卡死**：去重复用会话时按会话 ID 归一重置，自动登录不再永久失效
- **A3 孤儿浏览器清理失效**：PowerShell 输出改 `ConvertTo-Json` + serde 解析（剥引号双保险），taskkill 补 `/F /T`，失败升级 warn
- **A4 本地 RCE / 零鉴权**：新增 `src/web/auth.rs` token 中间件——启动生成随机 token 持久化 `config/.auth_token`，所有 `/api/*`、`/ws/*` 强制校验（`X-Auth-Token` / `Bearer` / `?token=`），豁免 `/api/auth/token`（CORS 读保护）、`/api/health`、OPTIONS；前端 `ensureAuthToken` 懒取 + 401 重试一次；强制 `Content-Type: application/json` 简单请求拦截
- **A5 uninstall.bat 注入**：base_path 元字符校验 + 拒绝非法路径，卸载接口纳入 token 鉴权
- **A6 SSRF TOCTOU**：新增 `src/web/ssrf.rs` 单一私网判定（IPv4/IPv6 全段）+ `secure_get`（DNS pin + 逐跳重校验重定向），repo/壁纸下载统一走 secure_get
- **A7 截图残留**：登录/浏览器任务截图全退出路径清理 + Worker 启动清空上次残留
- **A8 请求悬挂**：kill/shutdown 路径统一 drain pending（复用 handle_worker_exited 的 drain helper）
- **A9 配置隔离态污染**：reload 解析失败保留旧 runtime，默认值仅首次初始化
- **A10 useConfirm 并发抢占**：resolver 以 `null` 结算（区别于取消的 `false`），调用方将抢占与取消分开处理
- **A11 重复提交**：任务/脚本/定时任务执行按钮 per-id busy 守卫 + 后端 cron_loop per-task 串行
- **A12 ws_kicked 半死态**：横幅加「在此页恢复」按钮（resumeFromKicked）
- **A13 探测误判**：任一目标 Captive 即整体 Captive（劫持信号优先于连通）
- **A14 静默吞错**：loader spawn_blocking panic 改 error! + 上抛（前端见加载失败而非空列表）

### P2 性能包（后端 8 项 + 前端 8 项）

- 后端：exec_lock 按 task_id 分锁、/api/logs 尾部 512KB 读取、updater 进度 500ms 节流、truncate 按 chars 截断、网卡探测 TTL 缓存等
- 前端：日志 seq 单调键 + 去重、列表 5s lastFetchAt 守卫、status 按 uptime_seconds 新鲜度应用、显式字段映射（删索引签名）、步骤进度 running 态等
- WS 单连接限制：`ws_epoch_tx` 世代号，新页面顶替旧页面（`ws_kicked`），HTTP 多页并存

### P3 架构批次（本批完成项）

- **M2 ConfigService 锁模型**：`save_mutex` 拆分为 `settings_lock` / `profiles_lock` 双域锁（改 Profile 不再阻塞 settings 保存）；`reload_inner` 去写锁——依据「随机名 tmp + rename 原子替换」语义，(settings, active_profile) 按 settings 自带 active_profile_id 配对天然一致；新增双域并发写回归测试
- **M3 Windows Job Object 进程树治理**：新增 `src/bridge/job.rs`——spawn 后立即将 Worker 加入 `KILL_ON_JOB_CLOSE` Job，chromium 树自动继承；Worker 强杀 / 正常回收 / 主进程被强杀（句柄随进程终止关闭）时内核自动终止整棵树；失败降级告警回退应用层清理；真实进程语义测试验证句柄关闭后内核终止成员
- **M4 前端生命周期 footgun**：`onWsReconnect` 返回注销函数、`setupVisibilityChange` 幂等防 listener 泄漏
- **契约校验（M5）**：`/api/*` 路由收敛为 `route_table()` 单一声明源 + 与 openapi.json 双向 diff 契约测试
- **M7 调度器**：per-task 防重叠（running_ids + RunningGuard）、invalid_cron_ids 可见性（前端「表达式无效」标记）、task_change 通道关闭后 60s 降级轮询
- **M6 IPC 短期加固**：Worker stdout 强制 UTF-8 失败即拒启（SystemExit 3）；Rust 侧非 JSON 行计数 + 退出汇总告警
- **M8 资源与组件**：前端图标单一来源（删 resources/icons 重复 SVG）、CSS 按组件/页面拆分、`FieldHelp.vue` / `Modal.vue`（closeOnOverlay）共享组件替代 20+ 处内联模板
- **M3 前端拆分**：useRepoImport / useBackgroundImage / useCustomColors 独立 composable，pureMode 迁入 useConfig

### P3 挂起项（下一批）

- M1 ServiceContainer/AppState trait 化（LoginApi/ConfigStore 等 + 内存 mock，40+ handler 渐进改造）
- Pinia / 显式 store 收敛（17 个模块级单例 composable）

### 验证

- `cargo clippy --all-targets -D warnings` 零警告；`cargo test` 双 feature **324 项全过**（含 Job Object 真实进程语义测试、双域锁并发写测试）；pytest **56 项全过**；`npm run build`（vue-tsc + vite）零错误；`build.ps1` 完整构建通过

## 开发中（2026-08-16 第六轮：全库审计 — 死代码清理 -1615 行 + 契约修复）

> 三路并行审计（Rust / Python Worker / 前端）产出去重与瘦身清单，机械项已执行完毕；设计型修复与剩余重构见 `docs/optimization-plan.md`。

### 死代码清理（净 -1615 行，三批提交）

- **前端（-1403 行）**：tasks.css 清除约 87% 死规则（旧 repo 弹窗全局样式、已被 ConfirmDialog 取代的 danger 确认框、已被 JSON textarea 取代的可视化 step editor、旧版 DebugPanel 样式及底部重复定义）；settings.css 清除旧 OCR 区块/浏览器卡片等死规则；useUi 删除 8 个只写不读的 state 与 6 个死函数（AboutView/BrowserSettings 均用本地 ref）；useConfig 删除 OCR/stealth/reset 死层（视图直连 API）并改用 `structuredClone`；formatters 删除 5 个无引用导出；types 删除 4 个死类型；useAppearance/useTasks/useToast/api 层零散死导出清理
- **Python Worker（-122 行）**：models.py 删除 12 个协议死字段（Rust 侧从不发送：`button/modifiers/option_value/...`、`method/headers/body/...`）；删除未接入的 `_task_watchdog_timeout_ms`、`_safe_op` 死参数、`asyncio_sleep` 包装；`_close_browser`/`close_browser` 合并；测试改用 pytest 内置 capsys。清理中发现并修复 `_to_ms` 缺省值直通边界（缺省值原样返回、配置值 ×1000）
- **Rust（-89 行）**：移除 `bytes` 依赖（唯一用点改 `Default::default()`）；删除无消费点的 `schema::RuntimeMode`（字段改 String，JSON 不变）；删除 `EgressBinder` 预留接口与 `bind_interface_name` 死配置及 TcpProbe 永不触发的 bind 分支（上轮按"预留"保留，本轮按 YAGNI 移除）；手写 OpenProcess/TerminateProcess/kill FFI 收敛到 windows-sys/libc，`is_process_alive` 三平台实现合一；scheduler 锁中毒改用项目惯例恢复而非静默跳过；孤儿清理 JoinError 补 warn 日志；修复上轮遗留的 `bridge_ipc` 测试编译损坏（spawn_worker 加参未同步测试）

### 契约修复

- **monitor「物理网络连接检查」无效开关**：前端绑定的 `enable_local_check` 后端从无此字段（历史迁移已改名 `url_enabled` 且另有绑定），保存时被静默丢弃，删除该安慰剂开关
- **detect 接口补 `matched_profile_name`**：ProfilesView 匹配横幅此前永远显示 profile id（`matched_profile_name` 从未由后端返回），后端按 id 查 `ProfileData.name` 下发，前端类型同步

### 验证

- `cargo test` **312 项全过**；`uv run pytest -q` **47 项全过**；`npm run build`（vue-tsc + vite）零错误

## 开发中（2026-08-15 第五轮：todo 批次五~八 — 审查修复收尾）

> 对应 `docs/todo.md` 批次五（Bridge/更新器/环境/Python Worker）、批次六（前端契约）、批次七（Rust 清理 + Web 杂项 + 后端杂项）、批次八（验证）。

### 批次五：Bridge / 更新器 / 环境 / Python Worker

- **5.1 OCR 并发摧毁登录会话槽位（P1-6）**：`execute_inner` 对 `ocr_recognize` 走旁路——仍注册 pending 与 cancel 注册表，但不触碰 `current_session`/`current_cancel_id`/`current_request_id`/空闲计时器/`worker_state`；守卫改用轻量清理回调（只做 `pending.remove` + `cancel_registry.remove`，不复用 `reset_session` 匹配逻辑）。新增单测验证槽位不被 OCR 破坏
- **5.2 Bridge 调用方超时不清理会话槽位（P1-7）**：`execute_with_timeout` 自生成 cancel_id 并注入 `params["cancel_id"]`；超时分支发送 `SupervisorCommand::Cancel`；supervisor 的 Cancel 处理分支在发 IPC 的同时 `cancel_registry.trigger(cancel_id)`，本地 token 立即唤醒转发 task → 释放槽位。新增集成测试 `supervisor_超时_释放会话槽位`
- **5.3 更新主流程与 helper 交接断裂（P1-10）**：helper 等待主进程退出的超时后不再强制继续，改为报错退出并保留 staging 与 pending.json，把应用机会留给主进程下次启动 `apply_pending_on_startup`；`cleanup()` 使用 CLI `--staging` 传入的实际路径而非硬编码路径
- **5.4 uv 就绪判定与实际使用不一致（P1-11）**：新增 `uv_exe_path()` helper（本地存在返回本地路径，否则返回 `uv` 走 PATH 解析），`run_uv_sync` / `install_playwright` 统一改用；`UV_MIN_VERSION` 在 PATH 回退分支做最低版本校验。新增两分支单测
- **5.5 debug_stop/debug_run_all session_id 两端不一致（P1-21）**：Python 侧新增 `_debug_session_for()`——session_id 为空且恰有一个活跃会话时回退，多个时报错（与 Rust 单会话语义对齐）；`_close_browser` 与 EOF/shutdown 路径清理全部调试会话截图。新增 4 项 pytest
- **5.6 Worker 正常退出被记为崩溃（P2-4）**：`handle_worker_exited` 开头判 `code == 0` → info 日志 + 置 Idle，跳过 crash 计数 / 孤儿清理 / Error
- **5.7 OCR 模型每次调用重新加载（P2-5）**：模块级缓存 `_ocr_cache`（key = old 参数）+ 统一获取函数，两处调用改走缓存；`classification()` 包 `asyncio.to_thread` 避免阻塞事件循环。新增 pytest 断言同实例复用
- **5.8 环境模块 P2 三项**：E2 下载补 `tokio::time::timeout`；E6 Playwright 就绪检查改校验非空 + 读 `PLAYWRIGHT_BROWSERS_PATH`；E7 Unix 孤儿清理 `parse_ppid_from_stat` 改 `continue` 不中断全部
- **5.9 更新器 P2 四项**：U2 GitHub 源从 release assets 找 `.sha256` 伴随文件（找不到明示降级）；U4 `spawn_helper` 补 `CREATE_NO_WINDOW`；U6 循环外读一次 `check_on_startup` 决定"启动即查"、循环内只做周期检查、接入/删除 `update_channel`；U3 `apply_pending_on_startup` 应用前重算哈希 + `pending.version <= 当前版本` 则跳过清理

### 批次六：前端契约修复

- **6.1 新建配置方案必 404（P1-15）**：`profilesApi` 加 `create`（POST）；`saveProfile` 按 `_isNew` 分流——新建走 create、更新走 save，返回 `Promise<boolean>`
- **6.2 定时任务执行历史契约错位（P1-16）**：历史弹窗改用 `run_at`/`success`/`message`/`duration`，成功判定 `record.success`，时间 `run_at.replace('T',' ').substring(0,19)`；同步修类型定义
- **6.3 自定义运营商输入框敲首个字符即消失（P1-17）**：ProfilesView / AccountSettings 加独立 `showCustomCarrier` 状态 + watch；保存前 `isp==='自定义'` 且输入为空则 toast 拒绝
- **6.4 浏览器 channel 命名不一致（P1-18）**：前端统一由 `"playwright"` 改判 `"chromium"`（selectedBrowser 初始值、handleBrowserClick、BrowserSettings 两处），Chromium 自动下载不再永不触发
- **6.5 status_detail 幽灵字段（P1-19）**：删除 status_detail；`networkStatusText` 改按后端推送的 `network_state` 映射（已停止/在线监测中/检测到门户劫持/网络断开/暂停时段/正在启动监控）
- **6.6 前端死代码清理**：删除 `TaskEditor.vue`/`StepEditor.vue`/`BrowserSelector.vue`/`LoadingSpinner.vue`/`useMutation.ts`；清理 useUi/useConfig/formatters/file/constants 中零引用符号；移除 init 的 `Promise.allSettled` 失败统计
- **6.7 调试面板不可达**：`TasksView` 内联任务编辑器工具栏加"调试"按钮 → `useDebug.startDebug(taskId)` 接线
- **6.8 axios 遗留错误形状**：`useStatus.fetchAutostart`/`useConfig.toggleAutostart`/`useUi.checkInitStatus`/`useAppearance` 改 `instanceof ApiError` 读 `.status`，兜底分支不再失效
- **6.9 交互小修合集**：`useConfirm` 并发时先 resolve 旧 Promise；Dashboard `clearLoginHistory` 改调 useUi 带确认版；ProfilesView 保存失败保持编辑器打开；任务/脚本列表按 `task_type` 过滤；ScriptsView 拖拽排序持久化；SystemSettings 日志标签改正 + 日志级别改 `PUT /api/config/log-level` 热更新；AboutView `health.python_version` 改由 `/api/init-status` 推导
- **5.3 前端联动**：`AboutView.applyUpdate` 成功后弹确认"立即重启"→ 确定调 `systemApi.shutdown()` 优雅关闭

### 批次七：Rust 死代码清理 + Web 杂项

- **7.1 死代码删除**：`launcher._restart`、`EngineHandle::stop/into_completion/task_handle` + stop watch 整条路径、`EngineError::{ReloadFailed,ProfileNotFound,RestartExhausted}`、`EngineDeps.base_path`、`LoginError` 枚举、`rebuild_client`、`NetworkError::{Io,Socks5PortBusy,Socks5Crashed}`、`GatewayInfo`、`sort_interfaces`、`SchedulerStatus` + `status()`、`SchedulerError::{SubmitRejected,ExecutorError}`、`web/state` 的 axum_running 一系、托盘 orchestrator 字段、`cached_manifest`、`PlatformPackage.sig_url`、`ENC_KEY_FILE`、`ConfigError::KeyFileCorrupt`、`InstanceLock::release`、`Metrics::default`、`_worker_state_for_capability`、`run_uv_command`、`SessionGuard` 四方法 + `cancelled` 字段、`TaskError::{ExecutionCancelled,QueueFull}`、`_helper_path`、`WsMessage::Screenshot/StepProgress`。`EgressBinder` 与 `bind_interface_name` 按"预留"保留
- **7.2 Web API 杂项**：静态回退对 `/api` 前缀直接 404 JSON；`fetch_logs` limit `.min(2000)` 钳制；history total 在截断前取值；`error.rs` NotFound code 按资源区分 + 序列化失败映射 500；`execute_task` 恢复 BridgeError 类型化 + 409 WorkerBusy 映射；`create_job` 重复 id 返回 409 + JobCreateBody 补 description/timeout；手动触发与 cron 走同一并发闸；`start_monitor`/`stop_monitor` 错误如实返回；`patch_settings` 对 `carrier_custom` 显式丢弃；`import_tasks` 返回 `{imported, failed}`
- **7.3 其他后端杂项**：uninstall 脚本先 copy 到 `%TEMP%` 再执行自删 + 修正响应消息路径；`executor` 超时用 `taskkill /T /F /PID` 递归杀进程树；`network/detect` 新增 30s TTL 缓存（`CachingDetector`）；`utils/io` `atomic_write_json` 对齐 fsync 保证；`updater/download` 进度改防回绕；Engine 崩溃重启恢复项标注待评估

### 批次八：验证

- `cargo test` 默认 + no-embed 双 feature **311 项全过**（含 5.1/5.2/5.4 新增单测与 `supervisor_超时_释放会话槽位` 集成测试）
- `cargo clippy --all-targets -- -D warnings` **零警告**
- `frontend npm run build`（vue-tsc + vite）通过
- `python_worker` pytest **48 项全过**（原 41 + 新增 7）

### 冒烟验证发现并修复（2026-08-15）

- **浏览器数据持久化对齐 Python 原版**：持久化逻辑此前已存在（`persistent_context` 开关 + `launch_persistent_context`），但存在两处缺口——①持久化目录锚定在 Worker 脚本目录（`_WORKER_DIR/browser_data`），便携包更新/重建 Worker 时登录态会被清空；②前端无开关入口。修复：Rust `spawn_worker` 注入 `CAMPUS_AUTH_BASE_PATH` 环境变量，Worker 新增 `_browser_data_dir()` 锚定到 `<base_path>/config/browser-data/<channel>`（与 Python 原版 `config/browser-data` 对齐，缺失时回退脚本目录）；前端 `BrowserSettings` 在"浏览器常驻"区新增"保留浏览器数据"开关 + 数据目录提示（排除 firefox，对齐原版）。端到端验证 persistent chromium 启动后目录含完整用户数据（Default/Cookies/Cache 等）
- **Worker 健康检查在 asyncio 事件循环内误判浏览器不可用（P1 级启动阻断）**：`handle_browser_health_check` 是 async 函数，内部调用 `_ensure_browser`（其用 `sync_playwright`）。Playwright Sync API 在运行中的 asyncio 事件循环内调用会抛 `"Playwright Sync API inside the asyncio loop"`，被 `_ensure_browser` 的 `except Exception: pass` 吞掉后返回 `healthy=false` → Worker 首次 spawn 的健康检查永远失败 → 所有依赖 Worker 的功能（debug/登录/OCR）启动即超时。修复：`healthy = await asyncio.to_thread(_ensure_browser, channel)`，把同步检查丢到线程池（与 OCR `classification` 的处理一致）。修复后独立 base_path 全流程（环境引导 → Worker spawn → debug 会话 → OCR 并发 → debug 停止）验证通过
- 冒烟其余项通过：Web restart 无双进程互锁（PID 变化、单实例）；定时任务重复 id 返回 409、toggle 禁用/重启用正常；脚本 PUT 保存后重新打开字段完整；更新 helper 等待存活 PID 超时后 exit 1 且 staging/pending 保留；OCR 请求在 debug 会话活跃时被处理且会话不被破坏（对应 5.1 修复）

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
