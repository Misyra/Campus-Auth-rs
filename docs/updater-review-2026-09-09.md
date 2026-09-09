# 更新模块（updater + helper）现状梳理

- 日期：2026-09-09
- 范围：`src/updater/`（mod / check / download / apply / error）、`src/helper_main.rs`、
  `src/web/routes/system.rs`、`src/tray/mod.rs`、`src/launcher.rs`、`src/container.rs`、
  `src/config/schema.rs`、前端 `frontend/src/views/settings/NetworkSettings.vue` / `AboutView.vue`
- 基线校验：`cargo check --features no-embed --all-targets` 通过（0 error / 0 warning）
- 前置文档：`docs/updater-audit-2026-09-05.md`（v2，10 项）

---

## 一、结论速览

| 项 | 结论 |
|----|------|
| 2026-09-05 审计 10 项 | **全部已落地**（逐条对照见第二节） |
| 编译状态 | 通过，无警告 |
| 新发现问题 | 15 项：P2 × 4、P3 × 11（无 P0/P1） |
| 二次复核 | 1 条推翻重写（#5 现象描述错误）、1 条表述收紧（#1）、1 条风险上调（#3）、1 条概率下调（#7）、新增 3 条 |
| 总体判断 | 安全基线（URL 白名单 / 双重 SHA256 / pending 路径校验 / 大小与停滞上限）完整，**主要缺口集中在状态机与并发语义**，非安全缺陷 |

---

## 二、2026-09-05 审计 10 项落地对照

| # | 原问题 | 落地位置 | 状态 |
|---|--------|----------|------|
| 1 | `--base-path` 与 target 校验冲突 | `updater/mod.rs:704-723`（`is_pending_path_valid`：staging 锁 base 内 + target 比对 `current_exe`）、`helper_main.rs:511-528`（`resolve_target_exe`） | ✅ 已修（含单测 `test_is_pending_path_valid` / `test_resolve_target_exe`） |
| 2 | 启动 double-fire | `updater/mod.rs:222-290`（先睡后查 + `due_now`） | ✅ 已修 |
| 3 | `POST /api/system/update` 版本漂移 | `system.rs:481-552`（`ApplyUpdateBody::into_pinned`：URL 白名单 + 64 位 hex + semver 闸门）、`openapi.json:1959-2006`、前端 `NetworkSettings.vue:69-76` 传 pin | ✅ 已修 |
| 4 | 下载总超时 300s | `download.rs:28-35`（`DOWNLOAD_CONNECT_TIMEOUT` + `DOWNLOAD_STALL_TIMEOUT`）、`error.rs:31-33` | ✅ 已修 |
| 5 | `available` 只置 true 不清 false | `updater/mod.rs:825-832`（else 清 false）；消费方 `tray/mod.rs:748`（`update_menu_label`） | ✅ 已修 |
| 6 | 备份双轨 | 撤回 | — |
| 7 | pending 死循环 | 撤回 | — |
| 8 | GitHub 配额只认 429 | `check.rs:129-146` + `403-417`（`rate_limit_retry_after` 纯函数） | ✅ 已修（含 HeaderMap 单测） |
| 9 | 托盘 updater 死依赖 | `tray/mod.rs:398/623-646/715/748`（菜单项 + 事件分发 + 动态文本） | ✅ 已修（但落地不完整，见新问题 #4） |
| 10 | helper 自更新写 base_path | `helper_main.rs:280-284`（`install_dir` = exe 目录） | ✅ 已修 |

---

## 三、代码地图

| 文件 | 行数 | 职责 |
|------|------|------|
| `src/updater/mod.rs` | 985 | `UpdaterService`：服务编排、后台检查循环、`check_update` / `apply_update`、`pending` 启动应用、代理客户端、LastCheckState |
| `src/updater/check.rs` | 832 | 清单拉取（自定 latest.json / GitHub Release API）、通道选取、平台键推断、SHA256 伴随文件、semver 比较、URL 白名单 |
| `src/updater/download.rs` | 392 | 流式下载 + 增量 SHA256 + 停滞超时 + 大小上限 + 解压到 staging |
| `src/updater/apply.rs` | 176 | `pending.json` 原子读写、staging/helper/exe 常量、清理 |
| `src/updater/error.rs` | 93 | `UpdaterError`（18 变体） |
| `src/helper_main.rs` | 775 | 独立 binary：等主进程退出 → SHA 复核 → 备份 → 替换 → 分发目录 overlay → helper 自更新 → 启新进程 → 延迟删备份 → 清理 |
| `src/web/routes/system.rs` | 440-562 | `GET /api/check-update`、`GET /api/update-state`、`POST /api/system/update`（pin） |
| `src/tray/mod.rs` | 398/623-646/748 | 托盘"检查更新"菜单项与动态文本 |
| `src/container.rs` | 183-188, 254-277 | 构造 UpdaterService（Layer 9）+ 启动后台 pending 应用（2s 延迟） |
| `src/launcher.rs` | 606/644/872-877/978-983 | full / lightweight 启动后台检查；关机时补唤醒 helper |
| `tests/updater_channels.rs` | ~360 | 三通道 + 回退 + last_check 落盘集成测试（回环 mock） |

### 三条主链路

1. **检查链路**：`start_background_check`（T+5s 启动检查 → 循环"先睡后查"）与手动 `check_update()`
   共用 `perform_update_check` → `fetch_manifest_for_channel` → `select_platform` →
   `compare_versions` → `merge(PartialSnapshot::Update)` + 写 `update/last_check.json`。
2. **应用链路**：`POST /api/system/update` → `apply_update`（互斥 + 拒绝登录中）→
   `download_and_verify`（zip SHA256）→ `extract_to_staging` → 计算 **exe** SHA256 →
   写 `pending.json` → `spawn_helper`。前端随后调 `/api/system/shutdown` →
   `graceful_shutdown` → `ensure_helper_for_shutdown`（补唤醒）→ 主进程退出 → helper 替换重启。
3. **启动兜底**：`container` spawn `apply_pending_on_startup`（2s 延迟）→ 校验 pending 路径 /
   SHA256 / 版本 → `self_replace::self_replace` + 清理（"新版本下次启动生效"）。

---

## 四、配置面（`UpdaterSettings`，`config/schema.rs:374-431`）

| 字段 | 默认 | 前端 UI | 备注 |
|------|------|---------|------|
| `auto_check_enabled` | true | 有（总开关） | 关后仅手动检查；循环 60s 轮询热感知 |
| `check_interval_hours` | 24 | 有（0 / 24 / 168） | 0 = 仅启动检查；实际最小间隔 300s |
| `channel` | stable | 有（正式 / 测试 / 全通道） | Prerelease 无候选时回退正式版 |
| `release_source_url` | GitHub releases/latest | 无 | 非 `releases/latest` 形态无法枚举通道，warn 回退单清单 |
| `use_proxy` / `proxy_url` / `proxy_port` | false / "" / 7890 | 有 | `resolved_proxy_url()` 收敛；凭据脱敏入日志 |
| `check_on_startup` | true | **无 UI** | 见问题 #1 |

---

## 五、API 面

| 端点 | 方法 | 说明 |
|------|------|------|
| `/api/check-update` | GET | 手动检查；返回 `has_update` / `latest` / `current` / `url` / `sha256` / `error` |
| `/api/update-state` | GET | 只读回放 `update/last_check.json` |
| `/api/system/update` | POST | 可选 body `{version,url,sha256}`（pin），省略则服务端重查 |

状态落盘：`update/last_check.json`（UTC RFC3339，检查成败均刷新）；
`update/pending.json`（原子写 + fsync）；`update/staging/`（下载与解压产物）。

---

## 六、安全基线（现状完整）

| 基线 | 位置 |
|------|------|
| 更新源白名单（https 任意主机 / http 仅精确回环，拒绝 userinfo 与前缀绕过） | `check.rs:19-34` |
| 重定向收敛（最终 URL 复检） | `check.rs:122-124`、`download.rs:85-87` |
| zip SHA256 强制（缺失即拒，不降级） | `download.rs:56-59` |
| exe SHA256 二次复核（Rust 启动路径 + helper 替换路径） | `mod.rs:449-469/632-648`、`helper_main.rs:567-583` |
| staging 锁 base 内（防 pending 篡改 → 任意目录删除） | `mod.rs:710-723`、`helper_main.rs:538-544` |
| target 与 `current_exe` / 推导值比对（防任意文件覆写） | `mod.rs:704-712`、`helper_main.rs:511-528` |
| 大小上限 512MB + 停滞超时 60s（连接 15s） | `download.rs:28-37` |
| 备份 + 回滚 + 新进程双探活后才删 .bak | `helper_main.rs:220-234/296-326/596-605` |
| 代理凭据脱敏入日志 | `mod.rs:744-753` |

已知 tradeoff（非缺陷，文档注明）：无签名，信任根 = GitHub 账号 + HTTPS。

---

## 七、测试覆盖现状

| 层 | 覆盖 |
|----|------|
| `check.rs` 单测 | 版本比较、平台键推断、URL 白名单、配额头、列表 URL 推导、通道选取（8 组） |
| `download.rs` 单测 | 成功往返、摘要不符、URL/缺 SHA 拒绝（回环 axum mock，3 组） |
| `apply.rs` 单测 | pending 往返 + 旧格式兼容 + 原子写无 tmp 残留（2 组） |
| `mod.rs` 单测 | pending 跳过、互斥占位、路径校验（4 组） |
| `helper_main.rs` 单测 | 路径逃逸、target 三分支、SHA 复核、备份决策、overlay、摘要（6 组） |
| `tests/updater_channels.rs` | 三通道 + 回退 + last_check 落盘（7 组） |
| **缺口** | 后台循环时序（due_now / 开关热改 / interval 切换）、`apply_update` 全链路、helper 端到端、`ensure_helper_for_shutdown` 均无测试 |

---

## 八、新发现问题清单

> 复核说明（v2，2026-09-09）：初版 12 条经二次回源码核对，#1 表述收紧、#3 风险上调、
> **#5 现象描述推翻重写**（环境页 `envStatus` 来自 `environmentApi`，不读 status 快照），
> #7 触发概率下调，并新增 #13-#15 三条。

| # | 问题 | 位置 | 严重度 | 复核 |
|---|------|------|--------|------|
| 1 | `check_on_startup` 在默认路径下无效果 | `updater/mod.rs:226` + `:244` | P2 | 成立（表述收紧） |
| 2 | `update_in_progress` 成功后不释放 → 同进程内再更新恒 409 | `updater/mod.rs:379-407` | P2 | 成立，触发需"取消重启 + 刷新页面" |
| 3 | 双 helper 无互斥，最坏可致更新被回滚覆盖 | `updater/mod.rs:526-533` / `helper_main.rs` | **P2（上调）** | 成立，补充时序分析 |
| 4 | 托盘"检查更新"落到无更新入口的 `/about` | `tray/mod.rs:623-646` + `:748` | P2 | 成立 |
| 5 | **下载进度对前端完全不可见**（写入的字段无消费方） | `updater/mod.rs:421-429` | P3 | 成立，现象已重写 |
| 6 | 自动检查由关到开后首查等一整个周期 | `updater/mod.rs:245-271` | P3 | 成立（默认配置下） |
| 7 | `PlatformNotAvailable` 语义混淆 | `check.rs:208-226` | P3 | 成立，但触发概率低（`release.yml:154-193` 保证生成 `.sha256`） |
| 8 | U3 版本闸门在 `version` 解析失败时静默放行 | `updater/mod.rs:650-661` | P3 | 成立 |
| 9 | helper 侧无版本闸门 | `helper_main.rs:51-53` | P3 | 成立 |
| 10 | `file_sha256` 两处重复实现 | `updater/mod.rs:726` / `helper_main.rs:547` | P3 | 成立 |
| 11 | `archive_name_from_url` 未净化文件名 | `download.rs:205-213` | P3 | 成立（不可利用，防御性） |
| 12 | `UpdateInfo.size` / `notes` / `release_date` 无消费方 | `updater/mod.rs:99-102` | P3 | 成立 |
| 13 | 手动 `check_update` 不 merge `Update{available}`，托盘文本不更新 | `updater/mod.rs:297-359` | P3 | 新增 |
| 14 | 无当前平台包时前端显示"当前已是最新" | `check.rs:320-333` | P3 | 新增 |
| 15 | 更新长期不重启时 `update/staging/` 残留不清理 | `updater/mod.rs:410-479` | P3 | 新增 |

### 1. `check_on_startup` 在默认路径下无效果（P2）

`mod.rs:226` 启动检查由 `auto_check_enabled && check_on_startup` 决定；`mod.rs:244` 又令
`due_now = !startup_settings.check_on_startup`。在 **`auto_check_enabled = true`（默认）** 下推演：

| check_on_startup | interval | 首查 | 后续 | 结论 |
|---|---|---|---|---|
| true | 0 | T+5s（循环外） | 永不 | 1 次 |
| false | 0 | T+5s（循环首轮，`due_now=true`） | 永不 | 1 次 |
| true | 24 | T+5s | 每 24h | 等价 |
| false | 24 | T+5s | 每 24h | 等价 |

→ **默认路径下该开关对"启动是否检查"零影响**：关掉它照样在 T+5s 收到一次检查，
与 `schema.rs:377-378` 注释所称"在 `check_interval_hours == 0` 时独立生效"**不符**。
前端也无 UI 暴露（`constants.ts:165` 恒 true，全仓仅 `types.ts:215` 有类型声明）。

**修正表述（复核后）**：并非"完全等价"——唯一差异出现在 `auto_check_enabled` **初始为 false**
的场景：`check_on_startup=false` 时 `due_now` 恒为 true，用户后来打开总开关会**立即**补查；
`check_on_startup=true` 时 `due_now=false`，打开开关后要等一个周期（即问题 #6）。
即该字段当前只影响"开关由关到开后的补查时机"，与它的名字无关。

**修法（二选一）**：① 令 `due_now` 恒 false，把"启动是否检查"完全交回 `check_on_startup`；
② 删除该字段与 `types.ts:215` / `constants.ts:165` 引用（配置迁移需保留兼容反序列化）。

### 2. `update_in_progress` 无释放路径（P2）

`apply_update` 成功后**故意**不释放标记（`mod.rs:400-406` 注释：随进程消亡）。前提是
"调用方随后一定重启"。但前端 `NetworkSettings.vue:78-90` 用 `confirm` 询问是否重启，
用户点"取消"即破坏前提：helper 等待 60s 超时退出（`helper_main.rs:340-349`），pending 仍在，
标记仍为 true → 此后每次"立即更新"都返回 409「更新正在进行中」，唯一出路是重启应用。

**复核后补充触发条件**：apply 成功后前端把 `updateInfo` 置为 message 态
（`NetworkSettings.vue:78-82`），更新按钮被模板隐藏，因此需**刷新页面 → 重新"立即检查" →
再点"立即更新"**才会碰到 409（此时 `update_in_progress` 仍为 true）。触发路径存在但需两步操作。

**修法**：互斥语义改为"pending 存在即占用"（`apply::has_pending_update`）而非进程级永久标记；
或在 `ensure_helper_for_shutdown` / 下一次 `check_update` 时检测到 pending 失效（helper 已退出）后复位。

### 3. 双 helper 无互斥，最坏可致更新被回滚覆盖（P2，复核后上调）

正常更新路径必然 spawn 两个 helper：`apply_update → spawn_helper`（`mod.rs:402`）+ 关机时
`ensure_helper_for_shutdown`（`launcher.rs:876`，pending 存在即再 spawn）。
`mod.rs:526-533` 注释称"重复 spawn 双 helper 竞争时由实例锁互斥"——但 `helper_main.rs`
**全文没有任何文件锁/互斥代码**（已通读确认）。实际表现：

- B 先读到 pending.json → 与 A 并发复制同一 exe（内容相同）→ 各自启动新进程，第二个被主程序
  实例锁拒绝退出，helper.log 重复、`.bak` 二次覆盖；
- B 后读到（A 已 cleanup）→ `expected_sha` 为空 → `verify_staging_sha256` 拒绝 → exit(1)（安全）。

**复核后补充——最坏时序（这是把本条留在 P2 的理由）**：
A 在第 5 步复制完 exe 后，还要执行 `sync_distribution_files`（复制 `python_worker/` `docs/`
`resources/`，数十 MB，典型耗时数秒）**之后**才启动新 exe。若 B 因调度/IO 差异滞后到
"A 已启动新 exe"之后才走到自己的第 5 步：

1. B 的 `fs::copy(extracted → target)` 因新 exe 被占用而失败；
2. B 进入回滚分支（`helper_main.rs:242-256`）执行 `copy(backup → target)`；
3. 若 B 的 `.bak` 是在 A 替换**之前**做的（= 旧 exe）且回滚 copy 成功 →
   **磁盘上的新 exe 被旧 exe 覆盖，更新被静默撤销**；而 A 已完成 `cleanup`
   删除了 `pending.json` 与 staging，用户只能重新检查更新；
4. 更差：回滚 copy 也失败 → 日志"回退失败，exe 处于未知状态，请手动用备份恢复"。

触发需要 B 相对 A 滞后约数秒（A 的 sync 窗口），概率不高但后果明确。
其余情形（两者近乎同步）仅产生重复日志、`.bak` 二次覆盖、第二个新进程被实例锁挡下，功能不损坏。

**修法（三选一，推荐 1+3）**：
1. helper 启动时对 `<base>/update/helper.lock` 取文件锁（`utils::lock` 已有 fs4），抢不到即退；
2. `ensure_helper_for_shutdown` 仅在"apply_update 曾 spawn helper 失败"时补唤醒（加 flag）；
3. 第 5 步前先比对 `target_exe` 与 `extracted_exe` 的 SHA256，相同说明已被同伴替换完成 →
   跳过复制与回滚，直接退出（幂等，从根上消除回滚覆盖）。

### 4. 托盘更新入口落地不完整（P2）

`tray/mod.rs:623-646`：托盘"检查更新"发现新版本后打开 `http://127.0.0.1:{port}/about`；
但 `AboutView.vue` **完全没有更新 UI**（grep `update|更新` 零命中）——更新入口实际在
设置页「网络」区块（`NetworkSettings.vue:226-247`）。同时 `update_menu_label`
（`tray/mod.rs:748`）在有更新时把菜单文本改成"发现新版本，点击更新"，点击后却落在无按钮的页面。

**修法**：托盘跳转改为 `/settings/network`（或给 AboutView 增加版本更新卡片，展示
`latest` / `changelog` / "立即更新"），二者择一并让菜单文本与实际行为一致。

### 5. 下载进度对前端完全不可见（P3，复核后重写现象）

初版称"环境页永久显示『下载更新 100%』"——**该描述错误，已推翻**。复核证据：

- 环境页 `envStatus` 来自 `useEnvironment()` → `environmentApi.fetchStatus()`
  （`EnvironmentSettings.vue:13` + `useEnvironment.ts:18-27`），读的是环境模块自己的状态；
- `PartialSnapshot::Environment` 写入的是 status 快照的 `environment_progress`
  （`snapshot.rs:310-311`），而前端类型与模板中 **grep `environment_progress` 零命中**，
  即 WS 广播了但无人消费。

**修正后的现象**：`apply_update` 期间（下载 15~25MB，慢网数十秒）前端**没有任何进度反馈**，
设置页只有按钮文案"更新中..."（`NetworkSettings.vue:240`）。同时该字段写完后从不
`merge(None)` 清空，残留到进程结束；若与环境安装并发，还会与
`environment/mod.rs:598-611` 的 `report_progress` 互相覆盖。

**修法**：给 `PartialSnapshot::Update` 变体加 `progress: Option<InstallProgress>`（或独立
`UpdateProgress` 字段），前端在设置页更新区渲染百分比；下载结束/失败后显式置 `None`。
若暂不做 UI，至少应停止蹭 Environment 通道并在结束时清空。

### 6. 开启自动检查后首查延迟（P3）

启动时的 `startup_settings` 只读一次（`mod.rs:225`）。若启动时 `auto_check_enabled=false`，
启动检查被跳过且 `due_now=false`；用户随后打开开关，循环进入 `sleep(interval)`（默认 24h），
首查要等一整个周期。

**修法**：`auto_check_enabled` 由 false→true 的跃迁时置 `due_now=true`（循环内保存上一轮值比对）。

### 7. `PlatformNotAvailable` 语义混淆（P3）

`check.rs:208-226`：GitHub 路径下只要有平台包但**缺 `.sha256` 伴随文件**，`platforms` 为空 →
统一报 `PlatformNotAvailable("发布中未找到平台下载包")`。用户/日志看到的是"没有你的平台的包"，
真实原因是"发布没带校验值"。同理 `fetch_manifest_for_channel` 列表为空也复用该错误类型
（`check.rs:325`，文案不同）。

**复核后概率修正**：`release.yml:154-193` 对每个平台压缩包都生成并上传 `.sha256`，
故"有包缺 sha"仅在发布流程异常/手工上传时出现。该分支更常见的触发是**资产名无法识别平台键**
（旧 4.x 命名如 `campus-auth-4.2.3.zip`，`infer_platform_key` 返回 None）→ 此时文案
"未找到平台下载包"其实是准确的。因此本条属**可诊断性改进**，非高概率缺陷。

**修法**：拆分"无平台包"与"缺校验值"两种错误（后者复用 `MissingChecksum` 或新增
`ChecksumUnavailable`）；空列表用独立错误。

### 8. 版本闸门解析失败即跳过（P3）

`mod.rs:650-661`：`if let Ok(pending_ver) = Version::parse(&pending.version)` —— 解析失败时
**不进入分支、不告警、直接放行**。故障模式不安全（可被篡改的 pending 绕过），虽因 SHA256
复核在前而不可利用，但应显式拒绝。

**修法**：`Err` 分支按"拒绝 + 清理"处理（与 SHA 缺失同级）。

### 9. helper 侧无版本闸门（P3）

`helper_main.rs:51-53` 的 `version` 字段是 `#[allow(dead_code)]`，helper 替换路径完全不校验版本。
当前仅靠 `into_pinned`（`system.rs:509-518`）在 pin 路径拦截旧版本；非 pin 路径依赖清单版本必然更高。
纵深防御缺一环。

**修法**：helper 读取 `pending.version`，与自身 `CARGO_PKG_VERSION`（或"被替换 exe 的版本"）比较，
不高于则拒绝并保留备份。

### 10-12. 清理类（P3）

- `file_sha256` 在 `updater/mod.rs:726-740` 与 `helper_main.rs:547-561` 重复实现 → 提取到
  `utils::io`（helper 已依赖 `campus_auth::utils`）。
- `archive_name_from_url`（`download.rs:205-213`）直接用 URL 末段作落盘文件名，未做
  `..` / 分隔符 / 长度净化（当前因资产名来自 GitHub 且 URL 未解码而不可利用）。
- `UpdateInfo.size` / `notes` / `release_date`（`mod.rs:99-102`）在前后端均无消费方
  （`size` 不参与下载，进度取 `content_length`）——要么在设置页展示包体与更新说明，要么删除。

---

### 13. 手动检查不 merge `Update{available}`，托盘文本不更新（P3）

`UpdaterService::check_update`（`mod.rs:297-359`）只写 `last_check.json`，**不**调用
`status.merge(PartialSnapshot::Update { .. })`；只有后台 `perform_update_check`（`:825-832`）会。
因此托盘菜单文本（`tray/mod.rs:748`）只反映后台检查的结果——用户在前端/托盘手动检查发现新版本后，
菜单仍是"检查更新"。与 #4 同源，是 09-05 审计 #9 落地的残留缺口。

**修法**：手动路径复用同一 merge 逻辑（抽成函数供两条路径调用）。

### 14. 无当前平台包时前端显示"当前已是最新"（P3）

`check.rs:320-333`：手动检查若 `select_platform` 为 None，记录 `latest_version` 且
`has_update=false` 后返回 `Ok(None)`；前端 `updateCheckHint`（`NetworkSettings.vue:47-53`）
据此渲染"当前已是最新（远程 vX）"。但真实情况可能是"远程没有本平台的包"（见 #7），
用户会误以为自己装的就是最新版。

**修法**：`UpdateInfo` / last_check 增加"平台不可用"标记，前端区分两种文案。

### 15. 更新长期不重启时 staging 残留（P3）

`download_stage_and_pending`（`mod.rs:410-479`）把 15~25MB 的压缩包与解压产物落在
`base_path/update/staging/`，清理只发生在 helper 成功替换（`helper_main.rs:475-490`）或
启动 self_replace 之后（`apply.rs:94-107`）。用户点了"立即更新"却长期不重启、也不退出时，
该目录一直占用；重复点击（未触发 409 的情形，如重启后再更新）还会叠加旧版本 zip 残留
（`archive_name_from_url` 按资产名落盘，不同版本文件名不同）。

**修法**：`apply_pending_on_startup` / 启动检查时顺带清理超过 N 天的 staging 残留。

---

## 九、建议分批（v2）

| 批次 | 条目 | 改动 | 验收 |
|------|------|------|------|
| A（状态机） | #1 #2 #6 #13 | `updater/mod.rs` 后台循环与互斥、merge 收敛 | 补后台循环时序单测（开关热改 / due_now / 手动 merge）；`cargo test` |
| B（并发与入口） | #3 #4 | `helper_main.rs` 加锁或幂等跳过、`tray/mod.rs` 跳转改 `/settings/network` | helper 双启实验 + 托盘实机点检 |
| C（语义与清理） | #5 #7 #8 #9 #10 #11 #12 #14 #15 | 多文件小改 | 补错误分类单测；`cargo test` |

每个批次结束执行：`cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`。
