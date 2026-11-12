"""Playwright Worker 核心：命令编排组装与单一注册表（伴生模块门面）。

本模块是 Python 侧浏览器自动化执行平面的**组装点**。按职责拆分的伴生模块以
Mixin 形式多继承回 :class:`WorkerCore`，命令注册表 ``COMMANDS`` 在此收敛为单一
事实源（与 README 命令表逐项对应，共 18 条）。Rust 主进程通过 NDJSON IPC 调用
注册表中的处理器；Worker 仅作为"浏览器动作执行器"，单次动作返回
StructuredResult——重试、状态机、取消调度由 Rust 侧负责。取消通过 ``cancel_id``
映射到 ``threading.Event``，处理器在步骤边界检查该事件。

模块结构（伴生模块范式；纯逻辑层此前已拆出 models / step_handlers /
ocr_runtime / debug_session / config_session / auto_login / variable_resolver）：

- ``cancel_registry.py``    取消注册表（cancel_id → Event，跨线程）
- ``worker_paths.py``       产物路径锚定与启动残留清理
- ``browser_detect.py``     浏览器环境探测（registry + 安装完成标记）
- ``browser_launch.py``     上下文准备（启动参数/选项/launcher/stealth/直连/资源路由）
- ``browser_lifecycle.py``  浏览器生命周期（start/close/keep_alive/空闲回收/生命周期锁/弹窗接线）
- ``step_engine.py``        步骤执行引擎（run_steps 及失败归一）
- ``task_runner.py``        任务执行（_run_task/cookie 清理矩阵/execute_* 命令）
- ``debug_commands.py``     调试会话命令与截图治理
- ``profile_commands.py``   持久配置会话与自动登录命令
- ``capture_kit.py``        页面结构提取 + CSS/JS 资源快照（纯逻辑）
- ``capture_commands.py``   test_redirect / page_capture / feedback_capture（临时浏览器捕获类）
- ``ocr_commands.py``       OCR 识别命令

本文件保留：WorkerCore 组装（``__init__`` 状态拓扑 + 健康检查/shutdown）、
``WORKER_VERSION`` 与 ``COMMANDS`` 注册表；并统一再导出伴生模块符号，保持既有
导入面不变（worker_main、tests、Rust 环境探针 ``from playwright_worker import
WORKER_VERSION``）。
"""

from __future__ import annotations

import asyncio
import logging
import threading
from typing import Any, Callable

from models import Outcome, StepConfig, StructuredResult, TaskConfig
from step_handlers import (
    StepCancelled,
    StepContext,
    WorkerError,
    _check_cancel,
    _classify_navigation_error,
    _sleep_cancellable,
    _suppress_task,
    run_step_async,
)
from ocr_runtime import OCR_TIMEOUT_SECS, _get_ocr, _preload_ocr_deps, _preprocess_ocr_image
from debug_session import DebugSession, _build_steps_info
from config_session import (
    ProfileSessionState,
    RECORDER_BINDING_NAME,
    RecorderSink,
    build_recorder_init_script,
)
from auto_login import (
    AUTO_LOGIN_BACKFILL_SETTLE_MS,
    AUTO_LOGIN_LOAD_STATE_TIMEOUT_MS,
    auto_login_success_message,
    build_replay_steps,
    validate_auto_login_actions,
)
from variable_resolver import resolve

# ── 伴生模块组装与兼容再导出 ──
# 以下导入承担两类职责：Mixin 基类（WorkerCore 组装必需）与门面再导出（保持
# worker_main / tests / Rust 探针的历史导入面：``from playwright_worker import X``）。
# 再导出项刻意求全——本文件不再是实现归属地，符号的真实属主见各伴生模块。

from cancel_registry import CancelRegistry, cancel_registry  # noqa: F401
from worker_paths import (  # noqa: F401
    _MODULE_LOAD_TIME,
    _WORKER_DIR,
    _browser_data_dir,
    _capture_dir,
    _debug_screenshot_dir,
    _feedback_capture_dir,
    _purge_stale_debug_screenshots,
    _runtime_worker_project_dir,
)
from browser_detect import (  # noqa: F401
    _FALLBACK_PREFIXES,
    _INSTALLATION_COMPLETE,
    _REQUIRED_DIRS_CACHE,
    _REQUIRED_REGISTRY_NAMES,
    _SYSTEM_CHANNEL_LABELS,
    _browser_cache_dir,
    _ensure_browser,
    _engine_available,
    _fallback_available,
    _managed_engine,
    _missing_components,
    _registry_path,
    _required_dir_names,
    _unavailable_channel_hint,
)
from browser_launch import (  # noqa: F401
    BrowserLaunchMixin,
    _STEALTH_INIT_SCRIPT,
    _channel_supports_cdp,
    _nav_timeout,
    _to_ms,
)
from browser_lifecycle import (  # noqa: F401
    BrowserLifecycleMixin,
    BROWSER_IDLE_RELEASE_SECS,
    _DIALOG_MESSAGE_MAX_CHARS,
    _PERSISTENT_SESSION_STORAGE_ISOLATION_SCRIPT,
    _TASK_STORAGE_ISOLATION_SCRIPT,
    _WAIT_TIMEOUT_SECS,
    _WORKER_KEEP_ALIVE,
)
from step_engine import (  # noqa: F401
    _build_result,
    _is_truthy,
    _normalize_step_failure,
    _profile_login_url,
    _resolve_start_url,
    run_steps,
)
from task_runner import TaskRunnerMixin
from debug_commands import DebugCommandsMixin, DEBUG_FRAME_TIMEOUT_MS, TASK_SCREENSHOT_RETENTION_SECS  # noqa: F401
from profile_commands import ProfileCommandsMixin
from capture_kit import (  # noqa: F401
    _CAPTURE_MAX_SCREENSHOT_BYTES,
    _PAGE_RESOURCE_PROBE_JS,
    _RESOURCE_EXT_BY_MIME,
    _RESOURCE_FETCH_TIMEOUT_MS,
    _RESOURCE_MAX_BYTES,
    _RESOURCE_MAX_FILES,
    _RESOURCE_MAX_TOTAL_BYTES,
    _ResourceSink,
    _STRUCTURE_MAX_CONTROLS,
    _STRUCTURE_MAX_LOCAL_HTML,
    _capture_page_resources,
    _capture_page_structure,
    _cdp_resource_snapshot,
    _http_resource_snapshot,
    _iter_cdp_frame_resources,
    _resource_ext,
    _rewrite_resource_urls,
    _url_scheme_variants,
)
from capture_commands import (  # noqa: F401
    CaptureCommandsMixin,
    _REDIRECT_TEST_SETTLE_SECS,
    _REDIRECT_TEST_TEXT_LIMIT,
    _classify_redirect_test,
    _sanitize_capture_url,
)
from ocr_commands import OcrCommandsMixin

logger = logging.getLogger(__name__)

# Worker 版本（任务 10）：与 pyproject.toml 的 project.version 保持同步（手动维护），
# 随 worker_health_check 响应上报给 Rust 侧。单点定义：worker_main 从此处导入。
# Rust 环境探针（environment/health.rs）同样经 ``from playwright_worker import
# WORKER_VERSION`` 读取——本名字必须保留在本模块。
WORKER_VERSION = "1.0.0"


class WorkerCore(
    BrowserLaunchMixin,
    BrowserLifecycleMixin,
    TaskRunnerMixin,
    DebugCommandsMixin,
    ProfileCommandsMixin,
    CaptureCommandsMixin,
    OcrCommandsMixin,
):
    """管理 Playwright 浏览器实例生命周期，并分发浏览器动作命令。

    行为按职责拆分到伴生模块的 Mixin（基类列表即组装顺序）；状态字段全部在
    ``__init__`` 声明（与拆分前逐字段一致），命令处理器经 ``COMMANDS`` 注册表
    对外暴露。健康检查与 shutdown 是仅存的就地实现（进程级语义，不属于任何
    浏览器职责块）。
    """

    def __init__(self) -> None:
        self._playwright: Any = None
        self._browser: Any = None
        self._context: Any = None
        self._page: Any = None
        self._last_browser_settings: dict | None = None
        self._debug_sessions: dict[str, DebugSession] = {}
        self.emit: Callable[[str, dict], None] = lambda event_type, data: None
        self.shutdown_event: threading.Event | None = None
        # 当前任务期间捕获的页面弹窗文案（_run_task 期间重置，随 StructuredResult 上报）
        self._task_dialogs: list[str] = []
        # 当前会话类型（login / debug）：注入 step_progress 事件，供前端区分
        # 登录会话与调试会话的步骤进度（登录步骤不应污染调试面板）
        self._session_type: str = "login"
        # 运行时能力（任务 10）：由 worker_main._serve 注入（OCR 预加载探测结果，
        # 单一事实源在 ocr_runtime.OCR_CAPABILITIES，P2-1 影子模块修复后两端共享
        # 同一字典），随 worker_health_check 响应上报；未注入时为空 dict
        #（Rust 侧回退文件探测）
        self.capabilities: dict[str, bool] = {}
        # 浏览器空闲自动释放计时（keep_alive 关闭时武装，见 _arm_browser_idle_release）
        self._browser_idle_task: asyncio.Task | None = None
        # 浏览器生命周期互斥锁：串行化 ensure_browser / close_browser / _close_session
        # 与空闲回收的关闭段，消除"回收关闭进行中命令判定浏览器就绪"的交错
        # （P2-3）。只包生命周期段，命令内的步骤执行在锁外，不串行化命令本身。
        self._browser_lifecycle_lock = asyncio.Lock()
        # 延迟删除普通任务截图，确保 Rust WebSocket 有时间完成异步回读。
        self._screenshot_cleanup_tasks: set[asyncio.Task] = set()
        # 防止同一个 Page 重复绑定 dialog 处理器。
        self._wired_page_ids: set[int] = set()
        # 持久配置会话（profile_session_*）：同一时刻至多一个，与自动化浏览器
        # 完全独立（独立 Playwright 连接，不共享 context/page，不占单会话槽位）
        self._profile_session: ProfileSessionState | None = None

    async def handle_worker_health_check(self, params: dict) -> dict:
        """轻量健康检查：确认 Worker IPC/事件循环可用，不探测 Chromium。

        任务 10：响应向后兼容地扩展 ``version`` 与 ``capabilities``——
        Rust 侧（BridgeSupervisor.send_health_check）会缓存 capabilities，
        供 /api/ocr/status 在 Worker 存活时优先展示运行时 OCR 能力。
        """
        return {
            "healthy": True,
            "version": WORKER_VERSION,
            "capabilities": dict(self.capabilities),
        }

    async def handle_browser_health_check(self, params: dict) -> dict:
        """健康检查：确认 Playwright 与浏览器可用。"""
        bs = params.get("browser_settings") or {}
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        try:
            # _ensure_browser 现为纯文件系统/registry 探测（不再冷启 sync_playwright
            # driver），但目录遍历与 registry 读取仍是同步 IO：丢到线程池执行，
            # 避免慢盘上阻塞事件循环，与 OCR classification 的同步 CPU 推理处理一致。
            healthy = await asyncio.to_thread(_ensure_browser, channel, custom_path)
        except Exception as exc:  # noqa: BLE001 — 健康检查失败本身即结果（healthy=False），不能向 IPC 抛异常
            logger.warning(f"健康检查异常: {exc}")
            healthy = False
        result: dict = {"healthy": healthy, "channel": channel}
        if not healthy:
            # 上报缺失组件：Rust 侧据此给出「缺哪个目录」的可操作错误，
            # 而不是只剩通用「健康检查失败」（ENV-1 失败闭环）
            engine = _managed_engine(channel)
            if engine:
                result["missing"] = _missing_components(engine)
                # 托管引擎可由 uv playwright install 自动补齐
                result["reinstallable"] = True
            else:
                # 系统通道（msedge/chrome）与 custom 没有可补的 Playwright 二进制：
                # engine 为 None 时若上报空数组，Rust 侧只剩「未上报缺失组件明细」
                # 这一无指向性文案（启动超时/健康检查失败），排障方向完全偏离
                # 真实原因「配置指定的浏览器未安装」。此处改为给出可操作指向。
                result["missing"] = [_unavailable_channel_hint(channel, custom_path)]
                # 无二进制可下，Rust 侧不应再宣称「将尝试自动重装」
                result["reinstallable"] = False
        return result

    async def handle_shutdown(self, params: dict) -> dict:
        """关闭 Worker：置位 shutdown_event，主循环随后退出。"""
        if self.shutdown_event is not None:
            self.shutdown_event.set()
        return {}


# Worker 实例：进程内唯一（worker_main._serve 注入 emit / shutdown_event /
# capabilities 后由主循环驱动）
worker_core = WorkerCore()


# 命令注册表：method 名 → 处理器（单一事实源，与 README 命令表逐项对应共 18 条；
# worker_main._INTERRUPTIBLE_METHODS 的豁免清单按"是否以 self._page 为执行目标"
# 与本表逐命令对照：execute_login_attempt / execute_browser_task / feedback_capture /
# debug_start / debug_step / debug_run_all / debug_stop 进白名单，其余命令或不用页面
#（health/ocr/shutdown）、或用自建临时浏览器（test_redirect / page_capture）、
# 或用独立上下文（profile_* / close_browser 自带内部超时收敛），不进白名单）
COMMANDS: dict[str, Callable] = {
    "worker_health_check": worker_core.handle_worker_health_check,
    "browser_health_check": worker_core.handle_browser_health_check,
    "test_redirect": worker_core.handle_test_redirect,
    "execute_login_attempt": worker_core.handle_execute_login_attempt,
    "execute_browser_task": worker_core.handle_execute_browser_task,
    "close_browser": worker_core.handle_close_browser,
    "debug_start": worker_core.handle_debug_start,
    "debug_step": worker_core.handle_debug_step,
    "debug_run_all": worker_core.handle_debug_run_all,
    "debug_stop": worker_core.handle_debug_stop,
    "debug_status": worker_core.handle_debug_status,
    "feedback_capture": worker_core.handle_feedback_capture,
    "page_capture": worker_core.handle_page_capture,
    "profile_session_start": worker_core.handle_profile_session_start,
    "profile_session_stop": worker_core.handle_profile_session_stop,
    "profile_auto_login": worker_core.handle_profile_auto_login,
    "ocr_recognize": worker_core.handle_ocr_recognize,
    "shutdown": worker_core.handle_shutdown,
}
