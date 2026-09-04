# -*- coding: utf-8 -*-
"""轮询登录状态直到结束，输出摘要 + mock 服务器状态"""
import json
import sys
import time
import urllib.request

from _common import BASE, TOKEN, preflight

preflight()


def api(path, method="GET", body=None):
    req = urllib.request.Request(
        BASE + path,
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"X-Auth-Token": TOKEN, "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return {"error": e.code, "body": e.read().decode("utf-8", "replace")}


def mock_status():
    with urllib.request.urlopen("http://127.0.0.1:18765/status", timeout=5) as r:
        return json.loads(r.read())


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "wait"
    if cmd == "login":
        print("trigger:", api("/api/login", "POST", {}))
    s = api("/api/login/status")["data"]
    for i in range(30):
        s = api("/api/login/status")["data"]
        print(f"[{i}] {s['login_status']} net={s['network_status']} msg={s['login_message']!r} "
              f"worker={s['worker_state']} retry={s['retry_count']}")
        if s["login_status"] in ("success", "failed", "idle"):
            break
        time.sleep(6)
    m = mock_status()
    print("mock authenticated:", m["authenticated"], "user:", m["username"],
          "count:", m["login_count"])
    print("mock log:")
    for line in m["log"][-5:]:
        print("  ", line)
