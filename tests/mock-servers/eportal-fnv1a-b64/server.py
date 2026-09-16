#!/usr/bin/env python3
"""eportal 变体门户 mock：复刻 Desktop/Rare/login.sh 的登录协议框架，但更换加密算法。

与原版 login.sh（eportal-xor mock）的差异——加密算法从
「XOR 累积密钥 + 逐字符 ^key + 两位十六进制」换为
「FNV-1a 密钥 + 逐字节 ^key + Base64」：

  原版: key = XOR(ascii(c) for c in ip)；enc(s) = hex(ord(c) ^ key)
  本版: key = FNV1a_32(ip) & 0xFF；enc(s) = base64(bytes(b ^ key for b in s))

密钥协商说明：真实校园网中客户端（login.sh 的 WAN 接口 IP）与服务端（TCP 来源 IP）
看到的是同一个 IP。本地环回测试时二者不同（客户端主接口 192.168.x.x vs 回环
127.0.0.1），故支持 --accept-ip 显式指定"门户视角的客户端 IP"来对齐协商——
这正是真实门户按网卡入站方向天然完成的事。

协议契约其余保持同类（便于复用主程序直连渠道验证）：
- GET /eportal/portal/login?callback=dr1003&...&encrypt=1&v=2576（JSONP 响应恒 HTTP 200，
  成功/失败都要靠响应关键字判定——正是主程序「必须填成功+失败关键字」要防的门户形态）
- user_account 明文形如 ",0,{账号}{ISP}"；wlan_user_ip 为加密后的本机 IP
- 密钥由客户端来源 IP 推导（服务端按 TCP 对端地址解密，天然校验密钥协商一致性）
- 成功响应 dr1003({"result":1,...})；失败 result=0 且带中文 msg
启动：python server.py [port] [--accept-ip <ip>]   （默认 18101）
"""
import base64
import http.server
import socketserver
import sys
import urllib.parse

PORT = int(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else 18101
ACCEPT_IP = None
if "--accept-ip" in sys.argv:
    ACCEPT_IP = sys.argv[sys.argv.index("--accept-ip") + 1]
EXPECT_USER = "20230001"
EXPECT_PWD = "secret123"
EXPECT_ISP = "@cmcc"


def fnv1a32(data: bytes) -> int:
    """FNV-1a 32 位散列——替代原版的 XOR 累积做密钥推导"""
    h = 0x811C9DC5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def key_of(ip: str) -> int:
    # 取低 8 位作逐字节 XOR 密钥（0-255），与原版「单字节密钥」同量级
    return fnv1a32(ip.encode()) & 0xFF


def enc(s: str, k: int) -> str:
    raw = bytes((ord(c) ^ k) & 0xFF for c in s)
    return base64.b64encode(raw).decode()


def dec(b64: str, k: int):
    try:
        raw = base64.b64decode(b64, validate=True)
    except Exception:
        return None, "Base64 解码失败"
    # 逐字节 ^key 还原；非 ASCII 结果说明密钥不对（客户端用了别的算法/IP）
    out = []
    for b in raw:
        c = b ^ k
        if c > 0x10FFFF or (0xD800 <= c <= 0xDFFF):
            return None, f"解密结果非文本（key={k}）"
        out.append(chr(c))
    return "".join(out), None


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _send(self, body: str):
        data = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        qs = urllib.parse.parse_qs(u.query)
        src_ip = self.client_address[0]
        # 密钥协商视角：--accept-ip 模拟"门户按其网卡入站方向看到的客户端 IP"。
        # 真实校园网中入站 IP 即客户端 WAN IP（login.sh 的 get_ip 与之天然一致）；
        # 本地环回测试时客户端用主接口 IP 推导密钥，需用本参数对齐。
        key_ip = ACCEPT_IP or src_ip
        k = key_of(key_ip)

        # 登录页：写明本服务器的算法标识（页面前端脚本可用它对齐）
        if u.path in ("/", "/index.jsp"):
            self._send(
                "<html><body>"
                f"wlan_user_ip={src_ip}<!-- algo=fnv1a-xor-base64 key={k} -->"
                "</body></html>"
            )
            return

        if u.path != "/eportal/portal/login":
            self._send('{"result":0,"msg":"未知路径"}')
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
        encrypt = qs.get("encrypt", [""])[0]

        print(
            f"[mock] src={src_ip} key={k} encrypt={encrypt} path={u.path} "
            f"extra={ {kk: vv[0] for kk, vv in qs.items() if kk not in ('callback','login_method','user_account','user_password','wlan_user_ip','wlan_user_ipv6','wlan_user_mac','wlan_ac_ip','wlan_ac_name','jsVersion','terminal_type','lang','encrypt','v')} } "
            f"account={account!r} pwd={password!r} wip={wip!r}",
            flush=True,
        )

        # 判定顺序与原版一致：解密失败 → 未启用加密 → IP 防串号 → 凭据
        # IP 防串号基准同为 key_ip（真实门户拿"客户端声明的 IP"与入站方向比对；
        # 本地测试两者是不同地址，均对齐到 key_ip 视角）
        if account is None or password is None or wip is None:
            self._send('dr1003({"result":0,"msg":"字段解密失败，请确认加密算法"})')
        elif encrypt != "1":
            self._send('dr1003({"result":0,"msg":"未启用加密"})')
        elif wip != key_ip:
            self._send(f'dr1003({{"result":0,"msg":"IP 不匹配({wip}!={key_ip})"}})')
        elif account == f",0,{EXPECT_USER}{EXPECT_ISP}" and password == EXPECT_PWD:
            self._send('dr1003({"result":1,"msg":"认证成功"})')
        else:
            self._send('dr1003({"result":0,"msg":"账号或密码错误"})')

    def do_POST(self):
        # 注销对偶接口：直连渠道不使用；留作人工调试（同样要求加密字段）
        if urllib.parse.urlparse(self.path).path != "/eportal/portal/logout":
            self._send('{"result":0,"msg":"未知路径"}')
            return
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length).decode("utf-8", "replace")
        print(f"[mock] POST logout: {body!r}", flush=True)
        self._send('dr1003({"result":1,"msg":"已注销"})')


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    print(f"[mock] eportal 变体门户（FNV1a+XOR+Base64）监听 0.0.0.0:{PORT}", flush=True)
    with Server(("0.0.0.0", PORT), Handler) as httpd:
        httpd.serve_forever()
