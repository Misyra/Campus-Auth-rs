#!/usr/bin/env python3
"""eportal（Dr.COM）门户 mock：复刻 Desktop/Rare/login.sh 的 XOR 加密登录协议。

协议契约（严格对齐 login.sh）：
- 密钥 = 客户端来源 IP 字符串各字符 ASCII 的 XOR 累积（脚本里用 WAN 接口 IP，
  门户侧即 TCP 来源 IP，两者在真实校园网中相同）
- 每个字段逐字符 ord(c) ^ key，再 %02x 十六进制编码
- GET /eportal/portal/login?...&encrypt=1&v=2576
- user_account 明文形如 ",0,{账号}{ISP}"；wlan_user_ip 为加密后的本机 IP
- 成功响应含 dr1003

仅用于离线验证「我们的直连渠道能否复现该门户登录」，不参与 CI。
"""

import http.server
import socketserver
import sys
import urllib.parse

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18099
EXPECT_USER = "20230001"
EXPECT_PWD = "secret123"
EXPECT_ISP = "@cmcc"


def xor_key(ip: str) -> int:
    k = 0
    for ch in ip:
        k ^= ord(ch)
    return k


def enc(s: str, k: int) -> str:
    return "".join("%02x" % (ord(c) ^ k) for c in s)


def dec(h: str, k: int):
    if len(h) % 2:
        return None, "hex 长度非偶数"
    out = []
    for i in range(0, len(h), 2):
        try:
            b = int(h[i : i + 2], 16)
        except ValueError:
            return None, "非法 hex"
        out.append(chr(b ^ k))
    return "".join(out), None


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _send(self, body: bytes, ctype="text/html; charset=utf-8"):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        qs = urllib.parse.parse_qs(u.query)
        src_ip = self.client_address[0]
        k = xor_key(src_ip)

        # 登录页：把来源 IP 写进 HTML（供 ctx.page 提取的可行性验证）
        if u.path in ("/", "/index.jsp"):
            self._send(f"<html><body>wlan_user_ip={src_ip}</body></html>".encode())
            return

        if u.path != "/eportal/portal/login":
            self._send('{"result":0,"msg":"未知路径"}'.encode())
            return

        def dec_param(name):
            raw = qs.get(name, [""])[0]
            if raw == "":
                return ""
            s, err = dec(raw, k)
            return None if err else s

        account = dec_param("user_account")
        password = dec_param("user_password")
        wip = dec_param("wlan_user_ip")
        method = qs.get("login_method", [""])[0]
        encrypt = qs.get("encrypt", [""])[0]

        print(
            f"[mock] src={src_ip} key={k} enc={encrypt} method={method!r} "
            f"account={account!r} pwd={password!r} wip={wip!r}",
            flush=True,
        )

        if account is None or password is None:
            resp = 'dr1003({"result":0,"msg":"字段解密失败"})'
        elif encrypt != "1":
            resp = 'dr1003({"result":0,"msg":"未启用加密"})'
        elif wip != src_ip:
            # 真实门户要求 wlan_user_ip 与来源 IP 一致（防串号）
            resp = f'dr1003({{"result":0,"msg":"IP 不匹配({wip}!={src_ip})"}})'
        elif account == f",0,{EXPECT_USER}{EXPECT_ISP}" and password == EXPECT_PWD:
            resp = 'dr1003({"result":1,"msg":"认证成功"})'
        else:
            resp = 'dr1003({"result":0,"msg":"账号或密码错误"})'

        self._send(resp.encode())


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    print(f"[mock] eportal XOR 门户监听 0.0.0.0:{PORT}", flush=True)
    with Server(("0.0.0.0", PORT), Handler) as httpd:
        httpd.serve_forever()
