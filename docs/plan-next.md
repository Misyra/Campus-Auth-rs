# 后续计划（活跃入口）

> R/F/G/A/B 组已在 `docs/changelog.md` 第十一~十六轮落地并归档；历史归档目录 `docs/archive/` 已于 2026-09-17 删除，其中材料不可追溯。
> 当前活跃：`v5.2.0-beta.2`（`docs/changelog.md`）。P0/P1 与更新子系统待办的权威摘要以本文件与 `docs/known-issues.md` 为准（原 defect-recheck / updater-audit 过程报告已于 2026-09-12 删除）。
> 验证块：`cargo clippy --all-targets -- -D warnings` / `cargo test` 双 feature（含 `rust-tests-unix`）/ `cargo fmt --check` / `uv run pytest` / `npm run build` + `vitest` / `e2e-login-chain` 全链路。

## 全面审查修复战役（2026-11-21，八批全落地，**终检收口中**）

> 同日全面审查（报告 `docs/reports/code-review-2026-11-21.md`，本地不提交）发现的 P1×1 + P2×15 + P3×43(+1 nit) 全量修复 + 安卓 P1 前置①②③ + uv 哨兵提级项；八个域子代理并行（录制链 Python / 录制契约 Rust / Bridge 基座 / macOS 托盘与 launcher / Rust 杂项 / 前端 / 安卓 / CI 构建），逐项独立 commit，changelog「全面审查修复战役」分节集中记账。

- [x] **P1 录制明文密码双层防线**：Worker 启发式（autocomplete 密码语义 + WeakMap 首判锁定，84b0e22）+ Rust stop 结算等值归一（原文 + URL 编码恒等替换占位符，06f8eea）+ 诊断包脱敏登记 recorded_actions（703e27f）——三条扩散路径（方案落盘 / 任务仓库外发 / 诊断包）全封堵
- [x] **录制链 P2 五连修**：空闲回收豁免（c9ec922）/ iframe frame 契约三侧贯通（a5663f6 + 04fc505）/ shadow DOM composedPath（bc344cb）/ 点导航丢 pendingFill（5d7accc）/ starting 孤儿会话按轮次记账（2e17a06）
- [x] **macOS 系统托盘启用（f5cc2e1，架构批，known-issues W6 收口）**：主线程交还 AppKit + 托盘经 GCD 主队列投递；真机冒烟退出链 SIGTERM 1s 零残留（`NSApp.stop` 无事件唤醒不生效的真机发现与 exit 收法见 changelog）——**人工目检完成（2026-11-22）**：功能全过（图标常驻 / 左键控制台 / 右键菜单 / Dock 不出现，`lsappinfo` type=UIElement 佐证）；图标观感按反馈「有点灰」收口为模板渲染（be74a7c，浅色黑 / 深色白，停止态不再灰化）；轻量模式 macOS 恢复留后续
- [x] **回环免引导连接风暴根治（68641ba）**：WS 鉴权门 + `onAuthReady` 事件 + unauthorized 诚实分类（不再弹「请检查后端」误导 toast）+ 状态轮询降级复位
- [x] **CI 两项假防线**：clippy --all-targets 必红收口（54a0a31，验证口径教训）+ vitest 覆盖率门禁接通（be130fe）
- [x] **安卓壳层四项 + 前置①②③**：restart/shutdown android 门（d02e0c2）/ WebView 文件选择与下载含 blob（684f590）/ local-token 安卓下架 + `nativeBootstrapSecret` fragment 引导（a49fb36）/ FGS nativeStop 异步可重启（7b6aac8）/ 密钥路径挂 `CAMPUS_AUTH_BASE_DIR`（ac73d85）；BootReceiver 双状态源④按计划留 P2
- [x] **其余 P3 全量**：IPv6 剥括号（43acd2e）/ Docker TZ 文档（913b97f）/ XDG 转义与 Linux 观测（d9ad14f + b11a2eb）/ App Paths 注册表（b317e20）/ 通知 AUMID（e9d1bbb）/ uv 哨兵（a19bde4）/ 诊断脱敏等，逐项见 changelog 同名分节
- [ ] **终检与复验**：统一门禁全绿（fmt 已收口 56b2e1e；android target / 前端含覆盖率门禁已过）+ clippy --all-targets 与全量 cargo test 收口中 + 实例复验（连接风暴复测、录制弹窗回归）+ 托盘目检完成（2026-11-22，观感反馈已修 be74a7c）；CI 全矩阵（含 e2e-login-chain / rust-tests-unix 真机）待 PR 触发
- [ ] **日志按日文件名 UTC/本地错位（2026-11-22 实机发现，P3）**：`tracing_appender::rolling::daily` 文件名取 UTC 日期（rolling.rs:198 `OffsetDateTime::now_utc`）——02:18 CST 启动仍写 `app.log.2026-11-21`，而 `is_active_log_file` 按**本地**今天比对（logging.rs:294），本地 00:00–08:00 窗口内正在写的活跃文件被误判可清理（过期/配额清理可能删在写文件）；文件名日期标签对 UTC+8 用户恒滞后一天。修法方向：活跃判定改与 appender 同源的 UTC 口径，或清理守卫直接比对当前 writer 持有的目标文件名

## 录制生成独立任务（2026-11-21 立项，**已落地**）

> 用户诉求：「录制能不能做到填入账号密码？能做到就加上去，改成独立的任务形式，用户只需要完整录制内容」。拍板口径：录制升级为**完整操作录制**（含键盘输入），并可一键转换为**标准浏览器任务**（独立任务形式：可手动跑 / 定时调度 / 绑定方案），凭据永不落明文——密码框录 `{{PASSWORD}}`、用户名框录 `{{USERNAME}}` 占位符，运行时经现有变量解析管线注入。

- [x] **Worker 录制器 + 回放升级**（258192b，python_worker/config_session.py + auto_login.py）：注入脚本新增 `fill` 捕获（密码框值恒替换 `{{PASSWORD}}` 绝不落真实值；用户名启发式 → `{{USERNAME}}`；其余字面量；同 selector 防抖合并取最终值）与 `navigate` 导航标记（连续重定向合并最终落地页）；RecorderSink 放行新词表；回放侧支持 fill（locator.fill）与 navigate（goto + 沉降等待，首条与 portal_url 同值跳过），占位符经凭据映射以 `username`/`password`/`isp`/`auth_url` 参数键解析（profile_commands 接线 build_replay_variables）
- [x] **Rust 凭据下发 + stop 契约升级**（8ce4a68）：`build_persistent_login_params` params 增加四键（与 execute_browser_task 同名同信任边界，「凭据不下发」口径改写）；stop 端点响应携带 `recorded_actions` 动作数组（informational 回显）+ 新增 `apply=false` 结算参数（仅关浏览器取回动作、**不写方案**——任务录制零副作用）；RecordedAction doc 补词表（type 刻意 String 非枚举，零结构变更）
- [x] **前端录制生成任务**（b695348，views/tasks/）：任务页「录制新任务」三阶段弹窗 → 选方案 start(record=true) → 完整操作 → stop(apply=false) 直取响应动作数组 → 转换器（click→click / fill→input / change→select / navigate→navigate，占位符原样透传）→ 预览（**可单步删除**）+ 命名保存为浏览器任务；RecordedAction 判别联合扩展
- [x] **录制悬浮面板（追加批，2026-11-21 用户提出）**（258192b）：录制浏览器内注入可折叠悬浮面板——实时显示跨页权威全量动作列表（binding 回包即回程通道）、单步删除（未上报清缓冲、已上报走 `__delete__` 控制事件按身份删除、不进输出）、两层自防（面板根 stopPropagation + 录制器 closest 拦截）；两录制用途共用；前端预览列表同步可删
- [x] **验证**（2026-11-21）：pytest 462 / cargo test --lib 1477 + clippy 零警告 / vitest 811 + typecheck + stylelint 全绿；demo 门户端到端实测全通——录制 6 动作（2 navigate + 2 占位符 fill + change + click）→ 预览删 2 步 → 保存 4 步任务（`{{LOGIN_URL}}` + 占位符，无明文凭据）→ 立即运行回放 1.66s 成功（变量注入生效）；方案侧 `recorded_actions` 确认未被写入（apply=false 生效）。**途中修复**：init script 执行时 body/documentElement 均可能为 null，面板 appendChild 抛错打断脚本 → navigate 上报全部丢失（挂载推迟 DOMContentLoaded）+ navigate 上报短轮询等 binding 注入

## 代码审查收尾遗留（2026-11-19）

> 2026-11-19 全面审查（报告 `docs/reports/code-review-2026-11-19.md`，本地不提交）当日已修：P1 run_loop 内联探测、全部 12 项 P2、大部分 P3（逐项见 changelog 同日四节）。仍开放的低优先项：

- [ ] **断点 token 化**：全仓 14 种媒体查询值（层文件 640/768/900/1100/360，scoped 又引入 600/700/850/940/1280/1800/280/400/480；HttpFlowFields 单组件 5 种）——已并入「前端设计系统收口战役」阶段 1①（2026-11-20），完成后此处一并勾销
- [ ] **`run_flow_once` 拆分**：`src/login/http_login.rs:727-1125`（399 行），Transform / Request 分支各拆私有函数；同批评估 `routes/{profiles,config}`（3.6k 行级）与 `http_login`（4.4k 行）按 bridge 14 子模块先例的下一轮文件拆分
- [ ] **multipart「找 file 字段」骨架上提**：`routes/system.rs` 与 `routes/background.rs` 两处（本轮三组重复已上提 `web/validate.rs`，此组涉上传路径单独评估）
- [ ] **helper overlay 分发顺序统一**：`helper_main.rs` overlay 分发先于 exe 备份/替换 vs `apply_pending_locked` 先替换——两处顺序统一并注明取舍（现状有 pending / RESYNC 兜底自动收敛）
- [ ] **Python 测试替身迁移收尾**：`tests/fakes.py` 已收敛六替身、`test_worker_robustness.py` 已迁；`test_worker.py`（19 类）与 `test_worker_execution_contract.py`（21 类）待迁（清单在 fakes.py docstring）
- [ ] **stylelint-vue 新增 warning 清零（可选）**：.vue 进门禁后新增约 20 条 no-descending-specificity（7 文件，`:deep` 等 Vue SFC 伪类已按 overrides 放行），与既有基线同 severity 暂存
- [ ] **tokio feature 防回归钉（可选）**：ci.yml 加 `cargo tree -e features -p tokio` 快照比对
- [ ] **ffi_android TODO(P1) 收敛**（P0 骨架→正式模式）：见下节「安卓端开发」P1 条——nativeStatus 占位混淆、tracing 单次初始化、实例锁 / engine_watch / 自启语义、LogOnce 决议、日志对齐 settings；收敛前不合并 master
- [ ] **release.yml 嵌钥上传与签名清单非原子（P2）**：build 六矩阵各自 `gh release upload` 与 manifest job 签名清单上传非原子——`--sign-manifest` 失败时已公开的嵌钥版本按 SignedManifestMissing 拒一切更新直到下个成功发布；manifest 失败应视为发布失败（考虑 `gh release delete`）+ 维护规程要求配置 secret 前本地 `--gen-signing-key` 核对公钥一致
- [x] **uv 探测哨兵索引 panic（P2）**——已修（a19bde4，2026-11-21 修复战役）：`rank_mirror_urls` enumerate 回收真实 idx + `order_from_probe_results` 越界过滤（三个调用方单点收口），哨兵 `usize::MAX` 与越界成功样本防御测试钉住
- [ ] **python_mirror_ladder 直连殿后口径成文（P2）**：`uv/sync.rs:60-67` Auto 分支把直连固定殿后，与 mirror.rs 文档「Auto=实测吞吐自动选最优」及压缩包下载 Auto 口径分叉——注释声明取舍或改完整实测序
- [ ] **android 杂项（P3）**：jniLibs 仅 arm64-v8a（模拟器冒烟需补 x86_64）、WebView 停留判定收紧 scheme+host 且覆盖 localhost/::1、ACCESS_NETWORK_STATE 权限无使用点可删、`login/mod.rs:706` 渠道不支持文案按构建形态分派（现写死 OpenWrt）
- [ ] **CI/供应链杂项（P3）**：release.yml `packages: write` 下沉到 docker 两 job、公钥轮换流程成文（或不轮换取舍）、audit.yml 两个 uvx 步合成一步防工具版本漂移、drag.test 补过滤态对照用例、background symlink check-then-use TOCTOU 补注释声明
- [ ] **文档刷新（部分本轮已做）**：known-issues #29/#22④ 已就地刷新、AGENTS 与 openapi.rs「两端点」已改三端点；仍待：architecture.md 补 web/validate.rs 与 ffi_android 行、根 README.md 补 android/
- [ ] **脏树分批提交**：工作区 113 文件未提交（本日审查修复 + 既有 UI / 镜像批次混载），按「一次 commit 只做一件事」分批提交——已并入下方「前端设计系统收口战役」阶段 0

## 前端设计系统收口战役（2026-11-20 立项）

> 用户诉求：「目前的 UI 有点乱」（视觉不统一 + 代码难维护两头都有），并预告后续做手机（安卓 WebView 复用同一前端）。拍板（2026-11-20）：**不引入组件库**（Element Plus / Naive UI 一类与现有 token 体系、内嵌体积（730 KB JS / 188 KB CSS 已审计合理）、a11y 投入与 700+ 测试面冲突，维持 2026-11-17「桌面不引重库」拍板），走「token 收口 + 组件层升级」。病根实测：断点 14 种散值（768×11 / 640×10 / 1100×6 / 900×4 + 960/940/850/560/280/400/480 各自为政）、token 逃逸 72 处（ai_task 36 / profiles 21 / tasks 15 + UpdateDialog 15 处字号字面量）、同一视觉角色多套平行实现（两套日志色、`.page-content` 名为外壳实非外壳、`badge--mono` 依赖倒置）、styles/ 24 文件 7784 行（tasks.css 1076 行）。手机视角：地基件（dvh / overscroll / 触控目标 ≥24px / 底部导航）已齐，缺的正是统一响应式契约与页面外壳。2026-11-20 追加拍板：**重构彻底一点、不留技术债，之前挂账的问题本次统一清掉**——阶段 3 清债项全部转正、设置页 tab 化重建同步引入 schema 驱动混合架构（T8-1 转正，settingsSearch 的 DOM 抓取耦合一并退役）。

- [x] **阶段 0 前置卫生**（2026-11-20 完成：17 批分批落库 9c584d28..952b7a52，工作区清零）
- [x] **阶段 1 token 硬收口**（2026-11-20 全部完成）：
  - ① 断点收敛：实测仅 3 处真散值（850→900 / 700→768 / 600→640；台账中 940/1280/280/400/480 实为元素宽度非断点），四档 `--bp-*` + index.css 约定清单 + breakpoints.test.ts 守卫（047cffd2）
  - ② 日志级别色合一：徽标侧并入 `log-*` 单一体系，dashboard.css 零改动（d14a06b）
  - ③ token 逃逸清零：68 行等值回填，±2px 归并 47 值列目检清单（10f92ca）
  - ④ `badge--mono`：核实 T8-3 已迁移、无需动作
  - ⑤ stylelint 42 条警告全清零 + `--max-warnings 0` 门禁钉死（dec4803）；1c 建议的 12/6/2px 档位已由 V2 地基批落定义
- [x] **阶段 2 组件层升级 + 页面外壳**（a1223d2，2026-11-20：`.page-content` 布局契约单一出处（七页重复声明收敛为差异块）；pageEnter 退役、`.page-in` 唯一入场出处；EmptyState 基础组件落地 13 处（CardHeader/banner/Chip 评估后不落地避免新双轨）；scoped 变量通道新增 4 文件）
- [x] **阶段 3 清债转正（2026-11-20「不留技术债」拍板，原「可选增量」全部转正）**：AiTaskView 拆分（8f03cae：953→299 行，三 composable + 60 单测，覆盖率不降反升）；F13 a11y lint 守门 + F14 aria-describedby 系统化（a57f958：插件门禁 20 条规则全 error、47 告警清零、describedby 10→24 处）；F12 content-visibility——IntersectionObserver 前置已随 V4 scrollspy 落地，主体评估后**不落**（设置页 schema 分区已轻量，收益不抵复杂度，登记备查）；密度模式（容器 style queries）——留备选池未排期。
- [ ] **阶段 4 视觉重构（2026-11-20 用户三拍板并入：顶部 tab 替代左侧子导航 / 拆 8 区 / 先地基后视觉）**：
  - ① **[x] 滚动快修**（6c177a07：删 `:has()` 强制 hidden 回归 `.content-wrapper` 整体滚动，scrollIntoView 声明式定位 + save-bar sticky 吸附 + 监听迁移 wrapper + sr-only 守卫随迁）
  - ② **[x] 仪表盘重排**（0f39bc8：`--dash-split-h: 420px` 单变量钳制实现严格等高，诊断卡 footer 贴底 + 内部滚动吸收余高，历史卡全宽 420 限高收尾，stagger 动效接入；发现 `.page-in` 被 layout.css pageEnter 同特异度后到压制空挂——阶段 2 收口退役 pageEnter）
  - ③ **[x] 设置页 tab 化 8 区**（de14c92：sticky tab 条 + IO scrollspy + hash 同步；schema 纯数据模型 + SchemaRow 渲染器 + 22 槽位组件；settingsSearch 两段式重写、`ROW_UNIT_SELECTOR` 退役；旧路由名重定向零改动兼容；旧五分区文件退役；798 用例全过）
  - ④ **Catmail 设计语言映射**（结构模式移植，**不引 Tailwind / 组件库**）：新增 `--panel-elevation`（1px 贴地 + 大 blur 漫射双层阴影）/ `--ease-out-soft`（cubic-bezier(0.32,0.72,0,1)）/ accent-soft 派生 token（由 `--accent-rgb` color-mix 派生，兼容运行时注入）；card 质感统一（subtle 边 + 12px 圆角 + 双层阴影）、badge/banner 收敛为「淡底+浅边+深字」变量类、动效（页面入场 0.18s + 卡片级联 stagger 40ms + reduced-motion 全关）、hover-reveal 滚动条（F6 supports 护栏体系内）。**色彩体系不动**：不迁 OKLCH、不改 useAppearance 运行时注入 token 名；字号恒走 `--text-*`（font_scale 覆盖红线）。参考调研：Catmail（React19+Tailwind v4，`/Users/misyra/Dev/Catmail/frontend/src/index.css` token 全集）

- [x] **阶段 5 全站视觉巡检与打磨**（2026-11-20 完成：主实例全页面深浅两主题逐页 + 向导独立实例 + 1a 三处断点专项 + 1c 快照抽查 + 390px 九路由零溢出断言；结论「阶段 1-4 打底后无明显乱杂区块」，克制原则唯一改动 = AI 页破图守卫（9f44a80）；逐页处置清单与 50 截图存 gitignored `docs/reports/stage5-sweep/`，**待用户验收**）

**验收**：每阶段 `npm run lint:css` + `vitest`（覆盖率阈值不降）+ `typecheck` + `build`；视觉批次深浅两主题 + 窄屏目检（`docs/reports/ui-audit/visual-check.py` 可复跑则用）；changelog / plan-next 由主代理集中维护，子代理并发上限 5。

## 安卓端开发（2026-11-17 立项）

> 用户拍板（2026-11-17 凌晨，承接同日「前端框架/手机端选型」调研）：先做安卓端、只保留 HTTP 认证渠道、加后台运行；UI 用系统 WebView 加载本机 axum 复用现有 Vue 前端（`#auth=` bootstrap 流程零改动）；分发与更新走 GitHub Releases（APK 侧载，不经 Play 审核）；工程位置 monorepo——本仓新建 `android/` Gradle 工程，与 `openwrt/` 同级照抄其 feature 先例。iOS 不做（系统级无常驻后台，认证守护不可实现）。
> 系统约束核实（2026-11-17，developer.android.com 官方原文）：FGS `specialUse` 类型无运行时时长上限（`dataSync` 在 Android 15+ 为 6h/24h 强杀）；Android 15 的 BOOT_COMPLETED 禁启清单（dataSync/camera/mediaPlayback/phoneCall/mediaProjection/microphone）**不含 specialUse**；Doze 官方豁免清单不含前台服务 → **电池优化白名单（`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`）必做**；Android 16 无新 FGS 限制，但 LAN 流量 Local Network Permission 未来强制（校园门户在 LAN，targetSdk 上 36 时需前瞻声明）。

### 架构

```
APK（单进程）
├── MainActivity ── WebView → http://127.0.0.1:<port>（内嵌 Vue 前端，bootstrap 零改动）
├── AuthForegroundService（FGS specialUse + START_STICKY + 常驻通知，5s 轮询状态 → P1 换 WS）
│     └── JNI ↔ libcampus_auth.so（cargo-ndk arm64，feature `android` 照 openwrt 先例）
│           ├── axum @127.0.0.1：REST/WS + rust-embed 前端产物
│           ├── Engine：monitor 探测 → 证据判定 → http 渠道登录 → 网络验证
│           └── 不编译：tray / 浏览器渠道 / Python bridge / 脚本渠道 / 卸载助手
└── BootReceiver（BOOT_COMPLETED → autostart_enabled 才拉起 FGS）
```

### 后台运行对策

| 约束 | 对策 |
|------|------|
| FGS 类型 | `specialUse`（无时长上限）+ `FOREGROUND_SERVICE_SPECIAL_USE` + `PROPERTY_SPECIAL_USE_FGS_SUBTYPE` 用途文案 |
| Doze | 应用内引导请求电池优化白名单（豁免后 Doze 期间可用网络 + partial wakelock） |
| 国产 ROM（MIUI/HyperOS/ColorOS） | 应用内「保活指引」页：自启动权限、省电策略无限制、后台锁定（P2） |
| 开机自启 | `RECEIVE_BOOT_COMPLETED` → BootReceiver → FGS（specialUse 不在 Android 15 禁启清单） |
| Play 政策 | 不适用（GitHub Releases 侧载分发） |

### 分阶段计划

- [x] **P0 编译打通**（2026-11-17 完成，提交 bc02045 / cd6c627 / 35f4706；逐项更改见 changelog 同日条目）：Cargo `android` feature + `[lib] crate-type = ["lib", "cdylib"]` + `jni = "0.21"`（android target 专属）；cfg 门照 openwrt 先例（tray / notify-rust os 通道 target 门 / launcher 跳过 tray+browser / bridge 恒不懒加载 / autostart·uninstall·config 适配）；`CAMPUS_AUTH_BASE_DIR` 环境变量注入 base 目录；system 路由 `android` 平台字段；`src/ffi_android.rs` JNI 薄层（nativeInit/nativePort/nativeStatus/nativeStop，符号契约 `rs.campusauth.app.AuthNative`）；`android/` Gradle 骨架（MainActivity WebView + AuthForegroundService specialUse + BootReceiver + network_security_config 仅放行 127.0.0.1 cleartext）。**成功标准达成 = `cargo ndk -t arm64-v8a` 编出 libcampus_auth.so + clippy/fmt/test 全绿（lib 1454）**
- [ ] **P1 前置（2026-11-19 第二轮审查发现；2026-11-21 修复战役已收①②③，仅剩④）**：① 密钥路径——**已修（ac73d85）**：android 下 `default_key_path` 挂 `CAMPUS_AUTH_BASE_DIR`（仅 android target 生效——桌面读该变量会静默换钥致旧 `ENC:` 失解，刻意不扩平台）；真机必败点修正为「首次保存带密码凭据时 EACCES」而非 nativeInit 恒返 0（密钥懒加载不阻断容器构造，见审查报告）；② `MainActivity` 主线程同步调 nativeInit——**已修（a49fb36）**：移后台线程 + 完成后回 UI 线程以 `#auth=` fragment 加载 WebView（ForegroundServiceDidNotStartInTime 排队链一并消除，fetchLocalToken 路径删除）；③ FGS `onDestroy` 单程生命周期——**已收敛（7b6aac8）**：nativeStop 异步受理 + INSTANCE 复位可重启 + logging 二次 init 显式处理；launcher 正式 Android 模式仍留后续；④ BootReceiver autostart 双状态源——**未动**，P2 接设置 UI 时以 Rust 配置为唯一事实源
- [ ] **P1 真机 MVP**：真机全链路（掉线检测 → 自动重登 → 通知）；JNI 状态回传由 5s 轮询换 WS 订阅；启动编排从 ffi 薄层迁入 launcher 正式 Android 模式；mock 门户（`tests/mock-servers/full-portal`）+ 真机两套验证
- [ ] **P2 系统集成**：autostart 设置 UI、电池优化引导 + 保活指引页、PC 配置经 repo 导入导出迁移、前端按 `/api/system` android 字段隐藏浏览器/脚本渠道入口、Android 16 LNP 权限位前瞻声明
- [ ] **P3 发布链路**：release.yml Android job（cargo-ndk + gradle assembleRelease 签名）、应用内更新（版本检查 → APK 下载 → 安装意图，复用镜像回退）、四端同版注入（Gradle 版本号）、`docs/guides` 用户指南

## 前端现代化排查与优化批次（2026-11-13 立项）

> 用户诉求：「用 modern-web-guidance 这个 skill，看看前端还有没有需要优化的」，并要求「全面细致排查一遍，列一份计划」。
> 排查手段：`npx modern-web-guidance legacy-scan frontend/src --json`（33 篇指南 / 35 条确定性规则，命中 7 条，全部为 `::-webkit-scrollbar`）+ 人工过渲染路径（模板语义、composable 生命周期、CSS token 层与现代特性使用、构建产物、服务端静态资源响应头）。本轮为静态审查，未起实例跑真机；标注「需真机」的项在落地时补验证。
> 基线：30 个 JS chunk 合计 730 KB、11 个 CSS 合计 188 KB（minified）；服务端已有 `CompressionLayer`（`src/web/mod.rs:463`）与 hashed 资源 `immutable` 缓存（`src/web/static_files.rs:124`），传输与缓存层无需改动。
> 总判断：无结构性问题。真正的成本集中在两处——一张按 6 MiB 解码的 logo 蒙版、一个每次 scroll 事件重新查 DOM 的监听；其余为语义与移动端打磨。与本文件 `T8-1` / `T8-2` 两项差异项无重叠。

### 批次 1：零视觉改动（可独立合并）——已落地（2026-11-14，见 changelog 同日条目）

- [x] **F1 logo 资源降采样**：`frontend/public/logo.png` 为 1254×1254 RGBA / 229,690 B，被四处 `.logo-mark` 蒙版（`frontend/src/styles/components/misc.css:272`）与 favicon（`frontend/index.html:6`）共用；CSS `mask` 只读 alpha 通道，而最大渲染尺寸是 About 页 96px（`frontend/src/styles/pages/about.css:25`）。降采样到 256×256（13,928 B，−94%），解码内存 6.0 MiB → 256 KiB。`build.rs:21` 的 app.ico 按 16–256 条目生成，256 源仍满足，已有 `resources/icons/app.ico` 无需重生成。
- [x] **F2 设置页滚动监听收口**：`SettingsView.vue` 的 `updateActiveSection` 每次 scroll 事件对 6 个分区各做一次 `querySelector` + `offsetParent` + `getBoundingClientRect`（`visibleSection()`，第 51-56 行），而分区列表是静态的。改为挂载后缓存元素引用（`Map<id, HTMLElement>`）+ `requestAnimationFrame` 合并（监听保持 passive），卸载时取消在途帧。
- [x] **F3 表头语义**：4 个任务面板共 31 个 `<th>` 无 `scope="col"`（`frontend/src/views/tasks/{Browser,Http,Scripts,Scheduled}TasksPanel.vue`）。
- [x] **F4 `ToggleSwitch` 可访问名**：`role="switch"` 的可访问名落在按钮外的兄弟 `span` 上、按钮内容为空 knob（`frontend/src/components/common/ToggleSwitch.vue:29-36`）；`ScheduledTasksPanel.vue:291` 的行内用法未传 label。用 `useId()` + `aria-labelledby`（无 label 时接受 `aria-label` 透传），并在 `views/settings/settingsA11y.test.ts` 加静态断言。
- [x] **F5 时间格式口径**：`frontend/src/views/settings/SystemSettings.vue:192` 的「上次检查」用裸 `toLocaleString()`（跟随浏览器 locale），与全站 `formatTimestamp` / `formatMtime` 口径不一致；抽出一个固定 locale 的本地时间格式化函数并复用。

### 批次 2：视觉/交互微调（需深浅两主题目检）——已落地（2026-11-14，见 changelog 同日条目）

- [x] **F6 滚动条标准属性兜底**：全站只有 `::-webkit-scrollbar`（`frontend/src/styles/base.css:268-284`；日志区 4px 见 `styles/pages/dashboard.css:150-166`），`scrollbar-color` / `scrollbar-width` 一个未声明，Firefox 至今是系统默认粗滚动条。按指南在 `@supports not selector(::-webkit-scrollbar)` 内给 `html` 声明 `scrollbar-width: thin` + `scrollbar-color: rgba(var(--slate-rgb), .2) transparent`（两属性均继承），Chromium/Safari 的 6px/4px 观感不变。**注意**：无条件声明标准属性会让 Chrome 接管为 `thin`（≈11px），4px 日志滚动条会变粗，故必须用 supports 护栏。
- [x] **F7 弹窗动态视口高度**：壳层已有 `100vh` + `100dvh` 双声明（`frontend/src/styles/layout.css:4/24/93`），弹窗仍单用 `vh`：`modal.css:40`（85vh）/`:61`（92vh）/`:136`，`wizard.css:25`（90vh），`update-dialog.css:95`（52vh），`form.css:272`（60vh），`tasks.css` 多处。移动端浏览器工具栏会吃掉底部，补 dvh 双声明（vh 在前作回退）。
- [x] **F8 设置搜索按帧合并**：`SettingsView.vue` 的 `watch(searchQuery)` 每次按键对 6 个分区全量 `textContent` + 12 组选择器查询（`frontend/src/utils/settingsSearch.ts`），打字快时是按键延迟来源；改为 rAF 合并（或 80–120ms debounce）。
- [x] **F9 滚动链收口**：`.log-viewer` / `.history-list` / `.settings-scroll` / `.modal-body` 等内层滚动容器加 `overscroll-behavior: contain`，滚到尽头不再带动外层页面（当前全站 0 处声明）。
- [x] **F10 首屏壁纸预置**：`frontend/public/theme-init.js` 只预置主题与 `--bg-primary`，自定义壁纸要等 Vue 挂载后由 `body.has-custom-bg::before`（`frontend/src/styles/pages/appearance.css:64`）呈现，大图用户每次加载都闪一下底色。同一份 localStorage 预置 `--bg-image` / `--bg-blur` / `--bg-opacity` 与 `html[data-custom-bg]` 选择器，`applyAppearance` 同步改切换该属性（避免与现有 class 双写）。

### 批次 3：需专项 / 真机验证

- [x] **F11 触屏拖拽**：`frontend/src/utils/drag.ts` 走 HTML5 DnD（`mousedown` 才挂 `draggable`），触屏完全不可用，而窄屏（≤768px）有完整底部导航。二选一：Pointer Events 自绘拖拽，或行内「上移/下移」按钮（更简单且键盘可达）。**2026-11-14 用户选定行内按钮方案并落地**（`useDragSort.moveBy` + 三面板行首按钮，与拖拽同一份全量顺序语义与持久化载荷）。
- [ ] **F12 `content-visibility` 实验**：设置页 6 个分区全常驻挂载，首个分区之外可跳过布局。**前置**：分区定位依赖 `getBoundingClientRect` 实测值（`scrollToSection` / `updateActiveSection`），`contain-intrinsic-size` 的估算值会让目录高亮漂移——需先把定位换到 IntersectionObserver，或真机实测无漂移再上。
- [ ] **F13 a11y lint 守门**：当前只有 `eslint-plugin-vue`（strongly-recommended），无 `eslint-plugin-vuejs-accessibility`。接入 flat config 并清首批告警，把「可访问名 / 表格语义」这类现在靠静态测试人工兜的规则交给 lint。
- [ ] **F14 `aria-describedby` 系统化**：78 处 `.hint` / 错误文案（含非表单场景）vs 10 处 `aria-describedby`；优先把校验错误与危险操作提示挂到对应控件。

### 已核对、明确不动

- 服务端静态资源已压缩 + `immutable` 缓存；30 个懒加载 chunk 的切分与总体积对内嵌本地场景合理，不再拆分。
- 焦点管理（reka Dialog）、body 滚动锁（`useBodyScrollLock`）、skip link + `<main tabindex="-1">`、`prefers-reduced-motion` 与手动 reduce-motion、`.sr-only` 覆盖、触控目标（`.btn-icon-only` 28px ≥ WCAG 2.5.8 的 24px）均已到位。
- 日志 100 条上限 + WS 上行 5s 节流 + 重连指数退避/可见性重连/快照新鲜度比较均已实现，无需虚拟滚动或额外轮询改造。
- 登录历史 `:key="idx"`（`DashboardView.vue:291`）评估后保留：列表整体替换、行内无组件状态，改稳定键需自造复合键且有撞键风险，收益为负。

### 真机视觉检查（2026-11-13，1440×900 / 390×844，深浅两主题）——V1–V6 已全部处置（2026-11-14，见 changelog 同日条目）

> 实例：`target/debug/campus-auth --base-path /tmp/campus-visual --port 50731`（沙箱配置）；覆盖仪表盘 / 方案 / 任务五个 Tab / 设置六个分区 / 关于 / 命令面板 / 仓库导入弹窗 / 通知面板 / Toast，移动端覆盖仪表盘、任务、方案、设置。下列结论均来自 DOM 实测（`getBoundingClientRect` / `getComputedStyle`），非目测；横向溢出、底部导航遮挡、浮层错位均未发现。

- [x] **V1 设置页宽屏居中断裂**：`.settings-page { max-width:1080px; margin-inline:auto }`（`frontend/src/styles/pages/settings/list.css:2-9`）在 1440px 视口下把内容压到 291→1371，而页标题在 260、顶栏动作区右缘 1402——左右各错位 31px，与 changelog「卡片右缘与顶栏动作区对齐」相反（窄视口下才对齐）。**2026-11-14 用户拍板：保留居中列（可读性优先，标题错位属页面级取舍），不改代码。**
- [x] **V2 移动端设置路由标题被搜索框挤没**：390px 下 `.top-title-area` 仅 16px，标题「设置」溢出到 x=59，而搜索框容器从 x=44 起——标题被覆盖只剩「设」。仅设置路由（顶栏放真实输入框）出现；仪表盘 / 方案 / 任务移动端标题正常。**修复**：≤768px 下 `.topbar-search--input` 按视口宽度收缩 `clamp(110px, 30vw, 180px)`，实测标题区恢复 85px。
- [x] **V3 AI 任务页步骤条双 active**：`.ai-stepper` 在「已有捕获结果 + 配置改动未保存」时同时给第 1、3 步 `active`（实测 class：active / done / active），两处「当前步」并存。**修复**：第 3 步 active 条件补 `isConfigDone &&`，active 恒取第一个未完成步。
- [x] **V4 移动端任务表脚部折行**：390px 下 `.tsk-foot` 的「共 1 个任务」被压到 57px 折成两行（实测高 33px），右侧提示同样折两行。**修复**：计数 span nowrap + ≤768px 隐藏 spacer 与右侧提示，实测单行 36px。
- [x] **V5 浅色主题语义色 / 品牌色对比度**（实测 computed color/bg，已剔除渐变背景误报）：
  - `.tech-badge`（关于页）：vue 1.83 / websockets 2.18 / playwright 2.21 / rust 2.58 / tokio 2.86 / ddddocr 2.91 / python 3.55，深色下 python 亦 3.69，均低于 4.5:1；
  - `--success` 文本（「已就绪」「已安装」）浅色 1.86（深色 7.04）——浅色主题只加深了 `--warning` / `--warning-text`，success / error 未同步；
  - `btn-danger-ghost`（卸载 / 立即关闭浏览器）浅色约 2.8–3.6（深色 4.61）；
  - `badge--warn` 用 `--warning`（#d97706）而非已存在的 `--warning-text`（#a34a08），约 3.5。
  **修复**：沿用 `--warning` 直接加深先例，浅色主题 `--success` → #047857、`--error` → #b91c1c（`--success-rgb`/`--error-rgb` 不动，淡底色调不受影响；`--warning` 保持亮琥珀供圆点/指示器，文字走 `--warning-text`）；`badge--warn` 改用 `--warning-text`；`.tech-badge` 文字改 `color-mix(品牌色 45%, 正文色)` + 描边 40%，深浅两主题实测 ≥7.3:1。
- [x] **V6 关于页 `uv` 徽标呈圆形**（2 字符 pill + `radius-full`，像游离气泡），nit。**修复**：`.tech-badge` 统一 `min-width: 72px` + 内容居中。

### 验收

`npm run typecheck` / `lint` / `lint:css` / `test`（覆盖率阈值不降）/ `build`；批次 2 起加深浅两主题目检（`docs/reports/ui-audit/visual-check.py` 只读巡检，若本地已重建）。逐项完成在 `docs/changelog.md` 归档并勾销本节。

## 全面重构优化战役（2026-11-13 立项，2026-11-13 结役）

> 依据两轮全栈审查（`docs/reports/refactor-optimization-review-2026-11-12.md` / `-2026-11-13.md`，本地不提交）与用户选定增量立项，共 10 阶段，总盘子约 14-18 周（单线口径）。完整任务书（依赖图 / 逐项设计要点 / 备选池 / 分歧清单）见 `docs/reports/campaign-plan-draft-2026-11-13.md`。
> 用户已确认（2026-11-13）：阶段 8 全量执行；四项可选项全部纳入（OS 系统通知 / Dashboard 图表 / Ctrl+K 命令面板 / 主题跟随系统）；**连续推进不停等**——遇方案级分歧跳过并记入分歧清单，全部完成后统一呈报裁决；主代理派发+验收，子代理并发上限 5（2026-11-13 用户上调）滚动回填；每小时心跳自动化守护/续跑。
> 每阶段统一验收门禁：`cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` / `cargo test`（默认 + no-embed 双 feature）/ `uv run pytest` / 前端 `vitest` / `lint` / `lint:css` / `typecheck` / `build`；涉登录链阶段加跑 `e2e-login-chain`；涉前端视觉批次深浅两主题目检。

### 阶段 0：热修批次（3-5 天）——先于一切

- [x] **T0a-1** `/api/auth/token` 免鉴权裸发收口（P0 链：本地跨用户取 token → `/api/scripts/run` 任意脚本 RCE，auth.rs:191/108-115）：移除该端点，改 **0600 bootstrap 握手文件**（`config/.auth_bootstrap`，与 token 同生命周期配对生成）+ `POST /api/auth/exchange`（常量时间比对）；应用侧打开界面经 URL fragment（`#auth=`）携带握手秘钥，前端换取后即剥除 fragment；手动打开走前端引导页（托盘「复制界面链接」兜底）；预留 viewer 只读 token 权限矩阵设计注释
- [x] **T0a-2** Docker/LAN 收口（host.rs:34-43、entrypoint.sh:31）：非回环绑定且未配置 `management_password` 且未显式 `--allow-unauthenticated-bind` → 拒绝启动（fail closed）；新配置字段 `config.global.app.management_password`（serde default 免迁移，env 映射 + 设置 UI），`POST /api/auth/exchange` 仅在非回环监听下接受 `{password}` 引导
- [x] **T0a-3** AI 生成任务禁 `eval`/`custom_js` 步型（validate_generated_task，ai/generate.rs:48-78；堵 prompt 注入面）
- [x] **T0a-4** token/密钥/bootstrap 文件 `OpenOptions::mode(0o600)` 创建后写（auth.rs:65-74、crypto.rs:402-433）+ Windows icacls 返回值检查
- [x] **T0b-1** 迁移落盘原子化：v8→v9（migration.rs:440）/v9→v10（:544/:576）改 atomic_write_bytes；v9→v10 写回失败传播 Err 阻断版本提交（:62-67/:576-582 注释修正）；v8/v9 进入口 `.backup.*` 备份；补 3 个中断续跑测试
- [x] **T0b-2** 首启播种原子化 + 自愈（config/service.rs:277/:974、tasks/loader.rs:1010）：default.json / settings.json / ensure_default_task 原子写 + 解析失败重播
- [x] **T0c-1** page_capture 改独立非持久化临时浏览器（playwright_worker.py:2726-2728 clear_cookies / :1604-1617 关旧页摧毁 keep_alive；学 test_redirect :2121-2127 纪律）+「捕获不触碰常驻会话」回归测试
- [x] **T0c-2** Worker 脱敏收口：`_dispatch` 写响应前对 message/error 统一 `_sanitize_error`（worker_main.py:321 主信封、step_handlers.py 5 处裸拼、playwright_worker.py:363）+ 主信封脱敏测试
- [x] **T0d-1** CI 三件套：Swatinem/rust-cache（SHA-pin，per-job；release 按 matrix.target prefix-key）；ci.yml `permissions: contents: read`；各 job timeout-minutes；cargo test 加 `--locked`；`.github/dependabot.yml`（npm/cargo/github-actions weekly）
- [x] **T0d-2** release.yml docker job 加 `needs: build`（版本门禁覆盖镜像产物）

### 阶段 1：事件总线 + 推送通知（1.5-2 周）【用户选定】

- [x] **T1a** EventBus：类型化事件枚举（LoginSucceeded/LoginFailed(reason)/NetworkDown/NetworkUp/TaskFinished/TaskFailed/UpdateAvailable/EnvironmentReady/ProfileSwitched，serde + 时间戳 + 证据摘要）；tokio broadcast + 订阅注册 API；container 拓扑插入（15 服务口径同步 AGENTS.md）；关键源头接入（LoginOrchestrator 终态 / Engine 网络翻转 / Scheduler 任务终态 / Updater 结果，渐进吸收不改现有 watch）；事件流 JSONL 落盘 events/（30 天轮转）
- [x] **T1b** notification/ 服务：webhook 泛化（URL+自定义模板+超时+重试退避+密钥头），订阅事件子集可配置；预设 Bark / Server酱 / Telegram；settings UI 配置区 + 「测试发送」；失败静默（只记日志/事件，绝不影响登录主链路）
- [x] **T1c** OS 系统通知【已确认纳入】：notify-rust / 平台原生 toast（Windows toast / macOS notification），事件开关复用 T1b 配置

### 阶段 2：诊断体验（1 周）【用户选定】

- [x] **T2-1** 网络诊断面板：接口/网关/DNS/门户可达性逐项探测 + 最近失败归因（复用 monitor 证据结构），前端结构化展示
- [x] **T2-2** 诊断包一键导出：日志 + 脱敏配置（凭据/密钥/token 全剥除，脱敏校验测试钉住）+ 环境信息 + 最近 30 天事件流（T1a）→ zip
- [x] **T2-3** 前端诊断页入口 + 导出按钮 + 结果展示

### 阶段 3：浏览器持久会话配置模式（1.5-2 周）【用户指定】

> 设计基础=**门户侧「记住密码」**（凭据存 cookies/localStorage，门户自己的 JS 无头自动回填，非浏览器密码管理器），自动化只需**录制登录点击**。方案级 `persistent_session_enabled` + `recorded_actions`；每方案 `browser_profiles/<id>/` 持久化 user_data_dir（**utils::paths 常量 + uninstall 清单红线**）；Worker `profile_session_start`（headed+持久 context）/交互录制（默认=一次登录点击，ISP select 按序录）/`profile_session_stop`（先落 playwright_worker.py，T7-4 模块化时吸收）；无头持久 context 回放 recorded_actions + 网络验证兜底；失效→标记并提示重开配置会话。

- [x] T3-1 数据与路径（含卸载红线与测试钉住）
- [x] T3-2 Worker 配置会话命令 + 交互录制
- [x] T3-3 Rust 编排（复用 debug session 基建模式 + run_steps 引擎回放）
- [x] T3-4 失效回退（「过期回填方案凭据」进备选池）
- [x] T3-5 前端开关 / 打开浏览器配置按钮 / 配置会话横幅 / 重新录制
- [x] T3-6 测试（pytest 点击捕获 + 回放；Rust mock 门户全链路；并发占用互斥）
- [x] T3-7 安全面（profile 目录权限 / 共享机器提示 / 卸载 purge/keep 口径）

### 阶段 4：质量护栏（1.5-2 周）

- [x] T4-1 前端浏览器级 e2e（Playwright 驱动真实二进制 + UI 全流程，CI 接线）
- [x] T4-2 proptest（迁移往返 / IPC 解析 / PATCH 合并 / 登录抢占不变量）
- [x] T4-3 cargo-fuzz（IPC / 配置 JSON / 迁移三 target）
- [x] T4-4 覆盖率门禁（cargo-llvm-cov + vitest coverage，核心 crate 阈值进 CI）
- [x] T4-5 cargo-auditable + 月度 audit job
- [x] T4-6 bridge_supervisor 测试墙钟窗口改 timeout+轮询
- [x] T4-7 lightweight / login-once 集成测试两用例
- [x] T4-8 小项打包（.dockerignore tests / updater_channels 注释诚实化 / Worker P2-7 杂项）

### 阶段 5：契约战役（1.5-2 周）

- [x] T5-1 utoipa 全端点注解（~80+ 分域并行）+ schemars 导出 config JSON Schema
- [x] T5-2 openapi-typescript 生成前端类型；route_table↔openapi 契约测试切换；AGENTS.md 口径更新
- [x] T5-3 settings API 资源化（section 化 PATCH / 严格 Merge Patch，全量 PATCH deprecated 过渡）
- [x] T5-4 WsMessage / AiStreamEvent 从生成契约派生
- [ ] T5-5 api/index.ts 内联类型上收（与生成类型合并收口） （差异项：未执行——内联类型上收属大改，留待裁决）

### 阶段 6：平台补全（1.5 周）

- [x] T6-1 is_own_process mac 分支 proc_pidpath + 真机单测；T6-2 版本 marker 文件（与 T6-4 一次设计）；T6-3 launchd 自启动（--base-path/--startup-launch）；T6-4 manifest ed25519 离线签名 + 版本单调/回滚保护；T6-5 helper 卸载结果 osascript dialog；T6-6 build.ps1 mac 适配；T6-7 Docker 降权 + 四镜像 digest pin + 删 Dockerfile:28 COPY python_worker + 工具链一致性 CI 检查；T6-8 CI mac job 加 e2e-login-chain；T6-9 Windows 版本资源 + app.ico

### 阶段 7：后端结构（2-3 周）

- [x] T7-1 launcher.rs（1332 行）按职责拆模块；T7-2 http_login.rs 双路径收敛（两步走，e2e 护航）；T7-3 bridge/mod.rs（2709）拆 error/registry/worker/debug 四子模块 + environment/uv.rs 拆 download/verify/mirror；T7-4 playwright_worker.py 模块化（吸收 T3-2 新命令；OCR 三件下沉 ocr_runtime 消影子模块）；T7-5 四长函数拆分（submit / session::run / validate_task / handle_probe_message）；T7-6 Engine 冷却与 Updater 周期改墙钟（对齐 cron_loop 范本，保留可注入时钟）；T7-7 清理盲区（.trash 30 天 housekeeping + 卸载清单 / .tmp_ 扫三桶+environment / scan_json_bucket 跳过 `.` 开头）；T7-8 环境引导磁盘空间预检 + 中文文案；T7-9 AppState.container 移除 + container 构造前移 + resolve_*_task 收敛 + 直连测试 local_ip 同源；T7-10 Worker P2 群（evaluate 超时改 ASSERTION_FAILED / idle release 加锁 / test_redirect goto 可取消 / docstring 口径 / OCR 缓存 LRU）；T7-11 维持 ServiceHandle/EngineSlot 成文模式仅拆文件不换抽象；**差异项**：T7-9 的「AppState.container 字段移除」字面项保留——M1 细粒度依赖已落地、handler 不再触达容器，字段仍作装配载体

### 阶段 8：前端结构（3-4 周）【已确认全量】

- [ ] T8-1 设置页 schema 驱动（灰度：schema 模型+渲染器→单分区原型→行为等价验证→铺开） （差异项：未执行——schema 驱动设置页属周级大改，留待裁决）
- [ ] T8-2 AiTaskView（957 行）拆 3 composable + 测试；SSE 事件类型化（走 T5-4 生成契约） （差异项：未执行——AiTaskView 仍 957 行，留待裁决）
- [x] T8-3 UI 修复批：HttpTestResult chip 深色 1.13:1（P0）+ 未定义变量守卫测试；tsk-col-name 两面板迁移；15 处 aria-label；SetupWizard 迁 Reka Dialog+焦点陷阱测试；Toast live region 常驻；UpdateDialog token 化；设置域死规则群清理；badge--mono 迁移；--warning-text 加深；transition:all；日志双套色 token 化；首拉加载态
- [x] T8-4 前端结构批：App.vue 四浮层异步化；autostart 轮询页面驱动；死代码清理；autostartApi.toggle 具名返回
- [x] T8-5 依赖升级四线（vue-tsc3+TS5.9 / vitest5 / vite7-8+plugin-vue6 / stylelint18）+ eslint-plugin-vue 10 strongly-recommended + engines 字段
- [x] T8-6【已确认纳入】Dashboard 图表（echarts 按需 chunk，消费 T1a-3 事件流）+ 命令面板 Ctrl+K + 主题跟随系统/强调色

### 阶段 9：收尾（3-5 天）

- [x] T9-1 错误文案人话化扩展到全部域 + 错误码目录（可选）；T9-2 known-issues 坐标刷新 + #5 结论；T9-3 AGENTS.md 版本刷新（5.2.0-beta.1）+ 服务拓扑（EventBus）更新；T9-4 changelog/updatelog 汇总 + plan-next 勾销 + mermaid 架构图；**分歧清单统一呈报 + 总验收（全量门禁 + 深浅两主题视觉检查）**

> 备选池（协议模板库、过期回填凭据、Profile 环境匹配、IPv6 专项、viewer token 落地、Worker 池化、mac 托盘、i18n、URL scheme、CLI 子命令、配置快照回滚）与执行纪律细节见评审稿；过程报告一律落 gitignored `docs/reports/`；changelog/plan-next 由主代理按阶段集中维护。

## 登录新增第三种渠道：自定义脚本（2026-09-24 **已落地**）

> 用户诉求：「给登录任务增加一种渠道，就是自定义脚本，允许使用自定义脚本进行登录」。实现与验证见 `docs/changelog.md` 同日条目，脚本契约见 `docs/guides/custom-script-guide.md` 第 2 节。

**已落地**：`LoginChannel::Script`（`script`）+ 方案字段 `active_script_task`；复用任务页「立即运行」的执行路径叠加 `CAMPUS_USERNAME` / `CAMPUS_PASSWORD` / `CAMPUS_ISP` / `CAMPUS_AUTH_URL` 四个环境变量，退出码 `0` 视为本次成功且仍走登录后网络验证（实现口径见 `AGENTS.md`「登录渠道」与脚本契约文档）。

**为什么要这一渠道**：早期 HTTP 渠道只能固定发送下线、前置与登录请求，复杂门户所需的额外接口、文件操作与自定义解释器无法表达。2026-09-28 的有序 HTTP 流程已补上顺序请求、取值和纯计算；脚本渠道仍承担 HTTP 步骤无法覆盖的本地程序与文件能力，同时不要求 Python Worker 与 Playwright。

**本轮登记 / 遗留**：

- **脚本渠道终态已补齐（2026-09-29）**：退出码 `2` 表示需要人工操作、`3` 表示凭据无效，两者停止重试；其余非零退出码仍走重试预算。契约见 `docs/guides/custom-script-guide.md`。
- **脚本子进程不保证静默**：与「立即运行」同一条路径，未设 `CREATE_NO_WINDOW`，控制台版解释器可能闪现窗口（既有行为）。

## 全功能实测排查与修复（2026-09-24 **已落地**）

> 用户诉求：「再全面细致排查一遍各项功能，你使用 agent-browser 实际启动试试，确保各个功能没有问题」。手段：`agent-browser`（CDP）真实点完全部页面 + 91 条 API 全量扫 + 集成测试真起二进制；完整清单与证据在 `docs/reports/full-audit/README.md`（本地不提交），逐项修复见 `docs/changelog.md` 同日条目。

**修掉的一个真缺陷（F1，高）**：脚本渠道**在默认配置下完全跑不起来**——方案编辑器把「执行程序 = Python (项目内解释器)」存成**空串**（不是 `null`），而 `build_script_command` 只判 `None`，`Some("")` 被当成真实路径 → `Command::new("")`。修复：新增 `explicit_binary()`（空白串一律按未指定），四个分支统一走它，并使「解析出的程序名为空」提前返回可诊断的中文报错；细节、真机三连验证与「为什么测试全绿却没拦住」的教训见 `docs/changelog.md` 同日条目。

**本轮登记的待办**：

- ~~**系统设置页的部分控件不在可访问性树里**~~ **已落地（2026-11-12 全面审查修复战役）**：8 项控件全部补齐（5 个开关 `role="switch"`+`aria-checked`、「服务模式」/「全局日志级别」CustomSelect 补 aria-label、配置预设分段组补 `role="group"`），并加 5 例静态守卫测试钉住；附带新增设置页搜索特性（关键词过滤/高亮定位/空态/清空，14 例测试）。
- ~~**`TaskError::IoError` 全量人话化**~~ **已落地（2026-11-12 全面审查修复战役）**：新增 `io_kind_label`（14 类 ErrorKind）与 `io_error_at`/`io_error` 包装器，loader/executor 全部构造点改为「动作失败（路径）：类别（std::io 原文）」形状；`tasks::` 136 项全过（含 5 条跨平台文案断言）。
- **2026-11-12 全面审查修复战役收官**：`docs/reports/full-review-2026-11-12.md` 的全部 76 项发现处理完毕（74 修复 + B9/D9 两项记录决策），阶段3 Reka UI 落地（菜单收口/Modal/FieldHelp 迁移，Toast 评估后决策不迁移——现有实现健康）。逐项结果见 `docs/reports/fix-round-2026-11-12.md` 与 `docs/changelog.md` 同日条目；42 条 stylelint 基线 warnings、E-P3 第 7 条对比度、Toast live-region 改持久容器等已登记为后续小项。

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

## 任务仓库三渠道共享索引（2026-09-28 开发中）

浏览器、直连和脚本任务统一编辑 `index.json`，条目显式填写 `type`。Gitee 镜像和旧版客户端读取的 `index.http.json` 从它生成。主程序三个任务子页读取共享索引后按类型筛选。历史上的分类索引设计及其原因保留在 2026-09-24 的 changelog 中。

2026-09-28 已补有序 HTTP 登录流程（请求、响应取值、计算字段、结果来源与逐步测试），见 `docs/changelog.md` 同日条目。后续工作：补首个经审查的脚本任务；改进 HTTP 门户响应的失败原因分类；让脚本渠道的登录测试提供与 HTTP 登录任务同等清晰的网络验证结果。

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

## 长期挂账（2026-08-26 核实修订）

- M1 trait 化已完成；Pinia 不适用（前端无 Pinia）；utoipa 手写 `openapi.json` 仍为前端 `typegen` 数据源，待 `utoipa` 宏化后自动生成；
- `AppState.container` 后续可评估移除。
- 定时任务「启动后执行」增强（2026-09-12，已上线固定延迟方案）：可选增强为等待监测服务首次网络连通后再触发（接 StatusManager watch，超时兜底强制执行），彻底消除"网络未就绪烧掉一次尝试"的场景。

> 本文件为唯一活跃计划入口，完成一项后在 `docs/changelog.md` 归档并更新 `docs/known-issues.md`。
