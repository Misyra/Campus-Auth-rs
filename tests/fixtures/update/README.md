# 更新包 Fixtures

| 目录 | 来源 | 内容 | 说明 |
|---|---|---|---|
| `fake-update/` | `target/fake-update` | `latest.json` + `*.sha256` | 自更新 happy-path；`campus-auth-windows-x64.zip` 不入库，本地按需生成（`cargo build --release` 后 `build.ps1` 打包或手动 zip） |
| `su-zip/` | `target/su-zip` | 同上 | 同上，另一端口/版本数据 |
| `fake-traversal/` | `target/fake-traversal` | `traversal.zip`（240B） | 路径穿越负向用例，小文件已入库 |

生成 zip 后同步更新同目录下的 `.sha256`（`sha256sum campus-auth-windows-x64.zip`）。
