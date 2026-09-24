# 后续计划（活跃入口）

> R/F/G/A/B 组已在 `docs/changelog.md` 第十一~十六轮落地并归档；历史归档目录 `docs/archive/` 已于 2026-09-17 删除，其中材料不可追溯。
> 当前活跃：`v5.0.2`（`docs/changelog.md`）。P0/P1 与更新子系统待办的权威摘要以本文件与 `docs/known-issues.md` 为准（原 defect-recheck / updater-audit 过程报告已于 2026-09-12 删除）。
> 验证块：`cargo clippy --all-targets -- -D warnings` / `cargo test` 双 feature（含 `rust-tests-unix`）/ `cargo fmt --check` / `uv run pytest` / `npm run build` + `vitest` / `e2e-login-chain` 全链路。

## 未提交更改的全面审查与修复（2026-09-24 **已落地**）

> 用户诉求：「全面审查一下当前更改」→「你修复优化一下」。审查结论与证据在 `docs/reports/current-changes-review-2026-09-24.md`（本地不提交），逐项修复见 `docs/changelog.md` 同日条目「全面审查后的修复」。

**审查结论（复跑复核）**：`fmt` / `clippy -D warnings` / `cargo test`（lib 1014 + helper 12）/ `vue-tsc` / `vitest`（424）/ `vite build` / `audit.mjs`（ghosts=0 dead=36）/ 远端四个索引（9·1·9·1）全部与 changelog 声称一致。

**本轮修掉的六条 P1（都已配回归测试）**：

1. **跨桶 `untitled_N` 撞 id 会静默删文件**：前端取号只看自己那一类列表，而后端 `save_task` 写盘时会删掉**另外两个桶**里同 id 的文件——"第一个浏览器任务 + 第一个直连任务"必然撞车。改为按三类 id 并集取号（`useTaskDirectory.allTaskIds`），后端删别桶文件时改走 `warn` 留痕。
2. **「保留配置与任务」保留的配置解不开密码**：第一步 `POST /api/uninstall` 原先无条件删 `~/.campus_network_auth`（含 `.enc_key.rs`）。新增可选 body `{ keep_user_data }`，勾选时保留密钥目录——两步传同一个值。
3. **不可逆步骤排在可失败步骤之前**：`purge` 现在先查卸载助手是否在位（缺失即 400 + 明确文案），`detect` 回报 `helper.exists` 供界面提前拦下，避免"清完残留却删不掉程序"。
4. **`cancel_pending_update()` 挡不住"卸载后被更新回来"**：新增 `update_cancelled` 标记（`apply_update` / `apply_uploaded_package` / `finalize_staged_package` / `ensure_helper_for_shutdown` 四处复查）并复查清理结果，返回值改为"确实取消掉了"；取消失败时回执里明说"程序可能被重新安装"。
5. **定时任务新建的 `_isNew` 卡死**：`onSaved` 现在对"被后一发顶掉的成功"也生效（那是关于草稿的事实），且 `persist` 遇 409 降级为 PUT。
6. **脚本面板打字停顿会锁死 ID**：新建态把"ID 仍在输入中"计入缺口，由回车/失焦显式确认（`scriptIdPending` + `commitScriptId`）。

**P2/P3 一并收掉**：请求头保存闸对齐 64 KiB（与执行闸同值，`login_and_task_size_limits_same` 钉住）；`flush` 不重复落盘（在途载荷指纹）；`clear()` 使在途请求失效；detached 失败不再被序号判断吞掉；`DATA_DIR_NAMES` 取自 `utils::paths`；守卫拒绝 `target/` 构建输出；`InvalidTaskId` 在 GET/export 上也走 400；危险步骤与凭据脚本的提示恢复到编辑器内（自动保存下没有"保存前"了）；非每日 cron 不再被无关编辑改写成每日；`AGENTS.md` / `plan-next` / 指南 / `changelog` 的失真项同步修正。

## 更新助手卸载模式（2026-09-24 **已落地**）

> 用户诉求：「能否优化一下更新助手，把卸载功能加进去」。逐项实现与验证见 `docs/changelog.md` 同日条目。

**已落地**：`campus-auth-helper` 新增 `--uninstall` 模式（与 `--apply-update` 互斥），等待主进程退出后删除安装目录并弹系统提示框报告结果；删除范围与守卫收在 `src/uninstall/`（**单一事实源**：界面据此列清单、助手据此执行）；新增 `POST /api/uninstall/purge`（守卫 → 取消待应用更新 → spawn 助手 → 优雅退出），`GET /api/uninstall/detect` 改为返回逐项清单 + 守卫拒绝原因；前端卸载弹窗支持「保留配置与任务」勾选并逐项列出将删除的内容。**原状**：卸载只清 `base_path` 之外的系统残留，程序目录要用户自己删（Windows 上运行中的 exe 无法删除自己，而程序文件与用户数据同目录）。

**本轮登记 / 遗留**：

- ~~**未做真机端到端卸载演练**~~ → **2026-09-24 已补上**：独立 `--target-dir` 编一份 exe + helper，在仓库外的临时「安装目录」上跑全链路，**34 项断言全过**（全删 / 保留数据 / 守卫拒绝三轮；含"安装目录真的消失""`%TEMP%` 无残留"）。脚本 `docs/reports/uninstall-e2e/rehearse.ps1`（本地不提交，可复跑）。**这一步是必要的，不是走过场**：它当场推翻了两个只靠单测看不出的判断——`MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)` 在非管理员账户下必然失败（原先当主路径 = 每次卸载留一个几 MB 的临时副本），以及 cmd 的 `/c` 引号剥离规则（原先的 `""…""` 外层包裹会把整行拆坏，派发成功却什么都不删）。
- **路径含 cmd 特殊字符时（`% & ^ | < > " !`）退回 `MoveFileExW`**：该路径在非管理员账户下会失败，那份临时副本会留在 `%TEMP%`（不可见，系统也会自行清理）。概率极低，且"宁可留一个临时文件也不冒命令被拆坏的风险"是刻意取舍；要覆盖需改用 `CreateProcessW` + 自建管道或临时批处理，成本不成比例。
- **「保留配置与任务」不含 Playwright 浏览器缓存**：勾选后仍会清理浏览器缓存（属系统残留而非用户配置）。想"换目录重装且不重下浏览器"目前需手动保留缓存；若要支持，做法是把「保留」拆成两项（用户数据 / 浏览器缓存）。
- 卸载**没有进度界面**（有意）：助手是无界面进程，中间过程的反馈由系统提示框承担。

## 前端 UI 风格全面统一（2026-09-24 **已落地**）

> 用户诉求：「准备全面统一前端界面 UI 风格，你先探索，拿到结论」。探索结论与审计脚本在 `docs/reports/ui-audit/`（本地不提交，`audit.mjs` 按 Vue 编译器 AST 取模板 class 后与样式表对账，可复跑）。

**总判断**：token 层是健康的（`base.css` 之外字面 hex 仅 11 处、字面 rgba 仅 9 处），问题在**组件契约层**——同一视觉角色有 2–15 套平行实现。八项已落地（卡片头三套→一套、页面入场动画单一出口、chip 三份逐字重复→全局一套、note 四份同族→全局一套、开关开态对齐、死 CSS 清理、幽灵类 18→0、断点 11 个取值→4 个），逐项实现见 `docs/changelog.md` 同日条目。

**审计指标变化**（`audit.mjs` 可复跑验证）：幽灵类 18 → **0**；待处理死类 68 → **0**（其余 36 项全部核对为运行期施加：数据驱动的 `:class`、JS `classList`、Vue `<Transition>`）；断点取值 11 → **4**（640 / 768 / 900 / 1100）。

**本轮登记的前端待办**：

- **`.page-content` 仍不是真正的"页面外壳"**：入场动画已收成**单条** `.content-wrapper > *`（此前三条结构选择器会让任务页 / 设置页内外两层同时播，见下方 2026-09-24「自动保存共享控制器 + 首次真机目检」一节），但页面容器（flex 方向 / gap / 最大宽度）仍由 `.settings-page` / `.appearance-page` / `.tasks-page` / `.tsk-list-page` / `.ai-task-page` 各自声明。若要继续，方向是让 `.page-content` 提供布局契约、各页面只声明差异——但那会同时改动 5 个页面的间距，需配目检。**2026-09-24 起真机目检已具备条件**（见 `docs/reports/ui-audit/visual-check.py`，Playwright 只读巡检 + 计算样式断言，可复跑）。
- **`UpdateDialog.vue` 是全仓唯一把字号写成字面量的组件文件**（15 处 `12px/13px/20px`，数值恰好等于 `--text-sm/md/xl` 但不走 token）。改它是纯机械替换、零行为变化，但会碰更新弹窗（在途下载的界面），故留在"有目检条件时"再做——**该条件已具备**。
- **`ai_task.css` 36 处字面 spacing、`tasks.css` 15 处 + 4 个体半径、`profiles.css` 21 处**：token 逃逸集中在少数文件，属"间距微调值允许直接写 px"的既有约定边界，未逐一改动。
- **`.badge--mono` 定义在 `pages/settings/system.css`**：页面文件给全局组件类加修饰，依赖关系倒置，宜移入 `components/badge.css`。
- **`log-viewer.css` 与 `dashboard.css` 各有一套日志级别色**：前者 `level-*`（在用）、后者 `log-*`（在用，作用于行左边框与消息色）。两套命名并存但都有效，收敛需同时改模板与样式，未做。

## 定时任务改为「列表页 + 二级编辑页」（2026-09-24 **已落地**）

> 用户诉求（实机截图）：「定时任务怎么还是弹窗，能不能和前面两个一样改成打开新页面，一级页面显示列表」。逐项实现与验证见 `docs/changelog.md` 同日条目。

**已落地**：新增 `utils/scheduledDraft.ts`（草稿模型 + 缺口判定 + 落盘载荷，纯函数）+ `useScheduledTasks` 改为自动保存模型（首次落盘 POST、之后同 id PUT，删掉弹窗与显式保存）+ `views/tasks/ScheduledTasksPanel.vue`（整页列表 + 二级编辑页，旧 `ScheduledTasksView.vue` 删除）+ `useTaskEditorQuery` 新增可注入的 `ready` 门（定时任务用自己的列表就绪标记）；顺带收紧 `parseCronToSchedule`（星期/月限制与 6 字段表达式此前被判为"表单能表达"，保存即静默改成每日）。

**本轮新登记的待办**：

- **自动保存状态机已是第四份拷贝**：`useTasks` / `useHttpTasks` / `useScripts` / `useScheduledTasks` 各一份 `autosaveTimer + autosaveSeq + pendingChanges + lastSavedFingerprint + flushPendingAutosave`。本轮为了四个面板同构照搬了一份，抽成 `utils/autosave` 共享控制器的价值随之上升（注入 gaps / persist / toast 三个口子 + 各面板一份注入参数），并补齐 `useTasks` / `useScripts` / `useScheduledTasks` 的自动保存单测。
  - **2026-09-24 进度（本轮）**：单测这一半**已补齐**（`useTasks.test.ts` 5 例、`useScripts.test.ts` 6 例；`useScheduledTasks` / `useHttpTasks` 原有）。**抽取消仍待做**：测试就位后才是机械替换，而注入面（`fingerprintOf` / `gapsOf` / `persist(draft, {detached})` / 状态写入）四面板并不一致，`detached` 语义与"首次 POST 之后 PUT"的分支都在关键路径上——缺测试时动它风险大于收益。
  - **2026-09-24 收口：已落地。** `utils/autosave.ts` 新增 `createAutosaveController`（注入面收成三件：`fingerprintOf` / `blockReasonOf` / `persist`+`onSaved`，控制器不持有缺口清单），四个面板只剩注入参数；深度 watcher 一并由控制器持有。**四个面板的自动保存测试文件一行未改即通过**，即"机械替换"的证据。顺带修掉两个真问题：`useScheduledTasks` 的对账 watcher 挂在 `loadScheduledTasks` 里（每次拉取都新挂一个）、`clear()` 只复位状态机会漏掉一轮迟到的 watcher（会"删除后又写回一次"）。详见 `docs/changelog.md` 同日「自动保存状态机四份拷贝 → 共享控制器；首次真机目检」。
- ~~**删除/失效任务的编辑器不自动关**~~ → **2026-09-24 已落地**：新增 `utils/draftReconcile.ts`（纯判定 + 7 例单测），四个面板各挂一个列表对账 watcher；判据是「曾经在列表里、现在不在了」（用"现在不在列表里"会把刚新建、尚未刷进列表的草稿立刻踢出编辑器），且走 `clearXDraft()` 而非 `closeXEditor()`（后者"退出即落盘"，对已删除的任务再 PUT 只会再吃一次 404）。
- **执行历史仍是弹窗**（有意）：它是只读视图而非编辑，入口在列表 ⋯ 与编辑页头部两处都保留；改成第三层页面会让"返回"变成两级。

## 任务仓库索引按类别拆分（2026-09-24 **已落地**）

> 用户诉求：「任务仓库导入里两类方案共享一个 json，容易误解，能否使用两份 `index.json` 隔离」。逐项实现与验证见 `docs/changelog.md` 同日条目。

**已落地**：任务仓库侧 `index.json` / `index.gitee.json` 恢复为纯浏览器索引，新增 `index.http.json` / `index.http.gitee.json` 收直连任务（GitHub + Gitee 双端推送，含此前未推到 Gitee 的 haust 提交——Gitee 上那个任务文件此前是 404）；前端索引地址改为「类别 × 源」两维（`presetRepoIndexUrl`），切类别 / 切源都重取地址，`loaded` 区分"暂无条目"与"拉取失败"，异类条目计数提示取代静默过滤，混合索引的解释性文案删除；顺带修掉任务仓库 README 两处指向旧 Python 仓库（`Misyra/Campus-Auth`）的死链。

**对外依赖（更新）**：直连任务条目必须放进 `index.http.json` / `index.http.gitee.json`（带 `type: "http"`），浏览器任务条目放进 `index.json` / `index.gitee.json`（不带 `type`）。老版本客户端只读 `index.json`，行为与拆分前一致。

**有意不做**：
- **不为过渡期做"回退到混合索引"**：新客户端遇到该类索引取不到（远端尚未提供、或自定义地址写错）时按失败原文（含远端状态码）呈现，不做"回退到 `index.json` 再按 `type` 过滤"——那等于把刚拆开的两种真相又合回去。
- **`index.json` 不改名为 `index.browser.json`**：老客户端只会读 `index.json`，改名等于让它们的列表永久冻结在旧内容；"这个名字代表浏览器任务"由仓库 README 的索引表承担。

## 任务页深度复核（2026-09-23 **已落地**，同日第二轮）

> 用户诉求：「还有没有什么需要优化的，你检查一下」。做法是对未提交的方案 G 改动做四路并行复核（三面板行为矩阵 / CSS 死代码与令牌 / Rust 未提交 diff / 文档与实现一致性），再把确认的问题逐条修掉。逐项实现与验证见 `docs/changelog.md` 同日条目「任务页深度复核」。

**已落地**：三面板补 `flushPendingAutosave(switch|close)`（修掉"换编辑对象时在途的 500ms 改动被静默丢弃"，含 detached 落盘语义以防旧草稿响应污染新草稿指纹）；仓库导入撞 id 用未清洗原始名的确定性失败；浏览器面板 JSON 为空/语法错时状态字说谎 + `jsonGate`；脚本 ID 规则与后端 `is_valid_task_id` 对齐（`my-script` / `2fa` 这类既有脚本此前永远存不上）；脚本「立即运行」的成败判定 + 结果就地显示（此前无论成败都弹绿色"执行完成"、且输出无处可看）；定时任务手动运行的 toast 语义（known-issues E2）；浏览器面板四处调试入口的 busy 守卫；「加载默认模板」加确认；浏览器文件导入按类型过滤；脚本导入补 `?task=`、导出补 busy 守卫；菜单监听泄漏；删除浏览器任务的回退说明。后端：mtime 越界 panic（会让任务列表整片消失）、`order_tasks` 跨锁读改写、退出登录请求的执行顺序（提到抓登录页之前）、`InvalidTaskId → 400`。清理：确认为零引用的 CSS 规则成批删除（`responsive.css` 980px 块等）、`.icon-xs` 双定义收敛、`.skip-link` 补上真实实例、表格悬停底色移到单元格（圆角才对得上）+ 空态行不再装成可点。文档：三份指南补「退出登录请求」这一组、修正脚本落盘名与 stdout 去向等三处与代码冲突的说法。

**本轮新登记的待办**：

- **三份自动保存状态机仍是三份拷贝**（`useTasks` / `useHttpTasks` / `useScripts` 各一套 `autosaveTimer + autosaveSeq + pendingChanges + lastSavedFingerprint + flushPendingAutosave`）：这一轮的"换对象丢改动"正是因为三份里只有脚本那份做对了 flush。`pendingChanges` 与指纹判据必须一致，否则又会出现"面板说能存、后端说不能"这类漂移。建议抽成 `utils/autosave` 的共享控制器（注入 gaps / persist / toast 三个口子）并让三个 composable 各写一份注入参数；顺带补齐 `useTasks` / `useScripts` 的自动保存单测（目前只有 `useHttpTasks.test.ts` 盖到）。**（2026-09-24 定时任务改造后这份拷贝变成四份，权威口径见上方「定时任务改为列表页 + 二级编辑页」一节的同名待办。）**
- **直连新建草稿的 `{gateway_host}` 占位地址能落盘**：`httpTaskDraftGaps` 只判"请求地址非空"，于是新建后随便改一个字段（例如名称）就会落一份必然失败的任务，而面板的缺口条不会说它——只有点「发送测试请求」时才被拦。最小改法是把占位符本身当成一处缺口（常量需从 `useHttpTasks` 下移到 `utils/httpTask` 以防反向依赖）。
- **脚本面板没有「复制为新脚本」**：浏览器任务与直连任务的行尾菜单都有，脚本只能手抄内容。脚本 ID 是文件名兼定时任务引用值，故复制应落在**新建草稿**（预填原内容 + 建议 ID `<id>_copy`，交给用户改名），而不是像另两类那样立即落盘。
- **历史裸 `.py` 脚本若 ID 含点号则无法保存**：`read_py_summary` 直接取 `file_stem` 当 id（不校验），而 `is_valid_task_id` 不收 `.`——这种脚本在面板里缺口恒非空、ID 字段又是禁用的（落盘后 ID 固定），改不动也存不下。要么在面板给一条"另存为新 ID"的出口，要么在扫描时把非法 stem 规范化后再接受。
- **直连任务的测试入口与另两类不一致（有意维持）**：`POST /api/http-tasks/test` 接受未落盘的草稿内容，所以「发送测试请求」在新建草稿下仍可用（浏览器/脚本的调试与运行必须落盘后才放开）。若将来统一，应给按钮写明"用草稿当前内容测试"，而不是直接禁用。

## 任务页三面板复核（2026-09-23 **已落地**）

> 用户诉求：对方案 G（列表页 + 二级编辑页 + 自动保存）的落地内容做全面细致的优化并修掉各种 bug。逐项实现与验证见 `docs/changelog.md` 同日条目「任务页三面板全面复核」。

**已落地**：三面板共享版式收敛到 `styles/pages/tasks.css` 的 `tsk-*` 组（删掉约 350 行 × 2 份重复 scoped 样式与约 380 行旧版式）；编辑态判据改为「草稿非空」+ 新增 `useTaskEditorQuery` 统一 `?task=` 解析（修掉「返回不生效」与「陈旧深链整页白屏」）；脚本面板迁移方案 G（`useScripts` 自动保存 + `utils/scriptDraft` + 新建先命名再落盘）；`utils/autosave` 收敛状态字并让缺口态改口（修掉「缺口未补齐却显示已保存」的静默丢改动）；搜索态拖拽下标映射；行尾 ⋯ 菜单不再被卡片裁切。

**本轮有意不做（备查）**：

- **脚本重命名**：ID 落盘后固定，「改名 = 存新 id + 删旧 id」存在失败窗口（旧文件成孤儿），要换名请删除重建。
- **缺口态下切换编辑对象的提示**：只在关闭编辑器时提示，避免点列表里另一条时被弹一句（换编辑对象时补发的那一发以 detached 方式落盘、不改状态字，见同日「任务页深度复核」）。

## C 组收尾（已于 2026-08-26 落地，非活跃待办）

> 原计划出自 `docs/review-2026-08-24.md`，该文件已于 2026-08-31 随 `f015e41` 删除。**四个批次均已落地**，结论完整保留在 `docs/changelog.md` 第十二轮（`:1524-1551`）；本节仅留索引供追溯，不再列为待办：

- **批次五：A-4 孤儿清理降频 + 防护** — `changelog.md:1528-1531`（否决 Toolhelp32 替换，改为降频 + 5s 超时防护；commit `af249e1`）
- **批次六：S 级小尾巴五件** — `changelog.md:1533-1539`（`PerProbeDetail::new`、scripts 单次读盘、删 `SchedulerApi::history_dir`、`parse_host_port("::1")`、IconApp 收敛；commit `11c2233`）
- **批次七：A-5 system.rs 按域拆分** — `changelog.md:1541-1544`（`routes/background.rs` / `routes/uninstall.rs`，关闭 `state.container` 旁路；commit `207d9cd`）
- **批次八：B3 根治（调试会话存活期纳入槽位）** — `changelog.md:1546-1550`（commit `5505faa`）

## v5.0.0 之后（待排期，按复核优先级）

- **必修（P0，9 条）**：Bridge 槽位/取消注册表、录制器多 frame、任务创建 `..Default` 空 steps 等（原 76 条 v2 对质结论，报告已删）。
- **强烈建议（P1，约 12 项）**：`wait` 三方矛盾、`timeout` 钳制、`evaluate` 关页、别名漂移、`select` 空值、LLM `finish_reason`/`max_tokens`/重试、更新后 `uv sync`、错误映射 500/404、Esc 脏状态、~~前端保存防连点与 `validateConfig`/`cron` 校验~~。（`shell` 类型静默失效已解决：`type=shell` 现于反序列化明确报错并提示改用 `script`，见 `src/tasks/models.rs`。）（**2026-09-24**：「前端保存防连点与 `validateConfig`/`cron` 校验」整项已落地——`configRanges.ts` 作区间单一出处并接入 `validateConfig`（区间越界降级为警告、只把 NaN/非整数与端口越界当硬错误，依据是后端对这些都是裸 `u32` 直收）、`parseCronToSchedule` 补时/分区间（`99 99 * * *` 此前被判为有效）、保存防连点经逐条核查确认已满足故未改；详见 `docs/changelog.md` 同日「前端 UI 风格全面统一」条。）
- **更新子系统（4 项 P2 + 3 项 P3）**：A 正确性（base 分离与 helper 落点）；B 体验（停滞超时与配额 403/429）；C 接口（pin 版本闸门）；D 清理（double-fire 与 `available` 语义）。
- 已对齐项：`VALID_STEP_TYPES` 含 `evaluate`/`custom`（本轮已对齐 `task-writing-guide`）、`UpdateChannel` 三通道与 `update/last_check.json`（`check.rs`/`mod.rs` 已实现，本轮补文档）、CI 含 `e2e-login-chain` + `rust-tests-unix`。

## 2026-09-15 全仓审查后的剩余项（本轮已修 4 P1 + 8 P2 + 3 P3，见 `docs/changelog.md`）

未修项及理由（均为**已确认但本轮有意不改**，非遗漏）：

- **`openapi.json` 无字段级契约**（原报告 P2-4，含 P2-8 的各类表现面）：spec 当前有 **91 个路径**（`/api/*` 90 + `/ws/logs`，operations 合计 106；2026-09-24 复核时点为 91/106，此前写的 88/103 已过期）但 `components.schemas` 为 0，除 `/api/ai/capture/status` 的 200 响应带真实 schema（`openapi.json:3490-3499`）外，其余响应 schema 为 `{}`；`openapi_json_matches_route_table` 归一后**只比对「方法 + 路径」集合**，故字段名/类型、状态码、媒体类型、路径参数四类漂移无任何自动拦截。已抽查确认**当前一致**（`AssessmentReason` 10 变体、`AssessmentConfidence` 3 变体、`RecoveryAdvice`、`LocalLinkState`、`strict_login_mode` 默认值两侧一致）。属已书面化的取舍（待 `utoipa` 宏化），本轮不改为不引入半成套的生成链路。**廉价替代方案**：补一个「Rust 侧序列化全部枚举变体 → 与 `types.ts` 文本比对」的双向一致性测试，因枚举漂移最易发生且前端 `switch` 有 `default` 兜底、不会报错、只会静默显示"待确认"。
- **抢占总在取消分支不等旧会话收尾**（原报告 P2-11）：`src/login/mod.rs` 的取消分支 `return self.cancelled_handle(...)` 使已 `take()` 出的 `old` 被 drop，`wait_old_session_finished` 不执行。已核对：`submit_gate` 在整个 `submit` 期间持有，故**不会**造成空窗插队；代码注释表明"等待期间点取消即放弃本次提交"是**有意设计**。若需收紧，可让取消分支同样等 `old.finished`（代价：点取消后最多再等 18s）。
- ~~**`useConfig` 保存期间的编辑被折进快照**（原报告 P3）~~ → **2026-09-24 已落地**：`saveConfig` 改为在 `await patch` **之前**取 `submittedSnapshot`，保存成功后 `dirty = 当前值 !== submittedSnapshot`（`suppressDirty` 窗口内被抑制的 watcher 不会补跑，故必须自己算一次）。新增回归用例并验证过它在旧实现下会失败。
- **`TrayAction::Quit` 无退出 watchdog**（原报告 P3）：托盘退出只做 `deps.shutdown.cancel()` + dispatch，而 Web 退出与定时自重启都有 `spawn_exit_watchdog`。属不一致；原报告中"托盘线程可能无界阻塞 `join`"已被证伪（Windows 循环最多 50ms 回到循环头，无 `TrackPopupMenu`），故风险仅为"未来若托盘线程引入阻塞逻辑则无兜底"。
- **`docs/known-issues.md` 的全面对质已完成**（2026-09-17）：#2 / #7 / #3 / #15 / #19 已核实并修正引用坐标，#19 判定为已修（校验收敛至 `src/web/ssrf.rs`），"低危清理项"两条判定不成立并已清空；**#22 注（21 项 P3 挂账）与 MON-4-R1/R2/R3 仍未逐条复核**，状态未知，引用前需自行对质。
- **未覆盖的验证面**：Unix 平台未实跑（CI `rust-tests-unix` 覆盖）；未做真实校园网门户验证（需实际门户）；未做人工渗透测试（安全结论基于代码审查与既有负向用例）；`updater::apply_pending_on_startup` 是否响应 shutdown、`client.ts` 的 `ensureAuthToken` 占用超时预算、`useScheduledTasks` 列表写入无 epoch 三项存疑未验证。

> 另：原报告将 P1-4 归因于「登录收尾的 `close_browser` 关掉定时任务的浏览器」，经复核**该机制不成立**（Python `_serve` 是严格串行命令循环，`close_browser` 必须排队），真实破坏点是 `force_recycle` 无条件强杀 —— 本轮按后者修复。记录以免后来者按错误机制重复排查。

## 直连任务重构 + 直连任务仓库（2026-09-22 **已落地**）

> 用户诉求：①「直连请求」的参数编辑从方案编辑器里挪到任务页，成为独立的**直连任务**；② 直连任务能进**仓库**分享（和浏览器任务一样，别人上传后可导入）。
> 已定方向（用户三选一确认）：**独立具名任务，方案里选一个**；仓库里**只分享直连任务**（不含账号密码）；**复用现有任务仓库**（同一索引，多一类条目）。
> 七个阶段全部完成，逐项实现与验证见 `docs/changelog.md` 同日条目「直连请求重构为独立「直连任务」」+「任务页新增「直连任务」Tab」。

**模型**：直连任务 = `TaskKind::Http(HttpTaskConfig)`，落 `<base>/tasks/http/<id>.json`，`type: "http"`；字段是「请求形状」（method / url / auth_url / headers / body / success_pattern / failure_pattern / crypto_script / pre_request / logout_request / ignore_https_errors / metadata），**凭据仍属方案**（`username` / `password`）。方案新增 `active_http_task`，语义与 `active_task` 对齐（空 = 未绑定）；直连没有可内置的通用门户地址，故**没有默认任务**，未绑定时登录直接给出明确失败原因（浏览器渠道有 default 回退，直连没有，这是有意的不对称）。认证地址两边都有：**任务优先、留空回退方案的**。

**遗留小项（有意不做）**：
- ~~**直连任务不参与拖拽排序**~~ → **2026-09-23 已落地**：`.order.json` 本就是三类任务共用的一份扁平 id 表（`tasks/loader.rs` 的 `list_all_tasks` 按 id 排序，与类型无关），缺的只是接口层——`OrderBody` 新增 `http` 分组（`#[serde(default)]`，旧前端不发该字段仍可用）、`order_tasks` 一并写入，前端 `DragSortOptions` 补 `http`（**三组全为必填**，漏传一组会静默清空那组顺序）并给直连面板加了拖拽列。详见 `docs/changelog.md` 同日条目「直连任务纳入拖拽排序」。
- **`TaskExecutor` 对 http 是占位拒绝**：直连任务不经 Python Worker，`/api/tasks/{id}/execute` 与定时任务的 task_id 对它返回 400「执行接入属后续阶段」；它的验证入口是「发送测试请求」（任务编辑器内）。若将来要做「定时跑一次直连任务」，替换 `src/tasks/executor.rs` 的那两条臂即可。
- **旧分享文件里的直连配置会被忽略**：v9 及以前导出的方案 JSON 带着方案级 `http_*`（含脚本），导入时忽略并在响应里回报 `legacy_http_config_dropped`，前端提示用户去任务页重新配置。若要自动搬运，可在导入路径复用迁移里的 `build_legacy_http_task`。

**风险与取舍**（已书面化）：配置迁移是**单向**的（旧版本读到 v10 配置会忽略未知字段，但 `http_*` 已空）；导入他人方案时 `active_http_task` 一律清空（与 `active_task` 同口径），故分享方案不再携带直连配置——这正是把直连任务独立出去的意义。

**对外依赖**：任务仓库（`Misyra/campus-auth-tasks`）收录 `type: "http"` 的条目即会在「直连任务」子页的仓库导入里出现。**2026-09-23 起有第一条 `type: "http"` 条目**（`haust`，河南科技大学大学掌体系 CSRF），索引新增可选字段 `source`（来源仓库，导入弹窗渲染成链接）。**2026-09-24 起索引按类别拆分**（直连条目移入 `index.http.json` / `index.http.gitee.json`），本段原先"同一份索引多一类条目、老条目缺 `type` 按 `browser` 处理"的口径已作废，见上方「任务仓库索引按类别拆分」。

## 直连渠道「前置请求」能力（2026-09-23 **已落地**）

> 用户诉求：把一个实测脚本（河南科技大学「大学掌」体系的 CSRF POST）收进任务仓库。它卡在协议约束上：令牌绑定 TCP 连接（取 token 与登录必须同一条连接），而直连管线一次只发一个请求、且抓登录页与发登录各自建一个 `reqwest::Client`（连接池不共享）——配置层面绕不过去，故先补能力。

**已实现**：`HttpTaskConfig.pre_request`（可选：method / url / headers / body / extract / name），整次尝试共用一个 `Client`（登录页抓取、前置请求、登录请求三处），超时改由每个请求单独设定。取值只支持 `json:<点号路径>`（容忍 JSONP 与脏字符）；取到的值进 vars 后按秘密脱敏；取值失败即终态并把前置请求的请求与响应带进报告。

**顺带修掉的真缺陷**：`{local_ip}` / `{local_mac}` 此前只在"配了加密脚本"时才查询网卡，于是"模板里用了 `{local_ip}` 但没配脚本"的任务会**静默发空 IP**（把本机 IP 当必填参数的门户会请求照发、门户照拒）。现按"脚本要读 or 模板里写了"判断（`needs_local_address`）。

**来源纪律（记此备查）**：本轮我曾据"两个脚本"这句自行去一个**私有**仓库里找第二套协议，并把由它反推出来的条目写进了 PR；用户指出后已撤下（分支 force-push，索引与 README 同步清掉，只保留来源可公开引用的那一条）。**用户没给的来源不要自己去找**，缺材料先问。

**遗留（有意不做）**：
- 前置请求只支持 JSON 取值，不支持正则：令牌在 HTML 里的门户用「登录页原文 + 凭据变换脚本」表达更合适（脚本的 `ctx.page` 就是页面原文），不为它引入正则依赖。
- 不做多步链（A→B→C）：真需要多步的门户已属浏览器渠道的领域。
- 不做并发/重试：两次请求顺序发出，前置请求失败即终态。

## 直连请求渠道待办（2026-09-18 复核后新增）

> 背景：2026-09-18 对直连渠道（`src/login/http_login.rs` + `ProfileData.http_*`）做了一次缺口复核。
> 本轮已落地「HTTPS 证书策略可配」「UA 兜底」「响应头回显」「保存时校验体积」四项，见 `docs/changelog.md` 同日条目。

- **Cookie / 两步式门户**（本轮只补文档，未实现）：直连当前**只发一次请求**，`fetch_login_page` 抓到的登录页原文仅作为脚本 `ctx.page` 输入，其 `Set-Cookie` 不会带到登录请求，登录请求本身也不带 cookie jar（`Cargo.toml` 的 reqwest features 未启用 `cookies`）。
  需要「先取会话/令牌再提交」或依赖 Cookie 的门户因此无法直连，指南 §8 已如实写明边界。
  实现路径（若要做）：① 仅复用同源 Cookie——启用 `reqwest` 的 `cookies` feature，让页面抓取与登录请求共用 cookie jar，并让"无脚本也能抓页"成为可选项；② 完整两步请求——新增「前置请求」字段（方法/地址/请求头/响应取值提取），能力最全但需设计新契约与 UI，建议单独立项。**先定方向再动手**，两者成本差一个数量级。
- **判定只看响应体**：`success_pattern` / `failure_pattern` 仅对 body 做子串匹配（空成功关键字时回落 HTTP 2xx）。门户若用状态码或响应头（如 `Location`）表达成败则判定不了，需补响应头参与判定或允许按状态码判定。
- **HTTP 方法只有 GET/POST**（`HttpLoginMethod`）：PUT/PATCH 等少见但成本低，需同步 `frontend/src/utils/loginChannel.ts` 的 `HTTP_METHOD_OPTIONS` 与 `src/web/routes/profiles.rs` 中用 `"PATCH"` 断言 400 的既有用例。
- **测试端点与正式登录的 `local_ip` 来源不同**：正式走 `MonitorService::local_address()`（30s 缓存），测试端点自建 detector（`profiles.rs` 的 `test_http_login`）。两值理论上可能不一致，造成「测试通过但自动登录失败」的难排查组合，可考虑统一到同一来源。
- **登录历史不记渠道**：`LoginHistoryEntry` 只有 source/result/message 等字段，历史列表分不出某次是直连还是浏览器，只能从 message 文本辨认。若要按渠道统计/筛选需要加字段。

## 长期挂账（2026-08-26 核实修订）

- M1 trait 化已完成；Pinia 不适用（前端无 Pinia）；utoipa 手写 `openapi.json` 仍为前端 `typegen` 数据源，待 `utoipa` 宏化后自动生成；
- `AppState.container` 后续可评估移除。
- 定时任务「启动后执行」增强（2026-09-12，已上线固定延迟方案）：可选增强为等待监测服务首次网络连通后再触发（接 StatusManager watch，超时兜底强制执行），彻底消除"网络未就绪烧掉一次尝试"的场景。

> 本文件为唯一活跃计划入口，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。
