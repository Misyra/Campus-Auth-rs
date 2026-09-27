# 后续计划（活跃入口）

> R/F/G/A/B 组已在 `docs/changelog.md` 第十一~十六轮落地并归档；历史归档目录 `docs/archive/` 已于 2026-09-17 删除，其中材料不可追溯。
> 当前活跃：`v5.0.2`（`docs/changelog.md`）。P0/P1 与更新子系统待办的权威摘要以本文件与 `docs/known-issues.md` 为准（原 defect-recheck / updater-audit 过程报告已于 2026-09-12 删除）。
> 验证块：`cargo clippy --all-targets -- -D warnings` / `cargo test` 双 feature（含 `rust-tests-unix`）/ `cargo fmt --check` / `uv run pytest` / `npm run build` + `vitest` / `e2e-login-chain` 全链路。

## 登录新增第三种渠道：自定义脚本（2026-09-24 **已落地**）

> 用户诉求：「给登录任务增加一种渠道，就是自定义脚本，允许使用自定义脚本进行登录」。实现与验证见 `docs/changelog.md` 同日条目，脚本契约见 `docs/guides/custom-script-guide.md` 第 2 节。

**已落地**：`LoginChannel::Script`（`script`）+ 方案字段 `active_script_task`；复用任务页「立即运行」的执行路径叠加 `CAMPUS_USERNAME` / `CAMPUS_PASSWORD` / `CAMPUS_ISP` / `CAMPUS_AUTH_URL` 四个环境变量，退出码 `0` 视为本次成功且仍走登录后网络验证（实现口径见 `AGENTS.md`「登录渠道」与脚本契约文档）。

**为什么要这一渠道**：直连渠道的凭据变换脚本跑在无网络、无文件的 JS 沙箱里**发不出第二个请求**——"先取令牌再登录""按门户逻辑加密密码""多步跳转取参"这类门户在直连里写不出来；脚本渠道用自己的解释器与库补上这段空档，同时仍不要求 Python Worker 与 Playwright。

**本轮登记 / 遗留**：

- **登录历史不记渠道字段**（既有项）：权威口径见下方「全功能实测排查与修复」一节的同一条。
- **脚本渠道没有"不重试"的表达**：退出码只有成功/失败两态，凭证无效也会按重试预算重发（与浏览器渠道的验证码失败同类）。加第三语义（如 `exit 2 = 凭证无效`）代价是用户更容易写错，当前用全局重试策略兜底。
- **脚本子进程不保证静默**：与「立即运行」同一条路径，未设 `CREATE_NO_WINDOW`，控制台版解释器可能闪现窗口（既有行为）。

## 全功能实测排查与修复（2026-09-24 **已落地**）

> 用户诉求：「再全面细致排查一遍各项功能，你使用 agent-browser 实际启动试试，确保各个功能没有问题」。手段：`agent-browser`（CDP）真实点完全部页面 + 91 条 API 全量扫 + 集成测试真起二进制；完整清单与证据在 `docs/reports/full-audit/README.md`（本地不提交），逐项修复见 `docs/changelog.md` 同日条目。

**修掉的一个真缺陷（F1，高）**：脚本渠道**在默认配置下完全跑不起来**——方案编辑器把「执行程序 = Python (项目内解释器)」存成**空串**（不是 `null`），而 `build_script_command` 只判 `None`，`Some("")` 被当成真实路径 → `Command::new("")`。修复：新增 `explicit_binary()`（空白串一律按未指定），四个分支统一走它，并使「解析出的程序名为空」提前返回可诊断的中文报错；细节、真机三连验证与「为什么测试全绿却没拦住」的教训见 `docs/changelog.md` 同日条目。

**本轮登记的待办**：

- **登录历史不记渠道**（既有项，三渠道只能从消息文本辨认；权威条目）：建议给 `LoginHistoryEntry` 加 `channel` 字段（`#[serde(default)]` 兼容旧文件）+ 列表渲染渠道徽标。
- **「立即运行」后「最近结果」不刷新**：后端 spawn 后立刻回包（异步），前端随即拉列表拿到的仍是上一次结果，列表与编辑器会显示「尚未执行」/旧时间戳，需手动刷新或重进路由。面板自己写着「成败与耗时见「执行历史」」，危害有限；可选改法是运行后延时补拉一次。
- **「设置 · 任务与环境」的「当前任务」对直连/脚本渠道显示原始任务 ID**：组件里有注释的有意选择，但直连任务 ID 是自动生成的 `untitled_N`，对用户没有信息量；建议改为「任务名（ID）」。
- **系统设置页的部分控件不在可访问性树里**：启动后执行 / 运行模式 / 开机自启动 / 启用文件日志 / 全局日志级别 / 启动时打开控制台 / 任务通知 / 显示系统托盘图标都不带 role，读屏与自动化取不到（功能本身正常）。
- **`TaskError::IoError` 全量人话化**：本轮只堵住「程序名为空」这一条最常踩的路径，其余 IO 失败仍会把 `std::io` 原文抛给用户。

**本轮未能覆盖的验证面**：浏览器渠道的真机链路（`tests/login_chain`）在本机走 skip 分支——它的 `preflight()` 要求 `locate_python()` 命中的解释器能 `import PIL, ddddocr`，而仓库 `python_worker/.venv` 只装了 playwright（PIL/ddddocr 是可选能力，按设计不默认声明）；卸载全流程演练未复跑（需单独 `--target-dir` 编 exe），只验了守卫拒绝路径。

## 更新助手卸载模式（2026-09-24 **已落地**）

> 用户诉求：「能否优化一下更新助手，把卸载功能加进去」。逐项实现与验证见 `docs/changelog.md` 同日条目。

**已落地**：`campus-auth-helper --uninstall` 模式（与 `--apply-update` 互斥）+ `POST /api/uninstall/purge`；删除范围与守卫收在 `src/uninstall/`（**单一事实源**：界面据此列清单、助手据此执行）；前端卸载弹窗支持「保留配置与任务」勾选并逐项列出将删除的内容。逐项实现见 `docs/changelog.md` 同日条目。

**本轮登记 / 遗留**：

- **真机端到端卸载演练已补上**（2026-09-24，34 项断言全过：全删 / 保留数据 / 守卫拒绝三轮，含"安装目录真的消失""`%TEMP%` 无残留"；脚本 `docs/reports/uninstall-e2e/rehearse.ps1`，本地不提交，可复跑）。**这一步是必要的，不是走过场**：它当场推翻了两个只靠单测看不出的判断——`MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)` 在非管理员账户下必然失败（原先当主路径 = 每次卸载留一个几 MB 的临时副本），以及 cmd 的 `/c` 引号剥离规则（原先的 `""…""` 外层包裹会把整行拆坏，派发成功却什么都不删）。
- **路径含 cmd 特殊字符时（`% & ^ | < > " !`）退回 `MoveFileExW`**：该路径在非管理员账户下会失败，那份临时副本会留在 `%TEMP%`（不可见，系统也会自行清理）。概率极低，且"宁可留一个临时文件也不冒命令被拆坏的风险"是刻意取舍；要覆盖需改用 `CreateProcessW` + 自建管道或临时批处理，成本不成比例。
- **「保留配置与任务」不含 Playwright 浏览器缓存**：勾选后仍会清理浏览器缓存（属系统残留而非用户配置）。想"换目录重装且不重下浏览器"目前需手动保留缓存；若要支持，做法是把「保留」拆成两项（用户数据 / 浏览器缓存）。
- 卸载**没有进度界面**（有意）：助手是无界面进程，中间过程的反馈由系统提示框承担。

## 前端 UI 风格全面统一（2026-09-24 **已落地**）

> 用户诉求：「准备全面统一前端界面 UI 风格，你先探索，拿到结论」。探索结论与审计脚本在 `docs/reports/ui-audit/`（本地不提交，`audit.mjs` 按 Vue 编译器 AST 取模板 class 后与样式表对账，可复跑；当前指标：幽灵类 0、待处理死类 0、断点取值 640/768/900/1100 四档）。

**总判断**：token 层健康，问题在**组件契约层**——同一视觉角色有多套平行实现。八项已落地（卡片头 / chip / note 收敛为全局一套、页面入场动画单一出口、开关开态对齐、死 CSS 清理、断点收敛），逐项实现见 `docs/changelog.md` 同日条目。

**本轮登记的前端待办**：

- **`.page-content` 仍不是真正的"页面外壳"**：入场动画已收成**单条** `.content-wrapper > *`（此前三条结构选择器会让任务页 / 设置页内外两层同时播，见 `docs/changelog.md` 2026-09-24「自动保存共享控制器 + 首次真机目检」一节），但页面容器（flex 方向 / gap / 最大宽度）仍由 `.settings-page` / `.appearance-page` / `.tasks-page` / `.tsk-list-page` / `.ai-task-page` 各自声明。若要继续，方向是让 `.page-content` 提供布局契约、各页面只声明差异——但那会同时改动 5 个页面的间距，需配目检。**2026-09-24 起真机目检已具备条件**（见 `docs/reports/ui-audit/visual-check.py`，Playwright 只读巡检 + 计算样式断言，可复跑）。
- **`UpdateDialog.vue` 是全仓唯一把字号写成字面量的组件文件**（15 处 `12px/13px/20px`，数值恰好等于 `--text-sm/md/xl` 但不走 token）。改它是纯机械替换、零行为变化，但会碰更新弹窗（在途下载的界面），故留在"有目检条件时"再做——**该条件已具备**。
- **`ai_task.css` 36 处字面 spacing、`tasks.css` 15 处 + 4 个体半径、`profiles.css` 21 处**：token 逃逸集中在少数文件，属"间距微调值允许直接写 px"的既有约定边界，未逐一改动。
- **`.badge--mono` 定义在 `pages/settings/system.css`**：页面文件给全局组件类加修饰，依赖关系倒置，宜移入 `components/badge.css`。
- **`log-viewer.css` 与 `dashboard.css` 各有一套日志级别色**：前者 `level-*`（在用）、后者 `log-*`（在用，作用于行左边框与消息色）。两套命名并存但都有效，收敛需同时改模板与样式，未做。

## 定时任务改为「列表页 + 二级编辑页」（2026-09-24 **已落地**）

> 用户诉求（实机截图）：「定时任务怎么还是弹窗，能不能和前面两个一样改成打开新页面，一级页面显示列表」。逐项实现与验证见 `docs/changelog.md` 同日条目。

**已落地**：`utils/scheduledDraft.ts`（草稿模型 + 缺口判定 + 落盘载荷，纯函数）+ `useScheduledTasks` 自动保存模型（首次落盘 POST、之后同 id PUT）+ `views/tasks/ScheduledTasksPanel.vue`（整页列表 + 二级编辑页，旧 `ScheduledTasksView.vue` 删除）+ `useTaskEditorQuery` 新增可注入的 `ready` 门；顺带收紧 `parseCronToSchedule`（星期/月限制与 6 字段表达式此前被判为"表单能表达"，保存即静默改成每日）。逐项实现见 `docs/changelog.md` 同日条目。

**本轮新登记的待办**：

- **执行历史仍是弹窗**（有意）：它是只读视图而非编辑，入口在列表 ⋯ 与编辑页头部两处都保留；改成第三层页面会让"返回"变成两级。

## 任务仓库索引按类别拆分（2026-09-24 **已落地**）

> 用户诉求：「任务仓库导入里两类方案共享一个 json，容易误解，能否使用两份 `index.json` 隔离」。逐项实现与验证见 `docs/changelog.md` 同日条目。

**已落地**：任务仓库侧 `index.json` / `index.gitee.json` 恢复为纯浏览器索引，新增 `index.http.json` / `index.http.gitee.json` 收直连任务（GitHub + Gitee 双端）；前端索引地址改「类别 × 源」两维（`presetRepoIndexUrl`），`loaded` 区分"暂无条目"与"拉取失败"，异类条目计数提示取代静默过滤；顺带修掉任务仓库 README 两处指向旧 Python 仓库（`Misyra/Campus-Auth`）的死链。逐项实现见 `docs/changelog.md` 同日条目。

**对外依赖（更新）**：直连任务条目必须放进 `index.http.json` / `index.http.gitee.json`（带 `type: "http"`），浏览器任务条目放进 `index.json` / `index.gitee.json`（不带 `type`）。老版本客户端只读 `index.json`，行为与拆分前一致。

**有意不做**：
- **不为过渡期做"回退到混合索引"**：新客户端遇到该类索引取不到（远端尚未提供、或自定义地址写错）时按失败原文（含远端状态码）呈现，不做"回退到 `index.json` 再按 `type` 过滤"——那等于把刚拆开的两种真相又合回去。
- **`index.json` 不改名为 `index.browser.json`**：老客户端只会读 `index.json`，改名等于让它们的列表永久冻结在旧内容；"这个名字代表浏览器任务"由仓库 README 的索引表承担。

## 任务页深度复核（2026-09-23 **已落地**，同日第二轮）

> 用户诉求：「还有没有什么需要优化的，你检查一下」。做法是对未提交的方案 G 改动做四路并行复核（三面板行为矩阵 / CSS 死代码与令牌 / Rust 未提交 diff / 文档与实现一致性），再把确认的问题逐条修掉。逐项实现与验证见 `docs/changelog.md` 同日条目「任务页深度复核」。

**已落地**：三面板补 `flushPendingAutosave`（修掉"换编辑对象时在途的 500ms 改动被静默丢弃"，含 detached 落盘语义）、仓库导入撞 id 用未清洗原始名的确定性失败、脚本 ID 规则与后端 `is_valid_task_id` 对齐、脚本「立即运行」的成败判定与结果就地显示、后端 mtime 越界 panic 与 `order_tasks` 跨锁读改写等十余项修复与清理；完整清单见 `docs/changelog.md` 同日条目「任务页深度复核」。

**本轮新登记的待办**：

- **直连新建草稿的 `{gateway_host}` 占位地址能落盘**：`httpTaskDraftGaps` 只判"请求地址非空"，于是新建后随便改一个字段（例如名称）就会落一份必然失败的任务，而面板的缺口条不会说它——只有点「发送测试请求」时才被拦。最小改法是把占位符本身当成一处缺口（常量需从 `useHttpTasks` 下移到 `utils/httpTask` 以防反向依赖）。
- **脚本面板没有「复制为新脚本」**：浏览器任务与直连任务的行尾菜单都有，脚本只能手抄内容。脚本 ID 是文件名兼定时任务引用值，故复制应落在**新建草稿**（预填原内容 + 建议 ID `<id>_copy`，交给用户改名），而不是像另两类那样立即落盘。
- **历史裸 `.py` 脚本若 ID 含点号则无法保存**：`read_py_summary` 直接取 `file_stem` 当 id（不校验），而 `is_valid_task_id` 不收 `.`——这种脚本在面板里缺口恒非空、ID 字段又是禁用的（落盘后 ID 固定），改不动也存不下。要么在面板给一条"另存为新 ID"的出口，要么在扫描时把非法 stem 规范化后再接受。
- **直连任务的测试入口与另两类不一致（有意维持）**：`POST /api/http-tasks/test` 接受未落盘的草稿内容，所以「发送测试请求」在新建草稿下仍可用（浏览器/脚本的调试与运行必须落盘后才放开）。若将来统一，应给按钮写明"用草稿当前内容测试"，而不是直接禁用。

## 任务页三面板复核（2026-09-23 **已落地**）

> 用户诉求：对方案 G（列表页 + 二级编辑页 + 自动保存）的落地内容做全面细致的优化并修掉各种 bug。逐项实现与验证见 `docs/changelog.md` 同日条目「任务页三面板全面复核」。

**已落地**：三面板共享版式收敛到 `styles/pages/tasks.css` 的 `tsk-*` 组（删掉约 700 行重复样式）、编辑态判据改为「草稿非空」+ `useTaskEditorQuery` 统一 `?task=` 解析（修掉「返回不生效」与「陈旧深链整页白屏」）、脚本面板迁移方案 G、`utils/autosave` 收敛状态字与缺口态改口。完整清单见 `docs/changelog.md` 同日条目「任务页三面板全面复核」。

**本轮有意不做（备查）**：

- **脚本重命名**：ID 落盘后固定，「改名 = 存新 id + 删旧 id」存在失败窗口（旧文件成孤儿），要换名请删除重建。
- **缺口态下切换编辑对象的提示**：只在关闭编辑器时提示，避免点列表里另一条时被弹一句（换编辑对象时补发的那一发以 detached 方式落盘、不改状态字，见同日「任务页深度复核」）。

## v5.0.0 之后（待排期，按复核优先级）

- **必修（P0，9 条）**：Bridge 槽位/取消注册表、录制器多 frame、任务创建 `..Default` 空 steps 等（原 76 条 v2 对质结论，报告已删）。
- **强烈建议（P1，9 项）**：`wait` 三方矛盾、`timeout` 钳制、`evaluate` 关页、别名漂移、`select` 空值、LLM `finish_reason`/`max_tokens`/重试、更新后 `uv sync`、错误映射 500/404、Esc 脏状态。（已解决备查：`type=shell` 静默失效现于反序列化明确报错并提示改用 `script`（`src/tasks/models.rs`）；「前端保存防连点与 `validateConfig`/`cron` 校验」整项已落地——`configRanges.ts` 作区间单一出处并接入 `validateConfig`、`parseCronToSchedule` 补时/分区间、保存防连点经逐条核查已满足——见 `docs/changelog.md` 2026-09-24「前端 UI 风格全面统一」条。）
- **更新子系统（4 项 P2 + 3 项 P3）**：A 正确性（base 分离与 helper 落点）；B 体验（停滞超时与配额 403/429）；C 接口（pin 版本闸门）；D 清理（double-fire 与 `available` 语义）。
- 已对齐项：`VALID_STEP_TYPES` 含 `evaluate`/`custom`（本轮已对齐 `task-writing-guide`）、`UpdateChannel` 三通道与 `update/last_check.json`（`check.rs`/`mod.rs` 已实现，本轮补文档）、CI 含 `e2e-login-chain` + `rust-tests-unix`。

## 2026-09-15 全仓审查后的剩余项（本轮已修 4 P1 + 8 P2 + 3 P3，见 `docs/changelog.md`）

未修项及理由（均为**已确认但本轮有意不改**，非遗漏）：

- **`openapi.json` 无字段级契约**（原报告 P2-4，含 P2-8 的各类表现面）：spec 当前有 **91 个路径**（`/api/*` 90 + `/ws/logs`，operations 合计 106；2026-09-24 复核时点为 91/106，此前写的 88/103 已过期）但 `components.schemas` 为 0，除 `/api/ai/capture/status` 的 200 响应带真实 schema（`openapi.json:3490-3499`）外，其余响应 schema 为 `{}`；`openapi_json_matches_route_table` 归一后**只比对「方法 + 路径」集合**，故字段名/类型、状态码、媒体类型、路径参数四类漂移无任何自动拦截。已抽查确认**当前一致**（`AssessmentReason` 10 变体、`AssessmentConfidence` 3 变体、`RecoveryAdvice`、`LocalLinkState`、`strict_login_mode` 默认值两侧一致）。属已书面化的取舍（待 `utoipa` 宏化），本轮不改为不引入半成套的生成链路。**廉价替代方案**：补一个「Rust 侧序列化全部枚举变体 → 与 `types.ts` 文本比对」的双向一致性测试，因枚举漂移最易发生且前端 `switch` 有 `default` 兜底、不会报错、只会静默显示"待确认"。
- **抢占总在取消分支不等旧会话收尾**（原报告 P2-11）：`src/login/mod.rs` 的取消分支 `return self.cancelled_handle(...)` 使已 `take()` 出的 `old` 被 drop，`wait_old_session_finished` 不执行。已核对：`submit_gate` 在整个 `submit` 期间持有，故**不会**造成空窗插队；代码注释表明"等待期间点取消即放弃本次提交"是**有意设计**。若需收紧，可让取消分支同样等 `old.finished`（代价：点取消后最多再等 18s）。
- **`TrayAction::Quit` 无退出 watchdog**（原报告 P3）：托盘退出只做 `deps.shutdown.cancel()` + dispatch，而 Web 退出与定时自重启都有 `spawn_exit_watchdog`。属不一致；原报告中"托盘线程可能无界阻塞 `join`"已被证伪（Windows 循环最多 50ms 回到循环头，无 `TrackPopupMenu`），故风险仅为"未来若托盘线程引入阻塞逻辑则无兜底"。
- **`docs/known-issues.md` 的全面对质已完成**（2026-09-17）：#2 / #7 / #3 / #15 / #19 已核实并修正引用坐标，#19 判定为已修（校验收敛至 `src/web/ssrf.rs`），"低危清理项"两条判定不成立并已清空；**#22 注（21 项 P3 挂账）与 MON-4-R1/R2/R3 仍未逐条复核**，状态未知，引用前需自行对质。
- **未覆盖的验证面**：Unix 平台未实跑（CI `rust-tests-unix` 覆盖）；未做真实校园网门户验证（需实际门户）；未做人工渗透测试（安全结论基于代码审查与既有负向用例）；`updater::apply_pending_on_startup` 是否响应 shutdown、`client.ts` 的 `ensureAuthToken` 占用超时预算、`useScheduledTasks` 列表写入无 epoch 三项存疑未验证。

> 另：原报告将 P1-4 归因于「登录收尾的 `close_browser` 关掉定时任务的浏览器」，经复核**该机制不成立**（Python `_serve` 是严格串行命令循环，`close_browser` 必须排队），真实破坏点是 `force_recycle` 无条件强杀 —— 本轮按后者修复。记录以免后来者按错误机制重复排查。

## 直连任务重构 + 直连任务仓库（2026-09-22 **已落地**）

> 用户诉求：①「直连请求」的参数编辑从方案编辑器里挪到任务页，成为独立的**直连任务**；② 直连任务能进**仓库**分享（和浏览器任务一样，别人上传后可导入）。
> 已定方向（用户三选一确认）：**独立具名任务，方案里选一个**；仓库里**只分享直连任务**（不含账号密码）；**复用现有任务仓库**（同一索引，多一类条目）。
> 七个阶段全部完成，逐项实现与验证见 `docs/changelog.md` 同日条目「直连请求重构为独立「直连任务」」+「任务页新增「直连任务」Tab」。

**模型**：直连任务 = `TaskKind::Http(HttpTaskConfig)`，落 `<base>/tasks/http/<id>.json`（`type: "http"`）；字段是「请求形状」（method / url / headers / body / 成败关键字 / 凭据变换脚本 / 前置请求 / 退出登录请求等），**凭据仍属方案**（`username` / `password`）；方案新增 `active_http_task`，语义与 `active_task` 对齐（空 = 未绑定）。直连没有可内置的通用门户地址，故**没有默认任务**，未绑定时登录直接给出明确失败原因（浏览器渠道有 default 回退，直连没有，这是有意的不对称）。认证地址两边都有：**任务优先、留空回退方案的**。字段清单以 `frontend/src/types.ts` 与 `docs/changelog.md` 同日条目为准。

**遗留小项（有意不做）**：
- **`TaskExecutor` 对 http 是占位拒绝**：直连任务不经 Python Worker，`/api/tasks/{id}/execute` 与定时任务的 task_id 对它返回 400「执行接入属后续阶段」；它的验证入口是「发送测试请求」（任务编辑器内）。若将来要做「定时跑一次直连任务」，替换 `src/tasks/executor.rs` 的那两条臂即可。
- **旧分享文件里的直连配置会被忽略**：v9 及以前导出的方案 JSON 带着方案级 `http_*`（含脚本），导入时忽略并在响应里回报 `legacy_http_config_dropped`，前端提示用户去任务页重新配置。若要自动搬运，可在导入路径复用迁移里的 `build_legacy_http_task`。

**风险与取舍**（已书面化）：配置迁移是**单向**的（旧版本读到 v10 配置会忽略未知字段，但 `http_*` 已空）；导入他人方案时 `active_http_task` 一律清空（与 `active_task` 同口径），故分享方案不再携带直连配置——这正是把直连任务独立出去的意义。

**对外依赖**：任务仓库（`Misyra/campus-auth-tasks`）收录 `type: "http"` 的条目即会在「直连任务」子页的仓库导入里出现（索引契约见上方「任务仓库索引按类别拆分」一节；索引另有可选字段 `source`，导入弹窗渲染成来源链接）。

## 直连渠道「前置请求」能力（2026-09-23 **已落地**）

> 用户诉求：把一个实测脚本（河南科技大学「大学掌」体系的 CSRF POST）收进任务仓库。它卡在协议约束上：令牌绑定 TCP 连接（取 token 与登录必须同一条连接），而直连管线一次只发一个请求、且抓登录页与发登录各自建一个 `reqwest::Client`（连接池不共享）——配置层面绕不过去，故先补能力。

**已实现**：`HttpTaskConfig.pre_request`（可选：method / url / headers / body / extract / name），整次尝试共用一个 `Client`（登录页抓取、前置请求、登录请求三处），超时改由每个请求单独设定；取值只支持 `json:<点号路径>`，取值失败即终态并把前置请求的请求与响应带进报告。顺带修掉：`{local_ip}` / `{local_mac}` 此前只在"配了加密脚本"时才查询网卡，"模板里用了但没配脚本"的任务会**静默发空 IP**（现按 `needs_local_address` 判断）。详见 `docs/changelog.md` 同日条目。

**来源纪律（记此备查）**：本轮我曾据"两个脚本"这句自行去一个**私有**仓库里找第二套协议，并把由它反推出来的条目写进了 PR；用户指出后已撤下（分支 force-push，索引与 README 同步清掉，只保留来源可公开引用的那一条）。**用户没给的来源不要自己去找**，缺材料先问。

**遗留（有意不做）**：
- 前置请求只支持 JSON 取值，不支持正则：令牌在 HTML 里的门户用「登录页原文 + 凭据变换脚本」表达更合适（脚本的 `ctx.page` 就是页面原文），不为它引入正则依赖。
- 不做多步链（A→B→C）：真需要多步的门户已属浏览器渠道的领域。
- 不做并发/重试：两次请求顺序发出，前置请求失败即终态。

## 直连请求渠道待办（2026-09-18 复核后新增）

> 背景：2026-09-18 对直连渠道（`src/login/http_login.rs` + `ProfileData.http_*`）做了一次缺口复核。
> 本轮已落地「HTTPS 证书策略可配」「UA 兜底」「响应头回显」「保存时校验体积」四项，见 `docs/changelog.md` 同日条目。

- **Cookie 门户**（未实现）：直连当前**只发一次请求**，`fetch_login_page` 抓到的登录页原文仅作为脚本 `ctx.page` 输入，其 `Set-Cookie` 不会带到登录请求，登录请求本身也不带 cookie jar（`Cargo.toml` 的 reqwest features 未启用 `cookies`）。「先取会话/令牌再提交」类门户已由**前置请求**覆盖（见上方「直连渠道前置请求」一节），仍缺的是**依赖 Cookie 的门户**，指南 §8 已如实写明边界。实现路径（若要做）：启用 `reqwest` 的 `cookies` feature，让页面抓取与登录请求共用 cookie jar，并让"无脚本也能抓页"成为可选项。
- **判定只看响应体**：`success_pattern` / `failure_pattern` 仅对 body 做子串匹配（空成功关键字时回落 HTTP 2xx）。门户若用状态码或响应头（如 `Location`）表达成败则判定不了，需补响应头参与判定或允许按状态码判定。
- **HTTP 方法只有 GET/POST**（`HttpLoginMethod`）：PUT/PATCH 等少见但成本低，需同步 `frontend/src/utils/loginChannel.ts` 的 `HTTP_METHOD_OPTIONS` 与 `src/web/routes/profiles.rs` 中用 `"PATCH"` 断言 400 的既有用例。
- **测试端点与正式登录的 `local_ip` 来源不同**：正式走 `MonitorService::local_address()`（30s 缓存），测试端点自建 detector（`profiles.rs` 的 `test_http_login`）。两值理论上可能不一致，造成「测试通过但自动登录失败」的难排查组合，可考虑统一到同一来源。
- **登录历史不记渠道**（既有项）：权威口径见上方「全功能实测排查与修复」一节的同一条。

## 长期挂账（2026-08-26 核实修订）

- M1 trait 化已完成；Pinia 不适用（前端无 Pinia）；utoipa 手写 `openapi.json` 仍为前端 `typegen` 数据源，待 `utoipa` 宏化后自动生成；
- `AppState.container` 后续可评估移除。
- 定时任务「启动后执行」增强（2026-09-12，已上线固定延迟方案）：可选增强为等待监测服务首次网络连通后再触发（接 StatusManager watch，超时兜底强制执行），彻底消除"网络未就绪烧掉一次尝试"的场景。

> 本文件为唯一活跃计划入口，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。
