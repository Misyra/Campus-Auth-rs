# mock_portal — 已搬迁

> 本目录已于 2026-09-03 归档至 `tests/mock-servers/full-portal/`，以收口 `tests/` 统一测试入口并消除与 `tests/mock-servers/{captive,captcha-mock}` 的重复。
>
> **兼容**：旧路径 `python mock_portal/server.py` 仍可通过重定向说明找到新位置；外部脚本请改为 `python tests/mock-servers/full-portal/server.py`（端口仍为 `127.0.0.1:18765`）。

- 新位置：`tests/mock-servers/full-portal/server.py`（含验证码/captive 语义，`ThreadingHTTPServer`）
- 回归脚本：`tests/mock-servers/full-portal/test_phase_a1~a3.py` / `poll_login.py`
- 关联文档：`tests/README.md`、`docs/test-coverage-2026-08-30.md`

此目录保留仅为兼容过渡，下版本可删除。
