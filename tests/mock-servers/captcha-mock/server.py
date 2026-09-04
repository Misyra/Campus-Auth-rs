import http.server, urllib.parse, json, random
class H(http.server.BaseHTTPRequestHandler):
    CODE = "1234"
    def do_GET(self):
        if self.path.startswith("/generate_204"):
            self.send_response(302)
            self.send_header("Location", "/login.html")
            self.end_headers()
            return
        if self.path == "/login.html":
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(b'''<!doctype html><html><head><meta charset="utf-8"></head><body>
<h1>Captive Login</h1>
<form id="f" method="POST" action="/login">
<input name="username" placeholder="username">
<input name="password" type="password" placeholder="password">
<div style="margin:8px 0">
  <span>Captcha: <b id="code">1234</b></span>
  <input name="captcha" id="captcha" placeholder="captcha">
</div>
<button type="submit">Login</button>
</form>
<div id="result"></div>
<script>
document.getElementById("f").addEventListener("submit", async (e)=>{
  e.preventDefault();
  const fd = new FormData(e.target);
  const r = await fetch("/login", {method:"POST", body: fd});
  const t = await r.text();
  document.getElementById("result").innerText = t;
});
</script>
</body></html>''')
            return
        if self.path == "/success":
            self.send_response(200)
            self.send_header("Content-Type","text/plain")
            self.end_headers()
            self.wfile.write(b"success")
            return
        self.send_response(404); self.end_headers()
    def do_POST(self):
        if self.path == "/login":
            length = int(self.headers.get("Content-Length",0))
            body = self.rfile.read(length).decode(errors="ignore")
            qs = urllib.parse.parse_qs(body)
            u = qs.get("username",[""])[0]
            c = qs.get("captcha",[""])[0]
            self.send_response(200)
            self.send_header("Content-Type","text/plain; charset=utf-8")
            self.end_headers()
            if u=="testuser" and c==self.CODE:
                self.wfile.write("login success".encode())
            elif c != self.CODE:
                self.wfile.write(f"captcha error, got {c}".encode())
            else:
                self.wfile.write(b"auth failed")
            return
        self.send_response(404); self.end_headers()
    def log_message(self,*a,**k): pass

http.server.HTTPServer(("127.0.0.1",8767), H).serve_forever()
