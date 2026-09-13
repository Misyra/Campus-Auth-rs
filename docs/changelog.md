# 更改日志

> 本文件记录每一次代码、配置、接口与文档更改，供开发和问题追溯；面向用户的版本更新摘要见 `docs/updatelog.md`。历史轮次继续保留于本文件，过时规划见 `docs/archive/`，活跃计划见 `docs/plan-next.md` + `docs/known-issues.md`。最新活跃为“v5.0.0-alpha.10”。

## 开发中（2026-09-13 P2 评审项批量修复）

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
