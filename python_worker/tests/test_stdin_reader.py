"""stdin_reader NDJSON 分发边界：超限回执 / 非法行丢弃 / 取消 / 命令入队。

stdin_reader 在守护线程阻塞读 stdin，跨线程经
``loop.call_soon_threadsafe`` 入队；此处以 ``asyncio.to_thread`` 驱动，
排空到哨兵 None 即完成一次读取会话。
"""

from __future__ import annotations

import asyncio
import io
import json
import sys

import worker_main
from worker_main import _MAX_STDIN_LINE_BYTES, stdin_reader


def _run(coro):
    return asyncio.run(coro)


async def _drain(queue: asyncio.Queue) -> list:
    """运行 reader 至 EOF，返回哨兵前的全部入队项。"""
    loop = asyncio.get_running_loop()
    await asyncio.to_thread(stdin_reader, queue, loop)
    items = []
    while True:
        item = await queue.get()
        if item is None:
            return items
        items.append(item)


def _read_all(monkeypatch, text: str) -> list:
    worker_main.shutdown_event.clear()
    monkeypatch.setattr(sys, "stdin", io.StringIO(text))

    async def _run_drain():
        return await _drain(asyncio.Queue())

    try:
        return _run(_run_drain())
    finally:
        worker_main.shutdown_event.clear()


def test_command_enqueued_and_eof_sets_shutdown(monkeypatch):
    """合法命令入队；EOF 置位关闭事件并以哨兵结束。"""
    items = _read_all(monkeypatch, '{"id": 1, "method": "ping"}\n')
    assert items == [{"id": 1, "method": "ping"}]


def test_cancel_notification_triggers_without_enqueue(monkeypatch):
    """取消通知触发注册表且无响应、不入队。"""
    items = _read_all(monkeypatch, '{"cancel": "stdin-test-cancel-1"}\n')
    assert items == []
    from playwright_worker import cancel_registry

    assert cancel_registry.register("stdin-test-cancel-1").is_set()


def test_non_json_and_non_object_lines_ignored(monkeypatch):
    """非 JSON 行与非对象 JSON 被丢弃，不阻断后续合法命令。"""
    items = _read_all(
        monkeypatch, "not json\n[1, 2]\n42\n\n{\"id\": 2, \"method\": \"m\"}\n"
    )
    assert items == [{"id": 2, "method": "m"}]


def test_oversized_line_emits_error_with_id(monkeypatch):
    """超限行按 P9 语义：从有界前缀提取 id 回明确错误，命令不入队。"""
    calls = []
    monkeypatch.setattr(
        worker_main, "emit_response", lambda i, r: calls.append((i, r))
    )
    head = '{"id": 4242, "method": "m", "params": {"blob": "'
    padding = "x" * (_MAX_STDIN_LINE_BYTES + 1024)
    items = _read_all(monkeypatch, head + padding + '"}}\n')
    assert items == []
    assert len(calls) == 1
    msg_id, result = calls[0]
    assert msg_id == 4242
    assert result["success"] is False
    assert "MiB" in json.dumps(result, ensure_ascii=False)


def test_oversized_line_without_id_is_silent(monkeypatch):
    """超限且无 id 时静默丢弃（无处回包，不崩溃）。"""
    calls = []
    monkeypatch.setattr(
        worker_main, "emit_response", lambda i, r: calls.append((i, r))
    )
    padding = "x" * (_MAX_STDIN_LINE_BYTES + 1024)
    items = _read_all(monkeypatch, padding + "\n")
    assert items == []
    assert calls == []
