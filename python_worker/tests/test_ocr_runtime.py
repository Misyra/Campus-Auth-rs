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

    session = _OcrSession(_FakeOcr("5678"), (False, None), 0.5)
    assert session.classification(b"image") == "5678"


def test_ocr_session_timeout_evicts_stuck_instance() -> None:
    import ocr_runtime

    gate = threading.Event()

    class SlowOcr:
        def classification(self, _img: bytes) -> str:
            gate.wait(1)
            return "late"

    key = (False, None)
    instance = SlowOcr()
    ocr_runtime._ocr_cache.clear()
    ocr_runtime._ocr_cache[key] = instance
    session = ocr_runtime._OcrSession(instance, key, 0.01)

    try:
        with pytest.raises(TimeoutError, match="共享预算"):
            session.classification(b"image")
        assert key not in ocr_runtime._ocr_cache
    finally:
        # 释放 daemon 识别线程，避免测试进程中残留无意义工作。
        gate.set()
        ocr_runtime._ocr_cache.clear()


def test_get_ocr_subtracts_model_acquire_time(monkeypatch) -> None:
    import ocr_runtime

    fake = _FakeOcr()
    fake_module = SimpleNamespace(DdddOcr=lambda **_kwargs: fake)
    monkeypatch.setitem(sys.modules, "ddddocr", fake_module)
    monkeypatch.setattr(ocr_runtime, "OCR_TIMEOUT_SECS", 1.0)

    ticks = iter((10.0, 10.4))
    monkeypatch.setattr(ocr_runtime.time, "monotonic", lambda: next(ticks))
    ocr_runtime._ocr_cache.clear()
    try:
        session = ocr_runtime._get_ocr(False)
        assert session._instance is fake
        assert session._inference_timeout_secs == pytest.approx(0.6)
    finally:
        ocr_runtime._ocr_cache.clear()
