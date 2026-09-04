# runtime-envs — 隔离基座模板

每个子目录为一次 `base_path` 隔离运行的最小可复现模板（原散落在 `target/` 根）。

| 目录 | 场景 |
|---|---|
| `e2e-real/` | 多 Profile（default/dorm/lib）+ 定时任务 + 脚本 |
| `portal2/` | 门户登录脚本 + 定时任务 |
| `portal-e2e/` | 门户登录脚本最小集 |
| `fresh-py/` | Python 脚本任务集（hello-*）|
| `stop-test/` | 优雅停机最小基座 |
| `su-test/` | 自更新 happy-path 基座 |
| `su-bak-test/` | 自更新备份（需本地生成 `campus-auth.exe` / `.bak`）|
| `e2e-helper-isolated/` | helper 隔离（需本地生成 `campus-auth.exe` / `.bak`）|

模板仅保留 `config/profiles/*.json` + `settings.json`（如有）与 `tasks/*.json`；`environment/` / `logs/` / `config/.auth_token` 等瞬态已清理，`.gitkeep` 占位。

## 模板规范

- 不提交 `last_run` / `last_result` 等运行时状态（`ScheduledTask` 字段为 `Option`，缺省即 `None`）；
- `auth_url` / 探测 URL 统一指向当前 mock 门户 `http://127.0.0.1:18765`
  （`tests/mock-servers/full-portal/server.py`），不用 target 时代的旧端口；
- 更新包 fixture（`../update/`）的 `latest.json` 端口（`8765/8766`）是各场景自带假更新源端口，保持不动。
