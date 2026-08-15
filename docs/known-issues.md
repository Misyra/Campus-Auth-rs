# 已知问题清单

> 本文档沉淀代码审查中**仍然有效**的未修复项（已逐项对照当前代码核实，2026-08）。
> 原审查报告（`docs/*review-*.md`、`doc/*.md`）已删除，有效结论迁移至此。
> 条目的修复记录归档于 `docs/changelog.md`（按版本归档，不再在本文档保留已修复节）。

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

---

## 二、工程化缺口

| # | 问题 | 说明 |
|---|------|------|
| E3 | 测试短板 | 核心模块已补测试（web/routes handler 层、environment/、scheduler、tray 纯函数、python_worker 48 项 pytest）；集成测试仍偏少（bridge_ipc / bridge_supervisor / smoke） |

---

## 三、低危清理项

- 前端构建警告：`useTasks` ↔ `useScripts` 循环动态导入，无法拆 chunk
- 本地遗留目录可清理：`python_worker/.venv`（Worker 本地虚拟环境，约 100MB+，运行时按需重建）

---

> 历史已修复条目已归档至 `docs/changelog.md`（2026-08-14 三轮：初轮 + 第二轮 + 第三轮）。
