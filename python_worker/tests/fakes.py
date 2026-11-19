"""python_worker 测试共享替身（单一权威实现）。

收敛来源：test_worker.py / test_worker_execution_contract.py /
test_worker_robustness.py 三文件各自持有的同形 Fake* 定义
（2026-11-19 审查发现）。pytest 以 prepend 模式把 tests/ 插入
sys.path，测试文件按现有平铺风格 ``from fakes import ...`` 导入。

差异以「多数一致」为准，逐项记录：

- FakePage（dialog 记录页）：多数口径为覆盖式 ``on``（test_worker.py
  L860/L1094 两处）；test_worker_robustness.py 的 ``_WirePage``（L173）
  为 setdefault 追加式，按多数收敛为覆盖式——robustness 现有断言仅判
  ``handlers.get("dialog")`` 真值，两种口径均通过。``close`` 为 no-op
  并集补齐（原 ``_WirePage`` 即 no-op）。
- FakeDialog：``message`` 经构造参数注入，``accept`` 多数口径为 no-op
  （test_worker.py L903/L931）；L873 一处 accept 携带测试闭包副作用，
  保留各文件本地。
- FakeContext：并集口径 ``pages`` 实例列表 + ``new_page``→FakePage +
  ``on``/``close`` no-op（worker L856 仅 new_page、L1859 与 robustness
  ``_WireContext`` 为 no-op 家族、execution_contract L108 仅 ``pages``）。
- FakeBrowser：``is_connected`` 恒 True、``new_context``→FakeContext，
  ``close`` 为 no-op 并集补齐（worker L1869 有 no-op close，其余来源未
  定义）。计数式（worker L1105）、抛错式（L1145）、记录 kwargs 式
  （execution_contract L265/346/395/439）属测试特异变体，保留本地。
- FakeResult：execution_contract L536/L585/L742 三处定义逐字段一致，
  直接收敛。
- FakeLocator：仅 execution_contract L216 一份（审查所述「三文件各持
  一份」与实际不符），收敛入本模块防再复制；``inner_text`` 钉死探测
  超时 1500ms 契约。
- 「make_context」并非类替身：execution_contract L679/L817 的
  ``fake_make_context`` 是 monkeypatch ``core._make_context`` 的测试
  闭包（返回携带测试局部 page 的 StepContext），依赖测试局部状态，
  不收敛、保留内联。

待迁移清单（本次仅迁移 test_worker_robustness.py 作模板）：

- test_worker.py（19 处本地定义）：FakePage L860/L1094 → FakePage；
  FakeDialog L903/L931 → FakeDialog（L873 闭包副作用保留）；FakeContext
  L856/L1859 与 FakeCtx L1101 → FakeContext；FakeBrowser L1869 →
  FakeBrowser；FakeDdddOcr / HungPage / FakeRes / AliveBrowser /
  AliveContext 及计数式（L1105）、抛错式（L1145）、goto 挂死（L1850）
  等特异变体迁移时逐个评估。
- test_worker_execution_contract.py（21 处本地定义）：FakeResult
  L536/L585/L742 → FakeResult；FakeLocator L216 → FakeLocator；
  clear_cookies 计数式 FakeContext（L501/L801）、init_script 记录式
  FakePage（L99/L486）与 redirect 场景家族（FakeFrame/FakeResponse/
  FakePage L236/FakeContext L249/FakeBrowser L265）可后续以场景级替身
  收敛。
- test_worker_robustness.py（16 处本地定义）：已迁移——``_WirePage``/
  ``_WireContext``/热恢复 FakeBrowser（L232）→ 本模块；Resident* /
  Temp* / _Feedback* / Broken* 及 page_capture 家族（L626/L633/L655）
  为场景专属替身（闭包捕获取消触发、关闭断言等），保留本地。
"""

from __future__ import annotations

from typing import Any


class FakePage:
    """记录 ``on`` 注册事件的最小页面替身（多数口径：覆盖式）。"""

    def __init__(self) -> None:
        self.handlers: dict[str, Any] = {}

    def on(self, event: str, handler: Any) -> None:
        """覆盖式记录事件处理器（robustness 原追加式为少数派）。"""
        self.handlers[event] = handler

    async def close(self) -> None:
        """no-op 关闭（原 ``_WirePage`` 同为 no-op，供 _safe_close 调用）。"""


class FakeDialog:
    """携带文案的弹窗替身；``accept`` 多数口径为 no-op。"""

    def __init__(self, message: str = "账号或密码错误！") -> None:
        self.message = message

    async def accept(self) -> None:
        """no-op 确认（worker L873 的记录式 accept 为测试局部变体）。"""


class FakeContext:
    """最小 BrowserContext 替身：new_page 派发 FakePage，on/close no-op。"""

    def __init__(self) -> None:
        self.pages: list[Any] = []

    def on(self, *_args: Any) -> None:
        """no-op 事件注册（_wire_context 会调用）。"""

    async def new_page(self) -> FakePage:
        """派发一个新的 FakePage。"""
        return FakePage()

    async def close(self) -> None:
        """no-op 关闭（_close_session / _safe_close 调用）。"""


class FakeBrowser:
    """最小 Browser 替身：恒在线，new_context 派发 FakeContext。"""

    def is_connected(self) -> bool:
        """恒 True（热恢复快速路径的存活判定）。"""
        return True

    async def new_context(self, **_kwargs: Any) -> FakeContext:
        """派发一个新的 FakeContext（忽略 context 选项）。"""
        return FakeContext()

    async def close(self) -> None:
        """no-op 关闭（worker L1869 同为 no-op，其余来源属并集补齐）。"""


class FakeResult:
    """结构化结果替身：success 恒 True，``data`` 类属性可写。"""

    data = None

    def to_dict(self) -> dict:
        """与 StructuredResult.to_dict 的 success 信封同形。"""
        return {"success": True, "data": self.data}


class FakeLocator:
    """redirect 探测用定位器替身（execution_contract 的 1500ms 契约）。"""

    def __init__(self, selector: str) -> None:
        self.selector = selector

    async def inner_text(self, timeout: int) -> str:
        """钉死探测超时契约 1500ms，返回门户判定文案。"""
        assert timeout == 1500
        return "校园网 登录 认证 账号 密码"

    async def count(self) -> int:
        """password/form 选择器命中 1，其余 0。"""
        if "password" in self.selector or "form" in self.selector:
            return 1
        return 0
