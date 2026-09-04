# -*- coding: utf-8 -*-
"""手动 E2E 回归共享配置（非 CI 门禁，仅本地手动运行）。

- 地址与 token 均可经环境变量覆盖（CI/多实例并行时必需）：
  `CAMPUS_AUTH_BASE`（默认 http://127.0.0.1:50721）、
  `CAMPUS_AUTH_MOCK`（默认 http://127.0.0.1:18765）、
  `CAMPUS_AUTH_TOKEN`（优先于文件）、
  `CAMPUS_AUTH_BASE_PATH`（实例 base_path，用于定位 config/.auth_token）。
- `preflight()` 在实例或 mock 不可达时打印 SKIP 并 exit 0，
  避免无环境时抛 traceback；调用脚本应在首行执行它。
"""

import os
import sys
import urllib.error
import urllib.request

_LOOPBACK = {"127.0.0.1", "localhost", "::1"}


def _ensure_loopback_bypass() -> None:
    """回环直连：本机代理（如 127.0.0.1:7890）会劫持 urllib 的回环请求导致 502，
    此处把回环地址追加进 no_proxy（不覆盖用户已有值）。"""
    for _var in ("no_proxy", "NO_PROXY"):
        _have = {
            h.strip() for h in os.environ.get(_var, "").split(",") if h.strip()
        }
        if not _LOOPBACK <= _have:
            os.environ[_var] = ",".join(sorted(_have | _LOOPBACK))


_ensure_loopback_bypass()


BASE = os.environ.get("CAMPUS_AUTH_BASE", "http://127.0.0.1:50721")
MOCK = os.environ.get("CAMPUS_AUTH_MOCK", "http://127.0.0.1:18765")


def _repo_root() -> str:
    # 本文件位于 tests/mock-servers/full-portal/，仓库根为上三级
    return os.path.dirname(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    )


def load_token() -> str:
    """读取本地实例 token（随实例轮换，不入库）。"""
    if os.environ.get("CAMPUS_AUTH_TOKEN"):
        return os.environ["CAMPUS_AUTH_TOKEN"].strip()
    candidates = []
    base_path = os.environ.get("CAMPUS_AUTH_BASE_PATH")
    if base_path:
        candidates.append(os.path.join(base_path, "config", ".auth_token"))
    # 兼容：仓库 target/debug 直跑实例
    candidates.append(
        os.path.join(_repo_root(), "target", "debug", "config", ".auth_token")
    )
    for c in candidates:
        try:
            with open(c, encoding="utf-8") as f:
                tok = f.read().strip()
            if tok:
                return tok
        except OSError:
            continue
    return ""


TOKEN = load_token()


def _reachable(url: str, authed: bool) -> bool:
    """实例 `/api/*` 无 token 时回 401——同样证明是我们的实例在监听；
    其他非 2xx/401（如代理的 502）视为不可达。"""
    try:
        with urllib.request.urlopen(url, timeout=5):
            return True
    except urllib.error.HTTPError as e:
        return authed and e.code in (200, 401)
    except Exception:
        return False


def preflight() -> None:
    """实例或 mock 不可达时打印 SKIP 并 exit 0（手动回归，非 CI 门禁）。"""
    targets = (
        ("实例", BASE + "/api/init-status", True),
        ("mock", MOCK + "/status", False),
    )
    for name, url, authed in targets:
        if _reachable(url, authed):
            continue
        print(f"SKIP: {name}不可达（{url}）")
        print(
            "请先启动 mock（server.py）与主实例，"
            "或设置 CAMPUS_AUTH_BASE/_MOCK/_BASE_PATH。"
        )
        sys.exit(0)
