"""跨语言任务配置契约测试（B5）。

加载 ``tests/fixtures/task_contract.json``（手写样例，与 Rust
``src/tasks/models.rs`` 的浏览器 TaskConfig 序列化形状对齐——Rust 侧经
``TaskManager::embed_task_config`` 序列化为 ``task_config`` 键下发 Worker），
走 ``TaskConfig.from_dict`` 断言全字段解析、默认值应用与序列化往返一致。
两端 CI 共同护航该契约：Rust 侧改动序列化形状时，此处在同一次提交内同步。
"""

from __future__ import annotations

import json
from pathlib import Path

from models import StepConfig, TaskConfig

# fixture 路径（tests/fixtures/task_contract.json）
_FIXTURE = Path(__file__).parent / "fixtures" / "task_contract.json"


def _load() -> dict:
    return json.loads(_FIXTURE.read_text(encoding="utf-8"))


def test_contract_fixture_loads_and_covers_all_step_types():
    raw = _load()
    task = TaskConfig.from_dict(raw)
    # 全部 16 个 Rust VALID_STEP_TYPES（sleep/goto/custom_js 为 Python 别名）覆盖
    expected_types = {
        "input", "click", "select", "click_select", "wait", "wait_url",
        "eval", "screenshot", "sleep", "ocr", "custom_js", "navigate",
        "goto", "assert_text", "upload_file", "wait_for_selector",
    }
    actual_types = {s.step_type for s in task.steps}
    assert actual_types == expected_types, f"步骤类型覆盖缺口: {expected_types ^ actual_types}"
    assert len(task.steps) == len(raw["steps"]), "步骤数量应一一对应"


def test_contract_task_level_fields_roundtrip():
    task = TaskConfig.from_dict(_load())
    assert task.task_id == "contract_sample"
    assert task.name == "契约样例任务"
    assert task.description == "覆盖全字段与全步骤类型的浏览器任务样例"
    assert task.url == "http://192.168.7.1:8081/login"
    assert task.variables == {"USERNAME": "{{USERNAME}}", "GATEWAY": "192.168.7.1"}
    assert task.success_condition == "logged_in"
    assert task.reveal_hidden is True
    # Rust 侧 TaskConfig 的 timeout / navigation_wait 未在 Python 模型显式建模
    #（落入 extras 由两端按需读取）；step_delay 为 Python 显式字段
    assert task.step_delay == 0.5
    assert task.extras["timeout"] == 30000
    assert task.extras["navigation_wait"] == 1.0
    # on_success / on_failure / metadata 整体透传
    assert task.on_success == {"type": "notify", "message": "登录成功"}
    assert task.on_failure == {"type": "notify", "message": "登录失败"}
    assert task.metadata == {"author": "ci", "tags": ["contract", "browser"]}
    # _comment 文档键落入 extras（Python 不拒绝未知字段）
    assert "_comment" in task.extras


def test_contract_step_fields_parsed():
    task = TaskConfig.from_dict(_load())
    by_id = {s.id: s for s in task.steps}

    # input 全字段（含 frame / clear / timeout）
    s = by_id["s_input_username"]
    assert isinstance(s, StepConfig)
    assert s.selector == "#username"
    assert s.value == "{{USERNAME}}"
    assert s.timeout == 8000
    assert s.required is True
    assert s.clear is True
    assert s.frame == "#login-iframe"

    # click 候选选择器原样保留
    assert by_id["s_click_login"].selector == "#login-btn, button[type=submit]"

    # select / click_select
    assert by_id["s_select_isp"].value == "cmcc"
    assert by_id["s_click_select_campus"].option_selector == "#campus-option-south"

    # wait / sleep 时长
    assert by_id["s_wait_fixed"].duration == 1500
    assert by_id["s_sleep_alias"].duration == 500

    # wait_for_selector / wait_url
    assert by_id["s_wait_selector"].timeout == 5000
    assert by_id["s_wait_url"].pattern == "https?://[^/]+/success"

    # screenshot：path 字段 + full_page 扩展字段
    s = by_id["s_screenshot_page"]
    assert s.path == "after_login"
    assert s.extra_fields == {"full_page": False}

    # eval / custom_js：script 与 store_as
    assert by_id["s_eval_store"].script is not None
    assert "user-name" in by_id["s_eval_store"].script
    assert by_id["s_eval_store"].store_as == "logged_in"
    assert by_id["s_custom_js"].effective_script is not None

    # navigate / goto：目标 URL 与 wait_until 扩展
    assert by_id["s_navigate_portal"].value == "http://{{GATEWAY}}:8081/portal"
    assert by_id["s_navigate_portal"].extra_fields.get("wait_until") == "domcontentloaded"
    assert by_id["s_goto_alias"].extra_fields.get("url") == "http://{{GATEWAY}}:8081/home"

    # assert_text / upload_file
    assert by_id["s_assert_text"].value == "登录成功"
    assert by_id["s_upload_avatar"].path is None
    assert by_id["s_upload_avatar"].value == "C:/tmp/avatar.png"

    # ocr 全字段
    s = by_id["s_ocr_captcha"]
    assert s.target_selector == "#captcha-input"
    assert s.old is False
    assert s.char_range == "0123456789"
    assert s.store_as == "captcha_text"
    assert s.frame == "iframe[name=verify]"


def test_contract_step_defaults_aligned_with_rust():
    """B5 默认值契约：required=True / clear=True / duration=1000。

    与 Rust 侧 StepHelper::default（src/tasks/models.rs）逐项对齐：
    前端编辑器不写这些字段时，两端解析出的默认值必须一致。
    """
    task = TaskConfig.from_dict(_load())
    # fixture 中该步骤仅写 selector/value/required=false，其余走默认
    s = task.steps[1]
    assert s.id == "s_input_password_minimal"
    assert s.required is False  # 显式 false 保留
    assert s.clear is True  # 默认 true（B5 对齐）
    assert s.duration == 1000  # 默认 1000ms（B5 对齐）
    assert s.timeout is None
    assert s.frame is None

    # 完全最小步骤（无任何可选字段）
    minimal = StepConfig.from_dict({"id": "m", "type": "click"})
    assert minimal.required is True
    assert minimal.clear is True
    assert minimal.duration == 1000


def test_contract_roundtrip_preserves_shape():
    """from_dict → to_dict 往返：type 别名、extras、显式字段全部保留。"""
    raw = _load()
    task = TaskConfig.from_dict(raw)
    back = task.to_dict()

    # 顶层显式字段与 extras 均在
    assert back["task_id"] == raw["task_id"]
    assert back["url"] == raw["url"]
    assert back["variables"] == raw["variables"]
    assert back["success_condition"] == raw["success_condition"]
    assert back["timeout"] == 30000  # extras 回填
    assert back["_comment"] == raw["_comment"]

    # 步骤往返：type 别名还原、extras 合并、显式字段值不变
    steps_back = {s["id"]: s for s in back["steps"]}
    assert "step_type" not in steps_back["s_click_login"]
    assert steps_back["s_click_login"]["type"] == "click"
    assert steps_back["s_screenshot_page"]["full_page"] is False
    assert steps_back["s_navigate_portal"]["wait_until"] == "domcontentloaded"
    assert steps_back["s_input_username"]["frame"] == "#login-iframe"
    assert steps_back["s_ocr_captcha"]["char_range"] == "0123456789"
    # B5 默认值在往返后同样保持
    assert steps_back["s_input_password_minimal"]["clear"] is True
    assert steps_back["s_input_password_minimal"]["duration"] == 1000
