# docs/archive — 归档区

> 兑现 `docs/changelog.md` 顶部的“过时规划见 `docs/archive/`”承诺。本目录存放**不再活跃但需追溯**的文档与调试产物，不参与日常开发。

| 文件 | 说明 |
|---|---|
| `test-coverage-2026-08-30.md` | 2026-08-30 功能测试覆盖清单（29 项）与 6 缺陷验证；原 `docs/test-coverage-2026-08-30.md` 重定向桩已于 2026-09-12 删除 |

**已清理（不再出现在本目录）：**
- `feedback-bilibili-final.zip` — 已于 alpha.10 从版本库删除
- `step_screenshot_after_*.png` — 已于 2026-09-12 从版本库删除

**归档策略：**
- `changelog.md` 中 `v5.0.0` 之前的历史轮次后续按季度拆出至此（当前仍 inline，待下一版本执行瘦身）。
- `plan-next.md` 为唯一活跃计划入口，不归档。
- `test-coverage-*` 单日快照过期即归档，不删可追溯，CI 不依赖。
- 审计/复核/方案/Bug 扫描等过程报告**不进入版本库**（`.gitignore` 覆盖），优先写入 `docs/reports/`；过时历史报告已于 2026-09-12 删除，有效结论只保留在 `known-issues.md` 与 `plan-next.md`。
