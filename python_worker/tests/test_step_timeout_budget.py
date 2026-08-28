"""浏览器步骤超时预算回归测试。

锁住“一个步骤一个 deadline”的预算分配，确保正常 Playwright 操作不会把全部
超时吃光，导致 attached/JS 降级路径永远没有执行机会。
"""

from __future__ import annotations

from step_handlers import _primary_timeout_ms


def test_primary_timeout_reserves_fallback_for_normal_timeout():
    assert _primary_timeout_ms(10_000) == 9_000
    assert _primary_timeout_ms(5_000) == 4_000


def test_primary_timeout_reserves_at_most_half_for_short_timeout():
    assert _primary_timeout_ms(1_000) == 500
    assert _primary_timeout_ms(100) == 50
    assert _primary_timeout_ms(2) == 1


def test_primary_timeout_handles_zero_and_negative_values():
    assert _primary_timeout_ms(0) == 0
    assert _primary_timeout_ms(-100) == 0
