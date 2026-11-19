"""捕获与探测命令：test_redirect / page_capture / feedback_capture（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。共性纪律：

- **临时浏览器自包含**：test_redirect 与 page_capture 不得复用 ``self._context``
  ——keep_alive 常驻登录态依赖 Cookie 与心跳页全保留，复用会产生「已登录
  Cookie 掩盖门户」的误判或破坏常驻会话；feedback_capture 只读当前页；
- **产物脱敏**：``_sanitize_capture_url`` 剥 URL query/fragment（门户临时
  token），``_classify_redirect_test`` 的返回值刻意不含最终 URL 与页面正文；
- 结构提取与资源快照委托 capture_kit（纯逻辑）。

补丁语义约定：``_REDIRECT_TEST_SETTLE_SECS`` 属本模块（属主）；
``_capture_dir`` 属 worker_paths，经**模块属性访问**调用。
"""

from __future__ import annotations

import asyncio
import json
import logging
import shutil
import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any, AsyncIterator
from urllib.parse import urlsplit, urlunsplit

from models import Outcome
from step_handlers import StepCancelled, WorkerError, _suppress_task
import worker_paths
from browser_launch import _STEALTH_INIT_SCRIPT, _channel_supports_cdp, _nav_timeout
from browser_lifecycle import _WAIT_TIMEOUT_SECS
from cancel_registry import cancel_registry
from capture_kit import (
    _CAPTURE_MAX_SCREENSHOT_BYTES,
    _capture_page_resources,
    _capture_page_structure,
    _rewrite_resource_urls,
)

logger = logging.getLogger(__name__)

# 重定向检测首导航完成后继续观察 JS / Meta Refresh / 弹窗跳转的时长（秒）。
_REDIRECT_TEST_SETTLE_SECS = 5.0
# 重定向检测最多读取的可见文本，避免异常页面造成无界 IPC 前内存增长。
_REDIRECT_TEST_TEXT_LIMIT = 64 * 1024


def _sanitize_capture_url(url: str) -> str:
    """剥掉 URL 的 query/fragment，仅保留 scheme + host + path。

    门户登录后的最终 URL 常携带 ``token=`` / ``sid=`` 等临时会话凭证。
    ``page_capture`` 的 final_url 会写入常驻磁盘的 meta.json 并经 IPC 回传
    （进入 AI 提示词链路与 bundle 下载），与 ``_classify_redirect_test``
    「返回值刻意不包含最终 URL query」同一纪律，在写入前统一脱敏。
    """
    try:
        parts = urlsplit(url)
    except ValueError:
        return url
    if not parts.query and not parts.fragment:
        return url
    return urlunsplit((parts.scheme, parts.netloc, parts.path, "", ""))


def _classify_redirect_test(
    *,
    trigger_url: str,
    final_url: str,
    response_status: int | None,
    visible_text: str,
    password_inputs: int,
    account_inputs: int,
    forms: int,
) -> dict[str, Any]:
    """按地址变化、表单结构和页面语义综合判断重定向检测结果。

    单独出现“登录”很常见，不能直接判为校园网认证页；只有强校园网语义，或
    地址变化与登录结构/多项关键字相互印证时才返回 ``detected``。
    返回值刻意不包含最终 URL 或页面正文，避免门户临时 token 与账号信息进入 IPC。
    """
    text = " ".join(visible_text.lower().split())
    strong_keywords = (
        "校园网",
        "上网认证",
        "网络认证",
        "统一身份认证",
        "captive portal",
        "network authentication",
    )
    generic_keywords = (
        "登录",
        "认证",
        "账号",
        "用户名",
        "密码",
        "运营商",
        "portal",
        "sign in",
        "log in",
        "username",
        "password",
    )
    strong_hits = sum(1 for keyword in strong_keywords if keyword in text)
    generic_hits = sum(1 for keyword in generic_keywords if keyword in text)

    try:
        trigger = urlsplit(trigger_url)
        final = urlsplit(final_url)
        origin_changed = (
            trigger.scheme.lower(),
            (trigger.hostname or "").lower(),
            trigger.port,
        ) != (
            final.scheme.lower(),
            (final.hostname or "").lower(),
            final.port,
        )
        url_changed = trigger_url.rstrip("/") != final_url.rstrip("/")
    except ValueError:
        origin_changed = False
        url_changed = trigger_url != final_url

    # Windows NCSI 默认触发页在线时返回这段固定文本；204 同样属于直通在线。
    online_marker = "microsoft connect test" in text and len(text) < 512
    structural_score = min(password_inputs, 1) * 3
    structural_score += min(account_inputs, 1)
    structural_score += 1 if forms and (password_inputs or account_inputs) else 0
    redirect_score = 2 if origin_changed else (1 if url_changed else 0)
    semantic_score = min(generic_hits, 3) + min(strong_hits, 1) * 2
    score = structural_score + redirect_score + semantic_score

    detected = (
        score >= 5
        or (strong_hits > 0 and generic_hits >= 2)
        or (password_inputs > 0 and generic_hits > 0)
        or (origin_changed and structural_score >= 2 and generic_hits > 0)
    )
    if detected:
        return {"status": "detected"}
    if response_status == 204 or online_marker:
        return {"status": "online"}
    return {"status": "not_detected"}


class CaptureCommandsMixin:
    """捕获与探测命令（WorkerCore 的临时浏览器捕获职责）。"""

    @staticmethod
    async def _inspect_redirect_test_page(
        page: Any,
        trigger_url: str,
        response_status: int | None,
    ) -> dict[str, Any]:
        """读取主页面与 iframe 的有限可见信号，不回传原文或最终地址。"""
        text_parts: list[str] = []
        password_inputs = 0
        account_inputs = 0
        forms = 0
        remaining = _REDIRECT_TEST_TEXT_LIMIT
        for frame in list(page.frames):
            if remaining <= 0:
                break
            try:
                body_text = await frame.locator("body").inner_text(timeout=1500)
                if body_text:
                    clipped = body_text[:remaining]
                    text_parts.append(clipped)
                    remaining -= len(clipped)
            except Exception:  # noqa: BLE001 — 跨域/销毁中的 frame 可能无法读取，其余 frame 仍可判断
                pass
            try:
                password_inputs += await frame.locator("input[type='password']").count()
                account_inputs += await frame.locator(
                    "input[type='text'], input[type='email'], input[name*='user' i], "
                    "input[name*='account' i], input[name*='phone' i]"
                ).count()
                forms += await frame.locator("form").count()
            except Exception:  # noqa: BLE001 — 单个 frame 销毁不应让整次检测失败
                pass
        try:
            title = await page.title()
        except Exception:  # noqa: BLE001
            title = ""
        return _classify_redirect_test(
            trigger_url=trigger_url,
            final_url=str(getattr(page, "url", "") or ""),
            response_status=response_status,
            visible_text="\n".join([title, *text_parts]),
            password_inputs=password_inputs,
            account_inputs=account_inputs,
            forms=forms,
        )

    async def handle_test_redirect(self, params: dict) -> dict:
        """用独立、可见的临时浏览器验证触发地址能否到达校园网认证页。

        检测不得复用 ``self._context``：正常登录可能正在保留 Cookie、localStorage
        或门户心跳页面；复用会产生“已登录 Cookie 掩盖门户”的误判，也可能破坏
        keep_alive 会话。这里始终另启非持久化浏览器，完成后只回收自己的资源。
        """
        self._ensure_no_debug_session("重定向检测")
        trigger_url = str(params.get("trigger_url") or "").strip()
        if not trigger_url.startswith(("http://", "https://")):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "重定向检测仅支持 http/https 地址")

        from playwright.async_api import async_playwright

        bs = dict(params.get("browser_settings") or {})
        # 默认直连以观察校园网劫持；用户可单独允许检测使用代理，不修改原配置。
        policy = bs.get("network_policy")
        use_proxy = policy.get("mode") != "direct" if isinstance(policy, dict) else bool(bs.get("redirect_test_use_proxy", False))
        if not use_proxy:
            bs.pop("bind_proxy", None)
        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        pure_mode = bool(bs.get("pure_mode", False))
        cancel_id = str(params.get("cancel_id") or "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        created_playwright = False
        browser: Any = None
        context: Any = None
        response_status: int | None = None
        self._cancel_browser_idle_release()
        try:
            if self._playwright is None:
                self._playwright = await async_playwright().start()
                created_playwright = True
            launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
            # Chromium 默认继承系统代理；关闭开关时须显式绕过，纯净模式同样生效。
            if not use_proxy and self._is_chromium_channel(bs, channel):
                launch_args.append("--no-proxy-server")
            # 用户明确要求看到真实跳转过程：无论全局 headless 设置如何，检测恒为可见窗口。
            browser = await self._launch_browser(
                self._playwright,
                channel,
                custom_path,
                False,
                launch_args,
                bs,
            )
            context = await browser.new_context(**self._build_context_options(bs))
            if not pure_mode and bs.get("stealth_mode", False):
                custom = str(bs.get("stealth_custom_script") or "").strip()
                await context.add_init_script(custom or _STEALTH_INIT_SCRIPT)

            pages: list[Any] = []
            context.on("page", lambda new_page: pages.append(new_page))
            page = await context.new_page()
            if page not in pages:
                pages.append(page)
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled("重定向检测已取消")
            # goto 放入独立任务并轮询取消事件（与 _dispatch_guarded 同款）：
            # 挂死的 goto 只靠 task.cancel() 无法打断 CDP await，但取消能在
            # 轮询间隙即时生效并放弃该任务，不再被动等满 goto 上限
            # min(nav_timeout, 15s)（P2-4）。
            goto_task = asyncio.ensure_future(
                page.goto(
                    trigger_url,
                    wait_until="domcontentloaded",
                    timeout=min(_nav_timeout(bs), 15000),
                )
            )
            response: Any = None
            try:
                while not goto_task.done():
                    if cancel_event is not None and cancel_event.is_set():
                        goto_task.cancel()
                        await _suppress_task(goto_task)
                        raise StepCancelled("重定向检测已取消")
                    await asyncio.wait({goto_task}, timeout=0.1)
                response = goto_task.result()
                if response is not None:
                    response_status = response.status
            except StepCancelled:
                raise
            except Exception as exc:  # noqa: BLE001 — 导航失败属于“无法跟随”，不是 Worker 故障
                logger.info("重定向检测导航失败: %s", type(exc).__name__)
                return {"status": "not_detected"}
            finally:
                # 命令级超时等路径取消本协程时，弃掉的 goto 任务在此兜底回收
                if not goto_task.done():
                    goto_task.cancel()

            deadline = time.monotonic() + _REDIRECT_TEST_SETTLE_SECS
            while time.monotonic() < deadline:
                if cancel_event is not None and cancel_event.is_set():
                    raise StepCancelled("重定向检测已取消")
                await asyncio.sleep(0.25)

            active_page = page
            for candidate in reversed(pages):
                try:
                    if not candidate.is_closed():
                        active_page = candidate
                        break
                except Exception:  # noqa: BLE001
                    continue
            return await self._inspect_redirect_test_page(
                active_page,
                trigger_url,
                response_status,
            )
        finally:
            if cancel_id:
                cancel_registry.unregister(cancel_id)
            if context is not None:
                try:
                    await asyncio.wait_for(context.close(), timeout=_WAIT_TIMEOUT_SECS)
                except Exception:  # noqa: BLE001 — 临时窗口关闭失败不覆盖检测结论
                    logger.warning("关闭重定向检测上下文失败", exc_info=True)
            if browser is not None:
                try:
                    await asyncio.wait_for(browser.close(), timeout=_WAIT_TIMEOUT_SECS)
                except Exception:  # noqa: BLE001
                    logger.warning("关闭重定向检测浏览器失败", exc_info=True)
            if created_playwright and self._browser is None and self._context is None:
                try:
                    await self._playwright.stop()
                except Exception:  # noqa: BLE001
                    logger.debug("停止临时 Playwright 连接失败", exc_info=True)
                self._playwright = None
            elif self._browser is not None or self._context is not None:
                # 空闲回收武装口径与 handle_page_capture finally 的无条件武装
                # 刻意并存（终态等价，非行为分叉）：此处常驻浏览器在用时才重新
                # 武装——入口已取消旧计时，常驻不在用时没有可回收对象，武装只是
                # 空跑；page_capture 按防御纵深无条件武装，计时器触发时自复查
                # 常驻状态（_browser_idle_release 的空态直退分支）同样无害
                self._arm_browser_idle_release()

    @asynccontextmanager
    async def _temp_capture_browser(self, bs: dict) -> AsyncIterator[Any]:
        """独立非持久化临时浏览器（page_capture 专用）：browser + context + page 自包含。

        纪律同 ``handle_test_redirect``：**不得复用 ``self._context``**——keep_alive
        常驻登录态恰恰依赖 Cookie 与心跳页全保留（close_browser preserve_state），
        而共享会话路径的 clear_cookies / 关旧页会静默摧毁它；且捕获对象本就是
        “未登录门户”，不该携带常驻 Cookie。退出时整体关闭丢弃，仅当 Playwright
        连接为本命令新建且常驻浏览器不在使用时才停止连接。
        """
        from playwright.async_api import async_playwright

        channel = str(bs.get("browser_channel") or "playwright").strip().lower()
        custom_path = str(bs.get("browser_custom_path") or "").strip()
        pure_mode = bool(bs.get("pure_mode", False))
        headless = bs.get("headless", True)
        # 对齐 _start_browser：未绑定代理即绕过系统代理（纯净模式只关闭可选定制，
        # 不关闭直连保障），否则检测直连成功后捕获仍可能被失效代理阻断
        launch_args = [] if pure_mode else self._build_launch_args(bs, channel)
        if self._bypass_system_proxy(bs) and self._is_chromium_channel(bs, channel):
            if "--no-proxy-server" not in launch_args:
                launch_args.append("--no-proxy-server")

        created_playwright = False
        browser: Any = None
        context: Any = None
        try:
            if self._playwright is None:
                self._playwright = await async_playwright().start()
                created_playwright = True
            try:
                browser = await self._launch_browser(
                    self._playwright, channel, custom_path, headless, launch_args, bs,
                )
                context = await browser.new_context(**self._build_context_options(bs))
            except Exception as exc:  # noqa: BLE001 — 缺 chromium / 自定义路径不存在等
                # 保留原始异常细节（可执行文件路径等），包装为可诊断的中文错误
                raise WorkerError(
                    Outcome.UNKNOWN_ERROR, f"启动临时捕获浏览器失败: {exc}"
                ) from exc
            if not pure_mode:
                # 对齐 _apply_stealth_and_routes（作用于临时 context，不触碰常驻会话）
                if bs.get("low_resource_mode", False):
                    await context.route("**/*", self._handle_low_resource_request)
                if bs.get("stealth_mode", False):
                    custom = (bs.get("stealth_custom_script") or "").strip()
                    await context.add_init_script(custom or _STEALTH_INIT_SCRIPT)

            async def _auto_accept(dialog: Any) -> None:
                # 门户加载弹窗会阻塞导航与取 DOM：就地自动确认。不走共享会话的
                # dialog 事件通道，避免污染 _task_dialogs 与前端事件流
                try:
                    await dialog.accept()
                except Exception as exc:  # noqa: BLE001 — 弹窗可能已在间隙消失
                    logger.debug(f"临时捕获页弹窗确认失败（忽略）: {exc}")

            page = await context.new_page()
            page.on("dialog", lambda d: asyncio.ensure_future(_auto_accept(d)))
            yield page
        finally:
            if context is not None:
                try:
                    await asyncio.wait_for(context.close(), timeout=_WAIT_TIMEOUT_SECS)
                except Exception:  # noqa: BLE001 — 临时上下文关闭失败不覆盖捕获结论
                    logger.warning("关闭页面捕获临时上下文失败", exc_info=True)
            if browser is not None:
                try:
                    await asyncio.wait_for(browser.close(), timeout=_WAIT_TIMEOUT_SECS)
                except Exception:  # noqa: BLE001
                    logger.warning("关闭页面捕获临时浏览器失败", exc_info=True)
            if created_playwright and self._browser is None and self._context is None:
                try:
                    await self._playwright.stop()
                except Exception:  # noqa: BLE001
                    logger.debug("停止临时 Playwright 连接失败", exc_info=True)
                self._playwright = None

    @staticmethod
    async def _capture_mhtml(page: Any, target: Path) -> bool:
        """经 CDP 抓取完整布局 MHTML 快照并写入 target，成功返回 True。

        MHTML 单文件自包含样式/图片，供"保存页面文件"离线还原；Chromium 按
        设计不含 JS，脚本由 resources/ 补齐。捕获失败或快照为空时返回 False，
        不影响其余产物；detach 失败只记录日志，不能覆盖已经成功写盘的快照。
        """
        cdp = None
        captured = False
        try:
            cdp = await page.context.new_cdp_session(page)
            mhtml = await cdp.send("Page.captureSnapshot", {"format": "mhtml"})
            cdp_data = mhtml.get("data", "")
            if cdp_data:
                payload = cdp_data.encode("utf-8") if isinstance(cdp_data, str) else bytes(cdp_data)
                target.write_bytes(payload)
                captured = True
        except Exception as exc:  # noqa: BLE001 — MHTML 失败不影响其余产物
            logger.debug("MHTML 快照失败（跳过）: %s", exc)
        finally:
            if cdp is not None:
                try:
                    await cdp.detach()
                except Exception as exc:  # noqa: BLE001 — detach 失败不能覆盖已成功的快照
                    logger.debug("MHTML CDP 会话释放失败（忽略）: %s", exc)
        return captured

    @staticmethod
    def _write_capture_meta(
        cap_dir: Path,
        url: str,
        final_url: str,
        title: str,
        resources: dict[str, str],
        structure_summary: dict[str, int],
        *,
        mhtml_ok: bool,
        offline_ok: bool,
        note: str | None,
    ) -> dict[str, Any]:
        """构建并落盘 captures/latest/meta.json，返回 meta 供响应组装。

        资源目录仅在确有产物时写入路径；note 为空不写字段，保持 meta 结构
        与消费方（Rust 读盘侧）的既有契约一致。``mhtml_path`` / 离线副本路径
        同为可选字段：非 Chromium 渠道没有 MHTML，无资源时也没有离线副本。
        """
        meta: dict[str, Any] = {
            "captured_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "request_url": url,
            # final_url 统一剥 query/fragment：门户重定向后的 URL 常携带
            # 临时会话凭证（token= 等），此处是 meta 落盘与 IPC 响应的单一出口
            "final_url": _sanitize_capture_url(final_url),
            "title": title,
            "html_path": str(cap_dir / "page.html"),
            "screenshot_path": str(cap_dir / "screenshot.png"),
            "resources_dir": str(cap_dir / "resources") if resources else None,
            "resources_count": len(resources),
            "structure_path": str(cap_dir / "page_structure.json"),
            "structure_summary": structure_summary,
        }
        if mhtml_ok:
            meta["mhtml_path"] = str(cap_dir / "page.mhtml")
        if offline_ok:
            meta["offline_html_path"] = str(cap_dir / "page.offline.html")
        if note:
            meta["note"] = note
        (cap_dir / "meta.json").write_text(
            json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        return meta

    async def handle_page_capture(self, params: dict) -> dict:
        """导航到目标页面并落盘 HTML / CSS-JS 资源 / 全页截图（供 AI 任务生成）。

        与 ``feedback_capture`` 的差异：本命令自带导航（无活跃页面也可用），
        产物固定写到 ``captures/latest/``（每次覆盖），内容只经 Rust 侧读盘消费，
        不走 IPC 回传——NDJSON 单行上限 1 MiB，门户页 HTML/JS 普遍超限。
        会话语义：**独立非持久化临时浏览器**（纪律同 ``handle_test_redirect``，
        不得复用 ``self._context``）——捕获“未登录门户”既不该带常驻 Cookie，
        也不得清 Cookie / 关旧页破坏 keep_alive 常驻登录态；捕获完成后整体关闭
        丢弃，不占用常驻会话槽位。存在活跃调试会话时直接拒绝。
        """
        url = str(params.get("url") or "").strip()
        if not url:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "page_capture 缺少 url")
        if not url.startswith(("http://", "https://")):
            raise WorkerError(Outcome.UNKNOWN_ERROR, "仅支持 http/https 页面捕获")
        if self._debug_sessions:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "存在活跃调试会话，请先停止调试再捕获")
        bs = params.get("browser_settings", {}) or {}
        cancel_id = params.get("cancel_id", "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None

        def _ensure_not_cancelled(stage: str) -> None:
            """阶段边界取消检查（对齐 debug_start 的分阶段模式）。"""
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"页面捕获已取消（{stage}）")

        # 与 test_redirect 一致：捕获类命令先取消待触发的浏览器空闲回收
        self._cancel_browser_idle_release()
        try:
            _ensure_not_cancelled("临时浏览器启动前")
            async with self._temp_capture_browser(bs) as page:
                await self._navigate(page, url, _nav_timeout(bs))
                # 门户页常在加载后异步拉验证码/配置脚本，导航后等待一轮
                # networkidle 让 DOM 与已加载资源尽量齐全；长轮询页面等满超时
                # 即按当前状态继续（不致命）
                try:
                    await page.wait_for_load_state("networkidle", timeout=8000)
                except Exception:  # noqa: BLE001
                    logger.debug("networkidle 等待超时，按当前页面状态继续捕获")
                if cancel_event is not None and cancel_event.is_set():
                    raise StepCancelled("页面捕获已取消")
                # 落盘目录与 _debug_screenshot_dir 同语义：锚定运行时 worker 工程目录，
                # 不依赖进程 CWD；固定 captures/latest 每次覆盖，避免产物无界堆积
                _ensure_not_cancelled("HTML 获取前")
                cap_dir = worker_paths._capture_dir()
                shutil.rmtree(cap_dir, ignore_errors=True)
                cap_dir.mkdir(parents=True, exist_ok=True)
                html = await page.content()
                (cap_dir / "page.html").write_text(html, encoding="utf-8")
                _ensure_not_cancelled("截图前")
                png_bytes = await page.screenshot(full_page=True)
                screenshot_note: str | None = None
                if len(png_bytes) > _CAPTURE_MAX_SCREENSHOT_BYTES:
                    png_bytes = await page.screenshot(full_page=False)
                    screenshot_note = "全页截图过大，已改为当前视口截图"
                (cap_dir / "screenshot.png").write_bytes(png_bytes)
                _ensure_not_cancelled("结构提取前")
                try:
                    structure = await _capture_page_structure(page)
                except Exception as exc:  # noqa: BLE001 — 原始 HTML 仍可作为生成兜底
                    structure = {"version": 1, "frames": [], "error": str(exc)[:300]}
                (cap_dir / "page_structure.json").write_text(
                    json.dumps(structure, ensure_ascii=False, indent=2), encoding="utf-8"
                )
                frame_items = structure.get("frames", [])
                structure_summary = {
                    "frames": len(frame_items),
                    "forms": sum(len(item.get("forms", [])) for item in frame_items),
                    "controls": sum(len(item.get("controls", [])) for item in frame_items),
                    "captcha_candidates": sum(
                        len(item.get("captcha_candidates", [])) for item in frame_items
                    ),
                }
                mhtml_ok = False
                # MHTML 走 CDP，非 Chromium 渠道下调用必然失败：直接跳过，降级说明由
                # 资源快照那条统一给出（避免日志里出现无意义的英文 CDP 异常）
                if _channel_supports_cdp(bs):
                    mhtml_ok = await self._capture_mhtml(page, cap_dir / "page.mhtml")
                _ensure_not_cancelled("资源快照前")
                resources: dict[str, str] = {}
                aliases: dict[str, str] = {}
                note: str | None = None
                try:
                    resources, aliases, note = await _capture_page_resources(
                        page, cap_dir / "resources", bs=bs
                    )
                except Exception as exc:  # noqa: BLE001 — 资源快照失败不阻断 HTML/截图
                    note = f"资源快照失败: {exc}"
                _ensure_not_cancelled("离线副本生成前")
                # 离线副本：把 HTML 中的资源引用改写成 resources/ 相对路径，解压后直接
                # 双击即可还原（无 MHTML 的渠道下这是唯一可离线查看的形态）。原始
                # page.html 保持不变——它是 Rust 侧喂给 LLM 的材质，URL 语义不该被污染。
                offline_ok = False
                if resources:
                    try:
                        offline_html = _rewrite_resource_urls(html, {**resources, **aliases})
                        (cap_dir / "page.offline.html").write_text(offline_html, encoding="utf-8")
                        offline_ok = True
                    except Exception as exc:  # noqa: BLE001 — 离线副本失败不影响其余产物
                        extra = f"离线副本生成失败: {exc}"
                        note = f"{note}；{extra}" if note else extra
                if screenshot_note:
                    note = f"{note}；{screenshot_note}" if note else screenshot_note
                try:
                    title = await page.title()
                except Exception:  # noqa: BLE001 — 页面标题读取失败不致命
                    title = ""
                meta = self._write_capture_meta(
                    cap_dir,
                    url,
                    page.url,
                    title,
                    resources,
                    structure_summary,
                    mhtml_ok=mhtml_ok,
                    offline_ok=offline_ok,
                    note=note,
                )
                logger.info(
                    "[capture] 页面捕获完成: %s (html=%d chars, resources=%d)",
                    meta["final_url"],
                    len(html),
                    len(resources),
                )
                return {
                    "final_url": meta["final_url"],
                    "title": title,
                    "html_chars": len(html),
                    "png_bytes": len(png_bytes),
                    "resources_count": len(resources),
                    "structure_summary": structure_summary,
                    "note": note,
                }
        finally:
            # finally 而非 except Exception：命令级超时的 task.cancel() 产生
            # CancelledError（BaseException），except Exception 拦不住会跳过注销。
            # 无条件武装与 handle_test_redirect 的「常驻在用时才武装」刻意并存：
            # 防御纵深优先，不依赖调用方判断常驻浏览器是否在用——计时器触发时
            # 自复查常驻状态（_browser_idle_release 的空态直退分支），常驻不在用
            # 时只是空跑一轮计时任务，无副作用
            self._arm_browser_idle_release()
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def handle_feedback_capture(self, params: dict) -> dict:
        """捕获当前调试页面的完整 MHTML、截图与 CSS/JS 资源（供导出问题报告）。

        `page.content()` 仅含 HTML，外链 CSS/图片离线无法还原完整布局。
        能取到 CDP MHTML 时优先使用，否则回退 HTML；大内容统一落盘避免 IPC 1MiB 超限。
        Chromium 的 MHTML 序列化按设计不保存 JS（CSS 也只嵌内存缓存命中的部分），
        故额外经 Page.getResourceTree/getResourceContent 把已加载的脚本与样式表
        落盘到 resources/，并生成引用改写后的 page.html 供源码级离线还原。
        非 Chromium 渠道没有 CDP：MHTML 直接跳过，CSS/JS 由页面枚举 + HTTP 回补
        抓取（见 ``_capture_page_resources``），产物同样可用于离线还原。

        与 page_capture 对齐：注册 cancel_id 并在关键阶段检查取消；入口取消
        待触发的浏览器空闲回收；CDP detach 放入 finally，抓取失败也不泄漏会话。
        """
        if self._page is None:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "无活跃页面，无法捕获")
        cancel_id = str(params.get("cancel_id") or "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None

        def _ensure_not_cancelled(stage: str) -> None:
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"页面捕获已取消（{stage}）")

        # 与 test_redirect / page_capture 一致：捕获类命令先取消空闲自动释放
        self._cancel_browser_idle_release()
        try:
            _ensure_not_cancelled("开始前")
            # 落盘根目录先定（资源快照需要直接写入子目录），避免 IPC 1MiB 超限
            stamp = str(int(time.time() * 1000))
            fb_dir = worker_paths._feedback_capture_dir(stamp)
            # 会话用的浏览器设置决定 CDP 是否可用（MHTML 与资源快照都只存在于 Chromium）
            bs = self._last_browser_settings or {}
            # 尝试 MHTML（完整离线快照，含样式与图片），失败回退 HTML；
            # 非 Chromium 渠道不尝试：CDP 在协议层不存在，只会白记一条英文异常
            mhtml_bytes: bytes | None = None
            if _channel_supports_cdp(bs):
                cdp = None
                try:
                    cdp = await self._page.context.new_cdp_session(self._page)
                    mhtml = await cdp.send("Page.captureSnapshot", {"format": "mhtml"})
                    cdp_data = mhtml.get("data", "")
                    if cdp_data:
                        mhtml_bytes = (
                            cdp_data.encode("utf-8")
                            if isinstance(cdp_data, str)
                            else bytes(cdp_data)
                        )
                except Exception as exc:  # noqa: BLE001 — CDP 不可用时回退 content()
                    logger.debug("CDP MHTML 快照失败，回退 HTML: %s", exc)
                    mhtml_bytes = None
                finally:
                    # detach 必须无条件执行：抓取失败时也要释放 CDP 会话，
                    # 否则泄漏的会话会占住与页面的绑定直到页面对象销毁
                    if cdp is not None:
                        try:
                            await cdp.detach()
                        except Exception as exc:  # noqa: BLE001
                            logger.debug("MHTML CDP 会话释放失败（忽略）: %s", exc)
            _ensure_not_cancelled("MHTML 后")
            # CSS/JS 资源快照：MHTML 不含 JS，这里补齐脚本与样式表（全部 frame）
            resources: dict[str, str] = {}  # url -> resources/<name>
            aliases: dict[str, str] = {}  # HTML 里的原始书写形态 -> resources/<name>
            resource_note: str | None = None
            try:
                resources, aliases, resource_note = await _capture_page_resources(
                    self._page, fb_dir / "resources", bs=bs
                )
            except Exception as exc:  # noqa: BLE001 — 资源快照失败不影响其余产物
                resource_note = f"资源快照失败: {exc}"
            _ensure_not_cancelled("资源快照后")
            # 有资源时 HTML 也必须导出（MHTML 内无 JS，resources 需要引用方）；
            # MHTML 不可用时同样回退 HTML
            html: str | None = None
            if mhtml_bytes is None or resources:
                try:
                    html = _rewrite_resource_urls(
                        await self._page.content(), {**resources, **aliases}
                    )
                except Exception as exc:  # noqa: BLE001
                    raise WorkerError(Outcome.UNKNOWN_ERROR, f"获取页面内容失败: {exc}") from exc
            _ensure_not_cancelled("截图前")
            try:
                png_bytes = await self._page.screenshot(full_page=True)
            except Exception as exc:  # noqa: BLE001
                raise WorkerError(Outcome.UNKNOWN_ERROR, f"截图失败: {exc}") from exc
            _ensure_not_cancelled("落盘前")
            try:
                fb_dir.mkdir(parents=True, exist_ok=True)
                png_path = fb_dir / "screenshot.png"
                png_path.write_bytes(png_bytes)
                result: dict = {"png_path": str(png_path)}
                if mhtml_bytes is not None:
                    mhtml_path = fb_dir / "page.mhtml"
                    mhtml_path.write_bytes(mhtml_bytes)
                    result["mhtml_path"] = str(mhtml_path)
                if html is not None:
                    html_path = fb_dir / "page.html"
                    html_path.write_bytes(html.encode("utf-8"))
                    result["html_path"] = str(html_path)
                if resources:
                    result["resources_dir"] = str(fb_dir / "resources")
                    result["resources_count"] = len(resources)
                if resource_note:
                    result["resources_note"] = resource_note
                return result
            except Exception as exc:  # noqa: BLE001
                raise WorkerError(Outcome.UNKNOWN_ERROR, f"落盘失败: {exc}") from exc
        finally:
            # finally 而非 except Exception：命令级超时的 task.cancel() 产生
            # CancelledError（BaseException），except Exception 拦不住会跳过注销
            if cancel_id:
                cancel_registry.unregister(cancel_id)


__all__ = [
    "_REDIRECT_TEST_SETTLE_SECS",
    "_REDIRECT_TEST_TEXT_LIMIT",
    "_classify_redirect_test",
    "_sanitize_capture_url",
    "CaptureCommandsMixin",
]
