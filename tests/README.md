# tests — 测试套件与 Fixtures

> 原 `target/` 下散落的隔离运行时、mock 服务与更新包已归档至此；`target/` 现仅保留 Cargo 构建产物（`debug/` / `release/` / `doc/` / 跨编译目标）。

## 目录结构

```
├── mock-servers/               # 轻量 mock 校园网门户（本地 e2e 用）
│   ├── captive/                # 简易 captive 302 → login.html（原 target/captive）
│   │   ├── fake_204.py
│   │   └── login.html
│   ├── captcha-mock/           # 带验证码的 mock 登录页（原 target/captcha-mock）
│   │   ├── game.html
│   │   └── server.py
│   └── full-portal/            # 完整 mock 认证门户（原 mock_portal/，含验证码/captive/多阶段回归）
│       ├── server.py           # ThreadingHTTPServer 127.0.0.1:18765，生成 4 位数字验证码
│       ├── poll_login.py       # 轮询 /api/login/status 直至结束
│       └── test_phase_a1~a3.py # 分阶段回归（登录分支/探测/Profile/OCR/debug）
├── fixtures/
│   ├── runtime-envs/           # 隔离基座（base_path）模板，原 target/* 下的 e2e-* / portal* / fresh-py / stop-test / su-*
│   │   ├── e2e-real/           # 多 Profile + 定时任务 + 脚本（含 dorm/lib）
│   │   ├── portal2/            # 门户登录脚本 + 定时任务
│   │   ├── portal-e2e/         # 门户登录脚本最小集
│   │   ├── fresh-py/           # Python 脚本任务集（hello-*）
│   │   ├── stop-test/          # 优雅停机最小基座
│   │   ├── su-test/            # 自更新（su）测试基座
│   │   ├── su-bak-test/        # 自更新备份测试（需本地生成二进制，见下）
│   │   └── e2e-helper-isolated/# helper 隔离测试（需本地生成二进制，见下）
│   └── update/                 # 更新包 Fixtures（原 target/fake-update / su-zip / fake-traversal）
│       ├── fake-update/        # latest.json + sha256（zip 按需本地生成）
│       ├── su-zip/             # 同上
│       └── fake-traversal/     # traversal.zip（路径穿越负向用例）
```

## 使用约定

- **运行时瞬态文件已清理**：`config/.auth_token` / `.lock` / `.runtime_port` / `.agreed`、`logs/*`、`environment/*` 在入库时移除；测试启动时由程序自动重建。详见 `tests/.gitignore`。
- **大二进制不入库**：`*.exe` / `*.zip` / `*.bak`（约 29MB）已加入忽略；`fixtures/update/fake-update/` 与 `su-zip/` 仅保留 `latest.json` 与 `.sha256`，`fixtures/runtime-envs/su-*` / `e2e-helper-isolated` 的二进制需本地按需生成（`cargo build --release` 后拷贝或执行更新链路脚本）。
- **空目录占位**：`environment/` / `logs/` / `tasks/browser/` 等空骨架以 `.gitkeep` 占位，避免检出后缺目录。
- **不要直接以 `tests/fixtures/runtime-envs/*` 为 `base_path` 启动主程序**：它们是模板，测试应在 `tempfile::tempdir()` 中拷贝所需文件后启动，避免污染模板。

## 相关目录

- `tests/mock-servers/full-portal/` — 完整 mock 门户（原 `mock_portal/` 已搬迁至此，根 `mock_portal/README.md` 为兼容重定向）
- `python_worker/tests/` — Python Worker 单测
- `mock_portal/` — 已搬迁，根保留 `README.md` 重定向（下版本可删）

## 维护

- 新增 e2e 场景：在 `fixtures/runtime-envs/` 下以 `<场景名>/` 新建模板，仅提交 `config/`（`profiles/*.json` + `settings.json`）与 `tasks/`（`*.json`），不要提交 `logs/` / `environment/` / 锁文件 / 大二进制。
- 更新包：修改 `fixtures/update/*/latest.json` 后本地打包 `campus-auth-windows-x64.zip` 并更新 `.sha256`（CI 不依赖本地 zip，仅校验元数据形状）。

## 测试矩阵（统一入口）

| 端 | 位置 | 命令 | 说明 |
|---|---|---|---|
| Rust 单元 | `src/**/mod tests`（73 处，就地） | `cargo test --lib` | 与源码同目录是 Rust 惯例，不搬迁 |
| Rust 集成 | `tests/*.rs`（5 个 crate）+ `tests/common/` | `cargo test --test '*'` | 共享 helper 经 `mod common;` 接入；临时目录一律 `tempfile`，禁写 `target/` |
| Python | `python_worker/tests/`（11 文件 / 127 用例） | `cd python_worker && uv run pytest` | 函数级懒导入保证无 Playwright 也可 collect |
| 前端 | `frontend/src/**/*.test.ts`（7 文件 / 49 用例） | `cd frontend && npm test` | vitest node 环境 |
| 全链路 | `tests/login_chain.rs`（mock→二进制→Worker→success＋failonce 重试） | `cargo test --test login_chain` | 需 Python+Pillow+ddddocr+Playwright chromium，缺一 SKIP；CI `e2e-login-chain` 预装 |

手动 E2E 环境变量：`CAMPUS_AUTH_BASE`（默认 `http://127.0.0.1:50721`）、
`CAMPUS_AUTH_MOCK`（默认 `http://127.0.0.1:18765`）、`CAMPUS_AUTH_TOKEN`（优先于文件）、
`CAMPUS_AUTH_BASE_PATH`（定位 `config/.auth_token`）。`_common.py` 自带回环 `no_proxy`。
CI 接线见 `.github/workflows/ci.yml`（rust-clippy / rust-tests / rust-tests-unix / frontend-python / e2e-login-chain）。
