# 已知问题清单

> 本文档沉淀**仍然有效**的未修复项（已逐项对照当前代码核实，2026-09-06）。
> 原审查报告（`docs/*review-*.md`、`doc/*.md`）已删除，有效结论迁移至此；历史详见 `docs/archive/`。
> 条目的修复记录归档于 `docs/changelog.md`（按版本归档，不在本文档保留已修复节）。
> 原 defect-recheck（76 条 v2）与 updater-audit（更新子系统 v2）过程报告已于 2026-09-12 删除；P0/P1 与更新子系统待办摘要见 `docs/plan-next.md`，与本文档互补。

---

## 一、实质问题（按严重性排序）

| # | 严重度 | 问题 | 位置 |
|---|--------|------|------|
| 2 | ✅ 已修 | `mapBackendStatus` 用 `Object.assign(out, raw)` 混入非契约字段，后端新增同名字段会覆盖精心映射的值 | 已由 P17 改为逐字段显式映射（`frontend/src/composables/useStatus.ts:92-119`，其余 `Object.assign` 均不含 `raw`） |
| 3 | 🟢 低 | 定时任务 `next_fire_at` 以 UTC（RFC3339 `Z`）展示，与本地触发语义不一致，易误判 | `src/scheduler/mod.rs:658`（序列化点；字段定义 `:71`、`systemtime_to_iso` `:360`） |
| 5 | 🟢 低 | Profile 切换检测仍被 `is_any_pause_active` 阻断（原审查判定为误阻断，属设计取舍，需确认） | `src/engine/run_loop.rs:234-241`（门控）、`:1006`（`is_any_pause_active` 定义） |
| 7 | ✅ 已修 | ~~`uv sync` 失败无删除重试；`UV_SYNC_MAX_RETRIES` 常量定义未使用~~（已修：常量已移除，`uv sync` 实为 3 次重试，见 `src/environment/uv.rs` 的 `UV_DOWNLOAD_MAX_RETRIES`）。仍存：环境引导仅懒触发（任务执行 / OCR / 系统页），启动不自动引导 | `src/environment/uv.rs`、`src/environment/python.rs` |
| 15 | 🟢 低 | Profile 匹配无用户可配置 `priority` 字段（已按约束数降序确定性排序，抖动已修） | `src/config/profiles.rs:138-157` |
| 19 | 🟡 低中 | `repo.rs` IP 字面量校验**不完整**（IPv6 链路本地已修，见已修复记录；建议复核是否需补 `is_documentation` 等保留段） | `src/web/routes/repo.rs:108-125` |
| 20 | 🟢 低 | v5→v6 迁移把 `enable_local_check`（登录前物理网卡检查）误改名 `url_enabled`，存量迁移用户 `local_check_enabled` 永久缺省；forward 映射已修（2026-09-13），存量不回写（见注） | `src/config/migration.rs`（v5→v6） |
| 21 | 🟢 极低 | `ServiceContainer` 无 `Drop`：`new()` 内已 spawn 引擎/uptime/日志清理等常驻任务，若构造中途失败这些任务不会被显式取消。实际危害有限——`startup()` 不可失败，真实失败点在 `new()` 内且随即 `exit(1)` 由 OS 收尸（窗口期空转而非永久泄漏）；仅当未来启动流程变为"部分失败但进程存活"时才值得引入 Drop/回滚编排 | `src/container.rs`（2026-09-13 复核口径） |
| 22 | 🟢 极低 | 2026-09-13 P3 批量挂账（21 项，全部经复核确认为低危且有不修理由——已书面化的设计取舍、修复成本与收益不匹配、或依赖外部约定；逐项见下方 #22 注） | 见 #22 注 |
| 23 | 🟢 低 | 2026-09-13 便携版从 0 全功能实测发现（3 项 UI 问题，未阻断功能，详见 #23 注；实测报告本地 `E:\Test\reports\`，不入仓） | 见 #23 注 |

> 76 条清单的 P0/P1 已对质收敛（9 条 P0/P1 全部属实，待排期；摘要见 `docs/plan-next.md`），本文档不再重复展开；更新子系统 10 项摘要同见 `docs/plan-next.md`（含 `Stable/Prerelease/All` 通道与 `update/last_check.json` 相关项）。
>
> #20 注：v5（Python 版）源码佐证 `enable_local_check` 仅门控登录前物理网卡连接检查，URL 内容检测在 v5 无独立开关（列表非空即生效）。对已迁移用户（config_version 6-8）的影响：① 手动网络测试不做网卡诊断，设置页"手动测试时检查网卡"一键可补开；② `url_enabled=true` 对默认配置用户≈v5 实际行为，v5 清空过 URL 列表的用户得到"开关开+空目标"，空目标探测实际 Disabled，无功能实害。存量不做 v9 回写：已迁移配置中 `url_enabled=true` 无法与用户主动开启区分，回写反而会覆盖用户选择。
>
> #22 注（P3 批量挂账，2026-09-13）：① **WEB-2** PATCH 的 other_patch 未知键可合并顶层 settings——收紧为白名单是行为变化，老客户端会开始报错；② **WEB-5** 反馈包/日志导出含原文（凭据若曾入日志随包泄露）——行级脱敏难穷举字段且易掩盖排障信息；③ **COR-4** BroadcastLayer 推 WS 无字段级脱敏——同 ② 取舍；④ **UPD-4/ENV-8** 更新链与 uv 下载无代码签名（TLS + 同源 SHA256）——签名体系需发布基础设施与密钥管理，代码注释已表明"不上签名但不降级"的有意选择，uv 官方亦无签名资产；⑤ **COR-3** 控制台层同步 stderr——改非阻塞引入丢日志窗口与退出 flush 时序问题，CLI 场景实时性更可预期；⑥ **BRG-3** IPC 行上限 Rust 1MiB / Python 16MiB 不一致——统一 16MiB 抬高行缓冲内存峰值，统一 1MiB 需大载荷分块协议；⑦ **BRG-5** bridge/mod.rs 约 2300 行职责过宽——拆分是结构性工程，超时常量收敛已随 UPD-6 覆盖；⑧ **ENV-9** uv.rs 约 1700 行上帝模块——同 ⑦；⑨ **WE2-7** run_script 直跑路径不复检 binary 黑名单——复检会拒绝历史已存任务，属行为变化；⑩ **TSK-4** 解压 dest 未 canonicalize——纵深防御缺口而非现行漏洞，Windows `\\?\` 前缀与 junction 语义坑多；⑪ **TSK-6** is_own_process 子串匹配 + 260 固定缓冲——精确基名比较需先约定各发行形态 exe 名单；⑫ **TSK-8** 解压到字面名 `campus-auth.zip` 目录被误拦——WinRAR"解压到 campus-auth.zip\"形态，有 `--allow-temp` 逃生通道；⑬ **FE1-5** 单例 composable 测试依赖用例声明顺序——依赖关系已有注释自认，隔离改造收益低；⑭ **ENG-4** test_network 诊断任务未绑取消令牌——任务受自身超时约束无共享状态副作用，属良性；⑮ **ENG-6** SchedulerService::start 二次调用 panic——全仓调用点唯一，`.take().expect` 作内部不变量金丝雀合理；⑯ **UPD-8** helper 以 PID 轮询等待主进程退出存在 PID 复用 TOCTOU——5.3 起超时已 fail-safe（报错退出保留 staging）；⑰ **ENV-6** site-packages 候选路径硬编码 `python3.12`——PYTHON_VERSION_CONSTRAINT 已锁 `>=3.12,<3.13`，约束放宽时才会失准。⑱ **MON-4-R1** 门户检测跟随跳转的**同 host 豁免**存在 DNS rebinding 变体：首跳解析到公网承载 302、二次解析指向环回时，因 `same_host` 短路而不做目的地址校验。低危——盲 GET 无凭据、无外带通道、能改写 DNS 者本已处于 MitM 位置，且链路中任一跳已建钉扎客户端时被 `pinned` 复用天然阻断；要收紧须对同 host 也逐跳解析校验（代价是校园门户自身的相对路径跳转会反复解析）。⑲ **MON-4-R2** 域名钉扎只取 `addrs[0]` 无多地址回退：双栈域名跳转的首地址（常为 IPv6）不可达时该跳直接失败并误报 Offline，较修复前的 happy-eyeballs 有退化面；`web/ssrf.rs` 有"逐地址尝试"现成手法可借。⑳ **MON-4-R3** 钉扎端口粘滞：跨主机跳转把 `resolve(host, addr)` 写入 client 后，同 host 的后续隐式端口跳转仍复用该 client（hyper-util `set_port` 语义），实际连接端口可能与上报 URL 不一致——仅"探测端口与上报 URL 不一致"，无 SSRF 扩面。另：**NEW-1** LoginSource::Browser 文档与实现矛盾已随注释修正收口（不改 API 契约）。
> #23 注（便携版实测，2026-09-13）：① **E1 MON-4 检测候选被其他 Tab 保存顺带持久化**——~~账号页「自动检测」把候选地址填入认证地址输入框（设计语义为"由用户确认保存"），但设置页表单为全局共享状态，随后在任意 Tab 点「立即保存」都会把未确认候选连同 `auth_url` 写入方案（实测候选 `/portal?welcome=1` 被静默落盘并污染后续登录目标）。建议：候选填入后置"待确认"隔离态，或保存 `auth_url` 时与检测候选一致但未经显式确认则提示。~~ **2026-09-16 已修复（架构性消除）**：账号/认证地址/登录方式全部移交「方案」页单一入口后，`useConfig` 不再持有也不提交任何方案字段，设置页保存栏只提交 `GlobalConfig`——「任意 Tab 保存顺带落盘方案凭据」这条路径不存在了。回归由 `useConfig.test.ts` 的「保存载荷只含全局设置」用例锁定（逐字段断言 15 个方案域键不得出现）。② **E2 定时任务手动「运行」toast 语义误导**——点击即弹「执行成功」，实为"已触发"；实测触发后 3s 真实失败，toast 与结果矛盾（执行历史卡片数据正确）。建议改「已触发执行」并在失败时经任务通知通道补发。③ **E3 任务卡「上次： 失败」不随成功即时刷新**——重跑成功后后端 `last_result=[success]` 已更新，任务卡仍显示失败，刷新页面恢复正确；仅运行后局部状态未同步，数据无损。

---

## 二、工程化缺口

| # | 问题 | 说明 |
|---|------|------|
| E3 | 测试短板 | 核心模块已补测试（`web/routes` handler 层、`environment/`、`scheduler`、tray 纯函数、`python_worker` pytest、`engine::slot` 等）；全链路 `tests/login_chain.rs`（mock→二进制→Worker→success/failonce）已接入 `e2e-login-chain` CI，`rust-tests-unix` 双端齐跑；剩余偏少项见复核报告。**规模数字不再在此维护**（历史"127 项 / 约 659 项 / 约 10 文件"均已过时），需要时用 `cargo test -- --list`、`uv run pytest --collect-only -q`、`npm test -- --reporter=json` 现取 |

---

## 三、低危清理项

- 前端构建警告：`useTasks` ↔ `useScripts` 循环动态导入，无法拆 chunk
- 本地遗留目录可清理：`python_worker/.venv`（Worker 本地虚拟环境，约 100MB+，运行时按需重建）

---

## 四、三端兼容性（Windows / macOS / Linux）

> 2026-08-30 全量审计的 16 项（W1–W16）中 12 项已于同日修复并归档至 changelog（第十六轮）：W1 uv 资产格式、W2 venv 路径、W3 解压权限位、W4 更新器 tar.gz、W5 force_kill、W7 unix 卸载脚本、W10 CI unix job、W11 SIGTERM/SIGHUP、W12 进程组终止、W14 __pycache__、W15 备份名、W16 .bat 拒绝 + XDG 环境变量。
> W6（托盘）按用户决策收敛：**macOS 托盘禁用**（`TrayManager::spawn` 内单点拦截返回空句柄，轻量模式自动降级完整模式，Web 控制台 / `--stop` 仍可用），Linux 侧已修复（托盘线程内 `gtk::init` + glib 主循环，与 `tray-icon` 内部 `gtk 0.18` 同版本，待真机验证），不再跟踪；主力平台为 Windows。
> 剩余未修项如下（均为有指引或降级方案的低危）。

| # | 严重度 | 问题 | 位置 |
|---|--------|------|------|
| W8 | 🟢 低 | Linux 二进制动态链接 GTK3 / libayatana-appindicator / librsvg（托盘代价），无桌面发行版起不来；运行时依赖与安装命令已写入 Release 发布说明 | `.github/workflows/release.yml` |
| W9 | 🟢 低 | macOS 未 codesign / 公证，浏览器下载后带 quarantine 被 Gatekeeper 拦截；`xattr -cr` 解除指引已写入 Release 发布说明，真签名需 Apple 开发者证书 | `.github/workflows/release.yml` |
| W13 | ✅ 已修 | ~~linux-arm64 平台键存在但无产物（交叉链接缺 aarch64 GTK 库，暂不产包）~~ 已补 `ubuntu-24.04-arm` 原生构建（`.github/workflows/release.yml:55-56`，`685ff8f` 2026-09-08）；`infer_platform_key` 可将 `aarch64-unknown-linux-gnu` 正确映射为 `linux-arm64`（`src/updater/check.rs:394-399`） | `.github/workflows/release.yml` |

---

## 五、更新通道相关（与Updater审计联动）

- 通道枚举：`src/config/schema.rs::UpdateChannel::{Stable,Prerelease,All}`；`Stable` 单包（`releases/latest`）、`Prerelease` 仅预发布、`All` 取最高，`All` 枚举为空时回退单包口径（有意降级，避免“切通道后无候选”回归）。
- 状态落盘：`update/last_check.json`（UTC、best-effort），`GET /api/update-state` 回放，前端“上次检查”数据源；`auto_check_enabled==false` 时仅手动检查可刷新。
- 审计项摘要见 `docs/plan-next.md` 更新子系统分批建议（含下载代理收敛 `resolved_proxy_url`、单包 `.sha256` 伴随文件依赖等），此处不重复展开。

> 历史已修复条目已归档至 `docs/changelog.md`（2026-08 全量，含第十一~十六轮）；过时规划见 `docs/archive/` 与 `docs/plan-next.md`。
