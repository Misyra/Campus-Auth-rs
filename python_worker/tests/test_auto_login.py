"""持久自动登录（profile_auto_login）命令的契约测试（stub 级）。

本文件不启动真实浏览器：锁住命令路由、参数校验（浏览器启动前拦截）、回放
映射（click/change → 既有步骤处理器）、占用互斥（与配置会话共用注册表）、
取消阶段边界、幽灵收尾与响应信封形状（``_structured_result`` 语义，Rust 侧
``parse_ipc_response`` / classify 直接可消费）。真浏览器行为（门户回填、
select_option 真实落地、cookie 持久化）见 ``test_auto_login_browser.py``。
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

import worker_main
from auto_login import (
    AUTO_LOGIN_ACTION_TYPES,
    AUTO_LOGIN_MAX_ACTIONS,
    auto_login_success_message,
    build_replay_steps,
    build_replay_variables,
    validate_auto_login_actions,
)
from models import Outcome, StepConfig
from playwright_worker import COMMANDS, WorkerCore, cancel_registry
from step_handlers import StepCancelled, WorkerError

# ── 纯逻辑：校验与回放映射 ──


def test_action_types_vocabulary():
    """可回放动作词表与录制契约一致（click / change / fill / navigate）。"""
    assert AUTO_LOGIN_ACTION_TYPES == ("click", "change", "fill", "navigate")


def test_validate_actions_accepts_recorder_shape():
    """录制契约形状（click 无 value / change 带 value）校验通过并规范化。"""
    actions = validate_auto_login_actions([
        {"type": "click", "selector": "#login-btn", "strategy": "id", "ts": 200},
        {
            "type": "change", "selector": "[name=isp]", "strategy": "name",
            "ts": 100, "value": "telecom",
        },
    ])
    assert actions[0] == {
        "type": "click", "selector": "#login-btn", "strategy": "id", "ts": 200,
    }
    assert actions[1]["value"] == "telecom"


def test_validate_actions_preserves_order_without_resort():
    """回放按数组顺序（Rust 侧契约保证 ts 升序），不再重排。"""
    actions = validate_auto_login_actions([
        {"type": "click", "selector": "#b", "ts": 999},
        {"type": "click", "selector": "#a", "ts": 1},
    ])
    assert [a["selector"] for a in actions] == ["#b", "#a"]


def test_validate_actions_rejects_malformed():
    """畸形 / 不认识的 type / change 缺 value：逐条结构化报错（中文消息）。"""
    bad_cases = [
        ("非对象元素", ["nope"]),
        ("不认识的 type", [{"type": "hover", "selector": "#x"}]),
        ("缺 selector", [{"type": "click"}]),
        ("空 selector", [{"type": "click", "selector": "  "}]),
        ("change 缺 value", [{"type": "change", "selector": "#isp"}]),
        ("change 空 value", [{"type": "change", "selector": "#isp", "value": ""}]),
        ("change 非字符串 value", [{"type": "change", "selector": "#isp", "value": 3}]),
    ]
    for label, payload in bad_cases:
        with pytest.raises(ValueError) as ei:
            validate_auto_login_actions(payload)
        assert "录制动作" in str(ei.value), label


def test_validate_actions_rejects_over_cap():
    """防御上限：超出 AUTO_LOGIN_MAX_ACTIONS 启动浏览器前即报错。"""
    payload = [{"type": "click", "selector": f"#b{i}", "ts": i} for i in range(AUTO_LOGIN_MAX_ACTIONS + 1)]
    with pytest.raises(ValueError) as ei:
        validate_auto_login_actions(payload)
    assert str(AUTO_LOGIN_MAX_ACTIONS) in str(ei.value)


def test_validate_actions_accepts_empty():
    """空数组合法（只打开门户页等回填不点击）。"""
    assert validate_auto_login_actions([]) == []


def test_build_replay_steps_maps_click_and_change():
    """回放映射：click → click 处理器；change → select 处理器（value 必带）。"""
    actions = validate_auto_login_actions([
        {"type": "change", "selector": "[name=isp]", "value": "telecom", "ts": 1},
        {"type": "click", "selector": "text=\"登录\"", "ts": 2},
    ])
    steps = build_replay_steps(actions)
    assert all(isinstance(s, StepConfig) for s in steps)
    assert (steps[0].step_type, steps[0].selector, steps[0].value) == ("select", "[name=isp]", "telecom")
    assert (steps[1].step_type, steps[1].selector) == ("click", "text=\"登录\"")
    # 步骤 id 供进度/错误定位；不设显式 timeout（沿用上下文默认超时契约）
    assert [s.id for s in steps] == ["auto-login-1", "auto-login-2"]
    assert steps[0].timeout is None and steps[1].timeout is None


def test_success_message_phases():
    """成功人话短语区分回放与否。"""
    assert "回填" in auto_login_success_message(0)
    assert "1" in auto_login_success_message(1)


# ── 纯逻辑：fill / navigate 回放（完整录制升级）──


def test_validate_actions_accepts_fill_and_navigate():
    """fill（占位符/字面值）与 navigate（selector 缺失/空串归一）校验通过并规范化。"""
    actions = validate_auto_login_actions([
        {"type": "navigate", "strategy": "structure", "ts": 1,
         "value": "http://portal.example/step2"},
        {"type": "navigate", "selector": "", "ts": 2, "value": "http://portal.example/ok"},
        {"type": "fill", "selector": "#username", "strategy": "id", "ts": 3,
         "value": "{{USERNAME}}"},
        {"type": "fill", "selector": "#pwd", "strategy": "id", "ts": 4,
         "value": "{{PASSWORD}}"},
    ])
    assert actions[0]["selector"] == "", "navigate 无目标元素，selector 归一空串"
    assert actions[0]["value"] == "http://portal.example/step2"
    assert actions[1]["selector"] == ""
    assert (actions[2]["value"], actions[3]["value"]) == ("{{USERNAME}}", "{{PASSWORD}}")


def test_validate_actions_rejects_fill_navigate_without_value():
    """fill/navigate 缺非空 value：逐条结构化报错（数据损坏启动前拦截）。"""
    bad_cases = [
        [{"type": "fill", "selector": "#u"}],
        [{"type": "fill", "selector": "#u", "value": ""}],
        [{"type": "fill", "selector": "#u", "value": "  "}],
        [{"type": "navigate"}],
        [{"type": "navigate", "value": ""}],
        [{"type": "navigate", "selector": "", "value": 3}],
    ]
    for payload in bad_cases:
        with pytest.raises(ValueError) as ei:
            validate_auto_login_actions(payload)
        assert "录制动作" in str(ei.value)


def test_build_replay_variables_from_params():
    """凭据映射从命令参数键构建（与浏览器任务执行完全同名），None/缺失不注入。"""
    variables = build_replay_variables({
        "username": "202312345", "password": "p@ss", "isp": "cmcc",
        "auth_url": "http://portal.example/login", "other": "ignored",
    })
    assert variables == {
        "USERNAME": "202312345", "PASSWORD": "p@ss", "ISP": "cmcc",
        "LOGIN_URL": "http://portal.example/login",
    }
    assert build_replay_variables({}) == {}
    # None（Rust serde null）不注入：str(None) 会把字面 "None" 当凭据（P2-7）
    assert build_replay_variables({"username": None, "password": "x"}) == {"PASSWORD": "x"}


def test_build_replay_steps_resolves_placeholders():
    """fill/navigate 的占位符经变量映射解析；未传 variables 保持字面值。"""
    actions = validate_auto_login_actions([
        {"type": "fill", "selector": "#username", "strategy": "id", "ts": 1,
         "value": "{{USERNAME}}"},
        {"type": "fill", "selector": "#password", "strategy": "id", "ts": 2,
         "value": "{{PASSWORD}}"},
        {"type": "navigate", "ts": 3, "value": "{{LOGIN_URL}}?next=1"},
    ])
    variables = build_replay_variables({
        "username": "u1", "password": "p1", "auth_url": "http://p.example/login",
    })
    steps = build_replay_steps(actions, variables=variables)
    assert [s.step_type for s in steps] == ["input", "input", "navigate"]
    assert (steps[0].value, steps[1].value) == ("u1", "p1")
    assert steps[2].value == "http://p.example/login?next=1"
    # 不传 variables：占位符保留原样（既有调用方零感知）
    raw = build_replay_steps(actions)
    assert raw[0].value == "{{USERNAME}}"
    assert raw[2].value == "{{LOGIN_URL}}?next=1"


def test_build_replay_steps_fill_maps_to_input_handler():
    """fill → input 处理器（clear 默认 True → locator.fill 整值填充）。"""
    actions = validate_auto_login_actions([
        {"type": "fill", "selector": "#username", "strategy": "id", "ts": 1,
         "value": " literal "},
    ])
    (step,) = build_replay_steps(actions)
    assert isinstance(step, StepConfig)
    assert (step.step_type, step.selector, step.value, step.clear) == (
        "input", "#username", " literal ", True,
    )


def test_build_replay_steps_skips_first_navigate_matching_portal():
    """首条 navigate 与 portal_url 相同 → wait(0) 占位跳过（保持动作↔步骤 1:1）。"""
    actions = validate_auto_login_actions([
        {"type": "navigate", "ts": 1, "value": "http://portal.example/login"},
        {"type": "fill", "selector": "#u", "strategy": "id", "ts": 2, "value": "a"},
        {"type": "click", "selector": "#login", "strategy": "id", "ts": 3},
    ])
    steps = build_replay_steps(actions, portal_url="http://portal.example/login")
    assert [s.step_type for s in steps] == ["wait", "input", "click"], "首条 navigate 跳过"
    assert steps[0].duration == 0, "占位 wait 不产生任何页面操作"
    assert [s.id for s in steps] == ["auto-login-1", "auto-login-2", "auto-login-3"]
    assert len(steps) == len(actions), "步骤数与动作数保持 1:1（错误消息按下标取动作）"


def test_build_replay_steps_first_navigate_mismatch_then_same_kept():
    """首条 navigate 与 portal_url 不同 → 不跳过；且只检查首条（后续同值保留）。"""
    actions = validate_auto_login_actions([
        {"type": "navigate", "ts": 1, "value": "http://portal.example/other"},
        {"type": "navigate", "ts": 2, "value": "http://portal.example/login"},
    ])
    steps = build_replay_steps(actions, portal_url="http://portal.example/login")
    assert [s.step_type for s in steps] == ["navigate", "navigate"]


def test_build_replay_steps_navigate_resolution_before_portal_compare():
    """首条跳过的比较在占位符解析之后进行（{{LOGIN_URL}} 与门户同值即跳过）。"""
    actions = validate_auto_login_actions([
        {"type": "navigate", "ts": 1, "value": "{{LOGIN_URL}}"},
        {"type": "click", "selector": "#login", "strategy": "id", "ts": 2},
    ])
    variables = build_replay_variables({"auth_url": "http://portal.example/login"})
    steps = build_replay_steps(
        actions, portal_url="http://portal.example/login", variables=variables,
    )
    assert steps[0].step_type == "wait"


def test_build_replay_steps_change_value_not_resolved():
    """change 值保持录制字面量：占位符解析仅对 fill/navigate 生效。"""
    actions = validate_auto_login_actions([
        {"type": "change", "selector": "[name=isp]", "strategy": "name", "ts": 1,
         "value": "{{ISP}}"},
    ])
    (step,) = build_replay_steps(actions, variables=build_replay_variables({"isp": "cmcc"}))
    assert step.step_type == "select"
    assert step.value == "{{ISP}}", "change 值不解析，既有契约字节面不变"


# ── frame 链（iframe 内录制动作的定位契约，P2-3 Python 半边）──


def test_validate_actions_preserves_frame_field():
    """可选 frame 链原样保留（click/change/fill）；顶层动作不带该键。"""
    actions = validate_auto_login_actions([
        {"type": "click", "selector": "#login", "strategy": "id", "ts": 1,
         "frame": "iframe#outer >> iframe#inner"},
        {"type": "click", "selector": "#top", "strategy": "id", "ts": 2},
    ])
    assert actions[0]["frame"] == "iframe#outer >> iframe#inner"
    assert "frame" not in actions[1]


def test_validate_actions_frame_malformed_dropped_not_fatal():
    """frame 非字符串/空白：静默丢弃该键，不阻断动作本体与整段校验。"""
    actions = validate_auto_login_actions([
        {"type": "click", "selector": "#x", "ts": 1, "frame": "  "},
        {"type": "fill", "selector": "#y", "ts": 2, "value": "v", "frame": 3},
        {"type": "change", "selector": "#z", "ts": 3, "value": "cmcc", "frame": None},
    ])
    assert all("frame" not in a for a in actions)
    assert [a["selector"] for a in actions] == ["#x", "#y", "#z"]


def test_build_replay_steps_maps_frame_into_step_config():
    """frame 链映射进 StepConfig.frame；顶层动作恒 None（主文档执行）。"""
    actions = validate_auto_login_actions([
        {"type": "fill", "selector": "#u", "strategy": "id", "ts": 1, "value": "a",
         "frame": "iframe#outer >> iframe#inner"},
        {"type": "click", "selector": "#go", "strategy": "id", "ts": 2,
         "frame": "#single-frame"},
        {"type": "change", "selector": "[name=isp]", "strategy": "name", "ts": 3,
         "value": "cmcc", "frame": "url=portal-frame"},
        {"type": "click", "selector": "#top", "strategy": "id", "ts": 4},
    ])
    steps = build_replay_steps(actions)
    assert [s.step_type for s in steps] == ["input", "click", "select", "click"]
    assert steps[0].frame == "iframe#outer >> iframe#inner"
    assert steps[1].frame == "#single-frame"
    assert steps[2].frame == "url=portal-frame"
    assert steps[3].frame is None, "顶层动作无 frame 键 → 主文档执行"


# ── 浏览器替身（stub 级）──


def _make_stubs():
    """构造自动登录的 Playwright/上下文/页面替身与创建记录列表。"""
    contexts: list = []
    playwrights: list = []

    class FakePlaywright:
        def __init__(self) -> None:
            self.stopped = 0

        async def stop(self) -> None:
            self.stopped += 1

    class FakePage:
        def __init__(self) -> None:
            self.navigated: list[str] = []
            self.handlers: dict[str, list] = {}

        async def goto(self, url, **_kwargs):
            self.navigated.append(url)

        def on(self, event, cb) -> None:
            self.handlers.setdefault(event, []).append(cb)

    class FakeContext:
        def __init__(self) -> None:
            self.pages: list = []
            self.handlers: dict[str, list] = {}
            self.init_scripts: list[str] = []
            self.routes: list[str] = []
            self.bindings: dict = {}
            self.closed_count = 0
            self._closed = False
            self.launch_kwargs: dict = {}

        def is_closed(self) -> bool:
            return self._closed

        def on(self, event, cb) -> None:
            self.handlers.setdefault(event, []).append(cb)

        async def add_init_script(self, script) -> None:
            self.init_scripts.append(script)

        async def route(self, pattern, _handler) -> None:
            self.routes.append(pattern)

        async def expose_binding(self, name, callback) -> None:
            self.bindings[name] = callback

        async def new_page(self):
            page = FakePage()
            self.pages.append(page)
            return page

        async def close(self) -> None:
            self.closed_count += 1
            self._closed = True
            # 对齐真实 Playwright：close 触发 close 事件回调
            for cb in list(self.handlers.get("close", [])):
                cb(self)

    async def fake_start_pw(_self):
        pw = FakePlaywright()
        playwrights.append(pw)
        return pw

    async def fake_launch(
        _self, playwright, channel, custom_path, headless, launch_args,
        user_data_dir, ctx_opts, bs=None,
    ):
        ctx = FakeContext()
        ctx.launch_kwargs = {
            "headless": headless,
            "user_data_dir": user_data_dir,
            "channel": channel,
            "launch_args": list(launch_args),
        }
        contexts.append(ctx)
        return ctx

    return FakeContext, contexts, playwrights, fake_start_pw, fake_launch


def _patch_auto_login_browser(monkeypatch):
    """把 WorkerCore 的自动登录浏览器装配整体替换为 stub，返回创建记录。"""
    _fake_ctx_cls, contexts, playwrights, fake_start_pw, fake_launch = _make_stubs()
    monkeypatch.setattr(WorkerCore, "_start_profile_playwright", fake_start_pw)
    monkeypatch.setattr(WorkerCore, "_launch_persistent_context", fake_launch)
    return contexts, playwrights


def _auto_params(user_data_dir: Path, **overrides) -> dict:
    params = {
        "user_data_dir": str(user_data_dir),
        "portal_url": "http://portal.example/login",
        "recorded_actions": [
            {"type": "click", "selector": "#login-btn", "strategy": "id", "ts": 100},
        ],
    }
    params.update(overrides)
    return params


def _make_udd(tmp_path: Path) -> Path:
    """带标记文件的持久目录：命令结束后断言文件仍在（绝不清理 user_data_dir）。"""
    udd = tmp_path / "auto-login-udd"
    udd.mkdir(parents=True, exist_ok=True)
    (udd / "Cookies").write_text("portal-remember-me", encoding="utf-8")
    return udd


@pytest.fixture(autouse=True)
def _fast_settle_and_clean_cancel_registry(monkeypatch):
    """stub 级加速：沉降窗口压到最小（真浏览器批次保留真实窗口）+ 注册表隔离。"""
    import auto_login

    monkeypatch.setattr(auto_login, "AUTO_LOGIN_BACKFILL_SETTLE_MS", 10)
    monkeypatch.setattr(auto_login, "AUTO_LOGIN_LOAD_STATE_TIMEOUT_MS", 50)
    yield
    cancel_registry._events.clear()
    cancel_registry._pending.clear()


# ── 命令注册与参数校验（浏览器启动前拦截）──


def test_commands_registry_pins_auto_login_contract():
    """COMMANDS 注册表钉住：与 README 命令表逐项对应（18 项）。"""
    expected = {
        "worker_health_check", "browser_health_check", "test_redirect",
        "execute_login_attempt", "execute_browser_task", "close_browser",
        "debug_start", "debug_step", "debug_run_all", "debug_stop", "debug_status",
        "feedback_capture", "page_capture",
        "profile_session_start", "profile_session_stop",
        "profile_auto_login",
        "ocr_recognize", "shutdown",
    }
    assert set(COMMANDS) == expected


def test_auto_login_requires_user_data_dir():
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login({"portal_url": "http://p.example/"}))
    assert "user_data_dir" in ei.value.message
    assert ei.value.outcome == Outcome.UNKNOWN_ERROR.value


def test_auto_login_requires_portal_url():
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login({"user_data_dir": "/tmp/x"}))
    assert "portal_url" in ei.value.message


def test_auto_login_rejects_non_http_portal_url():
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(
            {"user_data_dir": "/tmp/x", "portal_url": "file:///etc/passwd"}))
    assert "http/https" in ei.value.message


def test_auto_login_rejects_non_list_actions(monkeypatch, tmp_path):
    """recorded_actions 非数组：结构化报错，且浏览器根本不启动。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(
            _auto_params(tmp_path / "u", recorded_actions="click #btn")))
    assert "recorded_actions" in ei.value.message
    assert contexts == [], "校验失败不得启动浏览器"


@pytest.mark.parametrize("bad_actions", [
    [{"type": "hover", "selector": "#x"}],
    [{"type": "click", "selector": ""}],
    [{"type": "change", "selector": "#isp", "value": ""}],
])
def test_auto_login_rejects_invalid_actions_before_launch(monkeypatch, tmp_path, bad_actions):
    """畸形动作在浏览器启动前结构化报错（不留半开资源）。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(
            _auto_params(tmp_path / "u", recorded_actions=bad_actions)))
    assert ei.value.outcome == Outcome.UNKNOWN_ERROR.value
    assert "recorded_actions" in ei.value.message
    assert contexts == []


def test_auto_login_accepts_missing_actions_as_empty(monkeypatch, tmp_path):
    """recorded_actions 缺省（None）= 空回放（只打开门户页等回填）。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    core = WorkerCore()
    params = {"user_data_dir": str(tmp_path / "u"), "portal_url": "http://portal.example/login"}
    result = asyncio.run(core.handle_profile_auto_login(params))
    assert result["outcome"] == Outcome.SUCCESS.value
    assert contexts[0].launch_kwargs["user_data_dir"] == str(tmp_path / "u")


# ── 主链路（stub 浏览器 + 回放执行器替身）──


def _stub_run_steps(monkeypatch, *, calls: list | None = None, fail_at: int | None = None,
                    exc: Exception | None = None):
    """把 run_step_async 替换为记录器：可指定第几步抛什么异常。"""
    calls = calls if calls is not None else []

    async def fake_run_step(page, step, context, step_index=None, total_steps=None):
        calls.append((step.step_type, step.selector, getattr(step, "value", None), step_index, total_steps))
        if fail_at is not None and step_index == fail_at and exc is not None:
            raise exc

    monkeypatch.setattr("step_handlers.run_step_async", fake_run_step)
    return calls


def test_auto_login_happy_path_headless_and_replay(monkeypatch, tmp_path):
    """主链路：headless 持久上下文 + 导航门户 + 沉降 + 逐条回放 + 收尾。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    calls = _stub_run_steps(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    params = _auto_params(udd, recorded_actions=[
        {"type": "change", "selector": "[name=isp]", "strategy": "name", "ts": 50, "value": "telecom"},
        {"type": "click", "selector": "#login-btn", "strategy": "id", "ts": 100},
    ])

    result = asyncio.run(core.handle_profile_auto_login(params))

    # headless 持久上下文，user_data_dir 原样传入，不注入录制器
    assert contexts[0].launch_kwargs["headless"] is True
    assert contexts[0].launch_kwargs["user_data_dir"] == str(udd)
    assert contexts[0].bindings == {}
    assert contexts[0].init_scripts == []
    # 门户导航 + 沉降由 handler 完成；回放按序、带 total_steps 元信息
    # （收尾后槽位已清空，从创建记录里取会话页断言）
    assert contexts[0].pages[0].navigated == ["http://portal.example/login"]
    assert calls == [
        ("select", "[name=isp]", "telecom", 0, 2),
        ("click", "#login-btn", None, 1, 2),
    ]
    # 成功信封（StructuredResult 形状，Rust parse_ipc_response 直接可消费）
    assert result["outcome"] == Outcome.SUCCESS.value
    assert result["screenshots"] == []
    assert "2" in result["message"]
    assert result["duration_ms"] >= 0
    # 收尾：上下文关闭、独占连接停止、槽位清空、user_data_dir 原样保留
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").read_text(encoding="utf-8") == "portal-remember-me"


def test_auto_login_replays_fill_and_navigate_actions(monkeypatch, tmp_path):
    """fill/navigate 动作经命令链路映射为 input/navigate 步骤回放（不再拒识）。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    calls = _stub_run_steps(monkeypatch)
    core = WorkerCore()
    params = _auto_params(tmp_path / "u", recorded_actions=[
        {"type": "fill", "selector": "#username", "strategy": "id", "ts": 10,
         "value": "{{USERNAME}}"},
        {"type": "navigate", "strategy": "structure", "ts": 20,
         "value": "http://portal.example/step2"},
        {"type": "click", "selector": "#login-btn", "strategy": "id", "ts": 30},
    ])
    result = asyncio.run(core.handle_profile_auto_login(params))
    assert result["outcome"] == Outcome.SUCCESS.value
    assert [c[0] for c in calls] == ["input", "navigate", "click"]
    assert calls[0][1] == "#username"
    assert calls[1][2] == "http://portal.example/step2"
    # 校验失败/拒识都不会走到这里：占位符 value 原样透传（解析依赖调用方
    # 传入 variables——portal_url/variables 接线在编排层，见 build_replay_steps）


def test_auto_login_default_direct_connection_args(monkeypatch, tmp_path):
    """未绑定代理（缺省 browser_settings）→ Chromium 追加 --no-proxy-server。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_auto_login(_auto_params(tmp_path / "u")))
    assert "--no-proxy-server" in contexts[0].launch_kwargs["launch_args"]


def test_auto_login_pure_mode_keeps_stealth_off(monkeypatch, tmp_path):
    """pure_mode：跳过 stealth/资源路由，但不关闭直连保障（口径对齐 _start_browser）。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_auto_login(
        _auto_params(tmp_path / "u", browser_settings={"pure_mode": True})))
    assert contexts[0].routes == []
    assert contexts[0].init_scripts == []
    assert "--no-proxy-server" in contexts[0].launch_kwargs["launch_args"]


def test_auto_login_applies_stealth_to_own_context(monkeypatch, tmp_path):
    """stealth_mode：反检测脚本作用于自动登录自己的上下文。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_auto_login(
        _auto_params(tmp_path / "u", browser_settings={"stealth_mode": True})))
    assert any("webdriver" in s for s in contexts[0].init_scripts)


def test_auto_login_replay_failure_maps_to_assertion_failed(monkeypatch, tmp_path):
    """回放步骤失败 → assertion_failed（带动作定位与原始消息），整场收尾。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(
        monkeypatch, fail_at=1,
        exc=WorkerError(Outcome.SELECTOR_FAILED, "未找到可点击元素: #missing-btn"),
    )
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(_auto_params(udd, recorded_actions=[
            {"type": "click", "selector": "#ok-btn", "ts": 1},
            {"type": "click", "selector": "#missing-btn", "ts": 2},
        ])))
    assert ei.value.outcome == Outcome.ASSERTION_FAILED.value
    assert "回放第 2/2" in ei.value.message
    assert "未找到可点击元素" in ei.value.message
    # 失败整场回收：不留半开浏览器，目录原样保留
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").exists()


def test_auto_login_replay_cancellation_propagates(monkeypatch, tmp_path):
    """回放中的取消保持 cancelled 语义（不被 asserttion_failed 吞掉）。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch, fail_at=0, exc=StepCancelled("步骤执行已取消"))
    core = WorkerCore()
    with pytest.raises(StepCancelled):
        asyncio.run(core.handle_profile_auto_login(_auto_params(tmp_path / "u")))
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1


def test_auto_login_navigation_failure_classified_and_torn_down(monkeypatch, tmp_path):
    """门户导航失败按既有分类上抛（navigation_timeout / network_error）并整场回收。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)

    async def timeout_nav(_self, _page, _url, _timeout):
        raise WorkerError(Outcome.NAVIGATION_TIMEOUT, "导航超时: http://portal.example/login")

    monkeypatch.setattr(WorkerCore, "_navigate", timeout_nav)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(_auto_params(udd)))
    assert ei.value.outcome == Outcome.NAVIGATION_TIMEOUT.value
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").exists()


def test_auto_login_ghost_context_close_mid_run(monkeypatch, tmp_path):
    """回放中途浏览器进程意外退出：close 事件收尾 + 失败归 assertion_failed。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()

    async def dying_step(page, step, context, step_index=None, total_steps=None):
        # 模拟浏览器崩溃：close 事件到达（槽位仍在）→ 幽灵收尾，随后步骤报错
        await core._profile_session.context.close()
        await asyncio.sleep(0)
        raise WorkerError(Outcome.SELECTOR_FAILED, "Target page closed")

    monkeypatch.setattr("step_handlers.run_step_async", dying_step)
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(_auto_params(udd)))
    assert ei.value.outcome == Outcome.ASSERTION_FAILED.value
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").exists()


# ── 取消（阶段边界）──


def test_auto_login_cancelled_before_launch(monkeypatch, tmp_path):
    """取消先于注册到达（pending 语义）：浏览器启动前即取消，零资源创建。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    cancel_registry.trigger("auto-cancel-pre")
    core = WorkerCore()
    with pytest.raises(StepCancelled) as ei:
        asyncio.run(core.handle_profile_auto_login(
            _auto_params(tmp_path / "u", cancel_id="auto-cancel-pre")))
    assert "已取消" in ei.value.message
    assert contexts == []


def test_auto_login_cancelled_between_stages(monkeypatch, tmp_path):
    """沉降阶段触发取消 → 回放步骤前的阶段边界拦截生效并整场收尾。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    core = WorkerCore()

    async def settle_triggers_cancel(_self, _page, _context):
        cancel_registry.trigger("auto-cancel-mid")

    monkeypatch.setattr(WorkerCore, "_settle_auto_login_page", settle_triggers_cancel)
    udd = _make_udd(tmp_path)
    with pytest.raises(StepCancelled) as ei:
        asyncio.run(core.handle_profile_auto_login(
            _auto_params(udd, cancel_id="auto-cancel-mid")))
    assert "回放动作 1" in ei.value.message
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").exists()


def test_auto_login_cancelled_at_completion_window(monkeypatch, tmp_path):
    """幽灵取消窗口（对齐配置会话）：完成与响应返回之间取消 → 不报成功。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)
    core = WorkerCore()
    settle_calls = {"n": 0}

    async def settle_cancel_on_last(_self, _page, _context):
        settle_calls["n"] += 1
        if settle_calls["n"] == 2:  # 第 2 次 = 回放后沉降，取消落在完成窗口
            cancel_registry.trigger("auto-cancel-done")

    monkeypatch.setattr(WorkerCore, "_settle_auto_login_page", settle_cancel_on_last)
    with pytest.raises(StepCancelled) as ei:
        asyncio.run(core.handle_profile_auto_login(
            _auto_params(tmp_path / "u", cancel_id="auto-cancel-done")))
    assert "完成阶段" in ei.value.message
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1


# ── 占用互斥（与配置会话共用同一注册表）──


def _plant_alive_session(core: WorkerCore, tmp_path: Path):
    """向占用注册表植入一个存活配置会话（模拟配置会话进行中 / auto_login 持锁）。"""
    from config_session import ProfileSessionState, RecorderSink

    fake_ctx_cls = _make_stubs()[0]
    ctx = fake_ctx_cls()
    session = ProfileSessionState(
        session_id="planted",
        user_data_dir=str(tmp_path / "planted"),
        portal_url="http://portal.example/login",
        record=False,
        sink=RecorderSink(),
        context=ctx,
    )
    core._profile_session = session
    return session, ctx


def test_auto_login_busy_while_config_session_alive(monkeypatch, tmp_path):
    """互斥（正向）：配置会话进行中 → auto_login 报占用，浏览器不启动。"""
    contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    core = WorkerCore()
    session, _ctx = _plant_alive_session(core, tmp_path)
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(_auto_params(tmp_path / "u2")))
    assert "已存在活跃配置会话" in ei.value.message
    assert core._profile_session is session, "占用方不受影响"
    assert contexts == []


def test_profile_session_start_busy_while_auto_login_holds_slot(monkeypatch, tmp_path):
    """互斥（反向）：auto_login 进行中（持有槽位）→ profile_session_start 报占用。

    auto_login 运行期间把自己的持久上下文挂进同一注册表（真实行为），此处
    以植入会话模拟该状态，验证两个方向的互斥共用同一占用语义。
    """
    _contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    core = WorkerCore()
    _session, _ctx = _plant_alive_session(core, tmp_path)
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_session_start({"user_data_dir": str(tmp_path / "u3")}))
    assert "已存在活跃配置会话" in ei.value.message


def test_auto_login_recycles_dead_session_then_proceeds(monkeypatch, tmp_path):
    """幽灵会话先收尾再放行（口径同 profile_session_start）。"""
    contexts, playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)
    core = WorkerCore()
    session, ctx = _plant_alive_session(core, tmp_path)
    ctx._closed = True  # 浏览器进程已退出

    result = asyncio.run(core.handle_profile_auto_login(_auto_params(tmp_path / "u2")))

    assert result["outcome"] == Outcome.SUCCESS.value
    assert ctx.closed_count == 1, "幽灵会话上下文被收尾关闭"
    assert core._profile_session is None
    assert len(contexts) == 1, "自动登录自己的上下文照常创建"


# ── 响应信封形状（worker_main._dispatch 真实出口，Rust 消费契约）──


def _dispatch_auto_login(monkeypatch, capsys, params: dict) -> dict:
    """以真实注册的 profile_auto_login 命令跑一次 _dispatch_guarded，返回唯一信封。"""
    params = {**params, "rust_timeout_ms": 60000}

    async def _run():
        await worker_main._dispatch_guarded({"id": 901, "method": "profile_auto_login", "params": params})
        lines = capsys.readouterr().out.strip().splitlines()
        assert len(lines) == 1
        return json.loads(lines[0])

    return asyncio.run(_run())


def test_dispatch_auto_login_success_envelope(monkeypatch, capsys, tmp_path):
    """成功信封：success=true + data.outcome=success + 人话 message（无 error）。"""
    _contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(monkeypatch)
    envelope = _dispatch_auto_login(monkeypatch, capsys, _auto_params(tmp_path / "u"))
    assert envelope["id"] == 901
    result = envelope["result"]
    assert result["success"] is True and result["error"] is None
    assert result["data"]["outcome"] == "success"
    assert result["data"]["message"]
    assert result["data"]["screenshots"] == []


def test_dispatch_auto_login_failure_envelope(monkeypatch, capsys, tmp_path):
    """失败信封：success=false + data.outcome=assertion_failed + error 中文消息。"""
    _contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    _stub_run_steps(
        monkeypatch, fail_at=0,
        exc=WorkerError(Outcome.SELECTOR_FAILED, "未找到可点击元素: #missing-btn"),
    )
    envelope = _dispatch_auto_login(monkeypatch, capsys, _auto_params(tmp_path / "u"))
    result = envelope["result"]
    assert result["success"] is False
    assert result["data"]["outcome"] == "assertion_failed"
    assert "回放第 1/1" in result["data"]["message"]
    assert result["error"] == result["data"]["message"]


def test_dispatch_auto_login_cancelled_envelope(monkeypatch, capsys, tmp_path):
    """取消信封：success=true（终态）+ data.outcome=cancelled（对齐既有取消语义）。"""
    _contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    cancel_registry.trigger("auto-cancel-env")
    envelope = _dispatch_auto_login(
        monkeypatch, capsys, _auto_params(tmp_path / "u", cancel_id="auto-cancel-env"))
    result = envelope["result"]
    assert result["success"] is True and result["error"] is None
    assert result["data"]["outcome"] == "cancelled"


def test_dispatch_auto_login_validation_error_envelope(monkeypatch, capsys, tmp_path):
    """参数错误信封：success=false + data.outcome=unknown_error（不重试语义）。"""
    _contexts, _playwrights = _patch_auto_login_browser(monkeypatch)
    envelope = _dispatch_auto_login(monkeypatch, capsys, {"user_data_dir": str(tmp_path / "u")})
    result = envelope["result"]
    assert result["success"] is False
    assert result["data"]["outcome"] == "unknown_error"
    assert "portal_url" in result["error"]
