"""generate_204 captive 语义 mock（本地 e2e 用）。

监听 127.0.0.1:8767：GET /generate_204 恒以 302 跳转登录页（未认证语义），
其余路径回落 SimpleHTTPRequestHandler 静态文件服务（如 login.html）。
"""
import http.server, socketserver
class H(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/generate_204":
            self.send_response(302)
            self.send_header("Location", "http://127.0.0.1:8767/login.html")
            self.end_headers()
            return
        return super().do_GET()
    def log_message(self, *a, **k): pass
with socketserver.TCPServer(("127.0.0.1", 8767), H) as httpd:
    print("captive on 8767")
    httpd.serve_forever()
