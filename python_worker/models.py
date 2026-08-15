"""数据模型定义。

本模块定义 Python Worker 与 Rust 侧 Bridge 之间传递的数据结构。
字段名（snake_case）与 Rust 侧 ``tasks/models.rs`` + ``config/schema.rs``
反序列化的 JSON 格式严格对齐，确保 NDJSON IPC 协议两端一致。

使用标准库 ``dataclasses`` 实现，无第三方依赖。
"""

from __future__ import annotations

from dataclasses import dataclass, field, asdict
from enum import Enum
from typing import Any


class Outcome(str, Enum):
    """浏览器动作的执行结果分类。

    与 Rust 侧 ``bridge::ipc::Outcome`` 枚举严格对齐。
    """

    SUCCESS = "success"
    CANCELLED = "cancelled"
    NAVIGATION_TIMEOUT = "navigation_timeout"
    SELECTOR_FAILED = "selector_failed"
    ASSERTION_FAILED = "assertion_failed"
    # 由 Rust 侧设置（登录决策/验证码预处理），Worker 不产生（P10）
    CAPTCHA_FAILED = "captcha_failed"
    # 由 Rust 侧设置（凭证预校验失败），Worker 不产生（P10）
    INVALID_CREDENTIAL = "invalid_credential"
    NETWORK_ERROR = "network_error"
    UNKNOWN_ERROR = "unknown_error"


@dataclass
class StructuredResult:
    """单次浏览器动作的结构化结果。

    对应 Rust 侧 ``StructuredResult``。执行类命令（execute_login_attempt /
    execute_browser_task / debug_step）的响应 ``data`` 字段即为此结构。
    """

    outcome: str = Outcome.UNKNOWN_ERROR.value
    """结果分类，见 ``Outcome``。"""

    message: str = ""
    """人类可读消息。"""

    duration_ms: int = 0
    """执行耗时（毫秒）。"""

    screenshots: list[str] = field(default_factory=list)
    """本次动作产生的截图路径/URL 列表。"""

    screenshot_url: str | None = None
    """单张截图的快捷访问地址（调试会话初始截图使用）。"""

    def to_dict(self) -> dict[str, Any]:
        """序列化为 JSON 字典（替代 Pydantic model_dump）。"""
        return asdict(self)


# ── 步骤配置 ──

# 已知字段集合，用于分离 extra fields
_STEP_KNOWN_FIELDS = frozenset({
    "id", "step_type", "type", "description", "selector", "value", "code",
    "script", "pattern", "timeout", "required", "store_as", "path",
    "clear", "duration", "option_selector",
    "old", "target_selector", "frame", "char_range",
})


@dataclass
class StepConfig:
    """步骤配置（扁平结构，由 ``type`` 字段区分步骤类型）。

    字段与 Rust 侧 ``StepConfig`` 对齐。未知字段保留在 ``extras`` 中，
    由各处理器按需读取（如 ``full_page``、``wait_until``）。
    """

    id: str = ""
    """步骤标识符（必填）。"""

    step_type: str = ""
    """步骤类型（反序列化自 JSON 的 ``type`` 字段）。"""

    description: str = ""
    """步骤描述。"""

    selector: str | None = None
    """目标元素选择器（CSS / XPath / text）。"""

    value: str | None = None
    """目标值（填写内容 / 导航 URL / 正则模式）。"""

    code: str | None = None
    """脚本内容（JS / Python 代码）。"""

    script: str | None = None
    """脚本路径或内容（``code`` 的别名）。"""

    pattern: str | None = None
    """正则表达式模式。"""

    timeout: int | None = None
    """步骤超时（毫秒）。"""

    required: bool = True
    """步骤是否必须成功。"""

    store_as: str | None = None
    """结果存储键名。"""

    path: str | None = None
    """文件路径（上传等场景）。"""

    clear: bool = False
    """input 步骤是否先清空再填写。"""

    duration: int = 0
    """wait 步骤等待时长（毫秒）。"""

    option_selector: str | None = None
    """click_select 步骤中目标选项的选择器。"""

    old: bool = False
    """ocr 步骤是否使用旧版识别模型。"""

    target_selector: str | None = None
    """ocr 步骤识别结果填入的目标输入框选择器。"""

    frame: str | None = None
    """iframe 选择器（URL、name 或 CSS 选择器）。"""

    char_range: str | int | None = None
    """OCR 识别字符范围（0-7 或自定义字符串）。"""

    extras: dict[str, Any] = field(default_factory=dict, repr=False)
    """未在已知字段中定义的扩展参数（替代 Pydantic extra='allow'）。"""

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> StepConfig:
        """从 JSON 字典构造，处理 ``type`` → ``step_type`` 别名和 extra fields。"""
        known: dict[str, Any] = {}
        extras: dict[str, Any] = {}
        for k, v in d.items():
            if k == "type":
                known["step_type"] = v
            elif k in _STEP_KNOWN_FIELDS:
                known[k] = v
            else:
                extras[k] = v
        step = cls(**known, extras=extras)
        # 规范化：code → script 合并
        if step.script is None and step.code is not None:
            step.script = step.code
        return step

    def to_dict(self) -> dict[str, Any]:
        """序列化为 JSON 字典。"""
        d = asdict(self)
        d.pop("extras", None)
        d["type"] = d.pop("step_type", "")
        return {**d, **self.extras}

    @property
    def extra_fields(self) -> dict[str, Any]:
        """返回未在显式字段中定义的扩展参数。"""
        return self.extras

    @property
    def effective_script(self) -> str | None:
        """返回有效脚本内容：优先 script，其次 code。"""
        return self.script if self.script is not None else self.code


@dataclass
class TaskConfig:
    """浏览器任务配置。

    对应 Rust 侧 ``TaskConfig``。``steps`` 为步骤列表，由 Worker 解释执行。
    同时包含原 ``CommonFields``（task_id / name / description）。
    """

    task_id: str = ""
    """任务唯一标识（通常由文件名推导，JSON 中可省略）。"""

    name: str = "未命名任务"
    """显示名称。"""

    description: str = ""
    """任务描述。"""

    url: str = ""
    """登录页 URL。"""

    on_success: Any = None
    """登录成功后回调。"""

    on_failure: Any = None
    """登录失败后回调。"""

    reveal_hidden: bool = False
    """是否揭示隐藏元素。"""

    step_delay: float = 0
    """步骤间默认延迟（秒）。"""

    variables: dict[str, str] | None = field(default_factory=dict)
    """自定义模板变量。"""

    success_condition: str = ""
    """成功判定变量名（eval 步骤 store_as 写入，非空时以该变量真值判定登录成功）。"""

    steps: list[StepConfig] = field(default_factory=list)
    """步骤列表。"""

    metadata: Any = None
    """用户自定义元数据（Worker 不执行）。"""

    extras: dict[str, Any] = field(default_factory=dict, repr=False)
    """未知扩展字段。"""

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> TaskConfig:
        """从 JSON 字典构造，递归解析 steps。"""
        known_keys = {
            "task_id", "name", "description", "url",
            "on_success", "on_failure", "reveal_hidden",
            "step_delay", "variables", "success_condition",
            "steps", "metadata",
        }
        known: dict[str, Any] = {}
        extras: dict[str, Any] = {}
        for k, v in d.items():
            if k in known_keys:
                known[k] = v
            else:
                extras[k] = v
        # 递归解析 steps
        raw_steps = known.pop("steps", None)
        task = cls(**known, extras=extras)
        if raw_steps and isinstance(raw_steps, list):
            task.steps = [
                StepConfig.from_dict(s) if isinstance(s, dict) else s
                for s in raw_steps
            ]
        return task

    def to_dict(self) -> dict[str, Any]:
        """序列化为 JSON 字典。"""
        d = asdict(self)
        d.pop("extras", None)
        # steps 内部也用 to_dict 以保留 extras
        d["steps"] = [s.to_dict() if isinstance(s, StepConfig) else s for s in self.steps]
        return {**d, **self.extras}
