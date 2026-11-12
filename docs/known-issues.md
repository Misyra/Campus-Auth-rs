# 已知问题清单

> 本文档沉淀**仍然有效**的未修复项。
> **核实口径**：本文件已于 2026-11-13（重构优化战役第 9 阶段 T9-2）完成**全量对质**——全部条目逐条核对坐标与状态，坐标按战役结构重构（launcher→launcher/ 八模块、bridge→bridge/ 模块目录、environment/uv→uv/ 五子模块、playwright_worker 模块化、http_login 旧管线脱糖单路径、事件总线与通知服务等）后的现状刷新，失效条目已移除并注明去向；此后的新增/修订条目遵循「写入/修订时点对照当时代码核实」，并非持续全量复核——引用前请自行对质当前代码。注意：对质当口尚有错误文案人话化批次在途（`src/` 多文件未提交改动），行级坐标可能 ±数行漂移，模块与函数名锚点不受影响。
> 原审查报告（`docs/*review-*.md`）已删除，有效结论迁移至此；历史归档目录 `docs/archive/` 亦已于 2026-09-17 删除，其中材料不可追溯，已修复条目的记录见 `docs/changelog.md`（按版本归档，不在本文档保留已修复节）。
> 原 defect-recheck（76 条 v2）与 updater-audit（更新子系统 v2）过程报告已于 2026-09-12 删除；P0/P1 与更新子系统待办摘要见 `docs/plan-next.md`，与本文档互补。

---

## 一、实质问题（按严重性排序）

| # | 严重度 | 问题 | 位置 |
|---|--------|------|------|
| 5 | 🟢 低 | Profile 自动切换检测被 `is_any_pause_active` 门控——**2026-11-13 对质结论：确认为有意设计取舍，维持现状**（原「需确认」收口）。语义：暂停（手动暂停或定时暂停窗口）冻结引擎全部**自动**动作——周期探测、自动登录与自动切换检测一并停摆；手动切换入口完全不受限（`POST /api/profiles/switch` 同步落盘并派发 ApplyProfile），仅切换后的立即探测仍受 F4 门控（暂停窗口内「只切换不探测」）。维持理由：① 与 F4 既定暂停语义一致——暂停时段不应发生任何网络动作与登录；② 若暂停期放行自动切换，会静默更换活跃方案且不探测（通知去重还已被 `on_profile_switch` 重置），用户暂停结束后面对无迹可循的方案变更；③ 手动入口始终可用，不存在可用性损失。A7 墙钟化与 A35 run_loop 拆分均原样保留该门控 | `src/engine/run_loop.rs:379-387`（主循环四重门控）、`:1295-1300`（`is_any_pause_active`）、`:488-530`（`handle_apply_profile`，立即探测 F4 门控见 `:526`）、`src/web/routes/profiles.rs:923-940`（手动切换不受限） |
| 7 | 🟢 低 | 环境引导仅懒触发（任务执行 / OCR / AI / 监测 / 系统页 / 登录编排 / Worker 拉起），启动只做**只读探测**不自动引导 | 触发点：`src/tasks/executor.rs:202/:341`、`src/login/mod.rs:1337`、`src/bridge/lifecycle.rs:132-134`、`src/web/routes/ai.rs:258` / `monitor.rs:165` / `system.rs:954` / `ocr.rs:204` / `debug.rs:47`；引导本体 `src/environment/bootstrap.rs`（uv 子模块 `src/environment/uv/`）；启动只读探测 `src/container.rs:149-150`（2026-11-13 坐标刷新） |
| 15 | 🟢 低 | Profile 匹配无用户可配置 `priority` 字段（已按约束数降序确定性排序，抖动已修） | `src/config/profiles.rs:350-369`（`detect_matching_profile`，`strength` 见 `:362`、排序见 `:367`）（2026-11-13 坐标刷新） |
| 21 | 🟢 极低 | `ServiceContainer` 无 `Drop`：`new()` 现为 async + `Result`（失败点：ConfigService / MonitorService / SchedulerService / `startup()` 四处），其间散布 spawn 的常驻任务（环境探测 / 待应用更新 / Engine / uptime / 日志清理）在构造失败路径上不会被显式取消——launcher 的 `?` 直接传播到 `main` 的 `exit(1)`，已 spawn 任务（均为 Launcher 持有的应用级关闭令牌的 child，但失败路径不走 cancel）靠进程退出收尸。危害评估与原判一致：启动失败即进程死亡，窗口期空转而非永久泄漏；仅当未来启动流程变为"部分失败但进程存活"时才值得引入 Drop/回滚编排（child token 基建已就位，A3） | `src/container.rs:107-264`（失败点 `:123/:168/:202/:261`、spawn 点 `:150/:216/:234/:257-258`）、`src/launcher/mod.rs:297`、`src/main.rs:101`（2026-11-13 复核口径） |
| 22 | 🟢 极低 | 2026-09-13 P3 批量挂账（21 项，全部经复核确认为低危且有不修理由——已书面化的设计取舍、修复成本与收益不匹配、或依赖外部约定；逐项见下方 #22 注；**2026-11-13 已逐条复核**：⑦⑧ 与 MON-4-R2、NEW-1 已解决，其余维持原取舍） | 见 #22 注 |
| 24 | 🟢 低 | 历史裸 `.py` 脚本若 ID 含 `.`（`read_py_summary` 直接取 `file_stem` 当 id，不校验）则**改不动也存不下**：前端 `SCRIPT_ID_PATTERN` 与后端 `is_valid_task_id` 都不收 `.`，脚本面板的缺口恒非空、而 ID 字段落盘后是禁用的（「定时任务」按它引用脚本）。修法二选一：面板给一条"另存为新 ID"出口，或扫描时把非法 stem 规范化后再接受 | `src/tasks/loader.rs:1220-1221`（`read_py_summary`）、`:1309`（`is_valid_task_id`）、`frontend/src/utils/scriptDraft.ts:26`（2026-11-13 坐标刷新） |
| 26 | 🟢 低 | **改任务类型后本面板的编辑器会被对账逻辑关掉**：把浏览器任务 JSON 里的 `type` 改成 `http`（或反之）并保存后，任务换了桶、本面板列表里不再有它，`draftReconcile` 判定「曾经在列表里、现在不在」→ 关掉编辑器并提示「正在编辑的任务已不存在，已退出编辑」——任务其实存在（在另一个子页）。判据本身是给「任务被别处删掉」设计的，这里属误报；措辞与行为都待打磨（可选修法：保存成功后把该 id 放进短时豁免集合） | `frontend/src/utils/draftReconcile.ts:48-50`（判据）+ 四个面板（2026-11-13 复核未变） |
| 27 | 🟢 低 | **「保留配置与任务」不含 `python_worker/captures/`**：AI 生成任务的页面捕获产物落在这个目录（`.gitignore` 也把它当运行时产物），而「保留数据」只跳过 `DATA_DIR_NAMES` 七个**顶层**目录（config/tasks/logs/events/environment/browser_profiles/update——browser_profiles 为 2026-11-13 战役新增，captures 仍不在列），`python_worker/` 整体照删。属已知口径（与浏览器缓存同类），但 captures 更接近用户数据；要保留需在 `remove_children_except` 之外给嵌套路径开例外 | `src/uninstall/mod.rs:44-52`（`DATA_DIR_NAMES` 七项）、`src/ai/mod.rs:266-269`（`captures/latest`）（2026-11-13 坐标刷新） |
| 28 | 🟢 极低 | **「放弃新建」不撤回已经发出的请求**：自动保存那一发在路上时用户点「放弃」，草稿从界面消失，但那一发落盘成功后文件仍会被创建。接口层面无法撤回（要真取消得给四个面板各配 `AbortController`）；当前口径是文案不承诺「什么都没写」 | `frontend/src/utils/autosave.ts:195-196`（2026-11-13 复核未变） |
| 29 | 🟢 低 | **uv 下载的 SHA256 信任锚与二进制同镜像池，且按首字节延迟重排覆盖「直连优先」**：sha256 校验文件与 uv 压缩包来自同一个 URL 池（GitHub 官方 + 4 个社区代理镜像）且各自独立按延迟排序取首个成功响应——被投毒的镜像可同时替换两者使校验形同虚设；且社区镜像可能排在 GitHub 官方前面采用，信任面从 GitHub 扩大到第三方代理站。缓解：全链路 HTTPS、SHA256 校验本身有效（防下载损坏/部分劫持）。修法（2026-09-26 审计评估为「问题不大」暂不修）：sha256 仅走 GitHub 直连（失败再退镜像），或对 `UV_FALLBACK_VERSION` 钉一份编译期已知摘要作最后防线。与 #22 注④（更新链无代码签名，TLS + 同源 SHA256 属既有取舍）部分重叠，本条聚焦「校验锚与载荷同池」这一结构问题 | `src/environment/uv/download.rs:29-50`（archive/sha URL 构造同池，同源自 `GITHUB_MIRRORS`）、`:63-135`（首字节延迟重排，排序见 `:118`）、`:274-298`（SHA 文本按测速排序）与 `src/environment/uv/mod.rs:118`（压缩包排序）两处独立、`src/environment/mod.rs:29-34`（镜像池）（2026-11-13 坐标刷新：模块拆分后路径由 `environment/uv.rs` 变更为 `environment/uv/`） |
| 30 | 🟢 低 | **HTTP 直连渠道旧管线脱糖后的行为变化（成文取舍，随 2026-11-13 战役「旧管线脱糖收敛单路径」产生）**：① 带 `steps` 的流程任务配了下线动作（logout）从「静默跳过」改为「第一步前执行」——旧配置若依赖"流程任务不踢下线"会感知到；② 旧任务（无 `steps` 的加密脚本 / 前置请求 / 单请求形态）并入流程口径：单步 `min(20s, 总预算剩余)` 超时、120s 总预算、变量预算（单变量 8KiB / 累计 128KiB；旧管线脚本产出原无本地预算）、判定文案统一为流程形态（「步骤「login」…」）、脚本 `ctx.vars` 带累计变量表。老配置无需迁移、执行结果等价，但超时 / 文案 / 预算边界行为与旧版存在可感知差异（登录文案与排障时按流程形态解读） | `src/login/http_login.rs:6-23`（模块头时序）、`:53-71`（预算常量）、`:673-680`（脱糖口径文档）、`:4191-4195`（决策①对照测试） |
| 31 | 🟢 低 | **持久会话自动登录的 Worker `Success` ≠ 登录成功（成文语义，排障入口）**：`profile_auto_login` 的回放完成（门户回填 + 录制动作回放）即返回 Success；沉降窗口（load 态等待 + 固定沉降）均为 best-effort、不作为成败判定；登录成败恒由 Rust 侧登录后网络验证兜底（计划 `has_explicit_success_condition` 对持久模式恒 `false`）。用户若见「自动登录完成」而网络仍不通，以登录历史的网络验证结论为准，勿以 Worker 消息定性 | `src/login/session.rs:209-212`（计划文档）、`:240-248`（成功判定恒走网络验证兜底）、`python_worker/auto_login.py:26-36`（沉降窗口 best-effort）、`src/bridge/mod.rs:93`（命令级预算 300s） |
| 32 | 🟢 极低 | **前端持久会话配置横幅不跨刷新恢复**：配置会话的 start/stop 是跨进程的后端状态，前端横幅只是观测——页面刷新后横幅即丢而后端会话仍在，此时再点「打开浏览器配置」会拿到 409（占用可能来自另一页面/另一方案）。组件已给统一行动指引（关闭已有配置浏览器再试）；根治需后端暴露会话状态查询供前端恢复横幅，属已知取舍 | `frontend/src/views/profiles/PersistentSessionSection.vue:10-11`（限制说明）、`:78`（409 固定文案） |

> 76 条清单的 P0/P1 已对质收敛（9 条 P0/P1 全部属实，待排期；摘要见 `docs/plan-next.md`），本文档不再重复展开；更新子系统 10 项摘要同见 `docs/plan-next.md`（含 `Stable/Prerelease/All` 通道与 `update/last_check.json` 相关项）。
>
>
> #22 注（P3 批量挂账，2026-09-13；**2026-11-13 战役逐条复核**，已解决项就地标注）：① **WEB-2** PATCH 的 other_patch 未知键可合并顶层 settings——收紧为白名单是行为变化，老客户端会开始报错（现口径未变，`src/web/routes/config.rs:220`）；② **WEB-5** 反馈包/日志导出含原文（凭据若曾入日志随包泄露）——行级脱敏难穷举字段且易掩盖排障信息；2026-11-13 战役已新增脱敏出口（`GET /api/diagnostics/bundle` 白名单收集 + 深度脱敏配置快照，`src/web/routes/diagnostics.rs:70-75`），`POST /api/debug/feedback-bundle` 仍含原文日志尾段与任务 JSON，取舍维持；③ **COR-4** BroadcastLayer 推 WS 无字段级脱敏——同 ② 取舍（`src/logging.rs:453-461`）；④ **UPD-4/ENV-8** 更新链与 uv 下载无代码签名（TLS + 同源 SHA256）——签名体系需发布基础设施与密钥管理，代码注释已表明"不上签名但不降级"的有意选择，uv 官方亦无签名资产；⑤ **COR-3** 控制台层同步 stderr——改非阻塞引入丢日志窗口与退出 flush 时序问题，CLI 场景实时性更可预期（`src/logging.rs:540`）；⑥ **BRG-3** IPC 行上限 Rust 1MiB / Python 16MiB 不一致——统一 16MiB 抬高行缓冲内存峰值，统一 1MiB 需大载荷分块协议（`src/bridge/mod.rs:103` / `python_worker/worker_main.py:55`）；⑦ **BRG-5** bridge/mod.rs 约 2300 行职责过宽——**已解决（2026-11-13 战役）**：按职责拆分为模块目录 `src/bridge/`（api/debug/error/ipc/job/lifecycle/orphan/process/session/state/worker），mod.rs 收敛至约 1400 行；⑧ **ENV-9** uv.rs 约 1700 行上帝模块——**已解决（2026-11-13 战役）**：拆分为 `src/environment/uv/` 五子模块（download/process/sync/verify/mod）；⑨ **WE2-7** run_script 直跑路径不复检 binary 黑名单——复检会拒绝历史已存任务，属行为变化（`src/web/routes/scripts.rs:115-137`，黑名单复检在 `src/tasks/loader.rs:725-728` 的任务校验路径）；⑩ **TSK-4** 解压 dest 未 canonicalize——纵深防御缺口而非现行漏洞，Windows `\\?\` 前缀与 junction 语义坑多（条目名 zip 本体已有 `enclosed_name` 穿越过滤，`src/utils/io.rs:165`）；⑪ **TSK-6** is_own_process 子串匹配 + 260 固定缓冲——精确基名比较需先约定各发行形态 exe 名单（`src/utils/lock.rs:179-196`）；⑫ **TSK-8** 解压到字面名 `campus-auth.zip` 目录被误拦——WinRAR"解压到 campus-auth.zip\"形态，有 `--allow-temp` 逃生通道（`src/utils/archive_guard.rs:30-31`）；⑬ **FE1-5** 单例 composable 测试依赖用例声明顺序——依赖关系已有注释自认，隔离改造收益低（`frontend/src/composables/useConfig.test.ts:5` 等）；⑭ **ENG-4** test_network 诊断任务未绑取消令牌——任务受自身超时约束无共享状态副作用，属良性；A23 已补并发上限与名额守卫（`src/engine/run_loop.rs:532-611`），取消令牌仍未绑；⑮ **ENG-6** SchedulerService::start 二次调用 panic——全仓调用点唯一，`.take().expect` 作内部不变量金丝雀合理（`src/scheduler/mod.rs:214-227`）；⑯ **UPD-8** helper 以 PID 轮询等待主进程退出存在 PID 复用 TOCTOU——5.3 起超时已 fail-safe（报错退出保留 staging）（`src/helper_main.rs:198-205`）；⑰ **ENV-6** site-packages 候选路径硬编码 `python3.12`——PYTHON_VERSION_CONSTRAINT 已锁 `>=3.12,<3.13`，约束放宽时才会失准（`src/environment/mod.rs:96`）。⑱ **MON-4-R1** 门户检测跟随跳转的**同 host 豁免**存在 DNS rebinding 变体：首跳解析到公网承载 302、二次解析指向环回时，因 `same_host` 短路而不做目的地址校验。低危——盲 GET 无凭据、无外带通道、能改写 DNS 者本已处于 MitM 位置，且链路中任一跳已建钉扎客户端时被 `pinned` 复用天然阻断；要收紧须对同 host 也逐跳解析校验（代价是校园门户自身的相对路径跳转会反复解析）（`src/monitor/portal.rs:267-286`）。⑲ **MON-4-R2** 域名钉扎只取 `addrs[0]` 无多地址回退——**已解决（2026-09-30，早于战役）**：钉扎改为传入全部已校验地址列表（`ClientBuilder::resolve_to_addrs`，`src/monitor/portal.rs:359-369` / `:377-389`），双栈多地址回退恢复。⑳ **MON-4-R3** 钉扎端口粘滞：跨主机跳转把解析结果写入 client 后，同 host 的后续隐式端口跳转仍复用该 client（hyper-util `set_port` 语义），实际连接端口可能与上报 URL 不一致——仅"探测端口与上报 URL 不一致"，无 SSRF 扩面（`src/monitor/portal.rs:225-229` 复用、`:267-286` 同 host 不重钉）。另：**NEW-1** LoginSource::Browser 文档与实现矛盾——**已解决**：注释口径修正已收口（`src/status/snapshot.rs:63-68`，明确其为活来源而非历史遗留）。
> #23 注（便携版实测，2026-09-13）——**2026-11-13 对质：两项均已解决，条目按本文档惯例移除**（记录见 `docs/changelog.md`）：① **E2** 手动「运行」toast 文案 2026-09-23 已修（「已触发执行，结果见「执行历史」」，`frontend/src/composables/useScheduledTasks.ts:318`）；原「失败时不主动补发通知」的残留亦由 2026-11-13 战役通知服务收口——任务失败可经 `task_failed` 事件可选推送（`src/config/schema.rs:489-499`，任务终态发布于 `src/tasks/executor.rs:408-415`）。② **E3** 「最近结果」列不随成功即时刷新——2026-09-29 已修：手动运行后按任务超时封顶轮询 `last_run`/`last_result` 并即时刷新列表与历史（`useScheduledTasks.ts:349-370`）。

---

## 二、工程化缺口

| # | 问题 | 说明 |
|---|------|------|
| E3 | 测试短板 | 核心模块已补测试（`web/routes` handler 层、`environment/`、`scheduler`、tray 纯函数、`python_worker` pytest、`engine::slot` 等）；全链路 `tests/login_chain.rs`（mock→二进制→Worker→success/failonce）已接入 `e2e-login-chain` CI，`rust-tests-unix` 双端齐跑；fuzz 已接入（`cargo-fuzz` 每周冒烟 + IPC/配置/迁移三个 target）与核心域覆盖率门禁；剩余偏少项不再单独列举。**规模数字不再在此维护**（历史"127 项 / 约 659 项 / 约 10 文件"均已过时），需要时用 `cargo test -- --list`、`uv run pytest --collect-only -q`、`npm test -- --reporter=json` 现取 |

---

## 三、三端兼容性（Windows / macOS / Linux）

> 2026-08-30 全量审计的 16 项（W1–W16）中 12 项已于同日修复并归档至 changelog（第十六轮）：W1 uv 资产格式、W2 venv 路径、W3 解压权限位、W4 更新器 tar.gz、W5 force_kill、W7 unix 卸载脚本、W10 CI unix job、W11 SIGTERM/SIGHUP、W12 进程组终止、W14 __pycache__、W15 备份名、W16 .bat 拒绝 + XDG 环境变量。
> W6（托盘）按用户决策收敛：**macOS 托盘禁用**（`TrayManager::spawn` 内单点拦截返回空句柄，轻量模式自动降级完整模式，Web 控制台 / `--stop` 仍可用），Linux 侧已修复（托盘线程内 `gtk::init` + glib 主循环，与 `tray-icon` 内部 `gtk 0.18` 同版本，待真机验证），不再跟踪；主力平台为 Windows。
> 剩余未修项如下（均为有指引或降级方案的低危）。

| # | 严重度 | 问题 | 位置 |
|---|--------|------|------|
| W8 | 🟢 低 | Linux 二进制动态链接 GTK3 / libayatana-appindicator / librsvg（托盘代价），无桌面发行版起不来；运行时依赖与安装命令已写入 Release 发布说明 | `.github/workflows/release.yml` |
| W9 | 🟢 低 | macOS 未 codesign / 公证，浏览器下载后带 quarantine 被 Gatekeeper 拦截；`xattr -cr` 解除指引已写入 Release 发布说明，真签名需 Apple 开发者证书 | `.github/workflows/release.yml` |

---

## 四、更新通道相关（与Updater审计联动）

- 通道语义（`src/config/schema.rs::UpdateChannel` 三通道与回退规则）、代理收敛 `resolved_proxy_url`、状态落盘 `update/last_check.json`（`GET /api/update-state` 回放）以 `AGENTS.md`「更新通道」一节与 `src/updater/` 实现为准；更新子系统审计待办摘要见 `docs/plan-next.md`。

> 历史已修复条目已归档至 `docs/changelog.md`；活跃计划见 `docs/plan-next.md`。
