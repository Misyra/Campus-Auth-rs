//! 任务文件 CRUD 管理：TaskManager
//!
//! 任务以 JSON 文件形式存储于 `<tasks_dir>/<类型桶>/` 子目录——`browser/`、`scripts/`、
//! `http/` 三类各占一桶，任务类型与存储桶一一对应（目录选择、残留清理、列表类型标注
//! 全部经 [`TaskManager::bucket_dir`] 单点分派，避免新增类型时漏改某一处）。
//! 任务排序记录于 `<tasks_dir>/.order.json`（一个扁平 id 列表，三类任务共用同一份排序）。
//! 所有写操作通过 `tokio::sync::Mutex` 串行化，避免并发写冲突。`task_id` 校验采用手动
//! ASCII 检查（避免引入 `regex` 依赖）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::tasks::TaskError;
use crate::tasks::models::*;
/// 任务排序记录（`.order.json`）
///
/// 只管排序：启用哪个任务是**每个方案各自的** `ProfileData::active_task`
/// （「账号」「配置方案」页按方案绑定，切方案即切任务）。历史上此处曾有全局
/// `active` 字段，已由 config v9 迁移（`migration::migrate_v8_to_v9`）搬空。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrderData {
    /// 排序后的任务 ID 列表
    pub order: Vec<String>,
}

/// 内置默认登录任务种子（`browser/default.json` 不存在时首启写入）。
///
/// 经 `include_str!` 编进二进制，便携包/新装同样可用；内容与历史 `通用登录`
/// 任务一致（JS 语义填表，`url` 取 `{{LOGIN_URL}}`，重定向模式由 Worker 先行导航）。
const DEFAULT_TASK_SEED: &str = include_str!("seed_default.json");

/// 内置默认任务 ID（不可删除，删除其他任务回退到它）
pub const DEFAULT_TASK_ID: &str = "default";

/// 非浏览器任务占用内置默认任务 ID 时的报错文案（`validate_task` 与 `save_task` 共用，
/// 避免两处文案漂移）
fn reserved_default_id_error() -> String {
    format!("任务 ID「{DEFAULT_TASK_ID}」保留给内置浏览器任务，请换一个 ID")
}

/// 任务摘要（列表/概览用，不含完整配置）
///
/// 带 `url` / `http_method` 两个**展示字段**：任务列表要在行内显示「请求地址摘要 +
/// GET/POST」，若摘要里没有它们，前端就得对每条任务再发一次详情请求（N+1，且每次
/// 列表刷新都要重来）。列表读取本来已经把整个 JSON 解析出来了，顺手取字段是免费的。
#[derive(Debug, Clone, Default, Serialize)]
pub struct TaskSummary {
    /// 任务 ID（= 文件名 stem）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 任务描述
    pub description: String,
    /// 任务类型：`browser` / `script` / `http`
    pub task_type: String,
    /// 任务地址：浏览器任务=登录页地址，直连任务=请求地址，脚本任务为空串
    pub url: String,
    /// 直连任务的请求方法；非直连任务为 `None`（前端据此决定是否渲染方法标签）
    pub http_method: Option<HttpRequestMethod>,
}

/// 任务详情（摘要 + 完整配置）
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    /// 任务摘要
    pub summary: TaskSummary,
    /// 完整配置
    pub config: TaskKind,
}

/// 任务文件 CRUD 管理器
pub struct TaskManager {
    /// `tasks/` 根目录
    tasks_dir: PathBuf,
    /// `tasks/browser/` 目录
    browser_dir: PathBuf,
    /// `tasks/scripts/` 目录
    scripts_dir: PathBuf,
    /// `tasks/http/` 目录（http 直连任务）
    http_dir: PathBuf,
    /// 文件写操作互斥锁
    lock: Mutex<()>,
}

/// H5：单条步型字段约束 `(适用步型, required, any_of, any_of 全空时的报错文案)`
type StepFieldRule = (
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
);

impl TaskManager {
    /// 构造管理器，确保子目录存在，并迁移旧版 `active.txt`、初始化 `.order.json`
    pub fn new(base_path: &Path) -> Arc<Self> {
        // 路径经 `utils::paths` 统一；`ensure_runtime_dirs` 已在启动预建，
        // 此处保留幂等创建以兼容测试直构（防御性，不作为权威）。
        let tasks_dir = crate::utils::paths::tasks_dir(base_path);
        let browser_dir = crate::utils::paths::browser_tasks_dir(base_path);
        let scripts_dir = crate::utils::paths::scripts_dir(base_path);
        let http_dir = crate::utils::paths::http_tasks_dir(base_path);
        // 构造期目录创建失败会导致后续所有任务读写连锁失败，必须告警
        if let Err(e) = std::fs::create_dir_all(&browser_dir) {
            tracing::warn!(
                path = %browser_dir.display(),
                error = %e,
                "创建浏览器任务目录失败，后续任务读写可能连锁失败"
            );
        }
        if let Err(e) = std::fs::create_dir_all(&scripts_dir) {
            tracing::warn!(
                path = %scripts_dir.display(),
                error = %e,
                "创建脚本任务目录失败，后续任务读写可能连锁失败"
            );
        }
        if let Err(e) = std::fs::create_dir_all(&http_dir) {
            tracing::warn!(
                path = %http_dir.display(),
                error = %e,
                "创建 http 直连任务目录失败，后续任务读写可能连锁失败"
            );
        }

        let mgr = Self {
            tasks_dir,
            browser_dir,
            scripts_dir,
            http_dir,
            lock: Mutex::new(()),
        };

        // 不存在则创建默认 .order.json
        if !mgr.order_path().exists() {
            if let Err(e) = mgr.write_order(&OrderData::default()) {
                tracing::warn!(
                    path = %mgr.order_path().display(),
                    error = %e,
                    "初始化默认 .order.json 失败，后续任务排序可能异常"
                );
            }
        }
        // 首启播种：缺内置默认任务则写入（新装开箱即用；启用状态由各方案绑定）
        mgr.ensure_default_task();
        Arc::new(mgr)
    }

    /// 列出所有任务摘要（按 `.order.json` 排序）
    pub async fn list_all_tasks(&self) -> Vec<TaskSummary> {
        let _guard = self.lock.lock().await;
        // 目录扫描为阻塞 I/O，放到 spawn_blocking 中执行以免阻塞 tokio worker 线程。
        // 所需路径字段提前 clone 后 move 进闭包；`.order.json` 的读取也一并放入
        // 闭包（同为同步磁盘 I/O，避免回到 async 后持 self.lock 再做同步读）。
        let browser_dir = self.browser_dir.clone();
        let scripts_dir = self.scripts_dir.clone();
        let http_dir = self.http_dir.clone();
        let order_path = self.order_path();
        let (mut summaries, order) = tokio::task::spawn_blocking(move || {
            let mut out: Vec<TaskSummary> = Vec::new();

            // 浏览器任务（browser/*.json）：桶即类型，文件内 type 缺失也按 browser 标注
            Self::scan_json_bucket(&browser_dir, Some("browser"), &mut out);
            // 脚本任务（scripts/*.json，排除 .meta.json）：该目录混放裸 .py 与历史
            // .meta.json，其中 JSON 任务的类型仍以文件内 type 为准（与 load_task 口径一致）
            Self::scan_json_bucket(&scripts_dir, None, &mut out);
            // http 直连任务（http/*.json）：桶即类型
            Self::scan_json_bucket(&http_dir, Some("http"), &mut out);

            // 旧版裸 .py 脚本兼容：同名 .json 任务已存在时跳过（G7），
            // 避免 scripts/foo.json 与 scripts/foo.py 以相同 id 重复出现在列表中
            let seen_ids: std::collections::HashSet<String> =
                out.iter().map(|s| s.id.clone()).collect();
            if let Ok(entries) = std::fs::read_dir(&scripts_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("py") {
                        continue;
                    }
                    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if seen_ids.contains(stem) {
                        continue;
                    }
                    if let Some(s) = Self::read_py_summary(&path) {
                        out.push(s);
                    }
                }
            }

            (out, Self::read_order_at(&order_path))
        })
        .await
        .unwrap_or_else(|e| {
            // JoinError 意味着扫描任务 panic（如目录元数据异常），
            // 静默返回空列表会让用户误以为任务全部丢失，必须记录错误
            tracing::error!("任务目录扫描失败（返回空列表）: {e}");
            (Vec::new(), OrderData::default())
        });

        // 按 order 排序，未在列表中的排末尾
        summaries.sort_by_key(|s| {
            order
                .order
                .iter()
                .position(|id| id == &s.id)
                .unwrap_or(usize::MAX)
        });
        summaries
    }

    /// 加载单个任务完整 JSON
    pub async fn load_task(&self, task_id: &str) -> Result<TaskKind, TaskError> {
        if !is_valid_task_id(task_id) {
            return Err(TaskError::InvalidTaskId(task_id.to_string()));
        }
        let _guard = self.lock.lock().await;
        let path = self
            .find_task_file(task_id)
            .ok_or_else(|| TaskError::TaskNotFound(task_id.to_string()))?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(TaskError::IoError)?;
        let task: TaskKind =
            serde_json::from_str(&strip_bom(content)).map_err(TaskError::JsonError)?;
        // 「保存强校验、加载宽校验 + 告警」（G8）：磁盘上被外部工具改坏的任务仍尽量
        // 加载（容错，不拒绝），但通过日志暴露校验失败，便于排查"看似正常却执行异常"
        if let Ok(value) = serde_json::to_value(&task) {
            if let Err(errors) = self.validate_task(&value) {
                tracing::warn!(
                    "任务 {} 语义校验失败（仍按原样加载）: {:?}",
                    task_id,
                    errors
                );
            }
        }
        Ok(task)
    }

    /// 将浏览器任务配置嵌入 `params["task_config"]`（供 Python Worker 执行步骤）。
    ///
    /// 收敛 debug 路由与登录侧两处嵌入样板（C6）：任务不存在 / 非浏览器类型 /
    /// 序列化失败时仅告警、不嵌入，由调用方按原有行为处理（缺失 task_config 时
    /// Worker 按空步骤执行）。返回是否成功嵌入。
    pub async fn embed_task_config(&self, task_id: &str, params: &mut Value) -> bool {
        if task_id.is_empty() {
            return false;
        }
        match self.load_task(task_id).await {
            Ok(TaskKind::Browser(tc)) => match serde_json::to_value(&tc) {
                Ok(task_val) => {
                    params["task_config"] = task_val;
                    true
                }
                Err(e) => {
                    tracing::warn!(task_id = task_id, reason = %e, "任务序列化失败，未嵌入 task_config");
                    false
                }
            },
            Ok(_) => {
                // 非浏览器任务属可预期分支，降为 debug
                tracing::debug!(
                    task_id = task_id,
                    reason = "非浏览器任务",
                    "未嵌入 task_config"
                );
                false
            }
            Err(e) => {
                tracing::warn!(task_id = task_id, reason = %e, "加载任务失败，未嵌入 task_config");
                false
            }
        }
    }

    /// 保存任务（存在即更新）。根据 `TaskKind` 选择子目录，并维护 `.order.json`
    pub async fn save_task(&self, task_id: &str, task: &TaskKind) -> Result<(), TaskError> {
        if !is_valid_task_id(task_id) {
            return Err(TaskError::InvalidTaskId(task_id.to_string()));
        }
        // 保留 ID 在这里也拦一次：`validate_task` 看的是 JSON 里的 task_id，而保存路径的
        // 权威 id 是本函数入参（`PUT /api/tasks/{id}` 的 body 未必带正确 task_id，且
        // task_id 回写到 JSON 发生在校验之后）
        if task_id == DEFAULT_TASK_ID && !matches!(task, TaskKind::Browser(_)) {
            return Err(TaskError::ValidationFailed(vec![
                reserved_default_id_error(),
            ]));
        }
        let _guard = self.lock.lock().await;

        // 校验 JSON 字段
        let value = serde_json::to_value(task).map_err(TaskError::JsonError)?;
        self.validate_task(&value)
            .map_err(TaskError::ValidationFailed)?;

        let subdir = self.bucket_dir(task);
        let path = subdir.join(format!("{task_id}.json"));
        // 同 ID 切换类型时删除其他桶的残留（否则同一 id 会留下两份定义，
        // 加载时按桶优先级取到过时的那份而看不出问题）
        let stale_paths = self.other_bucket_paths(task, task_id);

        let mut task = task.clone();
        // 将 task_id 写回 common（避免 JSON 中遗漏）
        task.common_mut().task_id = task_id.to_string();

        atomic_write_json(&path, &task)?;
        for stale in stale_paths {
            if stale.exists() {
                let _ = std::fs::remove_file(&stale);
            }
        }
        // 追加到 order（如不存在）
        let mut order = self.read_order();
        if !order.order.contains(&task_id.to_string()) {
            order.order.push(task_id.to_string());
            self.write_order(&order)?;
        }
        Ok(())
    }

    /// 删除任务文件 + 更新 order + 处理 active 回退
    pub async fn delete_task(&self, task_id: &str) -> Result<(), TaskError> {
        if !is_valid_task_id(task_id) {
            return Err(TaskError::InvalidTaskId(task_id.to_string()));
        }
        if task_id == DEFAULT_TASK_ID {
            return Err(TaskError::DeleteDefaultTask);
        }
        let _guard = self.lock.lock().await;

        let mut found = false;
        // 三个桶都尝试删除：id 全局唯一，但历史切换类型时别桶可能留有同名文件
        for (dir, _) in self.buckets() {
            let path = dir.join(format!("{task_id}.json"));
            if path.exists() {
                tokio::fs::remove_file(&path)
                    .await
                    .map_err(TaskError::IoError)?;
                found = true;
            }
        }
        // 清理关联 .meta.json / .py（best-effort，失败仅 debug；这类附属文件只存在于 scripts/ 桶）
        let meta = self.scripts_dir.join(format!("{task_id}.meta.json"));
        if meta.exists() {
            if let Err(e) = tokio::fs::remove_file(&meta).await {
                tracing::debug!(path = %meta.display(), error = %e, "清理关联 .meta.json 失败");
            }
        }
        let py = self.scripts_dir.join(format!("{task_id}.py"));
        if py.exists() {
            if let Err(e) = tokio::fs::remove_file(&py).await {
                tracing::debug!(path = %py.display(), error = %e, "清理关联 .py 文件失败");
            }
        }

        if !found {
            return Err(TaskError::TaskNotFound(task_id.to_string()));
        }

        let mut order = self.read_order();
        order.order.retain(|id| id != task_id);
        self.write_order(&order)?;
        Ok(())
    }

    /// 加载任务详情（摘要 + 完整配置）
    pub async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
        let task = self.load_task(task_id).await?;
        let summary = TaskSummary {
            id: task_id.to_string(),
            name: task.common().name.clone(),
            description: task.common().description.clone(),
            task_type: task.type_name().to_string(),
            url: task.summary_url().to_string(),
            http_method: task.http_request_method(),
        };
        Ok(TaskDetail {
            summary,
            config: task,
        })
    }

    /// 读取 `.order.json`
    pub async fn load_order(&self) -> OrderData {
        self.read_order()
    }

    /// 保存 `.order.json`
    pub async fn save_order(&self, order: &OrderData) -> Result<(), TaskError> {
        if let Some(invalid) = order.order.iter().find(|id| !is_valid_task_id(id)) {
            return Err(TaskError::InvalidTaskId(invalid.clone()));
        }
        let _guard = self.lock.lock().await;
        self.write_order(order)
    }

    /// 返回脚本任务的文件路径（供执行器定位）
    pub async fn get_script_path(&self, task_id: &str) -> Option<PathBuf> {
        let task = self.load_task(task_id).await.ok()?;
        if let TaskKind::Script(cfg) = task {
            if let Some(p) = cfg.script_path {
                return Some(if Path::new(&p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    self.scripts_dir.join(p)
                });
            }
        }
        None
    }

    /// 判断任务文件是否存在（供调度器校验关联目标任务）
    pub fn has_task(&self, task_id: &str) -> bool {
        is_valid_task_id(task_id) && self.find_task_file(task_id).is_some()
    }

    /// 校验任务 JSON 格式（公开 API，符合规划 §3.7）
    ///
    /// 步型字段约束采用表驱动 [`Self::STEP_FIELD_RULES`]；wait 的"selector 或
    /// duration>0"双语义无法用纯字段表表达，保留特判（口径对齐 Python 执行器）。
    ///
    /// H5：步型字段约束表（每行：`(适用步型, required, any_of, any_of 全空时报错)`）：
    ///
    /// - `required` 中每个字段都必须存在且非空白字符串，缺失时报 `需要 {字段名}`
    /// - `any_of` 非空时要求至少一个字段非空白，全部为空时报 `any_msg`
    /// - 统一 trim 语义：原实现部分字段放行纯空白值、执行层才失败，现在校验层提前拒绝
    /// - select 的 value 是操作目标（option 的 value/文本），缺失时执行层会静默
    ///   no-op——所有步骤"成功"但运营商根本没选，required 的显式拒绝是有意行为
    const STEP_FIELD_RULES: &[StepFieldRule] = &[
        (
            &["input", "click", "ocr", "wait_for_selector", "upload_file"],
            &["selector"],
            &[],
            "",
        ),
        (&["select", "click_select"], &["selector", "value"], &[], ""),
        (&["assert_text"], &["value"], &[], ""),
        (&["wait_url"], &["pattern"], &[], ""),
        (
            &["eval", "custom_js", "evaluate", "custom"],
            &[],
            &["script", "code"],
            "需要非空 script 或 code",
        ),
        (
            &["goto", "navigate"],
            &[],
            &["url", "value", "selector"],
            "需要 url、value 或 selector",
        ),
        (
            &["upload_file"],
            &[],
            &["path", "value"],
            "需要 path 或 value",
        ),
    ];

    /// 字段缺失或为纯空白视为空（H5：统一 trim 语义）
    fn is_blank_field(step: &Value, key: &str) -> bool {
        step.get(key)
            .and_then(|v| v.as_str())
            .is_none_or(|s| s.trim().is_empty())
    }

    /// 校验任务配置 JSON 的结构与字段约束（导入/保存前的统一闸口）
    ///
    /// - `config`：待校验的原始任务 JSON，`type` 缺省按 `browser` 处理；
    /// - `Ok(())`：通过当前任务类型的全部校验（name 非空、timeout 区间钳制、
    ///   steps 步型字段表 STEP_FIELD_RULES、script 必填项、http 请求地址/认证地址
    ///   协议与载荷体积上限、PowerShell 与路径穿越拦截等）；
    /// - `Err(Vec<String>)`：校验不通过，携带**全部**（而非首个）人读错误文案，
    ///   顺序即校验遍历顺序，供前端一次性整体展示。
    pub fn validate_task(&self, config: &Value) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = Vec::new();
        let kind = config
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("browser");

        let name = config.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.trim().is_empty() {
            errors.push("name 不能为空".to_string());
        }

        // 内置默认任务的 ID 保留给浏览器任务。
        //
        // 三类任务共用同一个 `task_id` 命名空间（保存时同 ID 切换类型会清掉另一个桶的
        // 残留），而 `default` 是浏览器渠道未绑定方案时的兜底任务：让直连/脚本任务占用
        // 它会出现两种都很难排查的后果——保存时残留清理删掉种子文件（浏览器渠道随即
        // "当前无可用浏览器任务"），或两份定义按桶优先级互相遮蔽（编辑 A 却生效 B）。
        // 故非浏览器类型一律拒绝，文案直接告诉用户换一个 ID。
        let task_id = config.get("task_id").and_then(Value::as_str).unwrap_or("");
        if task_id == DEFAULT_TASK_ID && kind != "browser" {
            errors.push(reserved_default_id_error());
        }

        match kind {
            "browser" => {
                // 浏览器任务 timeout 钳制：与脚本任务 clamp_timeout 口径对齐。
                // 无校验时导入的第三方 JSON 可传 0（执行时 max(1) 变 1ms 永远
                // 秒超时）或超大值（会话槽位被占一天，期间所有浏览器任务/登录被拒）
                let timeout = config
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(crate::tasks::DEFAULT_TASK_TIMEOUT_MS);
                if !(MIN_TASK_TIMEOUT_MS..=MAX_TASK_TIMEOUT_MS).contains(&timeout) {
                    errors.push(format!(
                        "timeout 需在 {MIN_TASK_TIMEOUT_MS}-{MAX_TASK_TIMEOUT_MS} 毫秒之间（当前 {timeout}）"
                    ));
                }
                let steps = config.get("steps").and_then(|s| s.as_array());
                match steps {
                    None => errors.push("steps 必须为数组".to_string()),
                    Some(arr) => {
                        if arr.is_empty() {
                            errors.push("steps 不能为空".to_string());
                        }
                        let mut ids = std::collections::HashSet::new();
                        for (i, step) in arr.iter().enumerate() {
                            let id = step.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            if id.is_empty() {
                                errors.push(format!("步骤[{i}] 缺少 id"));
                            } else if !is_valid_task_id(id) {
                                errors.push(format!("步骤[{i}] id 非法: {id}"));
                            } else if !ids.insert(id.to_string()) {
                                errors.push(format!("步骤 id 重复: {id}"));
                            }
                            let stype = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if !VALID_STEP_TYPES.contains(&stype) {
                                errors.push(format!("步骤[{i}] 未知类型: {stype}"));
                                continue;
                            }
                            // H5：步型字段约束表驱动（STEP_FIELD_RULES）——替代
                            // 逐臂手写校验；wait 的"selector 或 duration>0"双语义
                            // 无法用纯字段表表达，保留特判（口径对齐 Python 执行器
                            // handle_wait，与 AI 提示词 prompt.rs 一致）
                            for (types, required, any_of, any_msg) in Self::STEP_FIELD_RULES {
                                if !types.contains(&stype) {
                                    continue;
                                }
                                for field in required.iter() {
                                    if Self::is_blank_field(step, field) {
                                        errors.push(format!("步骤[{i}] 需要 {field}"));
                                    }
                                }
                                if !any_of.is_empty()
                                    && any_of.iter().all(|f| Self::is_blank_field(step, f))
                                {
                                    errors.push(format!("步骤[{i}] {any_msg}"));
                                }
                            }
                            if stype == "wait" {
                                let has_selector = !step
                                    .get("selector")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .is_empty();
                                // as_f64 兼容合法浮点（如 1.5s）：as_u64 对小数返回
                                // None 会被误判为「未配置 duration」
                                let has_duration =
                                    step.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                        > 0.0;
                                if !has_selector && !has_duration {
                                    errors.push(format!("步骤[{i}] 需要 selector 或 duration"));
                                }
                            }
                        }
                    }
                }
            }
            "script" => {
                let has_content = config
                    .get("content")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let has_path = config
                    .get("script_path")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                if !has_content && !has_path {
                    errors.push("script 任务需提供 content 或 script_path".to_string());
                }
                // 统一拦截 PowerShell：与 `web/routes/scripts.rs:check_supported_binary`
                // + `executor::is_supported_ext` 同口径，避免经 `POST /api/tasks`
                // 创建 script 任务绕过 PUT /api/scripts/{id} 的拦截
                if let Some(bp) = config.get("binary_path").and_then(|v| v.as_str()) {
                    let lower = bp.to_lowercase();
                    if lower.contains("powershell")
                        || lower.contains("pwsh")
                        || lower.ends_with(".ps1")
                    {
                        errors.push(
                            "不支持 PowerShell，仅支持 shell / bat / python / exe 四类脚本"
                                .to_string(),
                        );
                    }
                }
                if let Some(sp) = config.get("script_path").and_then(|v| v.as_str()) {
                    if sp.to_lowercase().ends_with(".ps1") {
                        errors.push(
                            "不支持 .ps1 脚本，仅支持 shell / bat / python / exe 四类".to_string(),
                        );
                    }
                    // 显式含 `..` 的穿越写法提前拦截（与 executor::resolve_script_source 同口径）
                    if std::path::Path::new(sp)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        errors.push(format!("script_path 含非法穿越: {sp}"));
                    }
                }
                if let Some(wd) = config.get("work_dir").and_then(|v| v.as_str()) {
                    if std::path::Path::new(wd)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        errors.push(format!("work_dir 含非法穿越: {wd}"));
                    }
                }
            }
            // Shell 任务已移除：历史存量 type=shell 明确拒绝，提示改用 script
            "shell" => {
                errors.push("任务类型 shell 已移除，请改用 script 类型".to_string());
            }
            "http" => {
                // 直连任务没有可内置的通用门户地址，地址缺失时执行层连请求都拼不出来，
                // 属于"存得下但必然失败"的配置，必须在保存/导入闸口就拒绝
                let url = config.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if url.trim().is_empty() {
                    errors.push("直连任务缺少请求地址".to_string());
                }
                // 认证页地址允许为空（运行时回退方案的 auth_url，老配置因此照旧可用）；
                // 非空时只要求「协议 http/https + 有主机名」，口径对齐
                // `login::http_login::HttpLoginRequest::validate_url`——但在此手写判断而
                // 不调用它，避免 tasks → login 的反向依赖
                let auth_url = config
                    .get("auth_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if !auth_url.is_empty() {
                    let (scheme, rest) = auth_url.split_once("://").unwrap_or(("", ""));
                    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
                        errors.push("直连任务的认证地址仅支持 http/https".to_string());
                    } else if host.is_empty() {
                        errors.push("直连任务的认证地址缺少主机名".to_string());
                    }
                }
                // 与 src/login/http_login.rs 的 MAX_SCRIPT_BYTES 同口径：任务里的
                // crypto_script 与方案里的 http_crypto_script 由同一套脚本引擎执行，
                // 任务侧放行更大体积会造成「保存通过、登录必然失败」的错位
                let script_bytes = config
                    .get("crypto_script")
                    .and_then(|v| v.as_str())
                    .map_or(0, str::len);
                if script_bytes > MAX_HTTP_SCRIPT_BYTES {
                    errors.push(format!(
                        "crypto_script 超过 {MAX_HTTP_SCRIPT_BYTES} 字节上限（当前 {script_bytes}）"
                    ));
                }
                // 请求头/请求体/URL 的上限是防呆（拦住误粘贴的大段内容），不表达安全边界
                for (field, limit) in [
                    ("url", MAX_HTTP_URL_BYTES),
                    ("headers", MAX_HTTP_HEADERS_BYTES),
                    ("body", MAX_HTTP_BODY_BYTES),
                ] {
                    let bytes = config
                        .get(field)
                        .and_then(|v| v.as_str())
                        .map_or(0, str::len);
                    if bytes > limit {
                        errors.push(format!("{field} 超过 {limit} 字节上限（当前 {bytes}）"));
                    }
                }
                // 前置请求（可选）：形状校验委托给 `HttpPreRequest::validate`，
                // 与执行层同一份判据（取值方式写错属于"保存通过、登录必然失败"）。
                // 体积上限另按本文件常量把关，与该分支对 headers/body 的口径一致。
                if let Some(raw) = config.get("pre_request").filter(|v| !v.is_null()) {
                    match serde_json::from_value::<HttpPreRequest>(raw.clone()) {
                        Ok(pre) => {
                            if let Err(e) = pre.validate() {
                                errors.push(e);
                            }
                            for (field, value, limit) in [
                                ("pre_request.url", pre.url.as_str(), MAX_HTTP_URL_BYTES),
                                (
                                    "pre_request.headers",
                                    pre.headers.as_str(),
                                    MAX_HTTP_HEADERS_BYTES,
                                ),
                                ("pre_request.body", pre.body.as_str(), MAX_HTTP_BODY_BYTES),
                            ] {
                                if value.len() > limit {
                                    errors.push(format!(
                                        "{field} 超过 {limit} 字节上限（当前 {}）",
                                        value.len()
                                    ));
                                }
                            }
                        }
                        Err(e) => errors.push(format!("pre_request 字段类型不正确: {e}")),
                    }
                }
            }
            other => errors.push(format!("未知任务类型: {other}")),
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    // ---------------- 私有辅助 ----------------

    /// 按任务类型选存储桶（三类任务目录分派的唯一出口）
    ///
    /// 目录与类型一一对应：新增任务类型时只改本函数与 [`Self::buckets`]，
    /// 保存/查找/删除/列表不会再各自漏改一处。
    fn bucket_dir(&self, kind: &TaskKind) -> &Path {
        match kind {
            TaskKind::Browser(_) => &self.browser_dir,
            TaskKind::Script(_) => &self.scripts_dir,
            TaskKind::Http(_) => &self.http_dir,
        }
    }

    /// 全部任务桶 `(目录, 任务类型名)`，顺序即查找优先级：browser → script → http
    ///
    /// 供目录遍历型操作（列表扫描、查找、删除）统一遍历，避免每处再写一遍三臂 match。
    fn buckets(&self) -> [(&Path, &'static str); 3] {
        [
            (&self.browser_dir, "browser"),
            (&self.scripts_dir, "script"),
            (&self.http_dir, "http"),
        ]
    }

    /// 除任务当前所属桶外，其他桶中该 id 的文件路径（切换类型后需要清理的残留）
    fn other_bucket_paths(&self, kind: &TaskKind, task_id: &str) -> Vec<PathBuf> {
        let current = self.bucket_dir(kind);
        let mut out = Vec::new();
        for (dir, _) in self.buckets() {
            if dir != current {
                out.push(dir.join(format!("{task_id}.json")));
            }
        }
        out
    }

    /// `.order.json` 路径
    fn order_path(&self) -> PathBuf {
        self.tasks_dir.join(".order.json")
    }

    /// 同步读取 `.order.json`（缺失或损坏时返回默认）
    fn read_order(&self) -> OrderData {
        Self::read_order_at(&self.order_path())
    }

    /// 按路径同步读取 `.order.json`（供 spawn_blocking 闭包内使用，无需 &self）
    fn read_order_at(path: &Path) -> OrderData {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str(&strip_bom(s)) {
                Ok(o) => o,
                Err(e) => {
                    // 损坏即静默重置会丢失用户自定义排序与活跃任务，必须留痕
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        ".order.json 损坏，已重置为默认排序与活跃任务"
                    );
                    OrderData::default()
                }
            },
            // 文件不存在属正常路径（首次使用），静默返回默认
            Err(_) => OrderData::default(),
        }
    }

    /// 同步写入 `.order.json`（原子）
    fn write_order(&self, order: &OrderData) -> Result<(), TaskError> {
        atomic_write_json(&self.order_path(), order)
    }

    /// 查找任务文件（按桶优先级 browser → script → http）
    ///
    /// 同名文件理论上只存在于一个桶（`save_task` 会清残留），顺序只作兜底：
    /// 手工把文件放进别的桶时以优先级最高的一份为准。
    fn find_task_file(&self, task_id: &str) -> Option<PathBuf> {
        if !is_valid_task_id(task_id) {
            return None;
        }
        for (dir, _) in self.buckets() {
            let candidate = dir.join(format!("{task_id}.json"));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    /// 首启播种内置默认任务（幂等）。
    ///
    /// 仅当 `browser/default.json` 缺失时写入种子（用户删改过的不碰——`default`
    /// 本就不可删除，能缺失只会是新装/手工清目录）。
    /// 任务的启用状态不在本层：它属于各方案的 `ProfileData::active_task`，
    /// 未绑定的方案由登录解析回退到本内置任务，从而保证新装开箱可用。
    fn ensure_default_task(&self) {
        let seed_path = self.browser_dir.join(format!("{DEFAULT_TASK_ID}.json"));
        if !seed_path.exists() {
            match std::fs::write(&seed_path, DEFAULT_TASK_SEED) {
                Ok(()) => tracing::info!(
                    path = %seed_path.display(),
                    "已内置默认登录任务 default（通用登录），新装开箱即用"
                ),
                Err(e) => {
                    tracing::warn!(
                        path = %seed_path.display(),
                        error = %e,
                        "写入内置默认登录任务失败，登录前需手动创建任务"
                    );
                }
            }
        }
    }

    /// 读取任务 JSON 的 `type` 字段并构造摘要（单次读盘，A 组小尾巴）
    ///
    /// 此前 scripts 扫描对同一文件先 `read_type` 再 `read_summary` 各完整
    /// 读盘解析一次；合并后一次读取同时取 type/name/description。
    /// `ttype` 为 None 时从 JSON `type` 字段推导（缺省 script）。
    fn read_summary_typed(path: &Path, ttype: Option<&str>) -> Option<TaskSummary> {
        // 读取/解析失败的任务从列表静默消失会让用户误以为任务丢失，必须留痕
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "读取任务文件失败，已从任务列表跳过"
                );
                return None;
            }
        };
        let v: Value = match serde_json::from_str(&strip_bom(content)) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "任务文件解析失败，已从任务列表跳过"
                );
                return None;
            }
        };
        let ttype = ttype
            .map(|t| t.to_string())
            .or_else(|| {
                v.get("type")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "script".to_string());
        Some(Self::summary_from_value(&v, path, &ttype))
    }

    /// 扫描单个任务桶，把其中的 JSON 任务摘要追加到 `out`
    ///
    /// 跳过 `.order.json`（排序文件，不在任何桶里，历史上曾混放）与 `.meta.json`
    /// （脚本附属元数据，不是任务本身）。`dir_type` 为 `Some` 时按桶语义固定标注任务
    /// 类型，为 `None` 时从文件 `type` 字段推导（缺省 `script`）——`scripts/` 桶混放
    /// 裸 `.py` 与历史 `.meta.json`，其中 JSON 任务的类型以文件内声明为准，与
    /// [`TaskManager::load_task`] 的读取口径保持一致。
    fn scan_json_bucket(dir: &Path, dir_type: Option<&str>, out: &mut Vec<TaskSummary>) {
        // 目录不存在（未预建 / 被外部删除）时静默跳过：其余桶仍应正常列出
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name == ".order.json" || name.ends_with(".meta.json") {
                continue;
            }
            // 单次读盘同时取 type 与摘要（原 read_type + read_summary 各读一次）
            if let Some(s) = Self::read_summary_typed(&path, dir_type) {
                out.push(s);
            }
        }
    }

    /// 从已解析的 JSON 值提取摘要字段（供两个读取入口复用）
    fn summary_from_value(v: &Value, path: &Path, ttype: &str) -> TaskSummary {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("未命名任务")
            .to_string();
        let description = v
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        TaskSummary {
            id: stem,
            name,
            description,
            task_type: ttype.to_string(),
            url: v
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // 只认合法方法名：手改出来的 "PUT" 之类解析失败即 None，宁可不出标签
            http_method: (ttype == "http")
                .then(|| v.get("method").cloned())
                .flatten()
                .and_then(|m| serde_json::from_value(m).ok()),
        }
    }

    /// 读取裸 `.py` 脚本摘要（`.meta.json` 优先，否则解析 `# name:` / `# description:` 注释）
    fn read_py_summary(path: &Path) -> Option<TaskSummary> {
        let stem = path.file_stem()?.to_string_lossy().to_string();
        let meta_path = path.with_extension("meta.json");
        let (name, description) = if meta_path.exists() {
            match std::fs::read_to_string(&meta_path)
                .ok()
                .map(strip_bom)
                .and_then(|c| serde_json::from_str::<Value>(&c).ok())
            {
                Some(v) => (
                    v.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string(),
                    v.get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                ),
                None => (String::new(), String::new()),
            }
        } else {
            let content = std::fs::read_to_string(path).ok()?;
            let mut name = String::new();
            let mut desc = String::new();
            for line in content.lines().take(10) {
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("# name:") {
                    name = rest.trim().to_string();
                } else if let Some(rest) = t.strip_prefix("# description:") {
                    desc = rest.trim().to_string();
                }
            }
            (name, desc)
        };
        let name = if name.is_empty() { stem.clone() } else { name };
        Some(TaskSummary {
            id: stem,
            name,
            description,
            task_type: "script".to_string(),
            url: String::new(),
            http_method: None,
        })
    }
}

/// 手动校验 task_id（等价于 `^[a-zA-Z0-9_-]{1,64}$`，避免引入 regex 依赖）
fn is_valid_task_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 剥离 UTF-8 BOM 前缀（TSK-1）
///
/// Windows 记事本默认以「带 BOM 的 UTF-8」保存，serde_json::from_str 遇前导
/// U+FEFF 直接报错——load_task 报解析失败、列表摘要 warn 后静默跳过，用户会
/// 误以为任务丢失。所有任务文件读取入口统一先经本函数。
fn strip_bom(content: String) -> String {
    match content.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => content,
    }
}

/// 原子写入 JSON（委托给 utils::io::atomic_write_json）
fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), TaskError> {
    crate::utils::atomic_write_json(path, value).map_err(TaskError::IoError)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ is_valid_task_id 校验 ============

    #[test]
    fn test_is_valid_task_id_alphanumeric() {
        // 纯字母数字 ID 有效
        assert!(is_valid_task_id("abc123"));
    }

    #[test]
    fn test_is_valid_task_id_with_underscore_and_hyphen() {
        // 含下划线和连字符的 ID 有效
        assert!(is_valid_task_id("my-task_01"));
    }

    #[test]
    fn test_is_valid_task_id_empty_rejected() {
        // 空 ID 无效
        assert!(!is_valid_task_id(""));
    }

    #[test]
    fn test_is_valid_task_id_too_long_rejected() {
        // 超过 64 字符的 ID 无效
        let long_id = "a".repeat(65);
        assert!(!is_valid_task_id(&long_id));
    }

    #[test]
    fn test_is_valid_task_id_max_length_accepted() {
        // 恰好 64 字符的 ID 有效
        let id = "a".repeat(64);
        assert!(is_valid_task_id(&id));
    }

    #[test]
    fn test_is_valid_task_id_special_chars_rejected() {
        // 含特殊字符的 ID 无效
        assert!(!is_valid_task_id("task/id"));
        assert!(!is_valid_task_id("task id"));
        assert!(!is_valid_task_id("task@id"));
        assert!(!is_valid_task_id("task.id"));
    }

    #[test]
    fn test_is_valid_task_id_chinese_rejected() {
        // 中文字符的 ID 无效
        assert!(!is_valid_task_id("任务ID"));
    }

    // ============ TaskKind 访问器（经 TaskManager 写回路径间接覆盖） ============

    #[test]
    fn test_common_mut_task_id_browser() {
        let mut task = TaskKind::Browser(TaskConfig::default());
        task.common_mut().task_id = "new_id".to_string();
        assert_eq!(task.common().task_id, "new_id");
    }

    #[test]
    fn test_common_mut_task_id_script() {
        let mut task = TaskKind::Script(ScriptTaskConfig::default());
        task.common_mut().task_id = "script_id".to_string();
        assert_eq!(task.common().task_id, "script_id");
    }

    #[test]
    fn test_task_name_extraction() {
        let mut cfg = TaskConfig::default();
        cfg.common.name = "测试任务".to_string();
        let task = TaskKind::Browser(cfg);
        assert_eq!(task.common().name, "测试任务");
    }

    #[test]
    fn test_task_type_name() {
        assert_eq!(
            TaskKind::Browser(TaskConfig::default()).type_name(),
            "browser"
        );
        assert_eq!(
            TaskKind::Script(ScriptTaskConfig::default()).type_name(),
            "script"
        );
    }

    // ============ TaskManager CRUD（需要临时目录 + ConfigService） ============

    async fn make_task_manager() -> (tempfile::TempDir, Arc<TaskManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = TaskManager::new(tmp.path());
        (tmp, mgr)
    }

    #[tokio::test]
    async fn test_validate_goto_accepts_selector_url() {
        let (_tmp, mgr) = make_task_manager().await;
        let task = serde_json::json!({
            "type": "browser",
            "name": "导航任务",
            "steps": [{
                "id": "go",
                "type": "goto",
                "selector": "https://example.com/login"
            }]
        });
        assert!(mgr.validate_task(&task).is_ok());
    }

    #[tokio::test]
    async fn test_validate_upload_file_requires_file_path() {
        let (_tmp, mgr) = make_task_manager().await;
        let invalid = serde_json::json!({
            "type": "browser",
            "name": "上传任务",
            "steps": [{
                "id": "upload",
                "type": "upload_file",
                "selector": "input[type=file]"
            }]
        });
        let errors = mgr.validate_task(&invalid).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("需要 path 或 value")));

        let valid = serde_json::json!({
            "type": "browser",
            "name": "上传任务",
            "steps": [{
                "id": "upload",
                "type": "upload_file",
                "selector": "input[type=file]",
                "path": "C:/tmp/avatar.png"
            }]
        });
        assert!(mgr.validate_task(&valid).is_ok());
    }

    #[tokio::test]
    async fn test_validate_eval_rejects_empty_script() {
        let (_tmp, mgr) = make_task_manager().await;
        let task = serde_json::json!({
            "type": "browser",
            "name": "脚本任务",
            "steps": [{
                "id": "check",
                "type": "eval",
                "script": "   "
            }]
        });
        let errors = mgr.validate_task(&task).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("需要非空 script 或 code")));
    }

    #[tokio::test]
    async fn test_validate_wait_accepts_duration_only() {
        // wait 双语义：无 selector 时按 duration 休眠（对齐 prompt.rs 与
        // Python handle_wait）——此前强制 selector 会让 AI 生成的休眠步骤两轮自纠全败
        let (_tmp, mgr) = make_task_manager().await;
        let task = serde_json::json!({
            "type": "browser",
            "name": "等待任务",
            "steps": [{
                "id": "w",
                "type": "wait",
                "duration": 2000
            }]
        });
        assert!(mgr.validate_task(&task).is_ok());
    }

    #[tokio::test]
    async fn test_validate_wait_requires_selector_or_duration() {
        let (_tmp, mgr) = make_task_manager().await;
        let task = serde_json::json!({
            "type": "browser",
            "name": "等待任务",
            "steps": [{
                "id": "w",
                "type": "wait"
            }]
        });
        let errors = mgr.validate_task(&task).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("需要 selector 或 duration"))
        );
    }

    #[tokio::test]
    async fn test_validate_select_requires_value() {
        // select 空 value 在执行层是静默 no-op（所有步骤绿但运营商没选），
        // 校验层必须显式拒绝
        let (_tmp, mgr) = make_task_manager().await;
        let invalid = serde_json::json!({
            "type": "browser",
            "name": "运营商任务",
            "steps": [{
                "id": "sel",
                "type": "select",
                "selector": "#isp"
            }]
        });
        let errors = mgr.validate_task(&invalid).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("需要 value")));

        let valid = serde_json::json!({
            "type": "browser",
            "name": "运营商任务",
            "steps": [{
                "id": "sel",
                "type": "select",
                "selector": "#isp",
                "value": "电信"
            }]
        });
        assert!(mgr.validate_task(&valid).is_ok());
    }

    #[tokio::test]
    async fn test_validate_click_select_requires_value() {
        let (_tmp, mgr) = make_task_manager().await;
        let invalid = serde_json::json!({
            "type": "browser",
            "name": "自定义运营商任务",
            "steps": [{
                "id": "sel",
                "type": "click_select",
                "selector": "#isp"
            }]
        });
        let errors = mgr.validate_task(&invalid).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("需要 value")));

        let valid = serde_json::json!({
            "type": "browser",
            "name": "自定义运营商任务",
            "steps": [{
                "id": "sel",
                "type": "click_select",
                "selector": "#isp",
                "value": "电信"
            }]
        });
        assert!(mgr.validate_task(&valid).is_ok());
    }

    #[tokio::test]
    async fn test_validate_browser_timeout_bounds() {
        // timeout 钳制：0 → 执行时 1ms 永远秒超时；超大值占住全局互斥的会话槽位
        let (_tmp, mgr) = make_task_manager().await;
        let zero = serde_json::json!({
            "type": "browser",
            "name": "超时任务",
            "timeout": 0,
            "steps": [{"id": "w", "type": "wait", "duration": 100}]
        });
        let errors = mgr.validate_task(&zero).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("timeout 需在")));

        let huge = serde_json::json!({
            "type": "browser",
            "name": "超时任务",
            "timeout": 86400000,
            "steps": [{"id": "w", "type": "wait", "duration": 100}]
        });
        let errors = mgr.validate_task(&huge).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("timeout 需在")));

        let ok = serde_json::json!({
            "type": "browser",
            "name": "超时任务",
            "timeout": 30000,
            "steps": [{"id": "w", "type": "wait", "duration": 100}]
        });
        assert!(mgr.validate_task(&ok).is_ok());
    }

    #[tokio::test]
    async fn test_validate_rejects_shell_type() {
        // Shell 类型已移除：校验明确拒绝并提示改用 script
        let (_tmp, mgr) = make_task_manager().await;
        let task = serde_json::json!({
            "type": "shell",
            "name": "旧 Shell",
            "command": "echo hi"
        });
        let errors = mgr.validate_task(&task).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("shell 已移除")));
    }

    #[tokio::test]
    async fn test_validate_accepts_evaluate_custom_alias() {
        // Python 执行器注册的 evaluate/custom 别名必须在 Rust 校验侧放行，
        // 否则录制/AI 产物保存时被误拒
        let (_tmp, mgr) = make_task_manager().await;
        let task = serde_json::json!({
            "type": "browser",
            "name": "别名任务",
            "steps": [
                {"id": "a", "type": "evaluate", "script": "return 1;"},
                {"id": "b", "type": "custom", "code": "return 2;"}
            ]
        });
        assert!(mgr.validate_task(&task).is_ok());
    }

    #[tokio::test]
    async fn test_save_and_load_browser_task() {
        // 浏览器任务的保存与加载往返
        let (_tmp, mgr) = make_task_manager().await;
        // 浏览器任务至少需要一个步骤才能通过校验
        let step_json = serde_json::json!({
            "id": "step1",
            "type": "input",
            "selector": "#user",
            "value": "test"
        });
        let step: StepConfig = serde_json::from_value(step_json).unwrap();
        let task = TaskKind::Browser(TaskConfig {
            common: CommonFields {
                name: "测试浏览器".to_string(),
                ..Default::default()
            },
            url: "http://example.com".to_string(),
            steps: vec![step],
            ..Default::default()
        });
        mgr.save_task("test_browser", &task).await.unwrap();
        let loaded = mgr.load_task("test_browser").await.unwrap();
        if let TaskKind::Browser(cfg) = loaded {
            assert_eq!(cfg.url, "http://example.com");
            assert_eq!(cfg.common.name, "测试浏览器");
            assert_eq!(cfg.steps.len(), 1);
        } else {
            panic!("应为 Browser 类型");
        }
    }

    #[tokio::test]
    async fn test_save_and_load_script_task() {
        // 脚本任务的保存与加载往返
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "脚本测试".to_string(),
                ..Default::default()
            },
            content: Some("print('hello')".to_string()),
            ..Default::default()
        });
        mgr.save_task("script1", &task).await.unwrap();
        let loaded = mgr.load_task("script1").await.unwrap();
        if let TaskKind::Script(cfg) = loaded {
            assert_eq!(cfg.content, Some("print('hello')".to_string()));
        } else {
            panic!("应为 Script 类型");
        }
    }

    // ============ http 直连任务 CRUD ============

    /// 构造一个可通过校验的 http 直连任务（url 非空是唯一的必填约束）
    fn http_task(name: &str) -> TaskKind {
        TaskKind::Http(HttpTaskConfig {
            common: CommonFields {
                name: name.to_string(),
                ..Default::default()
            },
            method: HttpRequestMethod::Post,
            url: "http://portal.example.com/login?user={username}".to_string(),
            auth_url: "http://portal.example.com/portal".to_string(),
            headers: "Content-Type: application/x-www-form-urlencoded".to_string(),
            body: "username={username}&password={password}".to_string(),
            ..Default::default()
        })
    }

    /// 构造一个可通过校验的脚本任务（脚本任务只需 content / script_path 之一）
    fn script_task(name: &str) -> TaskKind {
        TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: name.to_string(),
                ..Default::default()
            },
            content: Some("print('hello')".to_string()),
            ..Default::default()
        })
    }

    /// 构造一个可通过校验的浏览器任务（浏览器任务至少需要一个步骤）
    fn browser_task(name: &str) -> TaskKind {
        let step: StepConfig = serde_json::from_value(serde_json::json!({
            "id": "step1",
            "type": "input",
            "selector": "#user",
            "value": "test"
        }))
        .unwrap();
        TaskKind::Browser(TaskConfig {
            common: CommonFields {
                name: name.to_string(),
                ..Default::default()
            },
            url: "http://example.com".to_string(),
            steps: vec![step],
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_save_and_load_http_task_in_own_bucket() {
        // http 任务落在 tasks/http/，列表与详情标注 task_type=http，删除后彻底消失
        let (tmp, mgr) = make_task_manager().await;
        let path = tmp
            .path()
            .join("tasks")
            .join("http")
            .join("portal_http.json");

        mgr.save_task("portal_http", &http_task("门户直连"))
            .await
            .unwrap();
        assert!(path.exists(), "http 任务应落在 tasks/http/ 桶");
        assert!(
            !mgr.browser_dir.join("portal_http.json").exists(),
            "不得同时写入 browser/ 桶"
        );

        let summaries = mgr.list_all_tasks().await;
        let summary = summaries
            .iter()
            .find(|s| s.id == "portal_http")
            .expect("http 任务应出现在任务列表");
        assert_eq!(summary.task_type, "http");
        assert_eq!(summary.name, "门户直连");

        let detail = mgr.get_task_detail("portal_http").await.unwrap();
        assert_eq!(detail.summary.task_type, "http");
        assert!(mgr.has_task("portal_http"));

        let loaded = mgr.load_task("portal_http").await.unwrap();
        let TaskKind::Http(cfg) = loaded else {
            panic!("应为 Http 类型");
        };
        assert_eq!(cfg.method, HttpRequestMethod::Post);
        assert_eq!(cfg.url, "http://portal.example.com/login?user={username}");
        assert_eq!(cfg.auth_url, "http://portal.example.com/portal");
        assert_eq!(cfg.body, "username={username}&password={password}");
        assert_eq!(
            cfg.common.task_id, "portal_http",
            "保存时 task_id 应写回 common"
        );

        mgr.delete_task("portal_http").await.unwrap();
        assert!(!path.exists(), "删除后任务文件应消失");
        assert!(!mgr.has_task("portal_http"));
        assert!(
            !mgr.list_all_tasks()
                .await
                .iter()
                .any(|s| s.id == "portal_http"),
            "删除后不应再出现在任务列表"
        );
    }

    #[tokio::test]
    async fn test_save_task_clears_other_bucket_stale() {
        // 同一 id 换类型：只保留新桶文件，其他桶残留必须清掉（否则同一 id 有两份定义，
        // 加载会按桶优先级取到过时的那份）
        let (tmp, mgr) = make_task_manager().await;
        let script_path = tmp
            .path()
            .join("tasks")
            .join("scripts")
            .join("same_id.json");
        let http_path = tmp.path().join("tasks").join("http").join("same_id.json");

        // script → http
        mgr.save_task("same_id", &script_task("同 ID 脚本"))
            .await
            .unwrap();
        assert!(script_path.exists());
        mgr.save_task("same_id", &http_task("同 ID 直连"))
            .await
            .unwrap();
        assert!(http_path.exists());
        assert!(!script_path.exists(), "换成 http 后 scripts/ 不得残留");
        assert!(matches!(
            mgr.load_task("same_id").await.unwrap(),
            TaskKind::Http(_)
        ));
        assert_eq!(
            mgr.list_all_tasks()
                .await
                .iter()
                .filter(|s| s.id == "same_id")
                .count(),
            1,
            "同一 id 在列表中只能出现一次"
        );

        // http → script（反向同样清残留）
        mgr.save_task("same_id", &script_task("同 ID 脚本"))
            .await
            .unwrap();
        assert!(!http_path.exists(), "换回 script 后 http/ 不得残留");
        assert!(script_path.exists());
        let summaries = mgr.list_all_tasks().await;
        let summary = summaries.iter().find(|s| s.id == "same_id").unwrap();
        assert_eq!(summary.task_type, "script", "类型应跟随最后一次保存");

        // browser → http
        let browser_path = tmp
            .path()
            .join("tasks")
            .join("browser")
            .join("browser_then_http.json");
        mgr.save_task("browser_then_http", &browser_task("同 ID 浏览器"))
            .await
            .unwrap();
        assert!(browser_path.exists());
        mgr.save_task("browser_then_http", &http_task("同 ID 直连"))
            .await
            .unwrap();
        assert!(!browser_path.exists(), "换成 http 后 browser/ 不得残留");
        assert!(matches!(
            mgr.load_task("browser_then_http").await.unwrap(),
            TaskKind::Http(_)
        ));
    }

    #[tokio::test]
    async fn test_list_summary_carries_url_and_http_method() {
        // 列表行要显示「方法 + 请求地址摘要」，摘要必须自带这两个字段——否则前端只能
        // 对每条任务再发一次详情请求（N+1）
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Http(HttpTaskConfig {
            common: CommonFields {
                task_id: "portal".into(),
                name: "门户直连".into(),
                description: String::new(),
            },
            url: "http://10.0.0.1/login".into(),
            method: HttpRequestMethod::Post,
            ..HttpTaskConfig::default()
        });
        mgr.save_task("portal", &task).await.unwrap();

        let list = mgr.list_all_tasks().await;
        let row = list
            .iter()
            .find(|s| s.id == "portal")
            .expect("列表应含直连任务");
        assert_eq!(row.url, "http://10.0.0.1/login");
        assert_eq!(row.http_method, Some(HttpRequestMethod::Post));

        // 浏览器任务的摘要也带地址（登录页），但方法为 None：前端据此不渲染方法标签
        let browser = list
            .iter()
            .find(|s| s.id == DEFAULT_TASK_ID)
            .expect("内置默认浏览器任务应在列表里");
        assert!(!browser.url.is_empty(), "浏览器任务摘要应带登录页地址");
        assert!(browser.http_method.is_none());

        // 详情路径与列表路径给出同一个答案
        let detail = mgr.get_task_detail("portal").await.unwrap();
        assert_eq!(detail.summary.url, row.url);
        assert_eq!(detail.summary.http_method, row.http_method);
    }

    #[tokio::test]
    async fn test_validate_reserves_default_id_for_browser() {
        // 三类任务共用一个 task_id 命名空间：非浏览器任务占用内置默认任务 ID 会让
        // 保存时的残留清理删掉浏览器兜底任务（或两份定义互相遮蔽），一律拒绝
        let (_tmp, mgr) = make_task_manager().await;
        let http_default = serde_json::json!({
            "type": "http",
            "task_id": "default",
            "name": "直连任务",
            "url": "http://portal.example.com/login"
        });
        let errors = mgr.validate_task(&http_default).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("保留给内置浏览器任务")),
            "非浏览器任务不得占用 default: {errors:?}"
        );

        let script_default = serde_json::json!({
            "type": "script",
            "task_id": "default",
            "name": "脚本任务",
            "content": "echo hi"
        });
        assert!(mgr.validate_task(&script_default).is_err());

        // 浏览器任务自己用 default 是正常路径（种子任务就是它）：这里只断言不被
        // 「保留 ID」那条规则拦下——最小 JSON 可能触发别的字段规则，与本用例无关
        let browser_default = serde_json::json!({
            "type": "browser",
            "task_id": "default",
            "name": "通用登录",
            "steps": [{ "type": "sleep", "duration": 0.1 }]
        });
        let browser_errors = mgr
            .validate_task(&browser_default)
            .err()
            .unwrap_or_default();
        assert!(
            !browser_errors
                .iter()
                .any(|e| e.contains("保留给内置浏览器任务")),
            "浏览器任务不得被保留 ID 规则拦下: {browser_errors:?}"
        );

        // 保存路径同样被拦
        let err = mgr
            .save_task(
                DEFAULT_TASK_ID,
                &TaskKind::Http(HttpTaskConfig {
                    url: "http://portal.example.com/login".into(),
                    ..HttpTaskConfig::default()
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TaskError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn test_validate_http_requires_url() {
        // 直连任务没有可内置的通用门户地址：url 缺失/纯空白一律拒绝
        let (_tmp, mgr) = make_task_manager().await;
        let missing = serde_json::json!({ "type": "http", "name": "直连任务" });
        let errors = mgr.validate_task(&missing).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("直连任务缺少请求地址")),
            "缺失 url 应报缺地址: {errors:?}"
        );

        let blank = serde_json::json!({ "type": "http", "name": "直连任务", "url": "   " });
        let errors = mgr.validate_task(&blank).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("直连任务缺少请求地址")));

        let ok = serde_json::json!({
            "type": "http",
            "name": "直连任务",
            "url": "http://portal.example.com/login"
        });
        assert!(mgr.validate_task(&ok).is_ok());

        // 保存路径同样被拦（validate_task 是唯一闸口）
        let err = mgr
            .save_task("bad_http", &TaskKind::Http(HttpTaskConfig::default()))
            .await
            .unwrap_err();
        assert!(matches!(err, TaskError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn test_validate_http_auth_url_rules() {
        // 认证页地址是可选项：空 = 运行时回退方案的 auth_url（老配置照旧可用）；
        // 非空时只要求 http/https + 有主机名（口径对齐 HttpLoginRequest::validate_url）
        let (_tmp, mgr) = make_task_manager().await;
        let url = "http://portal.example.com/login";
        let build = |auth_url: serde_json::Value| {
            serde_json::json!({
                "type": "http",
                "name": "直连任务",
                "url": url,
                "auth_url": auth_url,
            })
        };

        // 缺失 / 空串 / 纯空白：允许（回退方案字段）
        assert!(mgr.validate_task(&build(serde_json::Value::Null)).is_ok());
        assert!(mgr.validate_task(&build("".into())).is_ok());
        assert!(mgr.validate_task(&build("   ".into())).is_ok());
        // 合法 http/https（含大小写与带路径/查询串）
        assert!(
            mgr.validate_task(&build("http://portal.example.com/portal".into()))
                .is_ok()
        );
        assert!(
            mgr.validate_task(&build("HTTPS://portal.example.com".into()))
                .is_ok()
        );

        // 非 http/https 协议
        let errors = mgr
            .validate_task(&build("ftp://portal.example.com/login".into()))
            .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("直连任务的认证地址仅支持 http/https")),
            "{errors:?}"
        );
        // 无协议（裸主机名）同样不属于 http/https
        let errors = mgr
            .validate_task(&build("portal.example.com/login".into()))
            .unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("仅支持 http/https")),
            "{errors:?}"
        );

        // 缺主机名（`http://` / `http:///path`）
        for missing_host in ["http://", "https:///login", "http:///?x=1"] {
            let errors = mgr.validate_task(&build(missing_host.into())).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|e| e.contains("直连任务的认证地址缺少主机名")),
                "{missing_host} 应报缺主机名: {errors:?}"
            );
        }

        // 空 auth_url 的直连任务可正常落盘（回退方案的认证地址）
        let none_auth = TaskKind::Http(HttpTaskConfig {
            common: CommonFields {
                name: "无认证页地址".to_string(),
                ..Default::default()
            },
            url: url.to_string(),
            ..Default::default()
        });
        assert!(mgr.save_task("no_auth_url", &none_auth).await.is_ok());
    }

    #[tokio::test]
    async fn test_validate_http_rejects_oversized_payload() {
        // 体积上限：crypto_script ≤ 128 KiB（与 login/http_login.rs 同口径），
        // headers / body ≤ 256 KiB（防呆）
        let (_tmp, mgr) = make_task_manager().await;
        let url = "http://portal.example.com/login";

        let oversized = serde_json::json!({
            "type": "http",
            "name": "超长脚本",
            "url": url,
            "crypto_script": "a".repeat(MAX_HTTP_SCRIPT_BYTES + 1),
        });
        let errors = mgr.validate_task(&oversized).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("crypto_script 超过")),
            "超限脚本应被拒: {errors:?}"
        );

        // 恰好等于上限放行（边界不误伤）
        let boundary = serde_json::json!({
            "type": "http",
            "name": "边界脚本",
            "url": url,
            "crypto_script": "a".repeat(MAX_HTTP_SCRIPT_BYTES),
        });
        assert!(mgr.validate_task(&boundary).is_ok());

        let oversized_headers = serde_json::json!({
            "type": "http",
            "name": "超长请求头",
            "url": url,
            "headers": format!("X-Big: {}", "a".repeat(MAX_HTTP_HEADERS_BYTES)),
        });
        let errors = mgr.validate_task(&oversized_headers).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("headers 超过")),
            "{errors:?}"
        );

        let oversized_body = serde_json::json!({
            "type": "http",
            "name": "超长请求体",
            "url": url,
            "body": "a".repeat(MAX_HTTP_BODY_BYTES + 1),
        });
        let errors = mgr.validate_task(&oversized_body).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("body 超过")), "{errors:?}");
    }

    #[tokio::test]
    async fn test_http_task_rejects_invalid_id() {
        // 三类任务共用同一套 task_id 校验（读写入口都要走）
        let (_tmp, mgr) = make_task_manager().await;
        let result = mgr.save_task("invalid/id", &http_task("直连")).await;
        assert!(matches!(result, Err(TaskError::InvalidTaskId(_))));
        let result = mgr.load_task("..\\http\\evil").await;
        assert!(matches!(result, Err(TaskError::InvalidTaskId(_))));
    }

    /// TSK-1：带 UTF-8 BOM 的任务文件（Windows 记事本默认保存格式）必须
    /// 正常加载并出现在列表摘要，不得解析失败或被静默跳过
    #[tokio::test]
    async fn test_load_task_with_utf8_bom() {
        let (tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "BOM 任务".to_string(),
                ..Default::default()
            },
            content: Some("print('hi')".to_string()),
            ..Default::default()
        });
        mgr.save_task("bom_task", &task).await.unwrap();

        // 模拟记事本保存：重写为带 BOM 的 UTF-8
        let path = tmp
            .path()
            .join("tasks")
            .join("scripts")
            .join("bom_task.json");
        assert!(path.exists(), "脚本任务文件应存在");
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("\u{feff}{content}")).unwrap();

        // 完整加载不受 BOM 影响
        let loaded = mgr.load_task("bom_task").await.unwrap();
        match loaded {
            TaskKind::Script(cfg) => assert_eq!(cfg.common.name, "BOM 任务"),
            other => panic!("应为 Script 类型，实际 {other:?}"),
        }

        // 列表摘要不被静默跳过
        let summaries = mgr.list_all_tasks().await;
        assert!(
            summaries.iter().any(|s| s.id == "bom_task"),
            "带 BOM 的任务必须出现在任务列表"
        );
    }

    #[tokio::test]
    async fn test_load_nonexistent_task_returns_error() {
        // 加载不存在的任务应返回 TaskNotFound
        let (_tmp, mgr) = make_task_manager().await;
        let result = mgr.load_task("nonexistent").await;
        assert!(matches!(result, Err(TaskError::TaskNotFound(_))));
    }

    #[tokio::test]
    async fn test_save_task_invalid_id_rejected() {
        // 无效 ID 应被拒绝
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Browser(TaskConfig::default());
        let result = mgr.save_task("invalid/id", &task).await;
        assert!(matches!(result, Err(TaskError::InvalidTaskId(_))));
    }

    #[tokio::test]
    async fn test_read_paths_reject_invalid_task_id() {
        // 读取入口也必须执行与写入入口相同的 ID 校验，防止 Windows 反斜杠穿越。
        let (_tmp, mgr) = make_task_manager().await;
        let result = mgr.load_task("..\\config\\settings").await;
        assert!(matches!(result, Err(TaskError::InvalidTaskId(_))));
        assert!(!mgr.has_task("../config/settings"));
    }

    #[tokio::test]
    async fn test_save_order_rejects_invalid_ids() {
        let (_tmp, mgr) = make_task_manager().await;
        let order = OrderData {
            order: vec!["safe".into(), "..\\outside".into()],
        };
        let result = mgr.save_order(&order).await;
        assert!(matches!(result, Err(TaskError::InvalidTaskId(_))));
    }

    #[tokio::test]
    async fn test_delete_task_removes_file() {
        // 删除任务后文件应不存在
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "待删除".to_string(),
                ..Default::default()
            },
            content: Some("print('bye')".to_string()),
            ..Default::default()
        });
        mgr.save_task("to_delete", &task).await.unwrap();
        assert!(mgr.has_task("to_delete"));

        mgr.delete_task("to_delete").await.unwrap();
        assert!(!mgr.has_task("to_delete"));
    }

    #[tokio::test]
    async fn test_delete_default_task_rejected() {
        // 不允许删除 default 任务
        let (_tmp, mgr) = make_task_manager().await;
        let result = mgr.delete_task("default").await;
        assert!(matches!(result, Err(TaskError::DeleteDefaultTask)));
    }

    #[tokio::test]
    async fn test_list_tasks_sorted_by_order() {
        // 任务列表应按 .order.json 排序
        let (_tmp, mgr) = make_task_manager().await;
        let script1 = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "任务B".to_string(),
                ..Default::default()
            },
            content: Some("print('b')".to_string()),
            ..Default::default()
        });
        let script2 = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "任务A".to_string(),
                ..Default::default()
            },
            content: Some("print('a')".to_string()),
            ..Default::default()
        });
        mgr.save_task("task_b", &script1).await.unwrap();
        mgr.save_task("task_a", &script2).await.unwrap();

        let tasks = mgr.list_all_tasks().await;
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        // task_b 先保存，所以在 order 中排前面
        assert_eq!(ids.first(), Some(&"task_b"));
    }

    #[tokio::test]
    async fn test_order_data_serde_roundtrip() {
        // OrderData 序列化/反序列化往返
        let order = OrderData {
            order: vec!["t1".to_string(), "t2".to_string()],
        };
        let json = serde_json::to_string(&order).unwrap();
        let back: OrderData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.order.len(), 2);
    }

    /// 旧版 `.order.json` 带 `active` 字段（v8 及以前）仍可读取排序：
    /// 该字段已随「任务启用改为按方案绑定」移除，serde 须忽略而非报错，
    /// 否则升级用户的排序会因反序列化失败被重置为默认。
    #[tokio::test]
    async fn test_read_order_tolerates_legacy_active_field() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join(".order.json"),
            r#"{"order":["t2","t1"],"active":"t1"}"#,
        )
        .unwrap();
        let mgr = TaskManager::new(tmp.path());
        let order = mgr.load_order().await;
        assert_eq!(order.order, vec!["t2", "t1"]);
    }

    #[tokio::test]
    async fn test_has_task() {
        // has_task 对存在和不存在的 ID 返回正确结果
        let (_tmp, mgr) = make_task_manager().await;
        assert!(!mgr.has_task("no_such_task"));

        let task = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "存在".to_string(),
                ..Default::default()
            },
            content: Some("print('exists')".to_string()),
            ..Default::default()
        });
        mgr.save_task("exists", &task).await.unwrap();
        assert!(mgr.has_task("exists"));
    }

    #[tokio::test]
    async fn test_list_tasks_dedupes_py_and_json_same_id() {
        // 同名 .json 与 .py 只保留 .json 条目（G7），裸 .py 单独存在时仍列出
        let (_tmp, mgr) = make_task_manager().await;
        std::fs::write(
            mgr.scripts_dir.join("foo.json"),
            r#"{"type":"script","name":"foo 脚本"}"#,
        )
        .unwrap();
        std::fs::write(
            mgr.scripts_dir.join("foo.py"),
            "#!/usr/bin/env python\nprint(1)\n",
        )
        .unwrap();
        std::fs::write(
            mgr.scripts_dir.join("bar.py"),
            "#!/usr/bin/env python\nprint(2)\n",
        )
        .unwrap();

        let tasks = mgr.list_all_tasks().await;
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        // foo 只出现一次（.json 优先），bar 为裸 .py 正常列出
        assert_eq!(ids.iter().filter(|id| **id == "foo").count(), 1);
        assert!(ids.contains(&"bar"));
        // foo 的名称来自 .json 定义
        let foo = tasks.iter().find(|t| t.id == "foo").unwrap();
        assert_eq!(foo.name, "foo 脚本");
        assert_eq!(foo.task_type, "script");
    }
    // ============ 首启播种内置默认任务 ============

    #[tokio::test]
    async fn test_first_run_seeds_default_task() {
        // 新目录首启：写入内置 default 任务文件（启用状态属于各方案，见 ProfileData::active_task）
        let (_tmp, mgr) = make_task_manager().await;
        let seed_path = mgr.browser_dir.join("default.json");
        assert!(seed_path.exists(), "首启应写入内置默认任务");
        let kind = mgr.load_task("default").await.expect("种子应为合法任务");
        assert!(matches!(kind, TaskKind::Browser(_)));
    }

    #[tokio::test]
    async fn test_seed_never_overwrites_existing_default() {
        // 已有 default.json（用户改过）的不覆盖
        let tmp = tempfile::tempdir().unwrap();
        let browser = tmp.path().join("tasks").join("browser");
        std::fs::create_dir_all(&browser).unwrap();
        std::fs::write(
            browser.join("default.json"),
            r#"{"type":"browser","name":"我的定制"}"#,
        )
        .unwrap();
        let _mgr = TaskManager::new(tmp.path());
        let kept = std::fs::read_to_string(browser.join("default.json")).unwrap();
        assert!(kept.contains("我的定制"), "已有默认任务不得被种子覆盖");
    }

    #[tokio::test]
    async fn test_seed_does_not_touch_order() {
        // 播种不参与"启用哪个任务"（已按方案绑定），也不得改动既有排序
        let tmp = tempfile::tempdir().unwrap();
        let browser = tmp.path().join("tasks").join("browser");
        std::fs::create_dir_all(&browser).unwrap();
        let task = TaskKind::Script(ScriptTaskConfig {
            common: CommonFields {
                name: "mine".to_string(),
                ..Default::default()
            },
            content: Some("print('mine')".to_string()),
            ..Default::default()
        });
        // 先手写 mine 任务与指向它的 order，再构造管理器
        std::fs::write(
            browser.join("mine.json"),
            serde_json::to_string(&task).unwrap(),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("tasks").join(".order.json"),
            r#"{"order":["mine"]}"#,
        )
        .unwrap();
        let mgr = TaskManager::new(tmp.path());
        assert_eq!(mgr.load_order().await.order, vec!["mine"]);
        assert!(
            mgr.browser_dir.join("default.json").exists(),
            "缺失的种子仍应补上"
        );
    }
}
