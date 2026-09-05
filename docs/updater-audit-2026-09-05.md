# 更新子系统（updater / helper）审计复核报告

- 审计日期：2026-09-05
- 代码基线：`9d6ecaa`（v5.0.0-alpha.7）
- 版本：**v2**（经独立复核后修订：关闭 #10 待确认项、补 3 处方案缺口、修正批次并行性）
- 范围：`src/updater/`（mod / check / download / apply / error）、`src/helper_main.rs`、
  `src/web/routes/system.rs`、`src/tray/mod.rs`、`src/main.rs`、`src/status/snapshot.rs`
- 方法：逐条核对文件:行号，判定成立 / 不成立 / 部分成立，并给出最优修复方案

## 修订记录（v1 → v2）

外部复核提出 2 处事实错误、3 处方案缺口、3 处小瑕疵，已全部核实并并入正文：

| 来源 | 处置 |
|------|------|
| #10 `resources/` 落点"待确认" | **关闭**：`web/routes/system.rs:418`（`/api/icons`）、`web/routes/tools.rs:16-19`（录制脚本 primary）均按 `base_path/resources/...` 解析 → 维持写 base_path 已被消费方锚定 |
| 批次 A/B "可并行" | **改为串行**：B 的 `.connect_timeout` 落在 `updater/mod.rs:122/:588`，与 A 同文件 |
| #3 `into_pinned` 缺版本比较 | **补 semver 闸门**（helper 替换路径无版本检查，见 `helper_main.rs:51-52` `#[allow(dead_code)] version`） |
| #4 移除总超时后 `send()` 裸奔 | **补 `send()` 超时包裹** |
| #1 helper fallback 弱于现状 | **放行条件加回 `is_within_base` 兜底** |
| 函数名 `resolve_worker_project_dir` | 更正为 `worker_project_dir`（`utils/paths.rs:155`） |
| #8 "429 只用于次级限流" | 表述放宽（GitHub 主限流亦可能回 429），方案本身已双状态码覆盖，无需改 |
| #5→#9 联动乐观 | 修正：托盘直接调 `check_update()` 并未读到 `update_available`，需菜单动态文本才算闭环 |

## 一、复核结论总表

| # | 问题 | 判定 | 严重度 | 影响面 |
|---|------|------|--------|--------|
| 1 | 自定义 `--base-path` 与更新路径校验冲突 | **成立**（另发现 1 处同源缺陷，见 #10） | P1 | 仅自定义 base 用户必现 |
| 2 | 启动 double-fire（重复拉清单） | **成立** | P3 | 每次启动多 1 次 API + N 次资产拉取 |
| 3 | `POST /api/system/update` 内部重查漂移 | **成立** | P2 | 版本窗口内前移会下到非展示版本 |
| 4 | 总超时 300s 卡慢网 | **成立，但量级需下修** | P2 | 慢网/大包场景更新必然失败 |
| 5 | 只置 `available:true` 不清 false | **成立，且字段无人消费** | P3 | 当前无实际危害（写而不读） |
| 6 | 备份双轨堆积 | 撤回（与原判一致） | — | — |
| 7 | helper 越界拒绝致 pending 死循环 | 撤回（与原判一致） | — | — |
| 8 | GitHub 配额误判（只认 429） | **成立** | P2 | 配额耗尽时报错不可读、不可恢复提示缺失 |
| 9 | 托盘 updater 是死依赖 | **成立** | P3 | 托盘用户无更新入口 |
| 10 | **新增**：helper 自更新写入 `base_path` 而非 exe 目录 | **成立** | P2 | 与 #1 同批；仅自定义 base 显现 |

---

## 二、逐条证据与修复方案

### 1. 自定义 `--base-path` 与更新路径校验冲突（P1，成立）

**证据链**

| 位置 | 事实 |
|------|------|
| `src/main.rs:16-29` | 默认 `base_path` = exe 所在目录；`--base-path` / `CAMPUS_AUTH_BASE_PATH` 可覆盖（`src/launcher.rs:39`） |
| `src/updater/mod.rs:263` | `staging_dir = base_path/update/staging`（数据目录，合理） |
| `src/updater/mod.rs:297` | `target_exe = env::current_exe()`（exe 目录，与 base 无关） |
| `src/updater/mod.rs:450-456` | `staging` **与** `target` 双双要求 `is_within_base` 否则 cleanup |
| `src/helper_main.rs:184-197` | helper 同样双双校验，越界则 `exit(1)` 且不清理 |

默认布局下 `base == exe 目录`，两个约束碰巧同时成立；一旦分离数据目录，`target` 恒越界：
helper 拒绝 → 不清理 → 下次启动 Rust 侧 `apply_pending_locked` 拒绝 → **cleanup 删除 staging** → 下载作废。

**补充纠正（对原判的两点修正）**

1. `--target` 不是"死 flag"：helper 侧 `helper_main.rs:170-179` 的 `cli.target.or_else(pending)` 分支
   确实生效，只是生产路径从来不传它，走的是 pending 回退。**即便显式传 `--target`，只要
   `is_within_base` 约束不改，问题依旧**——传参不是修复，改校验才是。
2. `pending.target_exe` 在 Rust 启动路径里**只用于校验，不参与替换**：`mod.rs:500` 重新取
   `current_exe()` 交给 `self_replace`。也就是说这个字段在 Rust 侧是纯冗余输入。

**最优修复（比"存在性+可写校验"更强，且代价相同）**

核心洞察：`target` 的合法值恒等于"当前进程自身"（Rust 侧）/"helper 的同级主 exe"（helper 侧），
两者都不来自 `pending.json` 的信任链，因此应当**直接比对真实路径**，而不是放宽为"存在且可写"
（后者会把篡改的 `pending.json` 变成任意可写文件覆写原语）。

`src/updater/mod.rs`：

```rust
// 校验处提前取 current_exe（原 :500 的重复取值删除）
let current_exe = std::env::current_exe().map_err(UpdaterError::CurrentExeResolveFailed)?;

// staging 是 remove_dir_all 的目标 → 必须锁在 base 内（防任意目录删除）
// target 只可能是「当前进程自身」（self_replace 替换的就是 current_exe），
// 与 base_path 无关，故直接与 current_exe 比对：比"位于 base 内"更强且兼容
// --base-path 与 exe 目录分离
if !is_within_base(&staging_dir, &self.base_path)
    || !same_existing_path(&target_exe, &current_exe)
{
    tracing::error!("pending 路径校验失败，已拒绝应用并清理");
    apply::cleanup_after_apply(&self.base_path).await;
    return Ok(false);
}

/// 判定两条路径指向同一文件（canonicalize 后比较；任一不存在即 false）
fn same_existing_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}
```

`src/helper_main.rs`（替换 :170-197）：

```rust
// 目标 exe 由 helper 自身位置推导：helper 与主 exe 同目录、文件名固定，
// 无需信任 pending.json / CLI 提供的 target（更强，且支持 base 与 exe 分离）
let derived = std::env::current_exe()
    .ok()
    .and_then(|p| p.parent().map(|d| d.join(exe_name())));
let provided = cli
    .target
    .or_else(|| pending.as_ref().map(|p| PathBuf::from(&p.target_exe)));
let target_exe = match (derived, provided) {
    (Some(d), p) if d.is_file() => {
        // 推导值存在：提供值必须与其一致，否则判定 pending 被篡改
        if let Some(ref p) = p {
            if !same_existing_path(p, &d) {
                log.error(&format!("拒绝执行：target 与推导的主程序不一致: {}", p.display()));
                std::process::exit(1);
            }
        }
        d
    }
    // 推导值缺失（主 exe 被重命名/删除）：退回提供值。
    // v2：此处不能退化为"任意已存在文件"——那比现状（要求 target 在 base 内）更弱，
    // 会把篡改的 pending.json 变成任意文件覆写原语。故保住旧约束作为兜底。
    (_, Some(p)) if p.is_file() && is_within_base(&p, &base_path) => p,
    _ => {
        log.error("无法确定目标 exe 路径（推导值缺失且提供值不可用或不在 base_path 内）");
        std::process::exit(1);
    }
};
// staging 仍是 remove_dir_all 的目标，维持 base 内约束
if !is_within_base(&staging_dir, &base_path) { /* 同现状 exit(1) */ }
```

`spawn_helper`（`mod.rs:356-363`）顺手显式传入 `--target`，让 helper 的 CLI 分支在生产路径上
真正被使用（一行，无副作用）：

```rust
cmd.arg("--apply-update").arg("--pid").arg(pid.to_string())
   .arg("--staging").arg(&staging_dir)
   .arg("--target").arg(&current_exe)      // 新增
   .arg("--base-path").arg(&self.base_path);
```

**行为取舍（v2 明示，非缺陷）**：Rust 侧 `same_existing_path(target, current_exe)` 比现状
**更严**——若用户在"下载完成"到"下次启动"之间重命名了主 exe，现状会照常应用更新，
新逻辑会判定不一致并清理 staging。影响极小（重新检查更新即可自愈），但属行为变化，
写进 changelog。helper 侧保留 `is_within_base` 兜底后，重命名场景在 helper 路径仍可自愈。

**验收**：新增单测——`base_path` 指向 tempdir、`current_exe` 路径写入 pending，
断言 `apply_pending_locked` 不再因越界清理；helper 侧为 `target_exe` 的推导/比对逻辑
补纯函数单测（含三分支：推导命中且一致 / 推导命中但提供值不一致 → 拒绝 / 推导缺失且
提供值在 base 内 → 放行）。

---

### 2. 启动 double-fire（P3，成立）

**证据**：`src/updater/mod.rs:161-194`。`sleep(5s)` → 启动查一次（:165-172）→ 进入 `loop`
**首轮不等直接再查**（:181-189）→ 才 sleep。默认 `check_on_startup = true`、
`check_interval_hours = 24`（`src/config/schema.rs:385-388`），故每次启动固定双查。

危害量级与原判一致：sha256 走 `browser_download_url`（对象存储，不占 API 配额），
实际多耗 1 次 REST 调用 + N 次资产拉取 + 启动后 5 秒的额外网络抖动。

**最优修复**：循环改为"先睡后查"，同时保持取消响应性。

```rust
loop {
    let settings = config.load_settings().global.updater;
    if settings.check_interval_hours == 0 {
        cancel.cancelled().await;
        break;
    }
    let interval_secs = (settings.check_interval_hours as u64).saturating_mul(3600);
    let interval = std::time::Duration::from_secs(interval_secs.max(300));
    // 先等待再检查：启动检查已在循环外完成，避免同一时刻连查两次
    tokio::select! {
        _ = cancel.cancelled() => break,
        _ = tokio::time::sleep(interval) => {},
    }
    let client = effective_client_for(&settings, fallback_client.clone());
    if let Err(e) = perform_update_check(&config, &status, &client, &current_version).await {
        log_check_failure("定期", &e);
    }
}
```

语义变化：启动检查（受 `check_on_startup` 控制）+ 每 `interval` 一次周期检查；
**关闭启动检查的用户，首次周期检查从 T+5s 推迟到 T+24h**（`interval` 默认 24h）。

**行为取舍（v2 明示）**：这是产品决策而非纯技术优化，须写进 changelog。
若想两全——关掉启动检查的用户仍希望首查在启动后不久发生——让循环首轮跳过 sleep：

```rust
// 启动检查已执行过则首轮先等待；未执行（check_on_startup=false）则首轮立即查一次
let mut due_now = !startup_settings.check_on_startup;
loop {
    let settings = config.load_settings().global.updater;
    if settings.check_interval_hours == 0 { cancel.cancelled().await; break; }
    let interval = Duration::from_secs(((settings.check_interval_hours as u64) * 3600).max(300));
    if !due_now {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(interval) => {},
        }
    }
    due_now = false;
    let client = effective_client_for(&settings, fallback_client.clone());
    if let Err(e) = perform_update_check(&config, &status, &client, &current_version).await {
        log_check_failure("定期", &e);
    }
}
```

（`startup_settings` 需从循环外带进来；两种写法任选其一，不要既 sleep 又立即查。）

---

### 3. `POST /api/system/update` 内部重查漂移（P2，成立）

**证据链**

| 位置 | 事实 |
|------|------|
| `frontend/src/views/AboutView.vue:42-52` | 「检查更新」→ `GET /api/check-update`，展示 `latest` / `url` / `sha256` |
| `AboutView.vue:55-58` | 「立即更新」→ `POST /api/system/update`，**无请求体** |
| `src/web/routes/system.rs:277-289` | handler 自行再调 `check_update()` 才 `apply_update` |
| `openapi.json:1949-1956` | 该端点 `requestBody.required = false`，schema 为空 |

用户点击"更新 v5.0.1"与实际下载的版本之间隔着一次网络往返；若此间发布新版本，
下到的是 v5.0.2 而 UI 展示的是 v5.0.1，且用户无法感知。每次 apply 还多一次全量 sha 拉取。

**最优修复**：接受 pin（服务端仍做完整校验，不引入任何新的信任面）。

1. 新增请求体（`src/web/routes/system.rs`）：

```rust
/// 前端"检查更新"已确认的版本快照；省略则服务端重新拉取清单（兼容旧调用方）
#[derive(Debug, Default, Deserialize)]
pub struct ApplyUpdateBody {
    version: Option<String>,
    url: Option<String>,
    sha256: Option<String>,
}

impl ApplyUpdateBody {
    /// 三项齐备且通过安全校验时，构造可直接下发的 UpdateInfo
    ///
    /// 任一校验不通过返回 `None`，调用方回退到服务端重新拉取清单。
    fn into_pinned(self) -> Option<UpdateInfo> {
        let (version, url, sha256) = (self.version?, self.url?, self.sha256?);
        // 与下载路径同一白名单：https 放行，http 仅精确回环
        if !crate::updater::check::is_allowed_update_url(&url) {
            return None;
        }
        // 摘要须为 64 位 hex（后续 download_and_verify 仍会再校验一次）
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        // v2 补：版本闸门。U3 的"pending 版本不高于当前则跳过"只存在于启动
        // self_replace 路径（`updater/mod.rs:490-499`），helper 替换路径完全没有
        // 版本检查（`helper_main.rs:51-52` 的 version 字段是 #[allow(dead_code)]）。
        // 缺此闸门时，pin 一个旧版本号可畅通走完"下载 → helper 替换"，形成降级通道。
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
        let remote = semver::Version::parse(&version).ok()?;
        if !crate::updater::check::compare_versions(&current, &remote) {
            tracing::warn!(%version, "拒绝应用固定版本：不高于当前版本");
            return None;
        }
        Some(UpdateInfo {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: version,
            update_available: true,
            url,
            sha256,
            size: None,
            notes: None,
            release_date: None,
        })
    }
}
```

**关于信任面**：pin 的 `url` 允许任意 https host 看似放宽，但 `release_source_url`
本就可由 config API 修改且走同一白名单，故未引入新的信任面，此论断经复核确认成立。
```

2. handler：

```rust
pub async fn apply_update(
    State(updater): State<Arc<dyn UpdaterApi>>,
    body: Option<Json<ApplyUpdateBody>>,
) -> Result<Json<Value>, ApiError> {
    let info = match body.and_then(|Json(b)| b.into_pinned()) {
        Some(pinned) => pinned,
        None => updater.check_update().await
            .map_err(|e| ApiError::Internal(format!("检查更新失败: {e}")))?
            .ok_or_else(|| ApiError::BadRequest("当前已是最新版本，无需更新".into()))?,
    };
    ... // 后续不变
}
```

3. 前端 `AboutView.applyUpdate()` 传入 `updateInfo` 里的 `latest`/`url`/`sha256`；
   同步更新 `openapi.json:1949` 的 requestBody schema 与前端类型。

注：`UpdateInfo.size` 不参与下载（`download.rs:67` 用 `response.content_length()`），
pin 路径下置 `None` 无副作用。

---

### 4. 总超时 300s（P2，成立，量级需下修）

**证据**：`src/updater/download.rs:28` `DOWNLOAD_TOTAL_TIMEOUT = 300s`；
`:54-56` `.timeout(DOWNLOAD_TOTAL_TIMEOUT)` 作用于 `RequestBuilder`。
reqwest 的请求级 timeout 覆盖"开始连接 → 响应体读完整"，即**整个下载全程**，确认成立。

**量级修正**：512MB 是 `MAX_UPDATE_ARCHIVE_BYTES` 上限，非实际包体。实测主 exe 14.2 MB，
发布 zip 量级约 15~25 MB → 触发阈值约 **50~85 KB/s 持续**，而非 1.7 MB/s。
结论不变（校园网慢节点/晚高峰确实会掉到这个量级），但严重度应从"必死"下修为"特定场景必死"。
更关键的是：300s 到点即整体丢弃且**无断点续传**（`download.rs:88-91` 明说不实现），
90% 进度的下载白费。

**最优修复**：拆成"连接超时 + chunk 间空闲超时"，取消总超时。

1. `download.rs`：删除 `.timeout(DOWNLOAD_TOTAL_TIMEOUT)`，新增常量与停滞判定：

```rust
/// 建立连接超时（TCP + TLS 握手，客户端级）
pub(crate) const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 停滞判定：等待响应头 / 相邻 chunk 之间的最大空闲时间。
/// 慢网只要还在出数据就允许继续，只有"彻底不出数据"才判失败。
pub(crate) const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);
```

`send()` 阶段同样要包（v2 补）：删掉 `.timeout()` 后，若只留 `connect_timeout`，
则**握手完成后服务端拖着不发响应头会无限挂起**——停滞超时只覆盖 body chunk，不覆盖
等待响应头这一段。

```rust
let response = match tokio::time::timeout(
    DOWNLOAD_STALL_TIMEOUT,
    async {
        client
            .get(&info.url)
            .header("User-Agent", "campus-auth-updater")
            .send()
            .await
            .map_err(UpdaterError::DownloadFailed)?
            .error_for_status()
            .map_err(UpdaterError::DownloadFailed)
    },
)
.await
{
    Ok(Ok(r)) => r,
    Ok(Err(e)) => return Err(e),
    Err(_) => {
        return Err(UpdaterError::DownloadStalled {
            idle_secs: DOWNLOAD_STALL_TIMEOUT.as_secs(),
            received_bytes: 0,
        })
    }
};
```

body 阶段沿用同一常量：

```rust
let chunk = match tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, stream.next()).await {
    Ok(Some(c)) => c.map_err(UpdaterError::DownloadFailed)?,
    Ok(None) => break,                       // 正常 EOF
    Err(_) => return Err(UpdaterError::DownloadStalled {
        idle_secs: DOWNLOAD_STALL_TIMEOUT.as_secs(),
        received_bytes: downloaded,
    }),
};
```

2. `error.rs` 新增变体（原 `DownloadFailed(reqwest::Error)` 无法承载 `Elapsed`）：

```rust
/// 下载停滞：超过上限时间未收到任何数据
#[error("下载停滞：{idle_secs} 秒内未收到数据（已接收 {received_bytes} 字节）")]
DownloadStalled { idle_secs: u64, received_bytes: u64 },
```

3. 连接超时放在客户端构建处（`updater/mod.rs:122` 与 `:588` 两处 builder 各加一行
   `.connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)`），连接类失败仍走 `DownloadFailed`。

取舍：移除总超时后，理论上可被"每 59 秒喂 1 字节"的服务端拖住；已有
`MAX_UPDATE_ARCHIVE_BYTES` 作为容量上界，如需硬兜底可再加一个宽松的
`DOWNLOAD_HARD_TIMEOUT = 2h`（可选，非必须）。

---

### 5. `available` 只置 true 不清 false（P3，成立，且字段无人消费）

**证据**：`src/updater/mod.rs:626-633` 命中才 `merge(Update { available: true })`；
`src/status/snapshot.rs:313` 是唯一写入点；全仓 `PartialSnapshot::Update` 仅 3 处引用
（`updater/mod.rs:630`、测试 `snapshot.rs:431`、`snapshot.rs:313`），**无任何 `false` 调用方**。

同进程内一次置 true 后不会被清除；重启归零，故跨启动无残留——原判"降 P3"成立。

**追加发现**：`snapshot.update_available`（`snapshot.rs:130`）在 `src/` 与 `frontend/src/`
**均无读取方**，是纯写字段。因此这一行的修复目前是"把没人看的字段改对"。

**最优修复**：一行补 else，并与 #9 一起决定该字段的去留。

```rust
if check::select_platform(&manifest).is_some()
    && check::compare_versions(current_version, &manifest.version)
{
    status.merge(PartialSnapshot::Update { available: true });
    tracing::info!("发现新版本: {} → {}", current_version, manifest.version);
} else {
    status.merge(PartialSnapshot::Update { available: false });
}
```

**v2 修正（联动口径）**：v1 称"#9 加托盘菜单项即可给该字段找到读者"过于乐观——
直接调 `deps.updater.check_update()` 并不读 `snapshot.update_available`。
要真正闭环，托盘须**订阅状态**并用 `update_available` 驱动菜单文本（见 #9 步骤 4）。
若不做步骤 4，则 `update_available` 应在 #9 中与 `TrayDeps.updater` 一并删除，
本条 else 分支也就没有存在意义——二者绑在同一个决策上，不要拆开做。

---

### 8. GitHub 配额误判（P2，成立）

**证据**：`src/updater/check.rs:127-136` 仅处理 `429`；`:138-140` 其余状态码一律
`error_for_status() → ManifestFetchFailed`。而未认证 REST 配额耗尽返回的是
**403 + `X-RateLimit-Remaining: 0` + `X-RateLimit-Reset: <epoch>`**（GitHub 主限流按类型
可能回 403 或 429，但只处理 429 会漏掉 403 这一主要分支），
故真实超限时用户看到的是"拉取发布清单失败: HTTP status client error (403 Forbidden)"，
而非可理解的"请在 N 秒后重试"。

**最优修复**：把配额判定抽成**纯函数**（便于单测，无需起 HTTP 服务），429/403 共用。

```rust
/// 从响应头判定是否为配额耗尽，并折算建议等待秒数
///
/// GitHub 未认证 REST 超限回 403（429 仅用于次级限流）；二者都带
/// `X-RateLimit-Remaining: 0`，`X-RateLimit-Reset` 为配额重置的 Unix 秒。
fn rate_limit_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let remaining_is_zero = headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.trim() == "0");
    if !remaining_is_zero {
        return None;
    }
    let until_reset = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .map(|reset| (reset - chrono::Utc::now().timestamp()).clamp(1, 3600) as u64);
    Some(until_reset.unwrap_or(60))
}
```

`fetch_manifest` 替换原 429 分支：

```rust
if matches!(response.status(), StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN) {
    if let Some(retry_after) = rate_limit_retry_after(response.headers()) {
        return Err(UpdaterError::RateLimited { retry_after });
    }
    // 429 但无配额头（代理/网关限流）：沿用 retry-after 头，缺省 60s
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = response.headers()
            .get("retry-after").and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok()).unwrap_or(60);
        return Err(UpdaterError::RateLimited { retry_after });
    }
}
```

403 且不带配额头时仍落 `ManifestFetchFailed`，与现状一致，无回归。
单测直接构造 `HeaderMap` 即可覆盖：剩余非 0 / 剩余 0 无 reset / 剩余 0 有 reset（含负数夹取）。

---

### 9. 托盘 updater 死依赖（P3，成立）

**证据**：`src/tray/mod.rs:29` 引入 `UpdaterService`、`:82-83` 声明 `TrayDeps.updater`，
全文件再无 `updater` 出现（grep 仅 2 处命中，均为声明）。菜单仅三项
（`:566-577`：`monitor_toggle` / `open_web` / `quit`），`TrayAction` 枚举（`:58-67`）
与事件分发（`:244-245`）均无更新项。托盘用户只能先打开 Web 控制台再去关于页。

**最优修复：接上消费方（而非删字段）**。理由：轻量模式下 Axum 默认不常驻，
托盘是用户唯一常驻 UI；删除字段等于放弃这条入口。新增菜单项同时给 #5 的
`update_available` 找到读者，两个问题一并收敛。

1. `TrayAction` 增 `CheckUpdate`；`build_menu()`（`:566-577`）在 `open_web` 之后插入：

```rust
Box::new(MenuItem::with_id(MenuId::new("check_update"), "检查更新", true, None)),
```

2. 事件分发（`:244-245` 附近）增 `"check_update" => TrayAction::CheckUpdate`。

3. 处理分支（仿 `TrayAction::OpenWeb`，`:465-501`）：

```rust
TrayAction::CheckUpdate => {
    let updater = deps.updater.clone();
    let port_hint = deps.port;
    let host = deps.host.clone();
    let base = deps.config.base_path();
    tokio::spawn(async move {
        match updater.check_update().await {
            Ok(Some(info)) => {
                info!("发现新版本 v{}，打开控制台关于页", info.latest_version);
                // 有更新：直接把用户送到关于页（history 模式，路由 /about）
                let port = crate::utils::paths::read_runtime_port(&base).unwrap_or(port_hint);
                let url = format!("http://127.0.0.1:{port}/about");
                if let Err(e) = open::that(&url) { warn!("打开浏览器失败 ({url}): {e}"); }
            }
            Ok(None) => info!("已是最新版本"),
            Err(e) => warn!("检查更新失败: {e}"),
        }
    });
    false
}
```

（轻量模式下若要复用"按需启动 Axum"逻辑，把 `resolve_web_port` + `app::start_axum`
那段抽成 `ensure_console_url(&deps, axum_handle).await` 供两个分支共用，避免复制粘贴。）

4. **（v2 补，决定 #5 是否成立）** 让菜单项文本由 `snapshot.update_available` 驱动，
   复用 `toggle_item` 已有的 `set_text` 机制（`update_tray`）：为 true 时文本改为
   "发现新版本，点击更新"，为 false 时复原为"检查更新"。这样后台检查的 merge
   （含 #5 的 else 清 false）才真正被消费；否则该字段仍是写而不读。

   注：`Update { available }` 变体不携带版本号，菜单文本只能表达"有/无"；
   若要显示 `v5.0.1`，需扩展该变体带 `latest: Option<String>`，属于额外改动，按需决定。

**备选**：若确定不做托盘更新入口，则同时删除 `TrayDeps.updater`、`tray/mod.rs:29` 的
`use` 与 `snapshot.update_available` 字段（连带 #5 的 else 分支也不必加）——
二选一，不留悬空依赖。

---

### 10. 【新增】helper 自更新写入 `base_path` 而非 exe 目录（P2，成立）

**证据**：`src/helper_main.rs:392-402`，`replace_helper` 的落点是
`base_path.join(helper_name)`；而 `spawn_helper`（`updater/mod.rs:337-339`）是
**从 exe 目录**找 helper。默认布局两者同目录所以无感；`--base-path` 分离后：
新版 helper 被复制到数据目录（一份无人调用的副本），exe 目录下的 helper 永不更新。

**最优修复**：自更新落点改为 helper 自身所在目录（= 主 exe 目录）。

```rust
// 5.5 helper 与主 exe 同目录；自更新目标必须是 exe 目录，不能是 base_path
// （--base-path 分离时 base_path 是数据目录，写进去的 helper 不会被 spawn 到）
let exe_dir = std::env::current_exe()
    .ok()
    .and_then(|p| p.parent().map(|d| d.to_path_buf()));
let install_dir = exe_dir.as_deref().unwrap_or(base_path);
sync_distribution_files(&extracted_dir, base_path);   // 数据目录：python_worker / docs
replace_helper(&extracted_dir, install_dir, &mut log); // 二进制目录：helper 自身
```

`sync_distribution_files` 保持写 `base_path` **三个目录全部正确，本次不动**：

- `python_worker/`：`utils::paths::worker_project_dir`（`src/utils/paths.rs:155`，
  历史名 `environment::resolve_worker_project_path`）主路径即 `<base_path>/python_worker`；
- `resources/`：**v2 关闭"待确认"** ——确有 Rust 侧消费方且按 `base_path` 解析：
  `web/routes/system.rs:418`（`GET /api/icons` 扫描 `base_path/resources/icons`）、
  `web/routes/tools.rs:16-19`（任务录制脚本 primary 候选 `base_path/resources/tools/`）。
  v1 误判为"Rust 侧无引用"（只看了 Python worker 生成的 `resources/<name>` 相对路径），
  结论虽同（维持 base_path），依据此前是错的；
- `docs/`：仅人工阅读，随数据目录即可。

---

## 三、分批实施建议

| 批次 | 条目 | 改动量 | 验收 |
|------|------|--------|------|
| A（正确性） | #1 + #10 | `updater/mod.rs` ~15 行、`helper_main.rs` ~30 行 | 新增 base 分离场景单测（三分支）；`cargo test` |
| B（体验） | #4 + #8 | `updater/mod.rs` +2（builder）、`download.rs` ~25 行、`error.rs` +5、`check.rs` ~20 行 | HeaderMap 纯函数单测 + 停滞超时单测；`cargo test` |
| C（接口） | #3 | `system.rs` ~45 行 + 前端 + openapi | 手工点"立即更新"验证版本一致；pin 旧版本应被拒 |
| D（清理） | #2 + #5 + #9 | `updater/mod.rs` ~8 行、`tray/mod.rs` ~40 行 | 托盘菜单实机验证；`cargo test` |

**执行顺序（v2 修正）**：A 与 B **不可并行**——B 的 `.connect_timeout` 要改
`updater/mod.rs:122` 与 `:588` 两处 builder，A 也改 `updater/mod.rs`（校验逻辑 +
`spawn_helper`），同文件冲突。按 A → B → C → D 串行；若确要并行，B 需等 A 合入后
rebase（改动量小，rebase 成本可接受）。C 涉及 openapi + 前端契约，须单独收口。

每批结束执行：`cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
（CI 要求零警告，见 `AGENTS.md`）。

## 四、维持原判

- **供应链**：sha 与 zip 同 release 同源（`release.yml:137-148`），防损坏不防投毒，
  信任根 = GitHub 账号 + HTTPS，无签名。已知 tradeoff，文档注明即可，不列为缺陷。
- **#6 备份双轨**：各路径只产一种备份且自清理（`mod.rs:507-514` / `:519-527`、
  `helper_main.rs:294-312`），无交叉残留。撤回，无误报。
- **#7 pending 死循环**：Rust 侧拒绝即清理（`mod.rs:453-455`），helper 拒绝不清理但把机会
  留给下次启动，**不清理正是刻意设计**（避免摧毁待应用更新）。撤回，无误报。
