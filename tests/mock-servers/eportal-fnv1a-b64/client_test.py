#!/usr/bin/env python3
"""服务端自测客户端：与 login.sh 同构（仅加密算法换成 FNV1a+XOR+Base64）。

用法：
  python client_test.py                 # 用内置期望凭据打 127.0.0.1:18101
  python client_test.py <user> <pwd> <isp> [base_url]
"""
import base64
import socket
import sys
import urllib.parse
import urllib.request


def fnv1a32(data: bytes) -> int:
    h = 0x811C9DC5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def key_of(ip: str) -> int:
    return fnv1a32(ip.encode()) & 0xFF


def enc(s: str, k: int) -> str:
    return base64.b64encode(bytes((ord(c) ^ k) & 0xFF for c in s)).decode()


def get_source_ip(target_host: str) -> str:
    """取「到 target_host 的路由」所用的本机源地址。

    与 login.sh 的 get_ip()（读 WAN 接口 IP）同目的：真实校园网中 WAN IP 即门户
    看到的 TCP 来源 IP。本机回环自测时 connect 到 127.0.0.1 会正确返回 127.0.0.1，
    避免「密钥用出口 IP、TCP 来源却是回环」的协商不一致（首轮自测踩到的坑）。
    """
    host = urllib.parse.urlparse(target_host if "//" in target_host else f"http://{target_host}").hostname
    addr = socket.gethostbyname(host)
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((addr, 80))
        return s.getsockname()[0]
    except Exception:
        return "127.0.0.1"
    finally:
        s.close()


def main() -> int:
    user = sys.argv[1] if len(sys.argv) > 1 else "20230001"
    pwd = sys.argv[2] if len(sys.argv) > 2 else "secret123"
    isp = sys.argv[3] if len(sys.argv) > 3 else "@cmcc"
    base = sys.argv[4] if len(sys.argv) > 4 else "http://127.0.0.1:18101"

    ip = get_source_ip(base)
    k = key_of(ip)
    print(f"[client] 目标={base} 源IP={ip} key={k}")

    account = f",0,{user}{isp}"
    url = (
        f"{base}/eportal/portal/login?"
        f"callback={enc('dr1003', k)}"
        f"&login_method={enc('1', k)}"
        f"&user_account={urllib.parse.quote(enc(account, k))}"
        f"&user_password={urllib.parse.quote(enc(pwd, k))}"
        f"&wlan_user_ip={urllib.parse.quote(enc(ip, k))}"
        f"&wlan_user_ipv6={enc('', k)}"
        f"&wlan_user_mac={enc('', k)}"
        f"&wlan_ac_ip={enc('', k)}"
        f"&wlan_ac_name={enc('', k)}"
        f"&jsVersion={enc('4.2.1', k)}"
        f"&terminal_type={enc('1', k)}"
        f"&lang={enc('zh-cn', k)}"
        f"&encrypt=1&v=2576"
    )
    print(f"[client] GET {url[:120]}…")
    with urllib.request.urlopen(url, timeout=10) as r:
        body = r.read().decode("utf-8")
    print(f"[client] 响应: {body}")

    ok = '"result":1' in body
    print(f"[client] {'SUCCESS: auth ok' if ok else 'FAIL: auth rejected'}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
