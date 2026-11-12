"""持久配置会话（profile_session_*）命令与录制器的契约测试。

本机无 chromium：全部按 stub 级覆盖（不启动真实浏览器），锁住命令路由、
参数校验、占用互斥、幽灵会话收尾、录制器去重/上限与 JS 构造契约；
真浏览器行为（headed 窗口、门户回填、binding 跨导航）留给 mock 门户集成批次。
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

from config_session import (
    RECORDER_BINDING_NAME,
    RECORDER_DEDUP_MS,
    RECORDER_MAX_ACTIONS,
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
    for token in (
        "__BINDING_NAME__", "__MAX_ACTIONS__", "__DEDUP_MS__",
        "__TEXT_MAX__", "__ATTR_MAX__", "__STRUCTURE_DEPTH__", "__STRATEGIES__",
    ):
        assert token not in js, f"模板占位符 {token} 未被替换"


def test_recorder_js_capture_scope():
    """捕获范围：<select> 的 change + 按钮/提交类点击；输入类不录。"""
    js = build_recorder_init_script()
    assert 'el.tagName !== "SELECT"' in js, "change 仅捕获 select（ISP 选择类）"
    assert (
        't.closest("button, [type=submit], input[type=submit], [role=button]")' in js
    ), "点击仅捕获按钮/提交类"
    assert 'document.addEventListener("change", ' in js
    assert 'document.addEventListener("click", ' in js
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
