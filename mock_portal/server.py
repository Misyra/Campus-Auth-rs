# -*- coding: utf-8 -*-
"""模拟校园网认证门户（本地 e2e 用）

提供带图片验证码的登录页，模拟真实校园网的 captive portal 语义：
- 未认证时 GET /generate_204 返回 200（内容页 → 客户端判定"被 captive"）
- 认证成功后 GET /generate_204 返回 204（→ 客户端判定"已在线"）

端点：
  GET  /                登录页（AJAX 提交，#result 显示结果）
  GET  /captcha         验证码 PNG（4 位数字，服务器记录当前文本）
  POST /login           JSON {username,password,captcha}，校验并切换认证状态
  POST|GET /logout      模拟掉线/被踢
  GET  /status          JSON {authenticated, username, captcha_text(调试), log}
  GET  /portal          已登录状态页
  GET  /generate_204    认证语义探测端点

有效账号：testuser/testpass、e2euser/e2epass、admin/admin123
调试辅助：POST /failonce  让下一次登录返回"验证码错误"（测试重试链路）
"""
import io
import json
import random
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from PIL import Image, ImageDraw, ImageFont

PORT = 18765
VALID_USERS = {
    "testuser": "testpass",
    "e2euser": "e2epass",
    "admin": "admin123",
}

lock = threading.Lock()
state = {
    "authenticated": False,
    "username": None,
    "last_login_time": None,
    "login_count": 0,
    "captcha_text": "",
    "failonce": False,
    "log": [],
}

FONT_CANDIDATES = [
    r"C:\Windows\Fonts\arialbd.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    r"C:\Windows\Fonts\segoeui.ttf",
]


def _font(size):
    for path in FONT_CANDIDATES:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


def gen_captcha():
    """绘制 4 位数字验证码，返回 (png_bytes, text)"""
    text = "".join(random.choices("0123456789", k=4))
    w, h = 132, 44
    img = Image.new("RGB", (w, h), (245, 247, 250))
    draw = ImageDraw.Draw(img)
    font = _font(34)

    # 每个字符轻微旋转后粘贴
    for i, ch in enumerate(text):
        char_img = Image.new("RGBA", (40, 48), (0, 0, 0, 0))
        cdraw = ImageDraw.Draw(char_img)
        cdraw.text((4, 2), ch, font=font, fill=(30 + random.randint(0, 60), 40, 90 + random.randint(0, 60)))
        char_img = char_img.rotate(random.uniform(-12, 12), expand=True, resample=Image.BICUBIC)
        img.paste(char_img, (6 + i * 30, random.randint(-2, 4)), char_img)

    # 干扰线与噪点（轻度，保证 ddddocr 可识别）
    for _ in range(3):
        x1, y1 = random.randint(0, w // 3), random.randint(0, h)
        x2, y2 = random.randint(w // 2, w), random.randint(0, h)
        draw.line([(x1, y1), (x2, y2)], fill=(120, 140, 180), width=2)
    for _ in range(60):
        draw.point((random.randint(0, w - 1), random.randint(0, h - 1)), fill=(150, 160, 190))

    buf = io.BytesIO()
    img.save(buf, "PNG")
    return buf.getvalue(), text


PAGE = """<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>模拟校园网认证</title>
<style>
  body { font-family: "Segoe UI", "Microsoft YaHei", sans-serif; background: #eef1f6; display: flex;
         justify-content: center; align-items: center; min-height: 100vh; margin: 0; }
  .card { background: #fff; border-radius: 12px; box-shadow: 0 4px 24px rgba(0,0,0,.08); padding: 36px 40px; width: 340px; }
  h1 { font-size: 20px; margin: 0 0 22px; color: #233; text-align: center; }
  .row { margin-bottom: 16px; }
  label { display: block; font-size: 13px; color: #567; margin-bottom: 6px; }
  input[type=text], input[type=password] { width: 100%; box-sizing: border-box; padding: 9px 10px;
         border: 1px solid #ccd; border-radius: 6px; font-size: 14px; }
  .captcha-row { display: flex; gap: 10px; align-items: center; }
  .captcha-row input { flex: 1; }
  #captcha-img { width: 132px; height: 44px; border-radius: 6px; border: 1px solid #dde; cursor: pointer; }
  #login-btn { width: 100%; padding: 11px; border: 0; border-radius: 6px; background: #2b6cb0;
               color: #fff; font-size: 15px; cursor: pointer; margin-top: 6px; }
  #login-btn:hover { background: #2c5282; }
  #result { margin-top: 16px; text-align: center; font-size: 14px; min-height: 20px; }
  #result.ok { color: #2f855a; }
  #result.err { color: #c53030; }
</style>
</head>
<body>
<div class="card">
  <h1>模拟校园网认证</h1>
  <div class="row"><label>账号</label><input id="username" name="username" type="text" placeholder="学号 / 工号"></div>
  <div class="row"><label>密码</label><input id="password" name="password" type="password" placeholder="密码"></div>
  <div class="row"><label>验证码</label>
    <div class="captcha-row">
      <input id="captcha-input" name="captcha" type="text" placeholder="输入右侧数字" maxlength="4">
      <img id="captcha-img" src="/captcha" title="点击刷新">
    </div>
  </div>
  <button id="login-btn">登 录</button>
  <div id="result"></div>
</div>
<script>
  const $ = (id) => document.getElementById(id);
  const refreshCaptcha = () => { $("captcha-img").src = "/captcha?ts=" + Date.now(); $("captcha-input").value = ""; };
  $("captcha-img").addEventListener("click", refreshCaptcha);
  $("login-btn").addEventListener("click", async () => {
    $("login-btn").disabled = true;
    try {
      const r = await fetch("/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          username: $("username").value.trim(),
          password: $("password").value,
          captcha: $("captcha-input").value.trim(),
        }),
      });
      const j = await r.json();
      $("result").textContent = j.message;
      $("result").className = j.ok ? "ok" : "err";
      if (j.ok) { window.location.href = "/portal"; } else { refreshCaptcha(); }
    } catch (e) {
      $("result").textContent = "网络错误: " + e;
      $("result").className = "err";
    } finally { $("login-btn").disabled = false; }
  });
</script>
</body>
</html>"""

PORTAL = """<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>在线 - 模拟校园网</title>
<style>body { font-family: "Segoe UI", "Microsoft YaHei", sans-serif; background: #eef6f0; display: flex;
justify-content: center; align-items: center; min-height: 100vh; margin: 0; }
.card { background: #fff; border-radius: 12px; padding: 40px 56px; text-align: center; box-shadow: 0 4px 24px rgba(0,0,0,.08);}
#result { font-size: 22px; color: #2f855a; margin: 0 0 10px; }</style></head>
<body><div class="card"><p id="result">登录成功，当前已在线</p>
<p id="user"></p><a href="/logout">模拟掉线（退出登录）</a></div>
<script>fetch("/status").then(r => r.json()).then(j => { document.getElementById("user").textContent = "账号: " + (j.username || "-"); });</script>
</body></html>"""


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

    def _log_line(self, msg):
        with lock:
            state["log"].append(f"[{time.strftime('%H:%M:%S')}] {msg}")
            state["log"] = state["log"][-50:]

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/login.html"):
            self._send(200, PAGE, "text/html; charset=utf-8")
        elif path == "/captcha":
            png, text = gen_captcha()
            with lock:
                state["captcha_text"] = text
            self._send(200, png, "image/png")
        elif path == "/status":
            with lock:
                self._json({k: state[k] for k in
                            ("authenticated", "username", "last_login_time", "login_count", "captcha_text", "log")})
        elif path == "/portal":
            with lock:
                ok = state["authenticated"]
            self._send(200, PORTAL if ok else '<meta charset="utf-8"><p id="result">未登录</p><a href="/">去登录</a>',
                       "text/html; charset=utf-8")
        elif path == "/generate_204":
            with lock:
                ok = state["authenticated"]
            if ok:
                self._send(204, b"")
            else:
                self._send(200, "captive portal: please login", "text/plain; charset=utf-8")
        elif path == "/logout":
            self._do_logout()
        else:
            self._json({"error": "not found"}, 404)

    def _do_logout(self):
        with lock:
            who = state["username"]
            state["authenticated"] = False
            state["username"] = None
        self._log_line(f"logout: {who} 已掉线")
        self._json({"ok": True, "message": "已退出登录（模拟掉线）"})

    def do_POST(self):
        path = self.path.split("?")[0]
        if path == "/logout":
            self._do_logout()
            return
        if path == "/failonce":
            with lock:
                state["failonce"] = True
            self._json({"ok": True, "message": "下一次登录将返回验证码错误"})
            return
        if path != "/login":
            self._json({"error": "not found"}, 404)
            return
        length = int(self.headers.get("Content-Length") or 0)
        try:
            payload = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            self._json({"ok": False, "message": "请求格式错误"}, 400)
            return

        username = str(payload.get("username") or "")
        password = str(payload.get("password") or "")
        captcha = str(payload.get("captcha") or "").strip()

        with lock:
            expected = state["captcha_text"]
            failonce = state["failonce"]
            state["failonce"] = False

        self._log_line(f"login attempt: user={username!r} captcha={captcha!r} expected={expected!r}")

        # 校验顺序与真实门户一致：先验证码，后账密
        if failonce or not captcha or captcha != expected:
            self._json({"ok": False, "message": "验证码错误"})
            return
        if VALID_USERS.get(username) != password:
            self._json({"ok": False, "message": "账号或密码错误"})
            return

        with lock:
            state["authenticated"] = True
            state["username"] = username
            state["last_login_time"] = time.strftime("%Y-%m-%d %H:%M:%S")
            state["login_count"] += 1
        self._log_line(f"login success: {username}")
        self._json({"ok": True, "message": "登录成功"})


def main():
    random.seed()
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"mock portal listening on http://127.0.0.1:{PORT}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
