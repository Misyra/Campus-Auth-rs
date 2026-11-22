"""持久自动登录（profile_auto_login）的真浏览器契约测试（本仓库首个真浏览器 pytest 批）。

用真实 chromium + 本地 mock 门户（threading http.server，随机端口）验证：
门户「记住密码」JS 自动回填 → 回放录制动作（change→select_option、
click→表单提交）→ cookie/localStorage 跨浏览器进程持久化。

环境守卫：chromium 可执行文件缺失时整文件 skip（CI 的 e2e 已装 chromium，
会真跑；本机开发环境未装时不阻塞其余测试）。mock 门户自带「记住密码」JS：
登录表单 + localStorage 存凭据 + onload 自动回填 + 提交后 Set-Cookie/303
跳转成功页；服务端请求日志作为「回填确实发生」的服务侧证据。
"""

from __future__ import annotations

import asyncio
import functools
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import pytest

from models import Outcome
from playwright_worker import WorkerCore
from step_handlers import WorkerError

# ── 环境守卫 ──


@functools.lru_cache(maxsize=1)
def _chromium_available() -> bool:
    """探测 Worker 环境的 chromium 是否可用（不启动浏览器，只查可执行文件）。"""
    try:
        from playwright.sync_api import sync_playwright

        with sync_playwright() as pw:
            exe = pw.chromium.executable_path
            return bool(exe) and Path(exe).exists()
    except Exception:  # noqa: BLE001 — playwright 缺失/损坏一律视为不可用
        return False


pytestmark = pytest.mark.skipif(
    not _chromium_available(),
    reason="本机未安装 playwright chromium（uv run playwright install chromium 后真跑）",
)

# ── mock 门户（「记住密码」语义的最小实现）──

_LOCALSTORAGE_CREDS_KEY = "mock_portal_creds"

_LOGIN_PAGE = f"""<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Mock Portal 登录</title></head>
<body>
<h1>校园网门户（Mock）</h1>
<form id="login-form" action="/do-login" method="post">
  <input id="username" name="username" type="text">
  <input id="password" name="password" type="password">
  <select id="isp" name="isp">
    <option value="cmcc">中国移动</option>
    <option value="telecom">中国电信</option>
    <option value="unicom">中国联通</option>
  </select>
  <label><input id="remember" name="remember" type="checkbox">记住密码</label>
  <button id="login-btn" type="submit">登录</button>
</form>
<script>
(() => {{
  document.getElementById("login-form").addEventListener("submit", () => {{
    if (document.getElementById("remember").checked) {{
      localStorage.setItem("{_LOCALSTORAGE_CREDS_KEY}", JSON.stringify({{
        username: document.getElementById("username").value,
        password: document.getElementById("password").value,
        isp: document.getElementById("isp").value,
      }}));
    }}
  }});
  // onload 自动回填（「记住密码」语义：门户自己的 JS 负责账号密码回填）
  window.addEventListener("load", () => {{
    try {{
      const saved = JSON.parse(localStorage.getItem("{_LOCALSTORAGE_CREDS_KEY}") || "null");
      if (saved) {{
        document.getElementById("username").value = saved.username || "";
        document.getElementById("password").value = saved.password || "";
        if (saved.isp) document.getElementById("isp").value = saved.isp;
        document.getElementById("remember").checked = true;
        window.__mockBackfilled = true;
      }}
    }} catch (e) {{}}
  }});
}})();
</script>
</body></html>"""

_SUCCESS_PAGE = """<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Mock Portal</title></head>
<body><h1 id="portal-welcome">登录成功</h1>
<script>localStorage.setItem("mock_portal_session_marker", "1");</script>
</body></html>"""


class _MockPortalHandler(BaseHTTPRequestHandler):
    """最小门户：/login（带 cookie 直接 303 成功页）/ /do-login / /success。"""

    def log_message(self, _format, *_args) -> None:  # stderr 静默（测试输出降噪）
        pass

    # ── 服务端请求日志（回填/回放的服务侧证据）──

    def _log(self, method: str, path: str, body: dict | None = None) -> None:
        self.server.request_log.append(
            {"method": method, "path": path, **({"body": body} if body else {})}
        )

    def _html(self, markup: str, status: int = 200) -> None:
        payload = markup.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _redirect(self, location: str, *, set_cookie: str | None = None) -> None:
        self.send_response(303)
        if set_cookie:
            self.send_header("Set-Cookie", set_cookie)
        self.send_header("Location", location)
        self.end_headers()

    def _has_session_cookie(self) -> bool:
        return "portal_session=" in (self.headers.get("Cookie") or "")

    def do_GET(self) -> None:
        path = urlsplit(self.path).path
        self._log("GET", path)
        if path == "/login":
            # 门户记忆了会话：直接放行到成功页（空回放场景的依赖行为）
            if self._has_session_cookie():
                self._redirect("/success")
            else:
                self._html(_LOGIN_PAGE)
        elif path == "/success":
            self._html(_SUCCESS_PAGE)
        else:
            self._html("<h1>404</h1>", status=404)

    def do_POST(self) -> None:
        path = urlsplit(self.path).path
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length).decode("utf-8") if length else ""
        fields = parse_qs(raw)
        body = {
            "username": (fields.get("username") or [""])[0],
            "password": (fields.get("password") or [""])[0],
            "isp": (fields.get("isp") or [""])[0],
        }
        self._log("POST", path, body=body)
        if path == "/do-login" and body["username"] and body["password"]:
            # Max-Age 必须显式给出：无 Expiry 的会话 cookie 不落盘，跨浏览器
            # 进程（持久目录重启）不保留，持久性用例会失真
            self._redirect(
                "/success",
                set_cookie="portal_session=mock-session-token; Path=/; Max-Age=3600",
            )
        else:
            self._redirect("/login")


@pytest.fixture
def mock_portal():
    """随机端口起最小门户，yield (base_url, 请求日志)；测试结束即关。"""
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockPortalHandler)
    server.request_log = []
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}", server.request_log
    finally:
        server.shutdown()
        server.server_close()


# ── 种子 / 断言辅助 ──


def _seed_portal_login(udd: Path, base_url: str, *, keep_session_cookie: bool = True) -> None:
    """真起持久上下文手动走一遍登录（填表 + 勾记住密码 + 提交），给 udd 播种。

    ``keep_session_cookie=False`` 时清掉会话 cookie 再关（模拟「门户会话已过期，
    记住的凭据还在」——自动登录依赖 localStorage 回填的典型场景）。
    """
    from playwright.sync_api import sync_playwright

    with sync_playwright() as pw:
        ctx = pw.chromium.launch_persistent_context(str(udd), headless=True)
        page = ctx.new_page()
        page.goto(f"{base_url}/login", wait_until="domcontentloaded")
        page.fill("#username", "seeduser")
        page.fill("#password", "seedpass")
        page.select_option("#isp", "cmcc")
        page.check("#remember")
        page.click("#login-btn")
        page.wait_for_url(f"{base_url}/success", timeout=15000)
        if not keep_session_cookie:
            ctx.clear_cookies()
        ctx.close()


def _run_auto_login(udd: Path, portal_url: str, recorded_actions: list, **overrides) -> tuple[WorkerCore, dict]:
    """以真实 chromium 执行一次 profile_auto_login 命令，返回 (core, 结果)。"""
    core = WorkerCore()
    params = {
        "user_data_dir": str(udd),
        "portal_url": portal_url,
        "recorded_actions": recorded_actions,
        **overrides,
    }
    return core, asyncio.run(core.handle_profile_auto_login(params))


def _logged_after(log: list, mark: int) -> list[dict]:
    return log[mark:]


# ── 真浏览器用例 ──


def test_seed_then_auto_login_full_chain(tmp_path, mock_portal):
    """用例 ①：种子 → 自动登录全链路（回填发生 + 回放 select/click + Success 信封）。"""
    base_url, log = mock_portal
    udd = tmp_path / "udd-chain"
    udd.mkdir()
    # 种子（清掉会话 cookie：门户会话已过期，只有 localStorage 记住的凭据）
    _seed_portal_login(udd, base_url, keep_session_cookie=False)
    mark = len(log)

    core, result = _run_auto_login(udd, f"{base_url}/login", [
        {"type": "change", "selector": "#isp", "strategy": "id", "ts": 100, "value": "telecom"},
        {"type": "click", "selector": "#login-btn", "strategy": "id", "ts": 200},
    ])

    # Success 信封（_structured_result 语义）
    assert result["outcome"] == Outcome.SUCCESS.value
    assert "2" in result["message"]
    assert core._profile_session is None, "命令结束必须释放占用槽位"

    phase = _logged_after(log, mark)
    posts = [e for e in phase if e["method"] == "POST" and e["path"] == "/do-login"]
    assert len(posts) == 1, "回放点击恰好触发一次表单提交"
    submitted = posts[0]["body"]
    # 回填证据：回放动作里没有任何 input 步骤，提交却带出了种子账号/密码
    assert submitted["username"] == "seeduser", "门户 JS 自动回填发生了"
    assert submitted["password"] == "seedpass"
    # 回放 select 证据：回放 change 覆盖了回填的 cmcc
    assert submitted["isp"] == "telecom"
    # 页面完成导航/加载：提交后到达成功页
    assert any(e["method"] == "GET" and e["path"] == "/success" for e in phase)


def test_empty_actions_with_session_cookie_reaches_success(tmp_path, mock_portal):
    """用例 ②：空 recorded_actions + 已有会话 cookie → 门户直接放行到成功页。"""
    base_url, log = mock_portal
    udd = tmp_path / "udd-empty"
    udd.mkdir()
    _seed_portal_login(udd, base_url, keep_session_cookie=True)
    mark = len(log)

    _core, result = _run_auto_login(udd, f"{base_url}/login", [])

    assert result["outcome"] == Outcome.SUCCESS.value
    assert "回填" in result["message"]

    phase = _logged_after(log, mark)
    # 无 POST（没有重新登录），/login 命中 cookie 放行 → 成功页到达
    assert not any(e["method"] == "POST" for e in phase)
    assert any(e["method"] == "GET" and e["path"] == "/login" for e in phase)
    assert any(e["method"] == "GET" and e["path"] == "/success" for e in phase)


def test_replay_selector_mismatch_assertion_failed(tmp_path, mock_portal):
    """用例 ③：录制动作 selector 失配 → assertion_failed + 中文 message + 整场收尾。"""
    base_url, log = mock_portal
    udd = tmp_path / "udd-mismatch"
    udd.mkdir()
    udd.joinpath("marker.txt").write_text("keep-me", encoding="utf-8")
    mark = len(log)
    core = WorkerCore()
    params = {
        "user_data_dir": str(udd),
        "portal_url": f"{base_url}/login",
        "recorded_actions": [
            {"type": "click", "selector": "#does-not-exist-btn", "strategy": "id", "ts": 1},
        ],
        # 收紧单步超时（browser_settings.timeout 单位为秒）：失配的点击不必等满默认 10s
        "browser_settings": {"timeout": 2},
    }

    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(params))

    assert ei.value.outcome == Outcome.ASSERTION_FAILED.value
    assert "回放第 1/1" in ei.value.message
    assert "does-not-exist-btn" in ei.value.message
    # 失败整场回收：不留半开浏览器、目录原样保留、槽位清空
    assert core._profile_session is None
    assert udd.joinpath("marker.txt").read_text(encoding="utf-8") == "keep-me"
    # 失配点击未产生任何提交
    assert not any(e["method"] == "POST" for e in _logged_after(log, mark))


def test_invalid_params_structured_error_without_browser(tmp_path, mock_portal):
    """用例 ④：user_data_dir 缺失 / 非法 portal_url → 结构化错误（不启动浏览器）。"""
    core = WorkerCore()
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login({"portal_url": "http://p.example/"}))
    assert ei.value.outcome == Outcome.UNKNOWN_ERROR.value
    assert "user_data_dir" in ei.value.message
    with pytest.raises(WorkerError) as ei:
        asyncio.run(core.handle_profile_auto_login(
            {"user_data_dir": str(tmp_path / "u"), "portal_url": "ftp://p.example/"}))
    assert "http/https" in ei.value.message


def test_persistence_across_browser_restarts(tmp_path, mock_portal):
    """用例 ⑦：持久性——auto_login 成功后 cookie 持久化，二次启动无需重新登录。"""
    base_url, log = mock_portal
    udd = tmp_path / "udd-persist"
    udd.mkdir()
    _seed_portal_login(udd, base_url, keep_session_cookie=False)

    # 第一次自动登录：回放点击完成登录（服务端种下会话 cookie）
    mark_1 = len(log)
    _core1, result_1 = _run_auto_login(udd, f"{base_url}/login", [
        {"type": "click", "selector": "#login-btn", "strategy": "id", "ts": 100},
    ])
    assert result_1["outcome"] == Outcome.SUCCESS.value
    posts_1 = [e for e in _logged_after(log, mark_1) if e["method"] == "POST"]
    assert len(posts_1) == 1

    # 第二次自动登录（新浏览器进程、空回放）：cookie 仍在 → /login 直接放行
    mark_2 = len(log)
    _core2, result_2 = _run_auto_login(udd, f"{base_url}/login", [])
    assert result_2["outcome"] == Outcome.SUCCESS.value
    phase_2 = _logged_after(log, mark_2)
    assert not any(e["method"] == "POST" for e in phase_2), "第二次启动不得重新提交登录"
    assert any(e["method"] == "GET" and e["path"] == "/success" for e in phase_2), (
        "cookie 持久化生效：门户直接放行到成功页"
    )

    # localStorage 残留：新开持久上下文直读，记住的凭据仍在（回填源头完好）
    from playwright.sync_api import sync_playwright

    with sync_playwright() as pw:
        ctx = pw.chromium.launch_persistent_context(str(udd), headless=True)
        page = ctx.new_page()
        page.goto(f"{base_url}/login", wait_until="domcontentloaded")
        stored = page.evaluate(f"localStorage.getItem('{_LOCALSTORAGE_CREDS_KEY}')")
        ctx.close()
    assert stored is not None
    assert "seeduser" in stored
