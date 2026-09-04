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
