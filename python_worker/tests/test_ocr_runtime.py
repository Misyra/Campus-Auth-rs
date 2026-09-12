"""OCR runtime 的共享预算与缓存淘汰回归测试。"""

from __future__ import annotations

import sys
import threading
from types import SimpleNamespace

import pytest


class _FakeOcr:
    def __init__(self, result: str = "1234") -> None:
        self.result = result

    def classification(self, _img: bytes) -> str:
        return self.result


def test_ocr_session_returns_classification_result() -> None:
    from ocr_runtime import _OcrSession

    session = _OcrSession(_FakeOcr("5678"), (False, None))
    assert session.classification(b"image") == "5678"


def test_ocr_session_timeout_evicts_stuck_instance() -> None:
    import ocr_runtime

    gate = threading.Event()

    class SlowOcr:
        def classification(self, _img: bytes) -> str:
            gate.wait(1)
            return "late"

    key = (False, None)
    session = ocr_runtime._OcrSession(SlowOcr(), key)
    ocr_runtime._ocr_cache.clear()
    ocr_runtime._ocr_cache[key] = session

    try:
        with pytest.raises(TimeoutError, match="单次预算"):
            session.classification_with_timeout(b"image", 0.01)
        assert key not in ocr_runtime._ocr_cache
    finally:
        # 释放 daemon 识别线程，避免测试进程中残留无意义工作。
        gate.set()
        ocr_runtime._ocr_cache.clear()


def test_ocr_session_explicit_budget_does_not_leak_to_next_call(monkeypatch) -> None:
    import ocr_runtime

    session = ocr_runtime._OcrSession(_FakeOcr("ok"), (False, None))
    with pytest.raises(TimeoutError):
        session.classification_with_timeout(b"image", 0)
    assert session.classification_with_timeout(b"image", 0.5) == "ok"


def test_get_ocr_keeps_cached_session_identity(monkeypatch) -> None:
    import ocr_runtime

    fake = _FakeOcr()
    fake_module = SimpleNamespace(DdddOcr=lambda **_kwargs: fake)
    monkeypatch.setitem(sys.modules, "ddddocr", fake_module)
    ocr_runtime._ocr_cache.clear()
    try:
        first = ocr_runtime._get_ocr(False)
        second = ocr_runtime._get_ocr(False)
        assert first is second
        assert first._instance is fake
    finally:
        ocr_runtime._ocr_cache.clear()


def test_get_ocr_marks_and_clears_load_started(monkeypatch) -> None:
    """首次构建期间登记加载标记，完成后清除（F5：供超时文案分流与恢复可见）。"""
    import ocr_runtime

    fake = _FakeOcr()
    build_gate = threading.Event()
    release = threading.Event()

    def slow_build(**_kwargs):
        build_gate.set()
        release.wait(2)
        return fake

    fake_module = SimpleNamespace(DdddOcr=slow_build)
    monkeypatch.setitem(sys.modules, "ddddocr", fake_module)
    ocr_runtime._ocr_cache.clear()
    ocr_runtime._ocr_load_started.clear()
    try:
        worker = threading.Thread(
            target=lambda: ocr_runtime._get_ocr(True), daemon=True
        )
        worker.start()
        assert build_gate.wait(1), "构建线程应启动"
        assert ocr_runtime.ocr_load_in_progress(True), "构建期间应标记加载中"
        assert not ocr_runtime.ocr_load_in_progress(False), "未构建的 key 不应标记"
        release.set()
        worker.join(timeout=2)
        assert not worker.is_alive()
        assert not ocr_runtime.ocr_load_in_progress(True), "构建完成应清除标记"
        assert (True, None) in ocr_runtime._ocr_cache, "完成后会话入缓存"
    finally:
        release.set()
        ocr_runtime._ocr_cache.clear()
        ocr_runtime._ocr_load_started.clear()
