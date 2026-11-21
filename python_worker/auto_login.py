"""持久自动登录纯逻辑：录制动作校验与回放映射（WorkerCore 拆分）。

「记住密码」模式的自动登录 = 无头持久上下文打开门户页（门户自己的 JS 自动
回填账号密码）+ 回放录制动作序列。本模块承载与 Playwright 无关的纯逻辑
（对齐 ``config_session.py`` / ``debug_session.py`` 的拆分纪律）：

- ``validate_auto_login_actions``：录制动作数组校验（畸形/不认识的 type 在
  启动浏览器**之前**拦截，失败即结构化错误，不留半开资源）；
- ``build_replay_variables``：从命令参数键（与浏览器任务执行完全同名：
  ``username`` / ``password`` / ``isp`` / ``auth_url``）构建系统变量映射，
  供 fill/navigate 的 ``{{USERNAME}}`` 等占位符解析；
- ``build_replay_steps``：录制动作 → 既有步骤处理器的回放映射
  （click → click 处理器、change → select 处理器、fill → input 处理器
  （``locator.fill`` 整值填充）、navigate → navigate 处理器），不另写一套
  执行器；fill/navigate 的 value 先经变量映射解析占位符；
- 沉降窗口常量：门户回填等待（load 态 + 固定沉降），窗口可测（测试按常量
  monkeypatch）。

命令编排（profile_auto_login）仍留在 WorkerCore——它驱动真实页面 I/O。
"""

from __future__ import annotations

from models import StepConfig
from variable_resolver import resolve

#: 回放可接受的录制动作类型（与录制契约一致）：click（按钮/提交类点击）、
#: change（仅 <select> 的选择）、fill（文本输入，值可为 {{USERNAME}}/
#: {{PASSWORD}} 占位符）、navigate（页面导航，value=落地地址，selector 恒为
#: 空串）。不认识的 type 在校验阶段结构化报错。
AUTO_LOGIN_ACTION_TYPES: tuple[str, ...] = ("click", "change", "fill", "navigate")

#: 回放动作数防御上限：录制器侧上限为 50（``RECORDER_MAX_ACTIONS``），此处放宽
#: 到 4 倍兜底畸形/未来放宽的载荷；超限在校验阶段报错而非让回放吃满命令级超时。
AUTO_LOGIN_MAX_ACTIONS = 200

#: 门户回填沉降的 load 态等待上限（毫秒）：``goto``（domcontentloaded）返回后
#: 等 ``load`` 事件给门户回填 JS 留出执行窗口；部分门户存在长挂资源永远到不了
#: load，该等待是 **best-effort**（超时忽略继续），不作为任何成败判定。
AUTO_LOGIN_LOAD_STATE_TIMEOUT_MS = 5000

#: 门户回填沉降的固定等待（毫秒）：load 态之后的额外沉降窗口，覆盖 onload 后
#: 异步回填（XHR 取用户信息、延迟赋值）等场景。窗口由常量钉住（可测），
#: 同样 best-effort，不作为成败判定——登录成败由 Rust 侧网络验证兜底。
AUTO_LOGIN_BACKFILL_SETTLE_MS = 1500


def validate_auto_login_actions(raw_actions: list) -> list[dict]:
    """校验并规范化录制动作数组（原样元素浅拷贝返回，畸形即 ``ValueError``）。

    逐条规则（与录制契约 / Rust 侧 ``RecordedAction`` 字段对齐）：

    - 元素必须是 JSON 对象；
    - ``type`` 必须在 :data:`AUTO_LOGIN_ACTION_TYPES` 内（不认识的 type 结构化
      报错，绝不静默跳过——静默跳过会让"少点了一次登录"伪装成成功）；
    - ``selector`` 必须为非空字符串（回放直接用录制串，Playwright 兼容形态）；
      ``navigate`` 例外：无目标元素，录制器恒发空串，缺失/空白归一为空串；
    - ``change`` / ``fill`` 必须携带非空字符串 ``value``（``select_option`` 的
      匹配目标 / 填入值；fill 的值可为 {{USERNAME}}/{{PASSWORD}} 占位符或
      字面量，录制器对两者恒写 value，缺失视为数据损坏，启动前拦截）；
      ``navigate`` 的 ``value``（落地地址）同理必填——无地址的导航无回放意义；
    - ``frame``（可选，仅 click/change/fill）：iframe 内录制的动作携带
      「主文档 → 目标 iframe」的 ' >> ' 分隔 CSS 链，回放侧映射进
      ``StepConfig.frame`` 逐层定位；非字符串/空白静默丢弃该键（顶层 frame
      录制不带、旧会话数据亦无此键），不阻断动作本体；
    - ``ts`` / ``strategy`` 不参与回放（序列本身即 ts 升序），不做强校验。
    """
    if len(raw_actions) > AUTO_LOGIN_MAX_ACTIONS:
        raise ValueError(
            f"录制动作数量超过上限 {AUTO_LOGIN_MAX_ACTIONS}（当前 {len(raw_actions)}）"
        )
    normalized: list[dict] = []
    for index, raw in enumerate(raw_actions):
        where = f"录制动作第 {index + 1} 项"
        if not isinstance(raw, dict):
            raise ValueError(f"{where} 必须是 JSON 对象")
        action_type = raw.get("type")
        if action_type not in AUTO_LOGIN_ACTION_TYPES:
            raise ValueError(
                f"{where} 的 type 不受支持: {action_type!r}（仅支持 "
                f"{'/'.join(AUTO_LOGIN_ACTION_TYPES)}）"
            )
        if action_type == "navigate":
            value = raw.get("value")
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"{where}（navigate）缺少非空 value")
            normalized.append({
                "type": "navigate",
                "selector": "",
                "strategy": str(raw.get("strategy") or "unknown"),
                "ts": raw.get("ts") if isinstance(raw.get("ts"), int) else 0,
                "value": value,
            })
            continue
        selector = raw.get("selector")
        if not isinstance(selector, str) or not selector.strip():
            raise ValueError(f"{where} 缺少 selector")
        action: dict = {
            "type": action_type,
            "selector": selector,
            "strategy": str(raw.get("strategy") or "unknown"),
            "ts": raw.get("ts") if isinstance(raw.get("ts"), int) else 0,
        }
        if action_type in ("change", "fill"):
            value = raw.get("value")
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"{where}（{action_type}）缺少非空 value")
            action["value"] = value
        # frame 链（可选）：非字符串/空白静默丢弃（录制器顶层 frame 不带该键）
        frame = raw.get("frame")
        if isinstance(frame, str) and frame.strip():
            action["frame"] = frame.strip()
        normalized.append(action)
    return normalized


def build_replay_variables(params: dict) -> dict[str, str]:
    """从命令参数构建系统变量映射（键名与浏览器任务执行完全同名）。

    ``username`` / ``password`` / ``isp`` / ``auth_url`` → ``{{USERNAME}}`` /
    ``{{PASSWORD}}`` / ``{{ISP}}`` / ``{{LOGIN_URL}}``，供 fill/navigate 的
    value 占位符解析（经 :func:`variable_resolver.resolve`，系统保留变量值
    不再链式展开——凭据本身含 ``{{X}}`` 字面序列时保持原样，P2-7）。
    键缺失或为 None 时不注入（对齐 task_runner 的 ``_system_variables``：
    ``str(None)`` 会把字面 "None" 当凭据注入模板）。
    """
    mapping = {
        "USERNAME": "username",
        "PASSWORD": "password",
        "ISP": "isp",
        "LOGIN_URL": "auth_url",
    }
    return {
        key: str(params[src])
        for key, src in mapping.items()
        if params.get(src) is not None
    }


def build_replay_steps(
    actions: list[dict],
    *,
    portal_url: str = "",
    variables: dict[str, str] | None = None,
) -> list[StepConfig]:
    """录制动作 → 既有步骤处理器的回放映射（保持数组顺序，即 ts 升序）。

    - ``click`` → ``click`` 处理器（候选拆分 / 隐藏降级 / 文本兜底全部复用，
      selector 直接用录制串）；
    - ``change`` → ``select`` 处理器（value 精确匹配 → 文本匹配链复用，
      最终落到 ``select_option``）；
    - ``fill`` → ``input`` 处理器（``clear`` 默认 True → 走 ``locator.fill``
      整值填充，与录制语义一致），value 先经 ``variables`` 解析占位符；
    - ``navigate`` → ``navigate`` 处理器（``goto`` 的 domcontentloaded 等待
      与门户首导航同口径），value 同样解析占位符；
    - ``frame``（iframe 内录制的动作携带）：映射进 ``StepConfig.frame``，
      回放侧逐层 ``frame_locator`` 下钻（' >> ' 分隔链由步骤处理器的
      嵌套解析展开）；顶层动作无该键 → ``frame=None`` 主文档执行。

    **序列首条 navigate 与 ``portal_url`` 相同则跳过**：录制器对每个新文档
    （含门户首页）都上报 navigate，回放主流程已先打开门户页并完成回填沉降，
    重复 goto 会重载门户页冲掉沉降成果。跳过语义以 ``wait(0)`` 占位表达而
    非删步——保持动作 ↔ 步骤 1:1 对齐（回放循环以同下标动作定位错误消息）。

    ``portal_url`` / ``variables`` 缺省时行为与旧版完全一致（不跳过、不解析），
    既有 click/change 调用方无需感知新参数。

    步骤 ``id`` 仅为进度/错误消息标识（``auto-login-<序号>``），不设显式
    ``timeout``——单步预算沿用上下文默认超时（``browser_settings.timeout``，
    既有超时契约）。
    """
    steps: list[StepConfig] = []
    first_navigate_checked = False
    for index, action in enumerate(actions):
        step_id = f"auto-login-{index + 1}"
        action_type = action["type"]
        # iframe 内录制的动作携带 frame 链；顶层动作无该键 → None 主文档执行
        frame = action.get("frame") or None
        if action_type == "click":
            steps.append(StepConfig(
                id=step_id, step_type="click", selector=action["selector"], frame=frame,
            ))
            continue
        if action_type == "change":
            steps.append(
                StepConfig(
                    id=step_id,
                    step_type="select",
                    selector=action["selector"],
                    value=action["value"],
                    frame=frame,
                )
            )
            continue
        # fill/navigate 的 value 先解析占位符（{{USERNAME}}/{{PASSWORD}}/{{ISP}}/
        # {{LOGIN_URL}}，映射由 build_replay_variables 构建；未命中保留原样）。
        # change 不解析——录制值是 select 选中值字面量，保持既有契约字节面
        value = action["value"]
        if variables:
            value = resolve(value, variables)
        if action_type == "fill":
            steps.append(
                StepConfig(
                    id=step_id,
                    step_type="input",
                    selector=action["selector"],
                    value=value,
                    frame=frame,
                )
            )
            continue
        # navigate：首条与门户地址相同则跳过（wait(0) 占位，见 docstring）；
        # load 级沉降不落在单步上——慢资源门户到不了 load，wait_until="load"
        # 会把「登录其实已成功」的回放打成导航失败，违背「沉降不作为成败
        # 判定」的既有纪律，load 级等待由回放后既有的沉降窗口承担
        if not first_navigate_checked:
            first_navigate_checked = True
            if portal_url and value.strip() == portal_url.strip():
                steps.append(StepConfig(id=step_id, step_type="wait", duration=0))
                continue
        steps.append(StepConfig(id=step_id, step_type="navigate", value=value))
    return steps


def auto_login_success_message(action_count: int) -> str:
    """成功信封的人话短语（回放为空 = 只打开门户页等回填）。"""
    if action_count == 0:
        return "门户页已打开并完成回填沉降（未配置回放动作）"
    return f"门户页已打开、{action_count} 个录制动作已全部回放"


__all__ = [
    "AUTO_LOGIN_ACTION_TYPES",
    "AUTO_LOGIN_BACKFILL_SETTLE_MS",
    "AUTO_LOGIN_LOAD_STATE_TIMEOUT_MS",
    "AUTO_LOGIN_MAX_ACTIONS",
    "auto_login_success_message",
    "build_replay_steps",
    "build_replay_variables",
    "validate_auto_login_actions",
]
