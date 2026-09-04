# -*- coding: utf-8 -*-
"""阶段A-登录分支测试：验证码错误重试 / 取消登录 / login_once"""
import json
import time
import urllib.request

from _common import BASE, MOCK, TOKEN, preflight

preflight()


def api(path, method="GET", body=None, base=BASE, timeout=20):
    req = urllib.request.Request(
        base + path, method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"X-Auth-Token": TOKEN, "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": e.read().decode("utf-8", "replace")[:200]}
    except TimeoutError:
        return {"trigger_timeout": True}  # /api/login 同步等结果，超时只说明还在跑
    except OSError as e:
        return {"connection_error": str(e)[:200]}


def login_status():
    return api("/api/login/status")["data"]


def wait_login(timeout=120):
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = login_status()
        if s["login_status"] in ("success", "failed", "idle"):
            return s
        time.sleep(4)
    return login_status()


def mock_log_tail(n=6):
    with urllib.request.urlopen(MOCK + "/status", timeout=5) as r:
        return json.loads(r.read())


print("=== T1 验证码错误重试（/failonce）===")
api("/failonce", "POST", {}, base=MOCK)
print("trigger:", api("/api/login", "POST", {}))
s = wait_login()
print("result:", s["login_status"], "| msg:", s["login_message"], "| retry:", s["retry_count"])
m = mock_log_tail()
print("mock log tail:")
for line in m["log"][-3:]:
    print("  ", line)

print()
print("=== T2 取消登录（重试等待期 cancel）===")
api("/failonce", "POST", {}, base=MOCK)
api("/failonce", "POST", {}, base=MOCK)
api("/api/login", "POST", {})
time.sleep(5)  # 第一次尝试失败后进入重试间隔
print("mid status:", login_status()["login_status"], login_status()["login_message"])
print("cancel:", api("/api/login/cancel", "POST", {}))
time.sleep(2)
s = wait_login(timeout=40)
print("result:", s["login_status"], "| msg:", s["login_message"])
m = mock_log_tail()
print("mock authenticated:", m["authenticated"])

print()
print("=== T3 login/once ===")
print("trigger:", api("/api/login/once", "POST", {}))
s = wait_login()
print("result:", s["login_status"], "| msg:", s["login_message"], "| src:", s["login_source"])
h = api("/api/history")["data"]
if isinstance(h, list) and h:
    print("history[0]:", json.dumps(h[0], ensure_ascii=False)[:200])
