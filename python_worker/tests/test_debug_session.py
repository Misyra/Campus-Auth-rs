"""debug_session 纯状态机单测：无 Playwright 环境直测（WorkerCore 拆分收益）。"""

from models import StepConfig, TaskConfig
from debug_session import DebugSession, _build_steps_info


def _make_task(step_count: int) -> TaskConfig:
    steps = [
        StepConfig(id=f"s{i}", step_type="click", description=f"步骤{i}")
        for i in range(step_count)
    ]
    return TaskConfig(task_id="t1", name="任务", steps=steps)


def test_build_steps_info_indexes_and_defaults():
    info = _build_steps_info(_make_task(3))
    assert [x["index"] for x in info] == [0, 1, 2]
    assert [x["id"] for x in info] == ["s0", "s1", "s2"]
    # 缺省 id 的步骤回退 step_{i}
    task = _make_task(1)
    task.steps[0].id = ""
    info = _build_steps_info(task)
    assert info[0]["id"] == "step_0"
    assert info[0]["type"] == "s0" or info[0]["type"] == "click"


def test_debug_session_cursor_defaults():
    session = DebugSession(
        session_id="d1",
        page=None,
        task_config=None,
        context=None,
    )
    # 自动步进游标从 0 开始，前端数据结构初始为空
    assert session.current_step == 0
    assert session.steps_info == []
    assert session.results == []
