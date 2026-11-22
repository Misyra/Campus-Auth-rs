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


def test_get_ocr_normalizes_blank_and_dirty_range(monkeypatch) -> None:
    """P2-6：空串/纯空白/脏数据与"未指定"共用默认档实例（key 规范化）。"""
    import ocr_runtime

    fake = _FakeOcr()
    created: list[int] = []

    def factory(**_kwargs):
        created.append(1)
        return fake

    monkeypatch.setitem(sys.modules, "ddddocr", SimpleNamespace(DdddOcr=factory))
    ocr_runtime._ocr_cache.clear()
    try:
        s_default = ocr_runtime._get_ocr(False)
        s_blank = ocr_runtime._get_ocr(False, "")
        s_spaces = ocr_runtime._get_ocr(False, "   ")
        s_dirty = ocr_runtime._get_ocr(False, ["bad"])  # 非 str/int 脏数据退默认档
        assert s_default is s_blank is s_spaces is s_dirty
        assert created == [1], "等价取值应共用一个模型实例"
    finally:
        ocr_runtime._ocr_cache.clear()


def test_get_ocr_failed_set_ranges_falls_back_to_default_instance(monkeypatch) -> None:
    """P2-6：set_ranges 失败 → key 归一为默认档，与未指定范围共用实例。"""
    import ocr_runtime

    class BrokenRangeFake:
        def set_ranges(self, _r: object) -> None:
            raise ValueError("bad range")

        def classification(self, _img: bytes) -> str:
            return "ok"

    inst = BrokenRangeFake()
    monkeypatch.setitem(sys.modules, "ddddocr", SimpleNamespace(DdddOcr=lambda **_kw: inst))
    ocr_runtime._ocr_cache.clear()
    try:
        s_bogus = ocr_runtime._get_ocr(False, "绝对非法")
        s_default = ocr_runtime._get_ocr(False)
        assert s_bogus is s_default, "set_ranges 失败的取值应归一到默认档实例"
        assert (False, None) in ocr_runtime._ocr_cache
    finally:
        ocr_runtime._ocr_cache.clear()


def test_get_ocr_lru_evicts_oldest_beyond_cap(monkeypatch) -> None:
    """P2-6：缓存实例数有上限（LRU），触顶逐项淘汰最久未使用。"""
    import ocr_runtime

    class RangedFake:
        def set_ranges(self, _r: object) -> None:
            pass

    monkeypatch.setitem(
        sys.modules, "ddddocr", SimpleNamespace(DdddOcr=lambda **_kw: RangedFake())
    )
    ocr_runtime._ocr_cache.clear()
    try:
        keys = [(False, f"0123456789{i}") for i in range(5)]
        for _, rng in keys:
            ocr_runtime._get_ocr(False, rng)
        assert len(ocr_runtime._ocr_cache) == ocr_runtime._OCR_CACHE_MAX_ENTRIES
        assert keys[0] not in ocr_runtime._ocr_cache, "最旧的实例应被淘汰"
        assert keys[-1] in ocr_runtime._ocr_cache, "最新的实例应保留"
        # 触碰 k1 使其变新，再插入新 key → 淘汰的是更旧的 k2
        ocr_runtime._get_ocr(False, keys[1][1])
        ocr_runtime._get_ocr(False, "extra-range")
        assert keys[1] in ocr_runtime._ocr_cache, "最近使用的实例不应被淘汰"
        assert keys[2] not in ocr_runtime._ocr_cache
    finally:
        ocr_runtime._ocr_cache.clear()
