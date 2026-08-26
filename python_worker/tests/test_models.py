"""SDK 解析测试：models.py 的 StepConfig / TaskConfig 反序列化与序列化。

验证 Rust 侧 JSON（type 字段、extras 透传、code→script 合并）到 Python
数据模型，再序列化回 JSON 的往返一致性，确保 NDJSON IPC 两端契约对齐。
"""

from __future__ import annotations

from models import Outcome, StepConfig, StructuredResult, TaskConfig


# ── StructuredResult ──

def test_structured_result_default_outcome():
    r = StructuredResult()
    assert r.outcome == Outcome.UNKNOWN_ERROR.value
    assert r.message == ""
    assert r.duration_ms == 0
    assert r.screenshots == []


def test_structured_result_to_dict():
    r = StructuredResult(outcome="success", message="ok", duration_ms=12, screenshots=["a.png"])
    d = r.to_dict()
    assert d["outcome"] == "success"
    assert d["message"] == "ok"
    assert d["duration_ms"] == 12
    assert d["screenshots"] == ["a.png"]


# ── StepConfig 解析 ──

def test_step_from_dict_type_alias():
    # Rust 侧用 "type" 字段，Python 模型用 step_type
    step = StepConfig.from_dict({"id": "s1", "type": "click", "selector": "#btn"})
    assert step.step_type == "click"
    assert step.selector == "#btn"


def test_step_from_dict_unknown_fields_go_to_extras():
    step = StepConfig.from_dict({"id": "s1", "type": "navigate", "full_page": True})
    assert step.extra_fields == {"full_page": True}


def test_step_code_to_script_merge():
    # code 字段存在时合并到 script，effective_script 优先 script
    step = StepConfig.from_dict({"id": "s1", "type": "evaluate", "code": "1+1"})
    assert step.script == "1+1"
    assert step.effective_script == "1+1"
    # script 显式存在时优先 script
    step2 = StepConfig.from_dict({
        "id": "s2", "type": "evaluate", "code": "1+1", "script": "2+2"
    })
    assert step2.effective_script == "2+2"


def test_step_roundtrip_preserves_type_and_extras():
    raw = {
        "id": "s1",
        "type": "screenshot",
        "description": "截图",
        "full_page": False,
        "timeout": 5000,
    }
    step = StepConfig.from_dict(raw)
    back = step.to_dict()
    assert back["type"] == "screenshot"
    assert back["full_page"] is False
    assert back["timeout"] == 5000
    # to_dict 不应泄漏内部 step_type 键
    assert "step_type" not in back


def test_step_required_default_true():
    step = StepConfig.from_dict({"id": "s1", "type": "click"})
    assert step.required is True


# ── B5: 步骤默认值契约（与 Rust StepHelper::default 对齐）──


def test_step_clear_default_true():
    # B5：未写 clear 字段默认 True（Rust 侧同），避免残留值与新值拼接
    step = StepConfig.from_dict({"id": "s1", "type": "input", "selector": "#u"})
    assert step.clear is True
    # 显式 False 仍应保留（真正的追加输入场景）
    step2 = StepConfig.from_dict({"id": "s1", "type": "input", "clear": False})
    assert step2.clear is False


def test_step_duration_default_1000():
    # B5：未写 duration 字段默认 1000ms（Rust 侧同）
    step = StepConfig.from_dict({"id": "s1", "type": "wait"})
    assert step.duration == 1000
    # 显式值覆盖默认
    step2 = StepConfig.from_dict({"id": "s1", "type": "wait", "duration": 2500})
    assert step2.duration == 2500


# ── TaskConfig 解析 ──

def test_task_from_dict_recursive_steps():
    raw = {
        "task_id": "t1",
        "name": "登录",
        "url": "http://portal/cas/login",
        "variables": {"USERNAME": "admin"},
        "steps": [
            {"id": "s1", "type": "input", "selector": "#u", "value": "{{USERNAME}}"},
            {"id": "s2", "type": "click", "selector": "#login"},
        ],
    }
    task = TaskConfig.from_dict(raw)
    assert task.task_id == "t1"
    assert task.url == "http://portal/cas/login"
    assert len(task.steps) == 2
    assert isinstance(task.steps[0], StepConfig)
    assert task.steps[0].step_type == "input"
    assert task.steps[1].step_type == "click"


def test_task_from_dict_unknown_keys_to_extras():
    task = TaskConfig.from_dict({"task_id": "t1", "custom_field": 42})
    assert task.extras == {"custom_field": 42}


def test_task_roundtrip():
    task = TaskConfig.from_dict({
        "task_id": "t1",
        "steps": [{"id": "s1", "type": "click", "selector": "#x", "extra_k": "v"}],
    })
    back = task.to_dict()
    assert back["task_id"] == "t1"
    assert back["steps"][0]["type"] == "click"
    assert back["steps"][0]["extra_k"] == "v"


def test_task_steps_missing_defaults_empty():
    task = TaskConfig.from_dict({"task_id": "t1"})
    assert task.steps == []