# -*- coding: utf-8 -*-
"""阶段A-3：OCR 接口 / debug 会话 / background SSRF / 日志级别 / 自启 / 系统信息"""
import base64
import json
import time
import urllib.request
import uuid

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
MOCK = "http://127.0.0.1:18765"


def api(path, method="GET", body=None, timeout=90, ct="application/json"):
    data = json.dumps(body).encode() if (body is not None and ct == "application/json") else body
    req = urllib.request.Request(BASE + path, method=method, data=data,
        headers={"X-Auth-Token": TOKEN, "Content-Type": ct} if ct else {"X-Auth-Token": TOKEN})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            try:
                return json.loads(raw)
            except Exception:
                return {"raw_bytes": len(raw), "status": r.status}
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": e.read().decode("utf-8", "replace")[:250]}


def mock_api(path):
    with urllib.request.urlopen(MOCK + path, timeout=5) as r:
        return r.read()


print("=== T10 OCR 识别接口 ===")
png = mock_api("/captcha")
text = json.loads(mock_api("/status"))["captcha_text"]
r = api("/api/ocr/recognize", "POST", {"image_base64": base64.b64encode(png).decode()})
print(f"expected={text} ->", json.dumps(r, ensure_ascii=False)[:250])

print()
print("=== T11 debug 会话（截图负载形状定案）===")
r = api("/api/debug/start", "POST", {"task_id": "mock-ocr-login"})
print("start:", json.dumps(r, ensure_ascii=False)[:300])
time.sleep(2)
r2 = api("/api/debug/step", "POST", {})
print("step1:", json.dumps(r2, ensure_ascii=False)[:300])
api("/api/debug/stop", "POST", {})
print("stop: sent")

print()
print("=== T12 background 上传 + SSRF 拒绝 ===")
boundary = uuid.uuid4().hex
png2 = mock_api("/captcha")
mp = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"t.png\"\r\n"
      f"Content-Type: image/png\r\n\r\n").encode() + png2 + f"\r\n--{boundary}--\r\n".encode()
r = api("/api/background/upload", "POST", mp, ct=f"multipart/form-data; boundary={boundary}")
print("upload:", json.dumps(r, ensure_ascii=False)[:200])
r = api("/api/background/fetch-url", "POST", {"url": "http://127.0.0.1:18765/captcha"})
print("fetch-url loopback:", json.dumps(r, ensure_ascii=False)[:220])

print()
print("=== T13 日志级别动态调整 ===")
print("set DEBUG:", api("/api/config/log-level", "PUT", {"level": "DEBUG"}))
time.sleep(1)
print("set INFO:", api("/api/config/log-level", "PUT", {"level": "INFO"}))

print()
print("=== T14 自启动开关 ===")
print("status:", json.dumps(api("/api/autostart/status"), ensure_ascii=False)[:120])
print("enable:", json.dumps(api("/api/autostart/enable", "POST", {}), ensure_ascii=False)[:120])
print("after enable:", json.dumps(api("/api/autostart/status"), ensure_ascii=False)[:120])
print("disable:", json.dumps(api("/api/autostart/disable", "POST", {}), ensure_ascii=False)[:120])
print("after disable:", json.dumps(api("/api/autostart/status"), ensure_ascii=False)[:120])

print()
print("=== T15 系统信息 / 日志 API ===")
print("system/info:", json.dumps(api("/api/system/info"), ensure_ascii=False)[:250])
logs = api("/api/logs?lines=5")
print("logs:", ("ok, " + str(len(json.dumps(logs))) + " bytes") if isinstance(logs, dict) else logs)
