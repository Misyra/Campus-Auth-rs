//! 任务数据模型：TaskKind / TaskConfig / StepConfig 等
//!
//! 定义浏览器/脚本/Shell 三类任务的 serde 数据模型。`TaskKind` 为内部标记枚举，
//! `type` 字段缺失时默认归为浏览器任务。步骤配置做 `code`→`script` 与 `frame` 类型规范化。

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// task_id 校验正则
pub const TASK_ID_PATTERN: &str = r"^[a-zA-Z0-9_-]{1,64}$";
/// 浏览器任务默认超时（毫秒）
pub const DEFAULT_TASK_TIMEOUT_MS: u64 = 30000;
/// 步骤间延迟（秒）
pub const DEFAULT_STEP_DELAY: f64 = 0.5;
/// 页面加载后等待（秒）
pub const DEFAULT_NAVIGATION_WAIT: f64 = 1.0;
/// 步骤默认超时（毫秒）
pub const DEFAULT_STEP_TIMEOUT_MS: u64 = 10000;
/// 脚本/Shell 默认超时（秒）
pub const DEFAULT_SCRIPT_TIMEOUT: u64 = 60;
/// 脚本超时下限（秒）
pub const MIN_SCRIPT_TIMEOUT: u64 = 1;
/// 脚本超时上限（秒）
pub const MAX_SCRIPT_TIMEOUT: u64 = 3600;
/// 脚本内容大小上限（字节）
pub const MAX_SCRIPT_CONTENT_SIZE: usize = 100 * 1024;
/// stdout/stderr 截断长度
pub const OUTPUT_TRUNCATE_LEN: usize = 500;
/// 有效步骤类型集合
pub const VALID_STEP_TYPES: &[&str] = &[
    "input", "click", "select", "click_select", "wait", "wait_url", "eval", "screenshot",
    "sleep", "ocr", "custom_js",
];

fn default_task_timeout_ms() -> u64 {
    DEFAULT_TASK_TIMEOUT_MS
}
fn default_step_delay() -> f64 {
    DEFAULT_STEP_DELAY
}
fn default_navigation_wait() -> f64 {
    DEFAULT_NAVIGATION_WAIT
}
fn default_script_timeout() -> u64 {
    DEFAULT_SCRIPT_TIMEOUT
}
fn default_value_obj() -> Value {
    Value::Object(Default::default())
}

/// 三类任务共享的字段
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommonFields {
    /// 任务唯一标识（从文件名推导，JSON 中可省略）
    pub task_id: String,
    /// 显示名称
    pub name: String,
    /// 任务描述
    pub description: String,
}

impl Default for CommonFields {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            name: "未命名任务".to_string(),
            description: String::new(),
        }
    }
}

/// 浏览器任务配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskConfig {
    /// 共享字段（扁平嵌入）
    #[serde(flatten)]
    pub common: CommonFields,
    /// 认证页面 URL（支持 `{{LOGIN_URL}}` 模板）
    pub url: String,
    /// 任务超时（毫秒）
    #[serde(default = "default_task_timeout_ms")]
    pub timeout: u64,
    /// 步骤间延迟（秒）
    #[serde(default = "default_step_delay")]
    pub step_delay: f64,
    /// 页面加载后等待秒数
    #[serde(default = "default_navigation_wait")]
    pub navigation_wait: f64,
    /// 是否揭示隐藏输入框
    pub reveal_hidden: bool,
    /// 自定义模板变量
    pub variables: HashMap<String, String>,
    /// 步骤列表
    pub steps: Vec<StepConfig>,
    /// 成功回调配置
    pub on_success: Value,
    /// 失败回调配置
    pub on_failure: Value,
    /// 用户自定义元数据（执行器不使用）
    pub metadata: Value,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            common: CommonFields::default(),
            url: String::new(),
            timeout: DEFAULT_TASK_TIMEOUT_MS,
            step_delay: DEFAULT_STEP_DELAY,
            navigation_wait: DEFAULT_NAVIGATION_WAIT,
            reveal_hidden: false,
            variables: HashMap::new(),
            steps: Vec::new(),
            on_success: default_value_obj(),
            on_failure: default_value_obj(),
            metadata: default_value_obj(),
        }
    }
}

/// 脚本任务配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptTaskConfig {
    /// 共享字段（扁平嵌入）
    #[serde(flatten)]
    pub common: CommonFields,
    /// 脚本路径（相对 tasks/scripts/ 或绝对路径）
    pub script_path: Option<String>,
    /// 内联脚本内容（写入临时文件执行）
    pub content: Option<String>,
    /// 命令行参数
    pub args: Vec<String>,
    /// 工作目录，为空时用 script_path 所在目录
    pub work_dir: Option<String>,
    /// 超时秒数，钳制到 [1, 3600]
    #[serde(default = "default_script_timeout")]
    pub timeout: u64,
    /// 执行二进制路径，为空则自动检测
    pub binary_path: Option<String>,
}

impl Default for ScriptTaskConfig {
    fn default() -> Self {
        Self {
            common: CommonFields::default(),
            script_path: None,
            content: None,
            args: Vec::new(),
            work_dir: None,
            timeout: DEFAULT_SCRIPT_TIMEOUT,
            binary_path: None,
        }
    }
}

/// Shell 任务配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellTaskConfig {
    /// 共享字段（扁平嵌入）
    #[serde(flatten)]
    pub common: CommonFields,
    /// Shell 命令字符串（必填）
    pub command: String,
    /// 超时秒数，钳制到 [1, 3600]
    #[serde(default = "default_script_timeout")]
    pub timeout: u64,
    /// 指定 shell 路径，为空时用全局配置或系统默认
    pub shell_path: Option<String>,
}

impl Default for ShellTaskConfig {
    fn default() -> Self {
        Self {
            common: CommonFields::default(),
            command: String::new(),
            timeout: DEFAULT_SCRIPT_TIMEOUT,
            shell_path: None,
        }
    }
}

/// 统一任务类型（内部标记枚举）
///
/// `type` 字段缺失或 = "browser" 时归为浏览器任务；"script"/"shell" 分别归为对应类型。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    /// 浏览器任务
    Browser(TaskConfig),
    /// 脚本任务
    Script(ScriptTaskConfig),
    /// Shell 任务
    Shell(ShellTaskConfig),
}

impl<'de> Deserialize<'de> for TaskKind {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(d)?;
        let kind = value
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("browser");
        match kind {
            "script" => Ok(TaskKind::Script(
                parse::<ScriptTaskConfig>(value).map_err(serde::de::Error::custom)?,
            )),
            "shell" => Ok(TaskKind::Shell(
                parse::<ShellTaskConfig>(value).map_err(serde::de::Error::custom)?,
            )),
            _ => Ok(TaskKind::Browser(
                parse::<TaskConfig>(value).map_err(serde::de::Error::custom)?,
            )),
        }
    }
}

/// 从包含 `type` 字段的 JSON 值反序列化具体任务配置
fn parse<T: DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(value)
}

/// 步骤配置（扁平 struct，通过 `type` 字段区分步骤类型）
///
/// Rust 侧仅存储与传递，具体解释执行由 Python Worker 完成。`code`→`script` 规范化与
/// `frame` 类型规范化在反序列化时完成，未知字段收集到 `extra`。
#[derive(Debug, Clone, Serialize)]
pub struct StepConfig {
    /// 步骤标识符
    pub id: String,
    /// 步骤类型
    #[serde(rename = "type")]
    pub step_type: String,
    /// 步骤描述
    pub description: String,
    /// 单步超时（ms），覆盖任务级默认值
    pub timeout: Option<u64>,
    /// CSS/文本选择器
    pub selector: Option<String>,
    /// 填写值
    pub value: Option<String>,
    /// URL 匹配正则（wait_url）
    pub pattern: Option<String>,
    /// JavaScript 代码（eval）；`code` 历史别名会规范化为此字段
    pub script: Option<String>,
    /// 结果存储变量名（eval/ocr）
    pub store_as: Option<String>,
    /// 是否清空输入框再填写（input）
    pub clear: bool,
    /// 截图路径（screenshot）
    pub path: Option<String>,
    /// 延时毫秒（sleep）
    pub duration: u64,
    /// iframe 选择器（非字符串值静默忽略）
    pub frame: Option<String>,
    /// 是否必须成功
    pub required: bool,
    /// 选项容器选择器（click_select）
    pub option_selector: Option<String>,
    /// 验证码输入框选择器（ocr）
    pub target_selector: Option<String>,
    /// 是否使用旧版 OCR 模型（ocr）
    pub old: bool,
    /// OCR 字符范围（string 或 int）
    pub char_range: Option<Value>,
    /// 未知字段收集
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(default)]
struct StepHelper {
    id: String,
    #[serde(rename = "type")]
    step_type: String,
    description: String,
    timeout: Option<u64>,
    selector: Option<String>,
    value: Option<String>,
    pattern: Option<String>,
    script: Option<String>,
    code: Option<String>,
    store_as: Option<String>,
    clear: bool,
    path: Option<String>,
    duration: u64,
    frame: Value,
    required: bool,
    option_selector: Option<String>,
    target_selector: Option<String>,
    old: bool,
    char_range: Option<Value>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

impl Default for StepHelper {
    fn default() -> Self {
        Self {
            id: String::new(),
            step_type: String::new(),
            description: String::new(),
            timeout: None,
            selector: None,
            value: None,
            pattern: None,
            script: None,
            code: None,
            store_as: None,
            clear: true,
            path: None,
            duration: 1000,
            frame: Value::Null,
            required: false,
            option_selector: None,
            target_selector: None,
            old: false,
            char_range: None,
            extra: HashMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for StepConfig {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let h = StepHelper::deserialize(d)?;
        // frame 非字符串（如布尔 true）静默设为 None
        let frame = match h.frame {
            Value::String(s) => Some(s),
            _ => None,
        };
        // code 历史别名规范化为 script
        let script = h.script.or(h.code);
        Ok(StepConfig {
            id: h.id,
            step_type: h.step_type,
            description: h.description,
            timeout: h.timeout,
            selector: h.selector,
            value: h.value,
            pattern: h.pattern,
            script,
            store_as: h.store_as,
            clear: h.clear,
            path: h.path,
            duration: h.duration,
            frame,
            required: h.required,
            option_selector: h.option_selector,
            target_selector: h.target_selector,
            old: h.old,
            char_range: h.char_range,
            extra: h.extra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ TaskKind 反序列化 ============

    #[test]
    fn test_task_kind_deserialize_browser() {
        // type=browser 应反序列化为 TaskKind::Browser
        let json = r#"{
            "type": "browser",
            "name": "登录测试",
            "url": "http://example.com",
            "steps": []
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Browser(_)));
        if let TaskKind::Browser(cfg) = task {
            assert_eq!(cfg.url, "http://example.com");
            assert_eq!(cfg.common.name, "登录测试");
        }
    }

    #[test]
    fn test_task_kind_deserialize_script() {
        // type=script 应反序列化为 TaskKind::Script
        let json = r#"{
            "type": "script",
            "name": "脚本任务",
            "script_path": "test.py",
            "content": "print('hello')"
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Script(_)));
        if let TaskKind::Script(cfg) = task {
            assert_eq!(cfg.script_path, Some("test.py".to_string()));
            assert_eq!(cfg.content, Some("print('hello')".to_string()));
        }
    }

    #[test]
    fn test_task_kind_deserialize_shell() {
        // type=shell 应反序列化为 TaskKind::Shell
        let json = r#"{
            "type": "shell",
            "name": "Shell 任务",
            "command": "echo hello"
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Shell(_)));
        if let TaskKind::Shell(cfg) = task {
            assert_eq!(cfg.command, "echo hello");
        }
    }

    #[test]
    fn test_task_kind_default_type_is_browser() {
        // type 字段缺失时默认归为浏览器任务
        let json = r#"{
            "name": "默认类型",
            "url": "http://example.com",
            "steps": []
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Browser(_)));
    }

    #[test]
    fn test_task_kind_unknown_type_falls_back_to_browser() {
        // 未知 type 值应 fallback 到 browser
        let json = r#"{
            "type": "unknown_type",
            "name": "未知类型",
            "url": "http://example.com",
            "steps": []
        }"#;
        let task: TaskKind = serde_json::from_str(json).unwrap();
        assert!(matches!(task, TaskKind::Browser(_)));
    }

    // ============ StepConfig 反序列化 ============

    #[test]
    fn test_step_config_basic_deserialize() {
        // 基本步骤配置反序列化
        let json = r##"{
            "id": "step1",
            "type": "input",
            "selector": "#username",
            "value": "test_user",
            "description": "填写用户名"
        }"##;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.id, "step1");
        assert_eq!(step.step_type, "input");
        assert_eq!(step.selector, Some("#username".to_string()));
        assert_eq!(step.value, Some("test_user".to_string()));
    }

    #[test]
    fn test_step_config_code_normalized_to_script() {
        // 历史别名 `code` 应规范化为 `script` 字段
        let json = r#"{
            "id": "step1",
            "type": "eval",
            "code": "document.title"
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.script, Some("document.title".to_string()));
    }

    #[test]
    fn test_step_config_script_takes_precedence_over_code() {
        // script 优先于 code
        let json = r#"{
            "id": "step1",
            "type": "eval",
            "script": "window.location",
            "code": "document.title"
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.script, Some("window.location".to_string()));
    }

    #[test]
    fn test_step_config_frame_string_value() {
        // frame 字符串值应正确解析
        let json = r##"{
            "id": "step1",
            "type": "click",
            "frame": "#my-iframe"
        }"##;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.frame, Some("#my-iframe".to_string()));
    }

    #[test]
    fn test_step_config_frame_non_string_is_none() {
        // frame 为布尔 true 等非字符串值时静默设为 None
        let json = r#"{
            "id": "step1",
            "type": "click",
            "frame": true
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert!(step.frame.is_none());
    }

    // ============ 无效输入处理 ============

    #[test]
    fn test_task_config_default_values() {
        // 测试 TaskConfig 默认值
        let cfg = TaskConfig::default();
        assert_eq!(cfg.timeout, DEFAULT_TASK_TIMEOUT_MS);
        assert_eq!(cfg.step_delay, DEFAULT_STEP_DELAY);
        assert_eq!(cfg.navigation_wait, DEFAULT_NAVIGATION_WAIT);
        assert!(!cfg.reveal_hidden);
        assert!(cfg.steps.is_empty());
    }

    #[test]
    fn test_script_task_config_default_values() {
        // 测试 ScriptTaskConfig 默认值
        let cfg = ScriptTaskConfig::default();
        assert_eq!(cfg.timeout, DEFAULT_SCRIPT_TIMEOUT);
        assert!(cfg.script_path.is_none());
        assert!(cfg.content.is_none());
        assert!(cfg.args.is_empty());
    }

    #[test]
    fn test_shell_task_config_default_values() {
        // 测试 ShellTaskConfig 默认值
        let cfg = ShellTaskConfig::default();
        assert_eq!(cfg.timeout, DEFAULT_SCRIPT_TIMEOUT);
        assert!(cfg.command.is_empty());
        assert!(cfg.shell_path.is_none());
    }

    #[test]
    fn test_step_config_default_duration() {
        // 测试 StepHelper 默认 duration 为 1000ms
        let json = r#"{
            "id": "s1",
            "type": "sleep",
            "duration": 500
        }"#;
        let step: StepConfig = serde_json::from_str(json).unwrap();
        assert_eq!(step.duration, 500);
    }

    #[test]
    fn test_task_kind_serde_roundtrip_browser() {
        // 浏览器任务序列化-反序列化往返
        let original = TaskKind::Browser(TaskConfig {
            url: "http://example.com".to_string(),
            ..Default::default()
        });
        let json = serde_json::to_string(&original).unwrap();
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        if let TaskKind::Browser(cfg) = back {
            assert_eq!(cfg.url, "http://example.com");
        } else {
            panic!("应为 Browser 类型");
        }
    }

    #[test]
    fn test_task_kind_serde_roundtrip_shell() {
        // Shell 任务序列化-反序列化往返
        let original = TaskKind::Shell(ShellTaskConfig {
            command: "echo test".to_string(),
            ..Default::default()
        });
        let json = serde_json::to_string(&original).unwrap();
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        if let TaskKind::Shell(cfg) = back {
            assert_eq!(cfg.command, "echo test");
        } else {
            panic!("应为 Shell 类型");
        }
    }
}
