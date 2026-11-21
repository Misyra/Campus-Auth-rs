"""持久配置会话（profile_session_*）命令与录制器的契约测试。

本机无 chromium：全部按 stub 级覆盖（不启动真实浏览器），锁住命令路由、
参数校验、占用互斥、幽灵会话收尾、录制器去重/上限与 JS 构造契约；
真浏览器行为（headed 窗口、门户回填、binding 跨导航）留给 mock 门户集成批次。
"""

from __future__ import annotations

import asyncio
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest

from config_session import (
    RECORDER_BINDING_NAME,
    RECORDER_DELETE_ACTION_TYPE,
    RECORDER_DEDUP_MS,
    RECORDER_FRAME_MAX_CHARS,
    RECORDER_MAX_ACTIONS,
    RECORDER_PLACEHOLDER_PASSWORD,
    RECORDER_PLACEHOLDER_USERNAME,
    RECORDER_TEXT_MAX_CHARS,
    RECORDER_USERNAME_HINTS,
    SELECTOR_STRATEGY_PRIORITY,
    RecorderSink,
    build_recorder_init_script,
)
from models import Outcome
from playwright_worker import COMMANDS, WorkerCore, cancel_registry
from step_handlers import StepCancelled, WorkerError

# ── 浏览器替身（stub 级）──


def _make_stubs():
    """构造配置会话的 Playwright/上下文/页面替身与创建记录列表。"""
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


def _patch_profile_browser(monkeypatch):
    """把 WorkerCore 的配置会话浏览器装配整体替换为 stub，返回创建记录。"""
    _fake_ctx_cls, contexts, playwrights, fake_start_pw, fake_launch = _make_stubs()
    monkeypatch.setattr(WorkerCore, "_start_profile_playwright", fake_start_pw)
    monkeypatch.setattr(WorkerCore, "_launch_persistent_context", fake_launch)
    return contexts, playwrights


def _start_params(user_data_dir: Path, **overrides) -> dict:
    params = {
        "user_data_dir": str(user_data_dir),
        "portal_url": "http://portal.example/login",
        "record": True,
    }
    params.update(overrides)
    return params


def _make_udd(tmp_path: Path) -> Path:
    """带标记文件的持久目录：收尾后断言文件仍在（绝不清理 user_data_dir）。"""
    udd = tmp_path / "profile-udd"
    udd.mkdir(parents=True, exist_ok=True)
    (udd / "Cookies").write_text("portal-remember-me", encoding="utf-8")
    return udd


# ── 命令注册与参数校验 ──


def test_commands_registry_pins_profile_session_contract():
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


def test_start_requires_user_data_dir():
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_session_start({"portal_url": ""}))
    assert "user_data_dir" in ei.value.message


def test_start_rejects_non_bool_record():
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_session_start({"user_data_dir": "/tmp/x", "record": "yes"}))
    assert "record" in ei.value.message


def test_start_rejects_non_http_portal_url():
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(
            core.handle_profile_session_start(
                {"user_data_dir": "/tmp/x", "portal_url": "ftp://portal.example/"}
            )
        )
    assert "portal_url" in ei.value.message


# ── start → stop 主链路 ──


def test_start_stop_happy_path_records_actions(monkeypatch, tmp_path):
    """start（headed + 持久目录 + 录制注入）→ 喂动作 → stop 返回按序动作。"""
    contexts, playwrights = _patch_profile_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()

    response = asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    session = core._profile_session
    assert response["recorded"] is True
    assert response["session_id"] == session.session_id
    ctx = session.context
    # headed 持久上下文：headless 恒为 False，user_data_dir 原样传入
    assert ctx.launch_kwargs["headless"] is False
    assert ctx.launch_kwargs["user_data_dir"] == str(udd)
    # 录制注入：上下文级 binding + init 脚本
    assert RECORDER_BINDING_NAME in ctx.bindings
    assert any("__campusAuthRecorderInstalled" in s for s in ctx.init_scripts)

    # 模拟页面 JS 经 binding 推送：乱序 ts + 点击/change 混合
    sink = session.sink
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#loginBtn", "strategy": "id", "ts": 200}))
    sink.add_json({}, json.dumps(
        {"type": "change", "selector": "[name=isp]", "strategy": "name",
         "value": "cmcc", "ts": 100}))
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "text=\"登录\"", "strategy": "text", "ts": 300}))

    stop = asyncio.run(core.handle_profile_session_stop({}))

    assert [a["selector"] for a in stop["recorded_actions"]] == [
        "[name=isp]", "#loginBtn", "text=\"登录\"",
    ]
    assert stop["recorded_actions"][0]["value"] == "cmcc"
    assert stop["truncated"] is False
    # 收尾：上下文关闭、独占连接停止、槽位清空、user_data_dir 原样保留
    assert ctx.closed_count == 1
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").read_text(encoding="utf-8") == "portal-remember-me"
    # stop 后可再次 start（槽位已释放）
    contexts.clear()
    playwrights.clear()
    again = asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    assert again["session_id"] != response["session_id"]


def test_start_record_false_returns_empty_actions(monkeypatch, tmp_path):
    """record=false：不注入录制器（binding/init 缺席），stop 返回空数组。"""
    contexts, _playwrights = _patch_profile_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()

    response = asyncio.run(core.handle_profile_session_start(_start_params(udd, record=False)))
    ctx = core._profile_session.context
    assert response["recorded"] is False
    assert ctx.bindings == {}
    assert not any("__campusAuthRecorderInstalled" in s for s in ctx.init_scripts)

    stop = asyncio.run(core.handle_profile_session_stop({}))
    assert stop["recorded_actions"] == []
    assert stop["truncated"] is False
    assert contexts[0].closed_count == 1


def test_start_uses_default_direct_connection_args(monkeypatch, tmp_path):
    """未绑定代理（缺省 browser_settings）→ Chromium 追加 --no-proxy-server（直连保障）。"""
    contexts, _playwrights = _patch_profile_browser(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start({"user_data_dir": str(tmp_path / "u")}))
    assert "--no-proxy-server" in contexts[0].launch_kwargs["launch_args"]


def test_start_navigates_portal_url(monkeypatch, tmp_path):
    """给定 portal_url → 会话页 goto 门户地址。"""
    _contexts, _playwrights = _patch_profile_browser(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start(_start_params(tmp_path / "u")))
    page = core._profile_session.page
    assert page.navigated == ["http://portal.example/login"]


# ── 占用互斥与幽灵会话 ──


def test_start_busy_while_session_alive(monkeypatch, tmp_path):
    """同一时刻仅允许一个活跃配置会话：重复 start 报 busy 类错误。"""
    _contexts, _playwrights = _patch_profile_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    first = asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    assert "已存在活跃配置会话" in ei.value.message
    # 首个会话不受影响
    assert core._profile_session.session_id == first["session_id"]


def test_start_recycles_dead_session_then_allows_restart(monkeypatch, tmp_path):
    """浏览器进程意外退出（幽灵会话）→ start 自动收尾后放行，不清理目录。"""
    contexts, playwrights = _patch_profile_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    # 模拟进程崩溃：上下文已关闭但槽位未收尾
    core._profile_session.context._closed = True
    old_pw = playwrights[0]

    response = asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    assert response["session_id"]
    assert core._profile_session.session_id == response["session_id"]
    assert old_pw.stopped == 1, "幽灵会话的独占连接应被收尾停止"
    assert (udd / "Cookies").exists()


def test_ghost_context_close_releases_slot(monkeypatch, tmp_path):
    """浏览器进程意外退出触发 close 事件 → 槽位释放，可直接再次 start。"""
    contexts, playwrights = _patch_profile_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()

    async def scenario():
        first = await core.handle_profile_session_start(_start_params(udd))
        ctx = core._profile_session.context
        # 进程崩溃：close 事件在槽位仍在时到达
        await ctx.close()
        for _ in range(4):
            await asyncio.sleep(0)  # 排空回调调度的收尾任务
        assert core._profile_session is None, "幽灵会话必须被收尾弹出"
        assert playwrights[0].stopped == 1
        # 收尾后可再次 start
        second = await core.handle_profile_session_start(_start_params(udd))
        assert second["session_id"] != first["session_id"]

    asyncio.run(scenario())


def test_stop_without_session_errors(monkeypatch, tmp_path):
    """未会话 stop：显式报错（对齐 debug_stop，不做静默幂等）。"""
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_session_stop({}))
    assert "没有活跃配置会话" in ei.value.message
    # 重复 stop 同理：第一次 stop 后槽位已空
    _contexts, _playwrights = _patch_profile_browser(monkeypatch)
    asyncio.run(core.handle_profile_session_start({"user_data_dir": str(tmp_path / "u")}))
    asyncio.run(core.handle_profile_session_stop({}))
    with pytest.raises(WorkerError):
        asyncio.run(core.handle_profile_session_stop({}))


def test_stop_with_unknown_session_id_errors(monkeypatch, tmp_path):
    """session_id 不匹配：报错且会话不受影响。"""
    _contexts, _playwrights = _patch_profile_browser(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start({"user_data_dir": str(tmp_path / "u")}))
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_session_stop({"session_id": "nope"}))
    assert "不存在" in ei.value.message
    assert core._profile_session is not None


# ── 启动失败 / 取消的幽灵收尾 ──


def test_start_portal_nav_failure_tears_down_everything(monkeypatch, tmp_path):
    """门户页导航失败 → 整场回收（不留半开会话），WorkerError 原样上抛。"""
    contexts, playwrights = _patch_profile_browser(monkeypatch)

    async def failing_nav(_self, _page, _url, _timeout):
        raise WorkerError(Outcome.NETWORK_ERROR, "门户页导航失败")

    monkeypatch.setattr(WorkerCore, "_navigate", failing_nav)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    with pytest.raises(WorkerError):
        asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    assert core._profile_session is None
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert (udd / "Cookies").exists()


def test_start_cancelled_after_establish_tears_down_ghost_session(monkeypatch, tmp_path):
    """取消恰好在会话建成后到达（对齐 debug_start 范式）→ 幽灵会话收尾。

    取消落在导航阶段：会话已建成，响应返回前 cancel 事件已置位 → 必须拆除
    会话、关闭持久浏览器、保留 user_data_dir，并以 cancelled 语义上抛。
    """
    contexts, playwrights = _patch_profile_browser(monkeypatch)

    async def nav_triggers_cancel(_self, _page, _url, _timeout):
        cancel_registry.trigger("ps-cancel-ghost")

    monkeypatch.setattr(WorkerCore, "_navigate", nav_triggers_cancel)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    with pytest.raises(StepCancelled) as ei:
        asyncio.run(core.handle_profile_session_start(
            _start_params(udd, cancel_id="ps-cancel-ghost")))
    assert "已取消" in ei.value.message
    assert core._profile_session is None, "幽灵配置会话必须被收尾弹出"
    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert (udd / "Cookies").exists(), "取消收尾不得清理 user_data_dir"
    # 收尾后可再次 start
    asyncio.run(core.handle_profile_session_start(_start_params(udd)))
    assert core._profile_session is not None


def test_shutdown_close_profile_session_tears_down_and_preserves_dir(monkeypatch, tmp_path):
    """EOF / shutdown 路径：close_profile_session 收尾配置会话，目录原样保留。"""
    contexts, playwrights = _patch_profile_browser(monkeypatch)
    udd = _make_udd(tmp_path)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start(_start_params(udd)))

    asyncio.run(core.close_profile_session())

    assert contexts[0].closed_count == 1
    assert playwrights[0].stopped == 1
    assert core._profile_session is None
    assert (udd / "Cookies").exists(), "退出兜底不得清理 user_data_dir"


def test_automation_close_browser_leaves_profile_session_alone(monkeypatch, tmp_path):
    """独立性：自动化浏览器回收（close_browser / 空闲回收共路）不殃及配置会话。"""
    contexts, playwrights = _patch_profile_browser(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start(
        {"user_data_dir": str(tmp_path / "u")}))

    asyncio.run(core.close_browser())

    assert contexts[0].closed_count == 0, "配置会话上下文不得被自动化回收关闭"
    assert playwrights[0].stopped == 0
    assert core._profile_session is not None


def test_teardown_profile_session_is_idempotent(monkeypatch, tmp_path):
    """收尾幂等：多路径并发触发（stop + close 事件 + 退出兜底）只关一次。"""
    _contexts, playwrights = _patch_profile_browser(monkeypatch)
    core = WorkerCore()
    asyncio.run(core.handle_profile_session_start({"user_data_dir": str(tmp_path / "u")}))
    session = core._profile_session
    ctx = session.context

    async def scenario():
        await core._teardown_profile_session(session)
        await core._teardown_profile_session(session)  # 第二次应为 no-op
        await core._teardown_profile_session(None)  # 空值安全

    asyncio.run(scenario())
    assert ctx.closed_count == 1
    assert playwrights[0].stopped == 1


# ── 录制器汇聚器（RecorderSink）──


def test_recorder_sink_dedup_window():
    """同一元素 500ms 内重复事件只记一次；窗口锚点为最近一条已记录动作。"""
    sink = RecorderSink()
    base = {"type": "click", "selector": "#btn", "strategy": "id"}
    assert sink.add({**base, "ts": 1000}) is True
    assert sink.add({**base, "ts": 1200}) is False, "窗口内重复丢弃"
    assert sink.add({**base, "ts": 1300}) is False, "窗口内重复丢弃"
    assert sink.add({**base, "ts": 1600}) is True, "超过窗口重新记录"
    # 锚点随最近记录前移：1600 记录后，2000 内的重复仍被丢弃
    assert sink.add({**base, "ts": 1800}) is False


def test_recorder_sink_dedup_only_consecutive_same_element():
    """去重按 (type, selector) 相邻判定：不同元素穿插不受影响。"""
    sink = RecorderSink()
    click_a = {"type": "click", "selector": "#a", "strategy": "id", "ts": 100}
    click_b = {"type": "click", "selector": "#b", "strategy": "id", "ts": 150}
    assert sink.add(click_a) is True
    assert sink.add(click_b) is True
    assert sink.add({**click_a, "ts": 200}) is True, "间隔内插入他元素后不误伤"


def test_recorder_sink_cap_and_truncated():
    """上限 50 条：触顶丢弃后续并标记 truncated。"""
    sink = RecorderSink()
    for i in range(RECORDER_MAX_ACTIONS + 5):
        sink.add({"type": "click", "selector": f"#b{i}", "strategy": "id", "ts": i})
    assert len(sink.recorded_actions()) == RECORDER_MAX_ACTIONS
    assert sink.truncated is True


def test_recorder_sink_truncated_marker_from_js():
    """JS 侧文档内触顶标记（{"type":"truncated"}）补报 truncated，不计为动作。

    文档计数随导航归零：动作分散在多个文档时 Python 权威计数可能未满，
    JS 侧丢弃必须经标记补齐，否则 truncated 语义漏报。
    """
    sink = RecorderSink()
    sink.add_json({}, json.dumps({"type": "click", "selector": "#a", "strategy": "id", "ts": 1}))
    sink.add_json({}, json.dumps({"type": "truncated"}))
    actions = sink.recorded_actions()
    assert len(actions) == 1, "truncated 标记不计为动作"
    assert actions[0]["selector"] == "#a"
    assert sink.truncated is True


def test_recorder_sink_add_json_normalizes_and_drops_malformed():
    """binding 载荷来自页面 JS（不可信）：畸形一律静默丢弃，字段规范化。"""
    sink = RecorderSink()
    bad_payloads = [
        "not json",
        "[1, 2]",
        json.dumps({"type": "hover", "selector": "#x"}),
        json.dumps({"type": "click"}),
        json.dumps({"type": "click", "selector": ""}),
        json.dumps({"selector": "#x", "ts": 1}),
        123,
    ]
    for payload in bad_payloads:
        sink.add_json({}, payload)
    assert sink.recorded_actions() == []

    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "x" * 600, "strategy": "id", "ts": 5}))
    actions = sink.recorded_actions()
    assert len(actions) == 1
    assert len(actions[0]["selector"]) == 512, "超长 selector 兜底截断"
    assert actions[0]["strategy"] == "id"


def test_recorder_sink_records_value_only_for_change():
    """value 仅对 change 捕获（select 选中值），点击不录 value。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "change", "selector": "[name=isp]", "strategy": "name",
         "value": "cmcc", "ts": 1}))
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#btn", "strategy": "id", "ts": 2}))
    change, click = sink.recorded_actions()
    assert change["value"] == "cmcc"
    assert "value" not in click


def test_recorder_sink_change_with_non_string_value_becomes_empty():
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "change", "selector": "[name=isp]", "strategy": "name",
         "value": 42, "ts": 1}))
    assert sink.recorded_actions()[0]["value"] == ""


def test_recorder_sink_recorded_actions_sorted_by_ts():
    """动作序列按时间排序（稳定排序，同 ts 保持到达顺序）。"""
    sink = RecorderSink()
    for ts in (30, 10, 20, 10):
        sink.add({"type": "click", "selector": f"#b{ts}", "strategy": "id", "ts": ts})
    assert [a["ts"] for a in sink.recorded_actions()] == [10, 10, 20, 30]


# ── 录制器 JS 构造（字符串断言 + Python 侧优先级表）──


def test_selector_strategy_priority_order():
    """启发式优先级表：顺序即优先级，是两端共同事实源（注入 JS 模板）。"""
    assert SELECTOR_STRATEGY_PRIORITY == (
        "id", "name", "data-testid", "data", "text", "structure",
    )


def test_recorder_js_embeds_python_constants():
    """JS 模板由 Python 常量构造：常量替换完整、两端数值同源。"""
    js = build_recorder_init_script()
    assert f"const MAX_ACTIONS = {RECORDER_MAX_ACTIONS};" in js
    assert f"const DEDUP_MS = {RECORDER_DEDUP_MS};" in js
    assert RECORDER_BINDING_NAME in js
    assert json.dumps(list(SELECTOR_STRATEGY_PRIORITY)) in js
    assert json.dumps(list(RECORDER_USERNAME_HINTS)) in js
    for token in (
        "__BINDING_NAME__", "__DELETE_TYPE__", "__MAX_ACTIONS__", "__DEDUP_MS__",
        "__TEXT_MAX__", "__ATTR_MAX__", "__STRUCTURE_DEPTH__", "__STRATEGIES__",
        "__URL_MAX__", "__FRAME_MAX__", "__FILL_PASSWORD__", "__FILL_USERNAME__",
        "__USERNAME_HINTS__",
    ):
        assert token not in js, f"模板占位符 {token} 未被替换"


def test_recorder_js_capture_scope():
    """捕获范围：<select> 的 change + 按钮/提交类点击 + 文本输入（fill）。"""
    js = build_recorder_init_script()
    assert 'el.tagName === "SELECT"' in js, "change 仅捕获 select（ISP 选择类）"
    assert (
        't.closest("button, [type=submit], input[type=submit], [role=button]")' in js
    ), "点击仅捕获按钮/提交类"
    assert 'document.addEventListener("change", ' in js
    assert 'document.addEventListener("click", ' in js
    assert 'document.addEventListener("input", ' in js, "文本输入录为 fill"
    # 事件用捕获相位注册（门户 JS stopPropagation 不影响录制）
    assert js.count("}, true);") >= 2


def test_recorder_js_debounce_and_cap_logic():
    """JS 侧防抖窗口与上限逻辑随 Python 常量注入（字符串构造断言）。"""
    js = build_recorder_init_script()
    assert "now - lastTs < DEDUP_MS" in js, "同一元素 500ms 内重复事件只记一次"
    assert "if (sent >= MAX_ACTIONS)" in js, "本文档触顶停止外发"
    assert '{ type: "truncated" }' in js, "JS 侧触顶回传 truncated 标记（防漏报）"
    assert "window.__campusAuthRecorderInstalled" in js, "幂等守卫防重复安装"
    assert 'text=\\"' in js, "text 策略输出 Playwright 引号精确匹配形态"


# ── fill / navigate 捕获（完整录制升级）──


def test_recorder_js_fill_capture_scope():
    """fill 捕获范围：文本类输入 + textarea；密码框恒替换占位符（绝不落真实值）。"""
    js = build_recorder_init_script()
    assert f'const PASSWORD_VALUE = "{RECORDER_PLACEHOLDER_PASSWORD}";' in js
    assert f'const USERNAME_VALUE = "{RECORDER_PLACEHOLDER_USERNAME}";' in js
    # 密码判定先于字面值读取且覆盖 autocomplete 形态（自绘密码框 type=text）：
    # isPasswordField 命中即返回占位符，el.value 永远不会进入 fill 载荷
    assert "const isPasswordField = (el) => {" in js
    assert '=== "password") return true;' in js
    assert '(el.getAttribute("autocomplete") || "").toLowerCase().indexOf("password") !== -1' in js
    # 首判锁定（WeakMap）：「显示密码」开关中途把 type 切成 text 后仍保持占位符
    assert "const fillKindCache = new WeakMap();" in js
    assert 'kind = isPasswordField(el) ? "password"' in js
    # 迟到升级：首判为字面量的元素中途切成密码形态时升为占位符
    assert 'if (kind === "literal" && isPasswordField(el)) {' in js
    # 文本类 type 词表 + textarea；checkbox/radio/file/hidden 等非文本类不录
    assert '"text", "password", "tel", "email", "number", "search", "url"' in js
    assert 'el.tagName === "TEXTAREA"' in js
    assert 'document.addEventListener("input", ' in js
    assert 'document.addEventListener("blur", ' in js
    assert 'document.addEventListener("keydown", ' in js


def test_recorder_js_username_heuristic():
    """用户名启发式：autocomplete="username" + 关键词正则（两端单一事实源注入）。"""
    js = build_recorder_init_script()
    assert '=== "username") return true;' in js, "autocomplete=username 直判用户名框"
    assert json.dumps(list(RECORDER_USERNAME_HINTS)) in js
    # 关键词经 join 构造大小写不敏感正则（name/id/placeholder/aria-label 命中）
    assert 'new RegExp("(" + USERNAME_HINTS.join("|") + ")", "i")' in js
    for hint in ("user", "email", "account", "mobile", "phone", "学号", "账号", "用户名"):
        assert hint in RECORDER_USERNAME_HINTS


def test_recorder_js_fill_debounce_and_navigate_shape():
    """fill 防抖合并 + navigate 上报形状（selector 恒空串、主框架/非 about:blank）。"""
    js = build_recorder_init_script()
    # input 空闲防抖：DEDUP_MS 窗口内只更新最终值，不逐键外发
    assert "setTimeout(flushFill, DEDUP_MS)" in js
    assert "pendingFill.value = value;" in js
    # 结算点：blur / change / Enter 提交 / 点击提交按钮
    assert js.count("flushFill();") >= 4
    # fill 不走窗口丢弃（值不同不可丢），连续 fill 由 Python 侧合并取最终值
    assert 'if (type !== "fill" && type === lastType' in js
    # navigate：主框架 + 非 about:blank 守卫；value=location.href
    assert "window.top === window" in js
    assert 'location.href !== "about:blank"' in js
    assert 'type: "navigate"' in js
    assert "location.href, URL_MAX" in js
    # navigate 就绪重试：init 阶段 binding 可能尚未注入（实测全部丢失），
    # 短轮询等 binding 出现后再报（40 次 ×50ms）
    assert "const navReport = (tries)" in js
    assert 'if (typeof BINDING === "function") {' in js
    assert "setTimeout(() => navReport(tries - 1), 50)" in js
    assert "navReport(40);" in js
    # 面板挂载推迟到 DOM 就绪：init script 执行时 body/documentElement 均可能
    # 为 null，appendChild 抛错会打断脚本（navigate 块在面板装配之后，实测
    # 因此全部丢失）
    assert 'document.readyState === "loading"' in js
    assert 'document.addEventListener("DOMContentLoaded", buildPanel, { once: true });' in js


def test_recorder_sink_accepts_fill_and_navigate():
    """fill/navigate 过守卫并规范化：navigate selector 归一空串、value 必需。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "fill", "selector": "#password", "strategy": "id",
         "value": RECORDER_PLACEHOLDER_PASSWORD, "ts": 1}))
    sink.add_json({}, json.dumps(
        {"type": "navigate", "strategy": "structure",
         "value": "http://portal.example/ok", "ts": 2}))
    actions = sink.recorded_actions()
    assert [a["type"] for a in actions] == ["fill", "navigate"]
    assert actions[0]["selector"] == "#password"
    assert actions[0]["value"] == RECORDER_PLACEHOLDER_PASSWORD
    assert actions[1]["selector"] == "", "navigate 无目标元素，selector 恒空串"
    assert actions[1]["value"] == "http://portal.example/ok"
    assert actions[1]["strategy"] == "structure"


def test_recorder_sink_fill_merge_takes_final_value():
    """连续同目标 fill 就地合并取最终值（不受去重窗口约束）。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "fill", "selector": "#user", "strategy": "id",
         "value": RECORDER_PLACEHOLDER_USERNAME, "ts": 1000}))
    sink.add_json({}, json.dumps(
        {"type": "fill", "selector": "#user", "strategy": "id",
         "value": "admin@school.edu", "ts": 60000}))
    actions = sink.recorded_actions()
    assert len(actions) == 1, "同目标连续 fill 合并为一条最终值"
    assert actions[0]["value"] == "admin@school.edu", "取最终输入值"
    assert actions[0]["ts"] == 60000


def test_recorder_sink_fill_merge_not_across_other_actions():
    """合并只看最近一条已记录动作：穿插其他动作后同目标 fill 各自保留。"""
    sink = RecorderSink()
    sink.add({"type": "fill", "selector": "#user", "strategy": "id", "value": "v1", "ts": 100})
    sink.add({"type": "click", "selector": "#next", "strategy": "id", "ts": 150})
    sink.add({"type": "fill", "selector": "#user", "strategy": "id", "value": "v2", "ts": 200})
    actions = sink.recorded_actions()
    assert [a["type"] for a in actions] == ["fill", "click", "fill"]
    assert (actions[0]["value"], actions[2]["value"]) == ("v1", "v2")


def test_recorder_sink_navigate_merge_keeps_last_landing():
    """重定向链多条 navigate 合并为最终落地页（连续重复只留最后一条）。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps({"type": "navigate", "value": "http://p/1", "ts": 1}))
    sink.add_json({}, json.dumps({"type": "navigate", "value": "http://p/2", "ts": 2}))
    sink.add_json({}, json.dumps({"type": "navigate", "value": "http://p/3", "ts": 3}))
    actions = sink.recorded_actions()
    assert len(actions) == 1
    assert actions[0]["value"] == "http://p/3"


def test_recorder_sink_navigate_and_fill_malformed_dropped():
    """navigate 缺 value / fill 缺 selector：视为畸形静默丢弃。"""
    sink = RecorderSink()
    for payload in (
        json.dumps({"type": "navigate", "ts": 1}),
        json.dumps({"type": "navigate", "value": "", "ts": 1}),
        json.dumps({"type": "navigate", "value": 3, "ts": 1}),
        json.dumps({"type": "navigate", "selector": "#x", "ts": 1}),
        json.dumps({"type": "fill", "value": RECORDER_PLACEHOLDER_PASSWORD, "ts": 1}),
    ):
        sink.add_json({}, payload)
    assert sink.recorded_actions() == []


def test_recorder_sink_fill_value_defensive_clip():
    """fill 字面值兜底截断（JS 已截断，此处防畸形/恶意载荷撑爆 IPC）。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "fill", "selector": "#a", "strategy": "id", "value": "x" * 600, "ts": 1}))
    assert len(sink.recorded_actions()[0]["value"]) == RECORDER_TEXT_MAX_CHARS


def test_recorder_sink_accepts_frame_field():
    """frame 链白名单放行：click/change/fill 保留；顶层动作与 navigate 不带。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "fill", "selector": "#u", "strategy": "id", "value": "a", "ts": 1,
         "frame": "#outer >> #inner"}))
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#b", "strategy": "id", "ts": 2}))
    sink.add_json({}, json.dumps(
        {"type": "navigate", "value": "http://p/ok", "ts": 3, "frame": "#f"}))
    actions = sink.recorded_actions()
    assert actions[0]["frame"] == "#outer >> #inner"
    assert "frame" not in actions[1], "顶层 frame 不带该键"
    assert "frame" not in actions[2], "navigate 恒主框架上报，frame 一并丢弃"


def test_recorder_sink_frame_malformed_or_overlong_normalized():
    """frame 畸形（非字符串/空白）静默丢弃该键；超长兜底截断防撑爆 IPC。"""
    sink = RecorderSink()
    for frame_value in (3, None, [], "  "):
        sink.add_json({}, json.dumps(
            {"type": "click", "selector": f"#b{frame_value!r}", "strategy": "id",
             "ts": 1, "frame": frame_value}))
        assert "frame" not in sink.recorded_actions()[-1]
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#long", "strategy": "id", "ts": 2,
         "frame": "f" * 600}))
    assert len(sink.recorded_actions()[-1]["frame"]) == RECORDER_FRAME_MAX_CHARS


def test_recorder_sink_fill_merge_at_cap_still_allowed():
    """触顶后的同目标 fill 合并仍放行（替换不增计数、不置 truncated）。"""
    sink = RecorderSink()
    for i in range(RECORDER_MAX_ACTIONS - 1):
        sink.add({"type": "click", "selector": f"#b{i}", "strategy": "id", "ts": i})
    sink.add({"type": "fill", "selector": "#user", "strategy": "id", "value": "v1", "ts": 800})
    sink.add({"type": "fill", "selector": "#user", "strategy": "id", "value": "v2", "ts": 900})
    actions = sink.recorded_actions()
    assert len(actions) == RECORDER_MAX_ACTIONS, "合并替换不增计数"
    assert (actions[-1]["value"], actions[-1]["ts"]) == ("v2", 900)
    assert sink.truncated is False


# ── 录制悬浮面板：单步删除与权威列表回推 ──


def test_recorder_sink_delete_removes_action_and_syncs_count():
    """__delete__ 控制事件：按身份删除对应动作、计数联动、控制事件不进输出。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#a", "strategy": "id", "ts": 100}))
    sink.add_json({}, json.dumps(
        {"type": "change", "selector": "[name=isp]", "strategy": "name",
         "value": "cmcc", "ts": 200}))
    sink.add_json({}, json.dumps(
        {"type": "navigate", "value": "http://p/ok", "ts": 300}))
    assert len(sink.recorded_actions()) == 3

    # add_json 返回值即权威全量列表（binding 返回值回推面板的唯一回程通道）
    listed = sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "200",
         "action_type": "change", "selector": "[name=isp]"}))
    assert [a["ts"] for a in listed] == [100, 300], "删除后计数联动"
    assert [a["ts"] for a in sink.recorded_actions()] == [100, 300]
    # 其余动作保持原序（ts 不变，输出恒按 ts 升序）
    assert [a["type"] for a in sink.recorded_actions()] == ["click", "navigate"]
    assert all(a["type"] != RECORDER_DELETE_ACTION_TYPE
               for a in sink.recorded_actions()), "控制事件不进 recorded_actions"
    # 仅 ts（无身份字段）也可删（宽容口径）
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "300"}))
    assert [a["ts"] for a in sink.recorded_actions()] == [100]


def test_recorder_sink_delete_identity_is_precise():
    """删除按 (ts, action_type, selector) 身份精确匹配：同 ts 不同身份不误删。"""
    sink = RecorderSink()
    sink.add({"type": "click", "selector": "#a", "strategy": "id", "ts": 100})
    sink.add({"type": "fill", "selector": "#a", "strategy": "id", "value": "v", "ts": 100})
    # 身份不符（action_type 不对）→ 不删除
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "100",
         "action_type": "navigate", "selector": "#a"}))
    assert len(sink.recorded_actions()) == 2
    # 身份不符（selector 不对）→ 不删除
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "100",
         "action_type": "click", "selector": "#b"}))
    assert len(sink.recorded_actions()) == 2
    # 精确身份命中 fill 条，click 条不受影响
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "100",
         "action_type": "fill", "selector": "#a"}))
    actions = sink.recorded_actions()
    assert len(actions) == 1
    assert actions[0]["type"] == "click"


def test_recorder_sink_delete_all_action_types():
    """navigate / fill / change / click 四类动作均可删除。"""
    sink = RecorderSink()
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#btn", "strategy": "id", "ts": 1}))
    sink.add_json({}, json.dumps(
        {"type": "change", "selector": "[name=isp]", "strategy": "name",
         "value": "cmcc", "ts": 2}))
    sink.add_json({}, json.dumps(
        {"type": "fill", "selector": "#pwd", "strategy": "id",
         "value": RECORDER_PLACEHOLDER_PASSWORD, "ts": 3}))
    sink.add_json({}, json.dumps(
        {"type": "navigate", "value": "http://p/step", "ts": 4}))
    for ts, atype, selector in ((1, "click", "#btn"), (2, "change", "[name=isp]"),
                                (3, "fill", "#pwd"), (4, "navigate", "")):
        sink.add_json({}, json.dumps(
            {"type": RECORDER_DELETE_ACTION_TYPE, "value": str(ts),
             "action_type": atype, "selector": selector}))
    assert sink.recorded_actions() == []


def test_recorder_sink_delete_malformed_ignored():
    """畸形删除载荷（value 缺失/非数值/无匹配）静默忽略，动作不受影响。"""
    sink = RecorderSink()
    sink.add({"type": "click", "selector": "#a", "strategy": "id", "ts": 100})
    for payload in (
        json.dumps({"type": RECORDER_DELETE_ACTION_TYPE}),
        json.dumps({"type": RECORDER_DELETE_ACTION_TYPE, "value": "abc"}),
        json.dumps({"type": RECORDER_DELETE_ACTION_TYPE, "value": None}),
        json.dumps({"type": RECORDER_DELETE_ACTION_TYPE, "value": "999"}),
        json.dumps({"type": RECORDER_DELETE_ACTION_TYPE, "value": []}),
    ):
        assert len(sink.add_json({}, payload)) == 1, "畸形/未命中仍回当前列表"
    assert [a["ts"] for a in sink.recorded_actions()] == [100]


def test_recorder_sink_delete_after_truncated_keeps_guard():
    """删除已截断会话中的动作：计数下降、容量重开接受新动作，truncated 不复位。"""
    sink = RecorderSink()
    # 多喂 3 条触发触顶丢弃（前 50 条全记录、后 3 条被弃）
    for i in range(RECORDER_MAX_ACTIONS + 3):
        sink.add({"type": "click", "selector": f"#b{i}", "strategy": "id", "ts": i})
    assert sink.truncated is True
    assert len(sink.recorded_actions()) == RECORDER_MAX_ACTIONS

    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "10",
         "action_type": "click", "selector": "#b10"}))
    actions = sink.recorded_actions()
    assert len(actions) == RECORDER_MAX_ACTIONS - 1
    assert all(a["ts"] != 10 for a in actions)
    assert sink.truncated is True, "truncated 记录『发生过触顶丢弃』，删除不复位"
    # 删除后容量自然重开：新动作重新被接受，truncated 语义仍然保持
    assert sink.add({"type": "click", "selector": "#new", "strategy": "id", "ts": 999}) is True
    assert len(sink.recorded_actions()) == RECORDER_MAX_ACTIONS
    assert sink.truncated is True


def test_recorder_sink_delete_recompute_dedup_anchor():
    """删除末条后去重/合并锚点重算为当前最近一条已记录动作（既有口径不变）。"""
    sink = RecorderSink()
    sink.add({"type": "click", "selector": "#a", "strategy": "id", "ts": 100})
    sink.add({"type": "click", "selector": "#b", "strategy": "id", "ts": 200})
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "200",
         "action_type": "click", "selector": "#b"}))
    # 锚点若仍指 #b，窗口内新 #b 点击会被误丢；重算为 #a 后 #b 正常记录
    assert sink.add({"type": "click", "selector": "#b", "strategy": "id", "ts": 300}) is True
    # 清空后锚点复位：全新动作不受历史窗口压制
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "300",
         "action_type": "click", "selector": "#b"}))
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "100",
         "action_type": "click", "selector": "#a"}))
    assert sink.recorded_actions() == []
    assert sink.add({"type": "click", "selector": "#a", "strategy": "id", "ts": 400}) is True


def test_recorder_sink_delete_then_fill_merge_uses_new_tail():
    """删除末条后 fill/navigate 合并目标跟随新末条（合并逻辑不受删除破坏）。"""
    sink = RecorderSink()
    sink.add({"type": "fill", "selector": "#user", "strategy": "id", "value": "v1", "ts": 100})
    sink.add({"type": "click", "selector": "#go", "strategy": "id", "ts": 150})
    sink.add_json({}, json.dumps(
        {"type": RECORDER_DELETE_ACTION_TYPE, "value": "150",
         "action_type": "click", "selector": "#go"}))
    # 末条变回 #user fill：同目标 fill 就地合并取最终值，而非新增一条
    sink.add({"type": "fill", "selector": "#user", "strategy": "id", "value": "v2", "ts": 500})
    actions = sink.recorded_actions()
    assert len(actions) == 1
    assert actions[0]["value"] == "v2"
    assert actions[0]["ts"] == 500


def test_recorder_sink_add_json_returns_list_on_every_payload():
    """返回值契约：任何载荷（含畸形/触顶标记/未命中删除）都返回权威全量列表。"""
    sink = RecorderSink()
    sink.add({"type": "click", "selector": "#a", "strategy": "id", "ts": 1})
    for payload in (
        "not json",
        "[1, 2]",
        json.dumps({"type": "hover", "selector": "#x"}),
        json.dumps({"type": "truncated"}),
        json.dumps({"type": RECORDER_DELETE_ACTION_TYPE, "value": "999"}),
    ):
        listed = sink.add_json({}, payload)
        assert isinstance(listed, list)
        assert [a["ts"] for a in listed] == [1], "返回值恒为当前权威全量列表"
    sink.add_json({}, json.dumps(
        {"type": "click", "selector": "#a", "strategy": "id", "ts": 2000}))
    assert [a["ts"] for a in sink.recorded_actions()] == [1, 2000]


# ── 录制悬浮面板：JS 构造契约（字符串断言）──


def test_recorder_js_panel_dom_and_labels():
    """面板 DOM 契约：右下角/最高层级/深色半透明/默认展开/中文徽标/aria-label/计数。"""
    js = build_recorder_init_script()
    assert 'const PANEL_ID = "__campusRecordPanel";' in js
    assert "position:fixed;right:12px;bottom:12px" in js, "固定右下角"
    assert "z-index:2147483647" in js, "层级极高"
    assert "background:rgba(17,20,26,0.88)" in js, "半透明深色底"
    assert "max-width:280px" in js, "宽 ≤280px"
    assert "max-height:200px;overflow-y:auto" in js, "列表高上限滚动"
    assert "let collapsed = false;" in js, "默认展开"
    assert 'textContent = "录制面板"' in js
    assert '"共 " + items.length + " 步"' in js, "头部「共 N 步」计数"
    assert 'textContent = "暂无动作"' in js
    for label in ("点击", "选择", "输入", "跳转", "输入中"):
        assert label in js, "类型徽标中文（含防抖待结算态）"
    assert 'setAttribute("aria-label", "删除第" + (i + 1) + "步");' in js
    assert '"收起录制面板"' in js and '"展开录制面板"' in js, "折叠按钮 aria-label"
    # fill 摘要直接取 value：占位符原样显示（不做任何替换）
    assert "String(s.value || \"\") + \" @ \" + String(s.selector || \"\")" in js
    assert "s.type === \"navigate\") return String(s.value || \"\")" in js, "navigate 显示 URL"


def test_recorder_js_panel_self_defense():
    """面板自防：根节点事件 stopPropagation + 录制器各捕获监听面板内判断。"""
    js = build_recorder_init_script()
    assert 'root.addEventListener(mutedEvents[i], (ev) => ev.stopPropagation());' in js, \
        "面板根所有事件 stopPropagation（拦门户冒泡监听）"
    # document 捕获先于面板根触发，录制器必须各自判断事件源是否在面板内
    assert "const inPanel = (t) => !!(panelRoot && t && panelRoot.contains(t));" in js
    assert js.count("if (inPanel(realTarget(ev))) return;") == 2, "click/keydown 自防"
    assert js.count("if (inPanel(el)) return;") == 3, "change/input/blur 自防（el 即 realTarget(ev)）"
    # click 自防先于 closest 匹配：面板 × 按钮自身就是 button，命中即被录成点击
    click_seg = js.split('document.addEventListener("click"')[1].split("}, true);")[0]
    assert "if (inPanel(realTarget(ev))) return;" in click_seg
    assert click_seg.index("if (inPanel(realTarget(ev))) return;") < click_seg.index('t.closest("button')


def test_recorder_js_delete_wiring_and_server_sync():
    """删除链路与权威列表回推契约：__delete__ 两端同源、身份字段、未上报清缓冲。"""
    js = build_recorder_init_script()
    assert f'const DELETE_TYPE = "{RECORDER_DELETE_ACTION_TYPE}";' in js, \
        "控制事件类型两端同源（Python 常量注入）"
    assert "type: DELETE_TYPE," in js
    assert "value: String(entry.ts)," in js
    assert "action_type: entry.type," in js
    assert "selector: entry.selector," in js
    # 未上报（防抖待结算）直接清页面缓冲，无控制事件
    assert "clearTimeout(pendingFill.timer); pendingFill = null;" in js
    assert "pageSteps = pageSteps.filter((s) => s !== entry);" in js
    # 权威全量列表经 binding 返回值回推，整体收敛面板显示
    assert "if (Array.isArray(v)) renderFromServer(v);" in js
    assert "serverSteps = Array.isArray(list) ? list : null;" in js
    # 捕获入列即镜像：乐观渲染先行，回包后收敛
    assert "pageSteps.push(mirror);" in js
    assert "renderPanel();" in js
    # 面板仅主框架渲染（iframe 不重复出现）
    assert "if (window.top !== window) return;" in js
    # pending fill 条目随输入实时更新最终值
    assert "if (pendingFill.panelEntry) pendingFill.panelEntry.value = value;" in js


# ── 录制器 JS 行为测试（Node 基座：最小 DOM/window 桩执行注入脚本）──
#
# 字符串断言只能钉住代码形状，防抖结算、密码首判锁定这类**行为**需要真执行
# 注入脚本。基座只实现录制器触达的最小面（面板挂载推迟到 DOMContentLoaded、
# 永不触发；navigate 上报以 about:blank 关掉），事件由场景代码手动派发，
# binding 上报整体捕获后回 Python 断言。无 Node 环境跳过（对齐
# test_task_recorder_contract 的守卫模式）。

_RECORDER_JS_STUBS = r"""
const listeners = {};
const payloads = [];
globalThis.window = {
  top: null,
  parent: null,
  frameElement: null,
  addEventListener(type, cb) { (listeners[type] = listeners[type] || []).push(cb); },
};
window["__campusRecordAction"] = (json) => {
  payloads.push(JSON.parse(json));
  return [];
};
__WINDOW_TOPOLOGY__
globalThis.document = {
  readyState: "loading",
  addEventListener(type, cb) { (listeners[type] = listeners[type] || []).push(cb); },
  getElementById() { return null; },
  createElement() { return null; },
  body: null,
  documentElement: null,
};
globalThis.location = { href: "about:blank" };

function makeEl(overrides) {
  const el = {
    nodeType: 1,
    tagName: "INPUT",
    id: "",
    name: "",
    textContent: "",
    attrs: {},
    value: "",
    children: [],
    parentElement: null,
    getAttribute(key) {
      return Object.prototype.hasOwnProperty.call(this.attrs, key) ? this.attrs[key] : null;
    },
    closest() { return null; },
    contains() { return false; },
  };
  return Object.assign(el, overrides || {});
}

// depth 层元素链（叶 + 包裹层）+ body 根：structure 策略测试用
function makeTree(depth, leafOverrides) {
  let node = makeEl(leafOverrides);
  const leaf = node;
  for (let i = 1; i < depth; i++) {
    const parent = makeEl({ tagName: "DIV" });
    parent.children = [node];
    node.parentElement = parent;
    node = parent;
  }
  const body = makeEl({ tagName: "BODY" });
  body.children = [node];
  node.parentElement = body;
  return { leaf, body };
}

function fire(type, ev) {
  (listeners[type] || []).forEach((cb) => cb(ev));
}

function inputEvent(el) {
  return { target: el, composedPath: () => [el] };
}
"""

# 顶层 frame 默认拓扑；iframe 场景由测试改写（window.top/parent/frameElement）
_RECORDER_JS_TOPOLOGY_TOP = "window.top = window;\nwindow.parent = window;"

_RECORDER_JS_EPILOGUE = "console.log(JSON.stringify(payloads));\nprocess.exit(0);\n"


def _run_recorder_js(scenario: str, *, topology: str = _RECORDER_JS_TOPOLOGY_TOP) -> list:
    """Node 基座执行录制器 init 脚本 + 场景代码，返回 binding 捕获的上报列表。"""
    node = shutil.which("node")
    if node is None:
        pytest.skip("当前环境未安装 Node.js（录制器 JS 行为测试需要）")
    harness = "\n".join((
        _RECORDER_JS_STUBS.replace("__WINDOW_TOPOLOGY__", topology),
        build_recorder_init_script(),
        scenario,
        _RECORDER_JS_EPILOGUE,
    ))
    with tempfile.TemporaryDirectory(prefix="recorder-js-") as tmpdir:
        harness_path = Path(tmpdir) / "recorder_harness.js"
        harness_path.write_text(harness, encoding="utf-8")
        result = subprocess.run(
            [node, str(harness_path)], capture_output=True, text=True, check=False,
        )
    assert result.returncode == 0, result.stderr or result.stdout
    return json.loads(result.stdout.strip())


def _fill_payloads(payloads: list) -> list:
    return [p for p in payloads if p["type"] == "fill"]


def test_recorder_username_hints_exclude_password_terms():
    """用户名关键词表不得包含 password/pwd——命中会把密码框错判成用户名占位符的邻域。"""
    lowered = " ".join(RECORDER_USERNAME_HINTS).lower()
    assert "password" not in lowered
    assert "pwd" not in lowered


def test_recorder_js_autocomplete_password_field_records_placeholder(tmp_path):
    """autocomplete 密码框（type=text 自绘形态）录占位符，真实值不进 fill 载荷。"""
    payloads = _run_recorder_js(
        "const pwd = makeEl({ id: 'pwd', attrs: { type: 'text', autocomplete: 'Current-Password' } });"
        "pwd.value = 'real-secret-1';"
        "fire('input', inputEvent(pwd));"
        "fire('blur', { target: pwd });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_PASSWORD
    assert all(p.get("value") != "real-secret-1" for p in payloads)


def test_recorder_js_new_password_autocomplete_records_placeholder():
    """autocomplete="new-password" 同样命中（改密场景的自绘密码框）。"""
    payloads = _run_recorder_js(
        "const pwd = makeEl({ id: 'pwd', attrs: { type: 'text', autocomplete: 'new-password' } });"
        "pwd.value = 'new-pass-2';"
        "fire('input', inputEvent(pwd));"
        "fire('blur', { target: pwd });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_PASSWORD


def test_recorder_js_type_switch_mid_session_keeps_placeholder():
    """「显示密码」开关中途把 type 切成 text：首判锁定的密码框仍录占位符。

    同一输入会话（防抖窗口内）后续按键合并取最终值——最终值必须仍是占位符，
    补输的真实后缀不得以字面量进入任何 fill 载荷。
    """
    payloads = _run_recorder_js(
        "const pwd = makeEl({ id: 'pwd', attrs: { type: 'password' } });"
        "pwd.value = 'sec';"
        "fire('input', inputEvent(pwd));"
        # 「显示密码」开关：type 被切成 text 后继续补输
        "pwd.attrs.type = 'text';"
        "pwd.value = 'sec-ret';"
        "fire('input', inputEvent(pwd));"
        "fire('blur', { target: pwd });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1, "同目标连续 fill 合并为一条"
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_PASSWORD
    assert all(p.get("value") not in ("sec", "sec-ret") for p in payloads)


def test_recorder_js_first_judgment_lock_survives_attr_removal():
    """首判锁定的密码框即使 type/autocomplete 全被移除也保持占位符。"""
    payloads = _run_recorder_js(
        "const pwd = makeEl({ id: 'pwd', attrs: { type: 'password', autocomplete: 'current-password' } });"
        "pwd.value = 'x';"
        "fire('input', inputEvent(pwd));"
        "delete pwd.attrs.type;"
        "delete pwd.attrs.autocomplete;"
        "pwd.value = 'leak-attempt';"
        "fire('input', inputEvent(pwd));"
        "fire('blur', { target: pwd });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_PASSWORD
    assert all(p.get("value") != "leak-attempt" for p in payloads)


def test_recorder_js_late_password_shape_upgrade_records_placeholder():
    """迟到升级：首判为字面量的元素中途切成密码形态 → 会话最终值升为占位符。"""
    payloads = _run_recorder_js(
        "const late = makeEl({ id: 'late', attrs: {} });"
        "late.value = 'first-chars';"
        "fire('input', inputEvent(late));"
        # 门户 JS 中途把字段切成密码形态
        "late.attrs.type = 'password';"
        "late.value = 'rest-of-secret';"
        "fire('input', inputEvent(late));"
        "fire('blur', { target: late });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1, "同目标连续 fill 合并取最终值"
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_PASSWORD


def test_recorder_js_username_and_literal_fields_unaffected():
    """启发式补洞不影响既有口径：用户名框录 {{USERNAME}}、普通框录字面值。"""
    payloads = _run_recorder_js(
        "const user = makeEl({ id: 'user', name: 'username' });"
        "user.value = 'student-001';"
        "fire('input', inputEvent(user));"
        "fire('blur', { target: user });"
        "const plain = makeEl({ id: 'search' });"
        "plain.value = 'hello';"
        "fire('input', inputEvent(plain));"
        "fire('blur', { target: plain });"
    )
    fills = _fill_payloads(payloads)
    assert [f["value"] for f in fills] == [
        RECORDER_PLACEHOLDER_USERNAME, "hello",
    ]


def test_recorder_js_click_flushes_pending_fill_before_button_check():
    """flushFill 先于 closest 判定：非按钮元素点击（<a> 导航）也结算输入会话。

    防抖窗口内点击链接跳转（「短信登录」）时文档随导航销毁，未结算 fill
    若不在点击时落盘即整体丢失。
    """
    payloads = _run_recorder_js(
        "const user = makeEl({ id: 'user', name: 'username' });"
        "user.value = 'student-001';"
        "fire('input', inputEvent(user));"
        # 防抖 500ms 未到即点击非按钮元素（<a>，closest 恒 null）
        "const link = makeEl({ id: 'nav-link', tagName: 'A' });"
        "fire('click', { target: link });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1, "非按钮点击也必须结算待决 fill"
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_USERNAME
    assert all(p["type"] != "click" for p in payloads), "非按钮点击本身仍不录入"


def test_recorder_js_button_click_flush_keeps_fill_before_click_order():
    """按钮点击前结算保持「输入 → 提交」时序（既有口径不回退）。"""
    payloads = _run_recorder_js(
        "const user = makeEl({ id: 'user', name: 'username' });"
        "user.value = 'student-001';"
        "fire('input', inputEvent(user));"
        "const btn = makeEl({ id: 'submit-btn', tagName: 'BUTTON' });"
        "btn.closest = () => btn;"
        "fire('click', { target: btn });"
    )
    assert [p["type"] for p in payloads] == ["fill", "click"]
    assert payloads[0]["value"] == RECORDER_PLACEHOLDER_USERNAME
    assert payloads[1]["selector"] == "#submit-btn"


def test_recorder_js_pagehide_flushes_pending_fill():
    """pagehide 兜底：未决 fill 在文档销毁前结算（最后一线）。"""
    payloads = _run_recorder_js(
        "const plain = makeEl({ id: 'search' });"
        "plain.value = 'last-segment';"
        "fire('input', inputEvent(plain));"
        "fire('pagehide', { target: null });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1
    assert fills[0]["value"] == "last-segment"


def test_recorder_js_click_listener_flush_precedes_closest():
    """静态钉住：click 监听中 flushFill 位于 closest 判定之前（inPanel 之后）。"""
    js = build_recorder_init_script()
    click_seg = js.split('document.addEventListener("click"')[1].split("}, true);")[0]
    assert click_seg.index("flushFill();") < click_seg.index('t.closest("button'), \
        "结算必须先于按钮判定，否则非按钮导航丢 pendingFill"
    assert click_seg.index("if (inPanel(realTarget(ev))) return;") < click_seg.index("flushFill();"), \
        "面板自防仍先于结算"
    # pagehide 兜底监听存在
    assert 'window.addEventListener("pagehide"' in js


def test_recorder_js_shadow_dom_input_captured_via_composed_path():
    """shadow DOM：事件 target 被 retarget 成宿主，composedPath()[0] 才是真实输入。"""
    payloads = _run_recorder_js(
        # open shadow root 场景：input 事件冒出边界后 target = 宿主 div
        "const host = makeEl({ id: 'host', tagName: 'DIV' });"
        "const inner = makeEl({ id: 'shadow-user', name: 'username' });"
        "inner.value = '202312345';"
        "fire('input', { target: host, composedPath: () => [inner] });"
        "fire('blur', { target: inner });"
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1, "宿主 retarget 不得吞掉 shadow 内输入"
    assert fills[0]["selector"] == "#shadow-user"
    assert fills[0]["value"] == RECORDER_PLACEHOLDER_USERNAME


def test_recorder_js_shadow_dom_without_composed_path_falls_back():
    """无 composedPath（合成事件/极端老浏览器）：回落 ev.target 保持旧行为。"""
    payloads = _run_recorder_js(
        "const host = makeEl({ id: 'host', tagName: 'DIV' });"
        "const inner = makeEl({ id: 'shadow-user', name: 'username' });"
        "inner.value = '202312345';"
        "fire('input', { target: host });"
        "fire('blur', { target: inner });"
    )
    # 宿主不是文本元素：旧口径下不捕获（回落语义与历史一致）
    assert _fill_payloads(payloads) == []


def test_recorder_js_shadow_dom_click_captured_via_composed_path():
    """shadow DOM 内按钮点击同样经 composedPath 取真实目标。"""
    payloads = _run_recorder_js(
        "const host = makeEl({ id: 'btn-host', tagName: 'DIV' });"
        "const shadowBtn = makeEl({ id: 'shadow-btn', tagName: 'BUTTON' });"
        "shadowBtn.closest = () => shadowBtn;"
        "fire('click', { target: host, composedPath: () => [shadowBtn] });"
    )
    clicks = [p for p in payloads if p["type"] == "click"]
    assert len(clicks) == 1
    assert clicks[0]["selector"] == "#shadow-btn"


# iframe 拓扑（P2-3 frame 链行为测试）：window 处于两层 iframe 之下，
# frameElement 逐层提供 iframe 元素（外层 #outer-frame、内层 #inner-frame）
_RECORDER_JS_TOPOLOGY_FRAME = """
const TOP = {};
TOP.top = TOP;
TOP.parent = TOP;
const OUTER = makeEl({ id: 'outer-frame', tagName: 'IFRAME' });
const INNER = makeEl({ id: 'inner-frame', tagName: 'IFRAME' });
const MID = { top: TOP, parent: TOP, frameElement: OUTER };
window.top = TOP;
window.parent = MID;
window.frameElement = INNER;
"""


def test_recorder_js_iframe_actions_carry_frame_chain():
    """iframe 内捕获的动作附带 frame 链（主文档 → 目标 iframe，' >> ' 分隔）。"""
    payloads = _run_recorder_js(
        "const fu = makeEl({ id: 'frame-user', name: 'account' });"
        "fu.value = 'u1';"
        "fire('input', inputEvent(fu));"
        "fire('blur', { target: fu });"
        "const fbtn = makeEl({ id: 'frame-btn', tagName: 'BUTTON' });"
        "fbtn.closest = () => fbtn;"
        "fire('click', { target: fbtn });",
        topology=_RECORDER_JS_TOPOLOGY_FRAME,
    )
    assert len(payloads) == 2
    for p in payloads:
        assert p["frame"] == "#outer-frame >> #inner-frame"
    # 顶层动作不带该键（对照：同桩默认拓扑）
    top_payloads = _run_recorder_js(
        "const u = makeEl({ id: 'user', name: 'username' });"
        "u.value = 'u2';"
        "fire('input', inputEvent(u));"
        "fire('blur', { target: u });"
    )
    assert all("frame" not in p for p in top_payloads)


def test_recorder_js_iframe_frame_chain_uncomputable_omits_key():
    """跨域等拿不到 frameElement：链不可计算即不带 frame 键（best-effort）。"""
    payloads = _run_recorder_js(
        "const u = makeEl({ id: 'user', name: 'username' });"
        "u.value = 'u3';"
        "fire('input', inputEvent(u));"
        "fire('blur', { target: u });",
        topology=(
            "const TOP = {};\n"
            "TOP.top = TOP;\n"
            "window.top = TOP;\n"
            "window.parent = TOP;\n"
            "window.frameElement = null;\n"
        ),
    )
    fills = _fill_payloads(payloads)
    assert len(fills) == 1
    assert "frame" not in fills[0]
