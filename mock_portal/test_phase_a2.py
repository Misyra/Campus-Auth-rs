# -*- coding: utf-8 -*-
"""阶段A-2：TCP/URL 探测 + Profile 管理"""
import json
import time
import urllib.request

import os

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _load_token() -> str:
    """读取本地实例 token（target/debug/config/.auth_token，随实例轮换，不入库）。"""
    try:
        with open(os.path.join(_REPO_ROOT, "target", "debug", "config", ".auth_token"),
                  encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return ""


TOKEN = _load_token()
BASE = "http://127.0.0.1:50721"


def api(path, method="GET", body=None, timeout=20):
    req = urllib.request.Request(
        BASE + path, method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"X-Auth-Token": TOKEN, "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": e.read().decode("utf-8", "replace")[:300]}


print("=== T4 TCP/URL 探测 ===")
# TCP: 18765 开放 + 端口 9 几乎必然关闭（race 语义观察）；URL: /status 内容含 authenticated
patch = {
    "monitor": {
        "check_interval_seconds": 10,
        "test_urls": ["http://127.0.0.1:18765/generate_204"],
        "enable_http_check": True,
        "enable_tcp_check": True,
        "ping_targets": ["127.0.0.1:18765", "127.0.0.1:9"],
        "url_check_urls": ["http://127.0.0.1:18765/status|authenticated"],
        "network_check_timeout": 5,
        "enable_local_check": False,
    }
}
r = api("/api/config", "PATCH", patch)
print("patch:", "ok" if "data" in r else r)
time.sleep(25)  # 等两个探测周期
m = api("/api/monitor/status")["data"]
print("net:", m["network_status"], "| probes:", m["probe_total"])

print()
print("=== T5 Profile 管理 ===")
print("create:", api("/api/profiles/test2", "POST", {
    "name": "备用网络", "username": "e2euser", "password": "e2epass",
    "auth_url": "http://127.0.0.1:18765/"}))
print("list:", [p["id"] for p in api("/api/profiles")["data"]])
print("switch:", api("/api/profiles/switch", "POST", {"id": "test2"}))
cfg = api("/api/config")["data"]
print("active now:", cfg.get("active_profile_id"), "| auth_url:", cfg.get("auth_url"), "| user:", cfg.get("username"))
print("detect:", json.dumps(api("/api/profiles/detect", "POST", {}), ensure_ascii=False)[:250])
print("auto-switch:", json.dumps(api("/api/profiles/auto-switch", "POST", {}), ensure_ascii=False)[:250])
print("switch back:", api("/api/profiles/switch", "POST", {"id": "default"}))
cfg = api("/api/config")["data"]
print("active restored:", cfg.get("active_profile_id"), "| user:", cfg.get("username"))
# 清理：删除 test2（default 不可删）
print("delete test2:", api("/api/profiles/test2", "DELETE"))
print("list after:", [p["id"] for p in api("/api/profiles")["data"]])
