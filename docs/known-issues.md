# 已知问题清单

> 本文档沉淀**仍然有效**的未修复项（已逐项对照当前代码核实，2026-09-06）。
> 原审查报告（`docs/*review-*.md`、`doc/*.md`）已删除，有效结论迁移至此；历史详见 `docs/archive/`。
> 条目的修复记录归档于 `docs/changelog.md`（按版本归档，不在本文档保留已修复节）。
> 另有两份审计复核报告独立留存：`docs/defect-recheck-2026-09-06.md`（76 条清单 v2 对质版）与 `docs/updater-audit-2026-09-05.md`（更新子系统 v2），判定口径与本文档互补。

---

## 一、实质问题（按严重性排序）

| # | 严重度 | 问题 | 位置 |
|---|--------|------|------|
| 2 | 🟢 低 | `mapBackendStatus` 用 `Object.assign(out, raw)` 混入非契约字段，后端新增同名字段会覆盖精心映射的值 | `frontend/src/composables/useStatus.ts:68` |
| 3 | 🟢 低 | 定时任务 `next_fire_at` 以 UTC（RFC3339 `Z`）展示，与本地触发语义不一致，易误判 | `src/scheduler/mod.rs:326-328` |
| 5 | 🟢 低 | Profile 切换检测仍被 `is_any_pause_active` 阻断（原审查判定为误阻断，属设计取舍，需确认） | `src/engine/run_loop.rs:127-130` |
| 7 | 🟢 低 | `uv sync` 失败无删除重试；`UV_SYNC_MAX_RETRIES` 常量定义未使用；环境引导仅懒触发（任务执行 / OCR / 系统页），启动不自动引导 | `src/environment/uv.rs`、`src/environment/python.rs` |
| 15 | 🟢 低 | Profile 匹配无用户可配置 `priority` 字段（已按约束数降序确定性排序，抖动已修） | `src/config/profiles.rs:138-157` |
| 19 | 🟡 低中 | `repo.rs` IP 字面量校验**不完整**（IPv6 链路本地已修，见已修复记录；建议复核是否需补 `is_documentation` 等保留段） | `src/web/routes/repo.rs:108-125` |

> 76 条清单的 P0/P1 已由 `docs/defect-recheck-2026-09-06.md` v2 对质收敛（9 条 P0/P1 全部属实，待排期），本文档不再重复展开；更新子系统 10 项见 `docs/updater-audit-2026-09-05.md` v2（含 `Stable/Prerelease/All` 通道与 `update/last_check.json` 相关项）。

---

## 二、工程化缺口

| # | 问题 | 说明 |
|---|------|------|
| E3 | 测试短板 | 核心模块已补测试（`web/routes` handler 层、`environment/`、`scheduler`、tray 纯函数、`python_worker` 127 项 pytest、`engine::slot` 等）；`cargo test` 约 659 项、`vitest` 约 10 文件；全链路 `tests/login_chain.rs`（mock→二进制→Worker→success/failonce）已接入 `e2e-login-chain` CI，`rust-tests-unix` 双端齐跑；剩余偏少项见复核报告 |

---

## 三、低危清理项

- 前端构建警告：`useTasks` ↔ `useScripts` 循环动态导入，无法拆 chunk
- 本地遗留目录可清理：`python_worker/.venv`（Worker 本地虚拟环境，约 100MB+，运行时按需重建）
- 文档：`docs/test-coverage-2026-08-30.md` 为历史重定向（已搬迁至 `docs/archive/test-coverage-2026-08-30.md`），下版本可删

---

## 四、三端兼容性（Windows / macOS / Linux）

> 2026-08-30 全量审计的 16 项（W1–W16）中 12 项已于同日修复并归档至 changelog（第十六轮）：W1 uv 资产格式、W2 venv 路径、W3 解压权限位、W4 更新器 tar.gz、W5 force_kill、W7 unix 卸载脚本、W10 CI unix job、W11 SIGTERM/SIGHUP、W12 进程组终止、W14 __pycache__、W15 备份名、W16 .bat 拒绝 + XDG 环境变量。
> W6（托盘）按用户决策收敛：**macOS 托盘禁用**（`TrayManager::spawn` 内单点拦截返回空句柄，轻量模式自动降级完整模式，Web 控制台 / `--stop` 仍可用），Linux 侧已修复（托盘线程内 `gtk::init` + glib 主循环，与 `tray-icon` 内部 `gtk 0.18` 同版本，待真机验证），不再跟踪；主力平台为 Windows。
> 剩余未修项如下（均为有指引或降级方案的低危）。

| # | 严重度 | 问题 | 位置 |
|---|--------|------|------|
| W8 | 🟢 低 | Linux 二进制动态链接 GTK3 / libayatana-appindicator / librsvg（托盘代价），无桌面发行版起不来；运行时依赖与安装命令已写入 Release 发布说明 | `.github/workflows/release.yml` |
| W9 | 🟢 低 | macOS 未 codesign / 公证，浏览器下载后带 quarantine 被 Gatekeeper 拦截；`xattr -cr` 解除指引已写入 Release 发布说明，真签名需 Apple 开发者证书 | `.github/workflows/release.yml` |
| W13 | 🟢 低 | linux-arm64 平台键存在但无产物（交叉链接缺 aarch64 GTK 库，暂不产包）；windows-arm64 已补 | `src/updater/check.rs` vs `release.yml` |

---

## 五、更新通道相关（与Updater审计联动）

- 通道枚举：`src/config/schema.rs::UpdateChannel::{Stable,Prerelease,All}`；`Stable` 单包（`releases/latest`）、`Prerelease` 仅预发布、`All` 取最高，`All` 枚举为空时回退单包口径（有意降级，避免“切通道后无候选”回归）。
- 状态落盘：`update/last_check.json`（UTC、best-effort），`GET /api/update-state` 回放，前端“上次检查”数据源；`auto_check_enabled==false` 时仅手动检查可刷新。
- 审计项见 `docs/updater-audit-2026-09-05.md`（含下载代理收敛 `resolved_proxy_url`、单包 `.sha256` 伴随文件依赖等），此处不重复展开。

> 历史已修复条目已归档至 `docs/changelog.md`（2026-08 全量，含第十一~十六轮）；过时规划见 `docs/archive/` 与 `docs/plan-next.md`。
