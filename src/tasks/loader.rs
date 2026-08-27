//! 任务文件 CRUD 管理：TaskManager
//!
//! 任务以 JSON 文件形式存储于 `<tasks_dir>/browser/` 与 `<tasks_dir>/scripts/` 子目录，
//! 任务排序与活跃任务记录于 `<tasks_dir>/.order.json`。所有写操作通过 `tokio::sync::Mutex`
//! 串行化，避免并发写冲突。`task_id` 校验采用手动 ASCII 检查（避免引入 `regex` 依赖）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::ConfigService;
use crate::tasks::TaskError;
use crate::tasks::models::*;

/// 任务排序与活跃任务记录（`.order.json`）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrderData {
    /// 排序后的任务 ID 列表
    pub order: Vec<String>,
    /// 当前活跃任务 ID
    pub active: String,
}

/// 任务摘要（列表/概览用，不含完整配置）
#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    /// 任务 ID（= 文件名 stem）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 任务描述
    pub description: String,
    /// 任务类型：`browser` / `script` / `shell`
    pub task_type: String,
}

/// 任务详情（摘要 + 完整配置）
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    /// 任务摘要
    pub summary: TaskSummary,
    /// 完整任务配置
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
    /// 配置服务（用于读取运行时默认参数等）
    #[allow(dead_code)]
    config: Arc<ConfigService>,
    /// 文件写操作互斥锁
    lock: Mutex<()>,
}

impl TaskManager {
    /// 构造管理器，确保子目录存在，并迁移旧版 `active.txt`、初始化 `.order.json`
    pub fn new(base_path: &Path, config: Arc<ConfigService>) -> Arc<Self> {
        let tasks_dir = base_path.join("tasks");
        let browser_dir = tasks_dir.join("browser");
        let scripts_dir = tasks_dir.join("scripts");
        let _ = std::fs::create_dir_all(&browser_dir);
        let _ = std::fs::create_dir_all(&scripts_dir);

        let mgr = Self {
            tasks_dir,
            browser_dir,
            scripts_dir,
            config,
            lock: Mutex::new(()),
        };

        // 迁移旧版 active.txt → .order.json
        mgr.migrate_active_file();
        // 不存在则创建默认 .order.json
        if !mgr.order_path().exists() {
            let _ = mgr.write_order(&OrderData::default());
        }
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
        let order_path = self.order_path();
        let (mut summaries, order) = tokio::task::spawn_blocking(move || {
            let mut out: Vec<TaskSummary> = Vec::new();

            // 浏览器任务（browser/*.json）
            if let Ok(entries) = std::fs::read_dir(&browser_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if path
                        .file_name()
                        .map(|n| n == ".order.json")
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    if let Some(s) = Self::read_summary(&path, "browser") {
                        out.push(s);
                    }
                }
            }

            // 脚本/Shell 任务（scripts/*.json，排除 .meta.json）
            if let Ok(entries) = std::fs::read_dir(&scripts_dir) {
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
                    if let Some(s) = Self::read_summary_typed(&path, None) {
                        out.push(s);
                    }
                }
            }

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
        let task: TaskKind = serde_json::from_str(&content).map_err(TaskError::JsonError)?;
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
                    tracing::warn!("任务 {task_id} 序列化失败，未嵌入 task_config: {e}");
                    false
                }
            },
            Ok(_) => {
                tracing::warn!("任务 {task_id} 不是浏览器任务，未嵌入 task_config");
                false
            }
            Err(e) => {
                tracing::warn!("加载任务 {task_id} 失败，未嵌入 task_config: {e}");
                false
            }
        }
    }

    /// 保存任务（存在即更新）。根据 `TaskKind` 选择子目录，并维护 `.order.json`
    pub async fn save_task(&self, task_id: &str, task: &TaskKind) -> Result<(), TaskError> {
        if !is_valid_task_id(task_id) {
            return Err(TaskError::InvalidTaskId(task_id.to_string()));
        }
        let _guard = self.lock.lock().await;

        // 校验 JSON 字段
        let value = serde_json::to_value(task).map_err(TaskError::JsonError)?;
        self.validate_task(&value)
            .map_err(TaskError::ValidationFailed)?;

        let subdir = match task {
            TaskKind::Browser(_) => &self.browser_dir,
            TaskKind::Script(_) | TaskKind::Shell(_) => &self.scripts_dir,
        };
        let path = subdir.join(format!("{task_id}.json"));

        let mut task = task.clone();
        // 将 task_id 写回 common（避免 JSON 中遗漏）
        task.common_mut().task_id = task_id.to_string();

        atomic_write_json(&path, &task)?;

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
        if task_id == "default" {
            return Err(TaskError::DeleteDefaultTask);
        }
        let _guard = self.lock.lock().await;

        let mut found = false;
        let b = self.browser_dir.join(format!("{task_id}.json"));
        if b.exists() {
            tokio::fs::remove_file(&b)
                .await
                .map_err(TaskError::IoError)?;
            found = true;
        }
        let s = self.scripts_dir.join(format!("{task_id}.json"));
        if s.exists() {
            tokio::fs::remove_file(&s)
                .await
                .map_err(TaskError::IoError)?;
            found = true;
        }
        // 清理关联 .meta.json / .py
        let meta = self.scripts_dir.join(format!("{task_id}.meta.json"));
        if meta.exists() {
            let _ = tokio::fs::remove_file(&meta).await;
        }
        let py = self.scripts_dir.join(format!("{task_id}.py"));
        if py.exists() {
            let _ = tokio::fs::remove_file(&py).await;
        }

        if !found {
            return Err(TaskError::TaskNotFound(task_id.to_string()));
        }

        let mut order = self.read_order();
        order.order.retain(|id| id != task_id);
        if order.active == task_id {
            order.active = "default".to_string();
        }
        self.write_order(&order)?;
        Ok(())
    }

    /// 返回活跃任务 ID
    pub async fn get_active_task(&self) -> String {
        self.read_order().active
    }

    /// 设置活跃任务
    pub async fn set_active_task(&self, task_id: &str) -> Result<(), TaskError> {
        if !is_valid_task_id(task_id) {
            return Err(TaskError::InvalidTaskId(task_id.to_string()));
        }
        let _guard = self.lock.lock().await;
        if self.find_task_file(task_id).is_none() {
            return Err(TaskError::TaskNotFound(task_id.to_string()));
        }
        let mut order = self.read_order();
        order.active = task_id.to_string();
        self.write_order(&order)
    }

    /// 加载活跃任务完整配置
    pub async fn load_active_task(&self) -> Result<TaskKind, TaskError> {
        let id = self.get_active_task().await;
        self.load_task(&id).await
    }

    /// 加载任务详情（摘要 + 完整配置）
    pub async fn get_task_detail(&self, task_id: &str) -> Result<TaskDetail, TaskError> {
        let task = self.load_task(task_id).await?;
        let summary = TaskSummary {
            id: task_id.to_string(),
            name: task.common().name.clone(),
            description: task.common().description.clone(),
            task_type: task.type_name().to_string(),
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
        if !order.active.is_empty() && !is_valid_task_id(&order.active) {
            return Err(TaskError::InvalidTaskId(order.active.clone()));
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

        match kind {
            "browser" => {
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
                            match stype {
                                "input" | "click" | "select" | "click_select" | "wait" | "ocr" => {
                                    if step
                                        .get("selector")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty()
                                    {
                                        errors.push(format!("步骤[{i}] 需要 selector"));
                                    }
                                }
                                "wait_url" => {
                                    if step
                                        .get("pattern")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty()
                                    {
                                        errors.push(format!("步骤[{i}] 需要 pattern"));
                                    }
                                }
                                "eval" | "custom_js" => {
                                    let has = step
                                        .get("script")
                                        .and_then(|v| v.as_str())
                                        .map(|s| !s.trim().is_empty())
                                        .unwrap_or(false)
                                        || step
                                            .get("code")
                                            .and_then(|v| v.as_str())
                                            .map(|s| !s.trim().is_empty())
                                            .unwrap_or(false);
                                    if !has {
                                        errors.push(format!("步骤[{i}] 需要非空 script 或 code"));
                                    }
                                }
                                "goto" | "navigate" => {
                                    let has_url = step
                                        .get("url")
                                        .and_then(|v| v.as_str())
                                        .map(|s| !s.trim().is_empty())
                                        .unwrap_or(false)
                                        || step
                                            .get("value")
                                            .and_then(|v| v.as_str())
                                            .map(|s| !s.trim().is_empty())
                                            .unwrap_or(false)
                                        || step
                                            .get("selector")
                                            .and_then(|v| v.as_str())
                                            .map(|s| !s.trim().is_empty())
                                            .unwrap_or(false);
                                    if !has_url {
                                        errors.push(format!(
                                            "步骤[{i}] 需要 url、value 或 selector"
                                        ));
                                    }
                                }
                                "assert_text" => {
                                    if step
                                        .get("value")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty()
                                    {
                                        errors.push(format!("步骤[{i}] 需要 value"));
                                    }
                                }
                                "upload_file" => {
                                    if step
                                        .get("selector")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty()
                                    {
                                        errors.push(format!("步骤[{i}] 需要 selector"));
                                    }
                                    let has_path = step
                                        .get("path")
                                        .and_then(|v| v.as_str())
                                        .map(|s| !s.trim().is_empty())
                                        .unwrap_or(false)
                                        || step
                                            .get("value")
                                            .and_then(|v| v.as_str())
                                            .map(|s| !s.trim().is_empty())
                                            .unwrap_or(false);
                                    if !has_path {
                                        errors.push(format!("步骤[{i}] 需要 path 或 value"));
                                    }
                                }
                                "wait_for_selector"
                                    if step
                                        .get("selector")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty() =>
                                {
                                    errors.push(format!("步骤[{i}] 需要 selector"));
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            "script" => {
                let has_content = config
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                let has_path = config
                    .get("script_path")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                if !has_content && !has_path {
                    errors.push("script 任务需提供 content 或 script_path".to_string());
                }
            }
            "shell" => {
                let cmd = config.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if cmd.trim().is_empty() {
                    errors.push("shell 任务 command 不能为空".to_string());
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
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => OrderData::default(),
        }
    }

    /// 同步写入 `.order.json`（原子）
    fn write_order(&self, order: &OrderData) -> Result<(), TaskError> {
        atomic_write_json(&self.order_path(), order)
    }

    /// 查找任务文件（browser/ 或 scripts/ 优先）
    fn find_task_file(&self, task_id: &str) -> Option<PathBuf> {
        if !is_valid_task_id(task_id) {
            return None;
        }
        let b = self.browser_dir.join(format!("{task_id}.json"));
        if b.exists() {
            return Some(b);
        }
        let s = self.scripts_dir.join(format!("{task_id}.json"));
        if s.exists() {
            return Some(s);
        }
        None
    }

    /// 迁移旧版 `active.txt`（`browser:default` 格式）到 `.order.json`
    fn migrate_active_file(&self) {
        let active_txt = self.tasks_dir.join("active.txt");
        if active_txt.exists() {
            if let Ok(content) = std::fs::read_to_string(&active_txt) {
                let id = content
                    .trim()
                    .split(':')
                    .nth(1)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "default".to_string());
                let mut order = self.read_order();
                order.active = id;
                let _ = self.write_order(&order);
            }
            let _ = std::fs::remove_file(&active_txt);
        }
    }

    /// 读取任务 JSON 的 `type` 字段并构造摘要（单次读盘，A 组小尾巴）
    ///
    /// 此前 scripts 扫描对同一文件先 `read_type` 再 `read_summary` 各完整
    /// 读盘解析一次；合并后一次读取同时取 type/name/description。
    /// `ttype` 为 None 时从 JSON `type` 字段推导（缺省 script）。
    fn read_summary_typed(path: &Path, ttype: Option<&str>) -> Option<TaskSummary> {
        let content = std::fs::read_to_string(path).ok()?;
        let v: Value = serde_json::from_str(&content).ok()?;
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

    /// 读取任务摘要（从 JSON 的 name/description 字段）
    fn read_summary(path: &Path, ttype: &str) -> Option<TaskSummary> {
        let content = std::fs::read_to_string(path).ok()?;
        let v: Value = serde_json::from_str(&content).ok()?;
        Some(Self::summary_from_value(&v, path, ttype))
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
        }
    }

    /// 读取裸 `.py` 脚本摘要（`.meta.json` 优先，否则解析 `# name:` / `# description:` 注释）
    fn read_py_summary(path: &Path) -> Option<TaskSummary> {
        let stem = path.file_stem()?.to_string_lossy().to_string();
        let meta_path = path.with_extension("meta.json");
        let (name, description) = if meta_path.exists() {
            match std::fs::read_to_string(&meta_path)
                .ok()
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
    fn test_common_mut_task_id_shell() {
        let mut task = TaskKind::Shell(ShellTaskConfig::default());
        task.common_mut().task_id = "shell_id".to_string();
        assert_eq!(task.common().task_id, "shell_id");
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
        assert_eq!(
            TaskKind::Shell(ShellTaskConfig::default()).type_name(),
            "shell"
        );
    }

    // ============ TaskManager CRUD（需要临时目录 + ConfigService） ============

    async fn make_task_manager() -> (tempfile::TempDir, Arc<TaskManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let config = ConfigService::new(tmp.path().to_path_buf(), tx)
            .await
            .unwrap();
        let mgr = TaskManager::new(tmp.path(), config);
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
    async fn test_save_and_load_shell_task() {
        // Shell 任务的保存与加载往返
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Shell(ShellTaskConfig {
            common: CommonFields {
                name: "Shell 测试".to_string(),
                ..Default::default()
            },
            command: "echo hello".to_string(),
            ..Default::default()
        });
        mgr.save_task("shell1", &task).await.unwrap();
        let loaded = mgr.load_task("shell1").await.unwrap();
        if let TaskKind::Shell(cfg) = loaded {
            assert_eq!(cfg.command, "echo hello");
        } else {
            panic!("应为 Shell 类型");
        }
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
            active: "safe".into(),
        };
        let result = mgr.save_order(&order).await;
        assert!(matches!(result, Err(TaskError::InvalidTaskId(_))));
    }

    #[tokio::test]
    async fn test_delete_task_removes_file() {
        // 删除任务后文件应不存在
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Shell(ShellTaskConfig {
            common: CommonFields {
                name: "待删除".to_string(),
                ..Default::default()
            },
            command: "echo bye".to_string(),
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
        let shell1 = TaskKind::Shell(ShellTaskConfig {
            common: CommonFields {
                name: "任务B".to_string(),
                ..Default::default()
            },
            command: "echo b".to_string(),
            ..Default::default()
        });
        let shell2 = TaskKind::Shell(ShellTaskConfig {
            common: CommonFields {
                name: "任务A".to_string(),
                ..Default::default()
            },
            command: "echo a".to_string(),
            ..Default::default()
        });
        mgr.save_task("task_b", &shell1).await.unwrap();
        mgr.save_task("task_a", &shell2).await.unwrap();

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
            active: "t1".to_string(),
        };
        let json = serde_json::to_string(&order).unwrap();
        let back: OrderData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.order.len(), 2);
        assert_eq!(back.active, "t1");
    }

    #[tokio::test]
    async fn test_set_active_task() {
        // 设置活跃任务后可正确查询
        let (_tmp, mgr) = make_task_manager().await;
        let task = TaskKind::Shell(ShellTaskConfig {
            common: CommonFields {
                name: "活跃任务".to_string(),
                ..Default::default()
            },
            command: "echo active".to_string(),
            ..Default::default()
        });
        mgr.save_task("my_active", &task).await.unwrap();
        mgr.set_active_task("my_active").await.unwrap();

        let active = mgr.get_active_task().await;
        assert_eq!(active, "my_active");
    }

    #[tokio::test]
    async fn test_has_task() {
        // has_task 对存在和不存在的 ID 返回正确结果
        let (_tmp, mgr) = make_task_manager().await;
        assert!(!mgr.has_task("no_such_task"));

        let task = TaskKind::Shell(ShellTaskConfig {
            common: CommonFields {
                name: "存在".to_string(),
                ..Default::default()
            },
            command: "echo exists".to_string(),
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
}
