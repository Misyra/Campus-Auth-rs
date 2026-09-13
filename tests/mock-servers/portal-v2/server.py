# -*- coding: utf-8 -*-
r"""复杂模拟校园网认证门户 v2（login_chain 扩展用例与本地实测共用）

相对 full-portal 的增强（选择器 #username/#password/#captcha-img/#login-btn 与
full-portal 一致，现有任务步骤无需修改即可驱动）：
- 传统 form POST + 三级 302 跳转链（POST /login → 302 /step1 → 302 /step2 → /portal）
- 5 位数字验证码（字符旋转 + 穿越弧线 + 噪点，保持 ddddocr 可识别），点击刷新
- Cookie 会话（sid）与来源 IP 记录，登录成功才计入 login_count
- captive 语义双端点：/generate_204（未认证 200 / 已认证 204）、
  /hotspot-detect.html（Success / captive）
- 失败/行为注入（均为一次性或限时，测试内可控）：
    POST /failonce           下一次登录验证码错误
    POST /failntimes?n=N     接下来 N 次账密错误
    POST /slowlogin?ms=N     下一次登录响应延迟 N 毫秒（单次生效）
    POST /ban?seconds=N      封禁期内登录一律拒绝（重定向 /?error=banned）
    GET|POST /kick           强制掉线（不消耗认证状态以外的状态）
- 跳转陷阱（MON-4 语义回归）：/redirect-loopback（302 → 同主机环回路径，同主机
  免校验应跟随）、/redirect-loopback2（302 → http://127.0.0.2:<port>/，跨主机
  环回应拒）、/redirect-zero（302 → http://0.0.0.0:1/，通配地址应拒）
- 调试：GET /status 兼容 full-portal 字段并扩展（kick_count/fail_remaining/
  slow_ms/ban_until/log），GET /api/me、GET /debug

有效账号：testuser/testpass、e2euser/e2epass、admin/admin123
启动：python server.py --port <port>（随机端口由测试传入）
"""
import io
import json
import random
import secrets
import threading
import time
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from PIL import Image, ImageDraw, ImageFont

VALID_USERS = {
    "testuser": "testpass",
    "e2euser": "e2epass",
    "admin": "admin123",
}

lock = threading.Lock()
state = {
    "authenticated": False,
    "username": None,
    "session_id": None,
    "login_ip": None,
    "last_login_time": None,
    "login_count": 0,
    "captcha_text": "",
    "failonce": False,
    "fail_remaining": 0,
    "ban_until": 0.0,
    "slow_ms": 0,
    "kick_count": 0,
    "log": [],
}

FONTS = [
    r"C:\Windows\Fonts\arialbd.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    r"C:\Windows\Fonts\segoeui.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
]


def _font(size):
    for path in FONTS:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


def gen_captcha():
    """5 位数字验证码：旋转 + 穿越弧线 + 双色噪点，轻度保持 OCR 识别率"""
    text = "".join(random.choices("0123456789", k=5))
    w, h = 150, 46
    img = Image.new("RGB", (w, h), (244, 246, 250))
    draw = ImageDraw.Draw(img)
    font = _font(32)

    for i, ch in enumerate(text):
        char_img = Image.new("RGBA", (44, 52), (0, 0, 0, 0))
        cdraw = ImageDraw.Draw(char_img)
        cdraw.text((4, 4), ch, font=font,
                   fill=(25 + random.randint(0, 70), 35 + random.randint(0, 30), 95 + random.randint(0, 80)))
        char_img = char_img.rotate(random.uniform(-14, 14), expand=True, resample=Image.BICUBIC)
        img.paste(char_img, (5 + i * 28, random.randint(-2, 4)), char_img)

    for _ in range(2):
        x1, y1 = random.randint(0, w // 3), random.randint(4, h - 4)
        x2, y2 = random.randint(w // 2, w - 1), random.randint(4, h - 4)
        draw.arc([x1 - 20, y1 - 30, x2 + 20, y2 + 30], start=random.randint(0, 90),
                 end=random.randint(180, 270), fill=(130, 145, 185), width=2)
    draw.line([(0, random.randint(8, h - 8)), (w, random.randint(8, h - 8))],
              fill=(140, 155, 190), width=1)
    for _ in range(110):
        draw.point((random.randint(0, w - 1), random.randint(0, h - 1)),
                   fill=(150, 160, 190) if random.random() < 0.7 else (90, 100, 130))

    buf = io.BytesIO()
    img.save(buf, "PNG")
    return buf.getvalue(), text


LOGIN_PAGE = """<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>校园网认证门户 v2</title>
<style>
  body { font-family: "Segoe UI", "Microsoft YaHei", sans-serif; background: linear-gradient(135deg,#e8eef7,#f2f0f7);
         display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; }
  .card { background: #fff; border-radius: 14px; box-shadow: 0 6px 30px rgba(20,30,60,.10); padding: 38px 42px; width: 360px; }
  h1 { font-size: 21px; margin: 0 0 6px; color: #1d2b45; text-align: center; }
  .sub { font-size: 12px; color: #8a93a6; text-align: center; margin-bottom: 24px; }
  .row { margin-bottom: 15px; }
  label { display: block; font-size: 13px; color: #556; margin-bottom: 6px; }
  input[type=text], input[type=password] { width: 100%; box-sizing: border-box; padding: 10px 11px;
         border: 1px solid #ccd3e0; border-radius: 7px; font-size: 14px; }
  .captcha-row { display: flex; gap: 10px; align-items: center; }
  .captcha-row input { flex: 1; }
  #captcha-img { width: 150px; height: 46px; border-radius: 6px; border: 1px solid #dde; cursor: pointer; }
  #login-btn { width: 100%; padding: 12px; border: 0; border-radius: 7px; background: #2b5fa8;
               color: #fff; font-size: 15px; cursor: pointer; margin-top: 4px; }
  .error { margin-top: 14px; text-align: center; font-size: 13px; color: #c0392b; min-height: 18px; }
</style>
</head>
<body>
<div class="card">
  <h1>校园网认证门户 v2</h1>
  <div class="sub">Campus Network Authentication Portal</div>
  <div class="row"><label>账号</label><input id="username" name="username" type="text" placeholder="学号 / 工号"></div>
  <div class="row"><label>密码</label><input id="password" name="password" type="password" placeholder="密码"></div>
  <div class="row"><label>验证码</label>
    <div class="captcha-row">
      <input id="captcha-input" name="captcha" type="text" placeholder="输入右侧 5 位数字" maxlength="5">
      <img id="captcha-img" src="/captcha" title="点击刷新"
           onclick="this.src='/captcha?ts='+Date.now(); document.getElementById('captcha-input').value='';">
    </div>
  </div>
  <button id="login-btn" onclick="doLogin()">登 录</button>
  <div class="error" id="error">__ERROR__</div>
</div>
<script>
  // 传统 form 语义：JS 组装表单后原生提交（POST → 302 跳转链 → /portal）
  function doLogin() {
    const f = document.createElement("form");
    f.method = "POST"; f.action = "/login";
    for (const [n, v] of [["username", document.getElementById("username").value],
                          ["password", document.getElementById("password").value],
                          ["captcha", document.getElementById("captcha-input").value]]) {
      const i = document.createElement("input"); i.type = "hidden"; i.name = n; i.value = v; f.appendChild(i);
    }
    document.body.appendChild(f); f.submit();
  }
  document.getElementById("password").addEventListener("keydown", e => { if (e.key === "Enter") doLogin(); });
</script>
</body></html>"""

PORTAL_PAGE = """<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>在线 - 校园网 v2</title>
<style>body { font-family: "Segoe UI", "Microsoft YaHei", sans-serif; background: #eef6f0; display: flex;
justify-content: center; align-items: center; min-height: 100vh; margin: 0; }
.card { background: #fff; border-radius: 14px; padding: 42px 58px; text-align: center; box-shadow: 0 6px 30px rgba(20,30,60,.10); }
h1 { color: #1d5c38; font-size: 22px; margin: 0 0 14px; }
.meta { font-size: 14px; color: #456; line-height: 1.9; margin: 0 0 18px; }
a { color: #2b5fa8; }</style></head>
<body><div class="card">
<h1>登录成功，当前已在线</h1>
<div class="meta" id="meta">加载中…</div>
<a href="/logout">模拟掉线（退出登录）</a>
</div>
<script>
fetch("/api/me").then(r => r.json()).then(j => {
  document.getElementById("meta").innerHTML =
    "账号: " + j.username + "<br>IP: " + j.ip + "<br>上线时间: " + j.since +
    "<br>本次流量: " + j.rx_mb + " / " + j.tx_mb + " MB";
});
</script></body></html>"""


def log_line(msg):
    with lock:
        state["log"].append(f"[{time.strftime('%H:%M:%S')}] {msg}")
        state["log"] = state["log"][-60:]


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="application/json; charset=utf-8", extra=None):
        data = body if isinstance(body, bytes) else body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def _json(self, obj, code=200):
        self._send(code, json.dumps(obj, ensure_ascii=False))

    def _redirect(self, location):
        self._send(302, b"", "text/plain", {"Location": location})

    # ---------------- GET ----------------
    def do_GET(self):
        u = urlparse(self.path)
        path, qs = u.path, parse_qs(u.query)

        if path in ("/", "/login.html"):
            err = qs.get("error", [""])[0]
            msgs = {"captcha": "验证码错误", "cred": "账号或密码错误",
                    "banned": "尝试过于频繁，请稍后再试", "kicked": "您已掉线，请重新登录"}
            self._send(200, LOGIN_PAGE.replace("__ERROR__", msgs.get(err, "")),
                       "text/html; charset=utf-8")
        elif path == "/captcha":
            png, text = gen_captcha()
            with lock:
                state["captcha_text"] = text
            self._send(200, png, "image/png")
        elif path == "/status":
            with lock:
                ban_left = max(0, round(state["ban_until"] - time.time()))
                self._json({"authenticated": state["authenticated"],
                            "username": state["username"],
                            "last_login_time": state["last_login_time"],
                            "login_count": state["login_count"],
                            "kick_count": state["kick_count"],
                            "fail_remaining": state["fail_remaining"],
                            "slow_ms": state["slow_ms"],
                            "ban_until": state["ban_until"],
                            "ban_remaining_s": ban_left,
                            "captcha_text": state["captcha_text"],
                            "log": list(state["log"])})
        elif path == "/api/me":
            with lock:
                ok, user, ip, since = (state["authenticated"], state["username"],
                                       state["login_ip"], state["last_login_time"])
            if not ok:
                self._json({"error": "not authenticated"}, 401)
                return
            seed = sum(ord(c) for c in (user or ""))
            self._json({"username": user, "ip": ip, "since": since,
                        "rx_mb": round((seed * 7) % 900 / 3.3, 1),
                        "tx_mb": round((seed * 3) % 400 / 4.7, 1)})
        elif path == "/portal":
            with lock:
                ok = state["authenticated"]
            if ok:
                self._send(200, PORTAL_PAGE, "text/html; charset=utf-8")
            else:
                self._redirect("/?error=kicked")
        elif path == "/debug":
            self._send(200, "<pre>" + json.dumps(
                {k: state[k] for k in ("authenticated", "username", "login_count", "log")},
                ensure_ascii=False) + "</pre>", "text/html; charset=utf-8")
        elif path == "/generate_204":
            with lock:
                ok = state["authenticated"]
            if ok:
                self._send(204, b"")
            else:
                self._send(200, "captive portal: please login (v2)", "text/plain; charset=utf-8")
        elif path == "/hotspot-detect.html":
            with lock:
                ok = state["authenticated"]
            self._send(200, "Success" if ok else "captive", "text/html; charset=utf-8")
        elif path == "/step1":
            self._redirect("/step2?token=" + secrets.token_hex(8))
        elif path == "/step2":
            self._redirect("/portal?welcome=1")
        elif path == "/redirect-loopback":
            self._redirect(f"http://127.0.0.1:{PORT}/loopback-target")
        elif path == "/redirect-loopback2":
            self._redirect(f"http://127.0.0.2:{PORT}/loopback-target")
        elif path == "/redirect-zero":
            self._redirect("http://0.0.0.0:1/zero")
        elif path == "/loopback-target":
            self._send(200, "you should not be here (loopback follow)", "text/plain; charset=utf-8")
        elif path in ("/logout", "/kick"):
            self._do_logout(origin="kick" if path == "/kick" else "logout")
        else:
            self._json({"error": "not found"}, 404)

    # ---------------- POST ----------------
    def do_POST(self):
        u = urlparse(self.path)
        path, qs = u.path, parse_qs(u.query)

        if path in ("/logout", "/kick"):
            self._do_logout(origin="kick" if path == "/kick" else "logout")
            return
        if path == "/failonce":
            with lock:
                state["failonce"] = True
            log_line("injected: failonce（下次验证码错）")
            self._json({"ok": True, "message": "下一次登录将返回验证码错误"})
            return
        if path == "/failntimes":
            n = int(qs.get("n", ["1"])[0])
            with lock:
                state["fail_remaining"] = n
            log_line(f"injected: 账密连错 {n} 次")
            self._json({"ok": True, "message": f"接下来 {n} 次登录将账密错误"})
            return
        if path == "/slowlogin":
            ms = int(qs.get("ms", ["3000"])[0])
            with lock:
                state["slow_ms"] = ms
            log_line(f"injected: 登录延迟 {ms}ms")
            self._json({"ok": True, "message": f"登录响应将延迟 {ms}ms"})
            return
        if path == "/ban":
            seconds = int(qs.get("seconds", ["30"])[0])
            with lock:
                state["ban_until"] = time.time() + seconds
            log_line(f"injected: 封禁 {seconds}s")
            self._json({"ok": True, "message": f"已封禁 {seconds}s"})
            return
        if path != "/login":
            self._json({"error": "not found"}, 404)
            return

        with lock:
            slow_ms = state["slow_ms"]
        if slow_ms:
            time.sleep(slow_ms / 1000)

        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length or 0) or b""
        ctype = self.headers.get("Content-Type", "")
        if "application/json" in ctype:
            try:
                payload = json.loads(raw or b"{}")
            except json.JSONDecodeError:
                self._redirect("/?error=cred")
                return
        else:
            payload = {k: v[0] for k, v in parse_qs(raw.decode("utf-8", "replace")).items()}

        username = str(payload.get("username") or "").strip()
        password = str(payload.get("password") or "").strip()
        captcha = str(payload.get("captcha") or "").strip()
        client_ip = self.client_address[0]

        with lock:
            expected = state["captcha_text"]
            failonce = state["failonce"]
            state["failonce"] = False
            fail_remaining = state["fail_remaining"]
            banned = time.time() < state["ban_until"]
            state["slow_ms"] = 0  # 延迟单次生效

        log_line(f"login attempt: user={username!r} captcha={captcha!r} expected={expected!r} ip={client_ip}")

        if banned:
            self._redirect("/?error=banned")
            return
        # 校验顺序与真实门户一致：先验证码，后账密
        if failonce or not captcha or captcha != expected:
            self._redirect("/?error=captcha")
            return
        if fail_remaining > 0:
            with lock:
                state["fail_remaining"] -= 1
            log_line(f"login rejected（注入连错，剩余 {state['fail_remaining']}）")
            self._redirect("/?error=cred")
            return
        if VALID_USERS.get(username) != password:
            self._redirect("/?error=cred")
            return

        sid = secrets.token_hex(12)
        with lock:
            state.update(authenticated=True, username=username, session_id=sid,
                         login_ip=client_ip,
                         last_login_time=time.strftime("%Y-%m-%d %H:%M:%S"))
            state["login_count"] += 1
        log_line(f"login success: {username} (sid={sid[:8]}…, 第 {state['login_count']} 次)")
        # 登录成功 → 302 跳转链（step1 → step2 → /portal）
        self._send(302, b"", "text/plain",
                   {"Location": "/step1", "Set-Cookie": f"sid={sid}; Path=/; HttpOnly"})

    def _do_logout(self, origin):
        with lock:
            who = state["username"]
            state.update(authenticated=False, username=None, session_id=None)
            state["kick_count"] += 1
        log_line(f"{origin}: {who} 已掉线")
        if origin == "kick":
            self._json({"ok": True, "message": "已强制掉线（模拟被踢）"})
        else:
            self._redirect("/?error=kicked")


PORT = 18999


def main():
    import argparse
    global PORT
    ap = argparse.ArgumentParser(description="复杂模拟校园网认证门户 v2（e2e 用）")
    ap.add_argument("--port", type=int, default=PORT)
    PORT = ap.parse_args().port
    random.seed()
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"mock portal v2 listening on http://127.0.0.1:{PORT}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
