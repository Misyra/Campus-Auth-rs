# 后续计划（活跃入口）

> R/F/G/A/B 组已在 `docs/changelog.md` 第十一~十六轮落地并归档；历史归档目录 `docs/archive/` 已于 2026-09-17 删除，其中材料不可追溯。
> 当前活跃：`v5.0.2`（`docs/changelog.md`）。P0/P1 与更新子系统待办的权威摘要以本文件与 `docs/known-issues.md` 为准（原 defect-recheck / updater-audit 过程报告已于 2026-09-12 删除）。
> 验证块：`cargo clippy --all-targets -- -D warnings` / `cargo test` 双 feature（含 `rust-tests-unix`）/ `cargo fmt --check` / `uv run pytest` / `npm run build` + `vitest` / `e2e-login-chain` 全链路。

## C 组收尾（已于 2026-08-26 落地，非活跃待办）

> 原计划出自 `docs/review-2026-08-24.md`，该文件已于 2026-08-31 随 `f015e41` 删除。**四个批次均已落地**，结论完整保留在 `docs/changelog.md` 第十二轮（`:1524-1551`）；本节仅留索引供追溯，不再列为待办：

- **批次五：A-4 孤儿清理降频 + 防护** — `changelog.md:1528-1531`（否决 Toolhelp32 替换，改为降频 + 5s 超时防护；commit `af249e1`）
- **批次六：S 级小尾巴五件** — `changelog.md:1533-1539`（`PerProbeDetail::new`、scripts 单次读盘、删 `SchedulerApi::history_dir`、`parse_host_port("::1")`、IconApp 收敛；commit `11c2233`）
- **批次七：A-5 system.rs 按域拆分** — `changelog.md:1541-1544`（`routes/background.rs` / `routes/uninstall.rs`，关闭 `state.container` 旁路；commit `207d9cd`）
- **批次八：B3 根治（调试会话存活期纳入槽位）** — `changelog.md:1546-1550`（commit `5505faa`）

## v5.0.0 之后（待排期，按复核优先级）

- **必修（P0，9 条）**：Bridge 槽位/取消注册表、录制器多 frame、任务创建 `..Default` 空 steps 等（原 76 条 v2 对质结论，报告已删）。
- **强烈建议（P1，约 12 项）**：`wait` 三方矛盾、`timeout` 钳制、`evaluate` 关页、别名漂移、`select` 空值、LLM `finish_reason`/`max_tokens`/重试、更新后 `uv sync`、错误映射 500/404、Esc 脏状态、前端保存防连点与 `validateConfig`/`cron` 校验。（`shell` 类型静默失效已解决：`type=shell` 现于反序列化明确报错并提示改用 `script`，见 `src/tasks/models.rs`。）
- **更新子系统（4 项 P2 + 3 项 P3）**：A 正确性（base 分离与 helper 落点）；B 体验（停滞超时与配额 403/429）；C 接口（pin 版本闸门）；D 清理（double-fire 与 `available` 语义）。
- 已对齐项：`VALID_STEP_TYPES` 含 `evaluate`/`custom`（本轮已对齐 `task-writing-guide`）、`UpdateChannel` 三通道与 `update/last_check.json`（`check.rs`/`mod.rs` 已实现，本轮补文档）、CI 含 `e2e-login-chain` + `rust-tests-unix`。

## 2026-09-15 全仓审查后的剩余项（本轮已修 4 P1 + 8 P2 + 3 P3，见 `docs/changelog.md`）

未修项及理由（均为**已确认但本轮有意不改**，非遗漏）：

- **`openapi.json` 无字段级契约**（原报告 P2-4，含 P2-8 的各类表现面）：spec 当前有 **88 个路径**（`/api/*` 87 + `/ws/logs`，operations 合计 103）但 `components.schemas` 为 0，除 `/api/ai/capture/status` 的 200 响应带真实 schema（`openapi.json:3490-3499`）外，其余响应 schema 为 `{}`；`openapi_json_matches_route_table` 归一后**只比对「方法 + 路径」集合**，故字段名/类型、状态码、媒体类型、路径参数四类漂移无任何自动拦截。已抽查确认**当前一致**（`AssessmentReason` 10 变体、`AssessmentConfidence` 3 变体、`RecoveryAdvice`、`LocalLinkState`、`strict_login_mode` 默认值两侧一致）。属已书面化的取舍（待 `utoipa` 宏化），本轮不改为不引入半成套的生成链路。**廉价替代方案**：补一个「Rust 侧序列化全部枚举变体 → 与 `types.ts` 文本比对」的双向一致性测试，因枚举漂移最易发生且前端 `switch` 有 `default` 兜底、不会报错、只会静默显示"待确认"。
- **抢占总在取消分支不等旧会话收尾**（原报告 P2-11）：`src/login/mod.rs` 的取消分支 `return self.cancelled_handle(...)` 使已 `take()` 出的 `old` 被 drop，`wait_old_session_finished` 不执行。已核对：`submit_gate` 在整个 `submit` 期间持有，故**不会**造成空窗插队；代码注释表明"等待期间点取消即放弃本次提交"是**有意设计**。若需收紧，可让取消分支同样等 `old.finished`（代价：点取消后最多再等 18s）。
- **`useConfig` 保存期间的编辑被折进快照**（原报告 P3）：`:212-235` 构造载荷 → `await patch` → 用**当前** `config` 写 `savedSnapshot`；PATCH 在途期间的编辑会被当作已保存基准并置 `dirty=false`，实际未提交。修法明确（在 `await` **之前**取快照），本轮未做——需同步调整既有 4 个 dirty 快照用例，改动面超出本轮范围。
- **`TrayAction::Quit` 无退出 watchdog**（原报告 P3）：托盘退出只做 `deps.shutdown.cancel()` + dispatch，而 Web 退出与定时自重启都有 `spawn_exit_watchdog`。属不一致；原报告中"托盘线程可能无界阻塞 `join`"已被证伪（Windows 循环最多 50ms 回到循环头，无 `TrackPopupMenu`），故风险仅为"未来若托盘线程引入阻塞逻辑则无兜底"。
- **`docs/known-issues.md` 的全面对质已完成**（2026-09-17）：#2 / #7 / #3 / #15 / #19 已核实并修正引用坐标，#19 判定为已修（校验收敛至 `src/web/ssrf.rs`），"低危清理项"两条判定不成立并已清空；**#22 注（21 项 P3 挂账）与 MON-4-R1/R2/R3 仍未逐条复核**，状态未知，引用前需自行对质。
- **未覆盖的验证面**：Unix 平台未实跑（CI `rust-tests-unix` 覆盖）；未做真实校园网门户验证（需实际门户）；未做人工渗透测试（安全结论基于代码审查与既有负向用例）；`updater::apply_pending_on_startup` 是否响应 shutdown、`client.ts` 的 `ensureAuthToken` 占用超时预算、`useScheduledTasks` 列表写入无 epoch 三项存疑未验证。

> 另：原报告将 P1-4 归因于「登录收尾的 `close_browser` 关掉定时任务的浏览器」，经复核**该机制不成立**（Python `_serve` 是严格串行命令循环，`close_browser` 必须排队），真实破坏点是 `force_recycle` 无条件强杀 —— 本轮按后者修复。记录以免后来者按错误机制重复排查。

## 直连任务重构 + 直连任务仓库（2026-09-22 **已落地**）

> 用户诉求：①「直连请求」的参数编辑从方案编辑器里挪到任务页，成为独立的**直连任务**；② 直连任务能进**仓库**分享（和浏览器任务一样，别人上传后可导入）。
> 已定方向（用户三选一确认）：**独立具名任务，方案里选一个**；仓库里**只分享直连任务**（不含账号密码）；**复用现有任务仓库**（同一索引，多一类条目）。
> 七个阶段全部完成，逐项实现与验证见 `docs/changelog.md` 同日条目「直连请求重构为独立「直连任务」」+「任务页新增「直连任务」Tab」。

**模型**：直连任务 = `TaskKind::Http(HttpTaskConfig)`，落 `<base>/tasks/http/<id>.json`，`type: "http"`；字段是「请求形状」（method / url / auth_url / headers / body / success_pattern / failure_pattern / crypto_script / pre_request / ignore_https_errors / metadata），**凭据仍属方案**（`username` / `password`）。方案新增 `active_http_task`，语义与 `active_task` 对齐（空 = 未绑定）；直连没有可内置的通用门户地址，故**没有默认任务**，未绑定时登录直接给出明确失败原因（浏览器渠道有 default 回退，直连没有，这是有意的不对称）。认证地址两边都有：**任务优先、留空回退方案的**。

**遗留小项（有意不做）**：
- **直连任务不参与拖拽排序**：`POST /api/tasks/order` 的载荷只含浏览器任务与脚本 id，`order_tasks` 会 `retain` 只保留提交过的 id —— 故 http 任务的 id 不进 `.order.json`（迁移写进去的那条会在用户下次拖拽后被清掉），列表顺序回落为后端扫描顺序。不丢数据、不影响功能；若要做到可排序，需要把 http id 一并纳入 order 载荷（改浏览器面板与后端 `order_tasks`）。
- **`TaskExecutor` 对 http 是占位拒绝**：直连任务不经 Python Worker，`/api/tasks/{id}/execute` 与定时任务的 task_id 对它返回 400「执行接入属后续阶段」；它的验证入口是「发送测试请求」（任务编辑器内）。若将来要做「定时跑一次直连任务」，替换 `src/tasks/executor.rs` 的那两条臂即可。
- **旧分享文件里的直连配置会被忽略**：v9 及以前导出的方案 JSON 带着方案级 `http_*`（含脚本），导入时忽略并在响应里回报 `legacy_http_config_dropped`，前端提示用户去任务页重新配置。若要自动搬运，可在导入路径复用迁移里的 `build_legacy_http_task`。

**风险与取舍**（已书面化）：配置迁移是**单向**的（旧版本读到 v10 配置会忽略未知字段，但 `http_*` 已空）；导入他人方案时 `active_http_task` 一律清空（与 `active_task` 同口径），故分享方案不再携带直连配置——这正是把直连任务独立出去的意义。

**对外依赖**：任务仓库（`Misyra/campus-auth-tasks`）按新约定收录 `type: "http"` 的条目即会在「直连任务」子页的仓库导入里出现；老条目缺 `type` 按 `browser` 处理，不受影响。**2026-09-23 起有第一条 `type: "http"` 条目**（`haust`，河南科技大学大学掌体系 CSRF），索引新增可选字段 `source`（来源仓库，导入弹窗渲染成链接）与 `type` 分流的说明。

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
