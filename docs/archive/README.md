# docs/archive — 归档区

> 兑现 `docs/changelog.md` 顶部的“过时规划见 `docs/archive/`”承诺。本目录存放**不再活跃但需追溯**的文档与调试产物，不参与日常开发。

| 文件 | 说明 |
|---|---|
| `test-coverage-2026-08-30.md` | 2026-08-30 功能测试覆盖清单（29 项）与 6 缺陷验证，原 `docs/test-coverage-2026-08-30.md` 已搬迁至此 |
| `feedback-bilibili-final.zip` | 用户反馈包（Bilibili 录屏/截图，2.4MB），从 `logs/` 归档，仅留存追溯 |
| `step_screenshot_after_*.png` | 根 `debug/` 失活截图（14KB），已从 `debug/` 归档，`debug/` 目录已移除，统一使用 `python_worker/debug/` |

**归档策略：**
- `changelog.md` 中 `v5.0.0` 之前的历史轮次后续按季度拆出至此（当前仍 inline，待下一版本执行瘦身）。
- `plan-next.md` 为唯一活跃计划入口，不归档。
- `test-coverage-*` 单日快照过期即归档，不删可追溯，CI 不依赖。
