"""OCR 识别命令：ocr_recognize 编排（WorkerCore 拆分）。

自 playwright_worker.py 迁出（Mixin 化）：方法签名与行为逐字段保留，经
WorkerCore 多继承挂回（见 playwright_worker.py 的组装）。模型加载 / 图片预处理 /
推理超时等运行时逻辑属 ocr_runtime（纯逻辑层），本模块只承载命令编排：
总预算（模型加载 + 推理共享 OCR_TIMEOUT_SECS）内的两阶段超时与取消响应。
"""

from __future__ import annotations

import asyncio
import base64
import logging
import time
from typing import Any

from models import Outcome
from step_handlers import StepCancelled, WorkerError
from cancel_registry import cancel_registry
from ocr_runtime import OCR_TIMEOUT_SECS, _get_ocr, _preprocess_ocr_image

logger = logging.getLogger(__name__)


class OcrCommandsMixin:
    """OCR 识别命令（WorkerCore 的 ocr 职责）。"""

    async def handle_ocr_recognize(self, params: dict) -> dict:
        """识别 base64 图片中的文本（ddddocr），模型加载与推理共享总超时预算。

        注册 cancel_id：Rust 侧 /api/ocr/uninstall 据此取消在途识别（Windows 上
        onnxruntime DLL 被占用会导致 uv remove 失败），模型加载与推理阶段均响应取消。
        """
        cancel_id = params.get("cancel_id", "")
        cancel_event = cancel_registry.register(cancel_id) if cancel_id else None
        try:
            return await self._ocr_recognize_impl(params, cancel_event)
        finally:
            # finally 而非 except Exception：命令级超时的 CancelledError 也要注销
            if cancel_id:
                cancel_registry.unregister(cancel_id)

    async def _ocr_recognize_impl(self, params: dict, cancel_event: Any) -> dict:
        image_base64 = params.get("image_base64", "")
        if not image_base64:
            raise WorkerError(Outcome.UNKNOWN_ERROR, "ocr_recognize 缺少 image_base64")

        def _ensure_not_cancelled(stage: str) -> None:
            if cancel_event is not None and cancel_event.is_set():
                raise StepCancelled(f"OCR 识别已取消（{stage}）")

        deadline = time.monotonic() + OCR_TIMEOUT_SECS

        def remaining_timeout() -> float:
            return max(0.0, deadline - time.monotonic())

        _ensure_not_cancelled("模型加载前")
        try:
            # 模型构造（DdddOcr()）同步加载 onnx 模型，可能首次加载较慢。
            # 与后续 classification 共用 OCR_TIMEOUT_SECS 总预算，避免两阶段各吃满一次超时。
            remaining = remaining_timeout()
            if remaining <= 0:
                raise asyncio.TimeoutError
            ocr = await asyncio.wait_for(
                asyncio.to_thread(_get_ocr, bool(params.get("old", False))),
                timeout=remaining,
            )
        except asyncio.TimeoutError:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"OCR 处理超时（>{OCR_TIMEOUT_SECS}s，模型加载阶段）。"
                "若持续超时请检查 OCR 依赖是否完整",
            ) from None
        except WorkerError:
            raise
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"ddddocr 未安装或加载失败: {exc}。请在设置页安装 OCR 依赖后重试",
            ) from exc
        _ensure_not_cancelled("推理前")
        try:
            img_bytes = base64.b64decode(image_base64)
        except Exception as exc:  # noqa: BLE001
            raise WorkerError(Outcome.UNKNOWN_ERROR, f"图片解码失败: {exc}") from exc
        # 截图/上传图片可能是 RGBA，先规整为 ddddocr 友好的 RGB，提升识别准确率
        img_bytes = _preprocess_ocr_image(img_bytes)
        # classification 是同步 CPU 推理，丢到线程池避免阻塞事件循环；
        # 只使用模型加载后的剩余预算，保证单次 OCR 的墙钟上限稳定。
        try:
            remaining = remaining_timeout()
            if remaining <= 0:
                raise asyncio.TimeoutError
            text = await asyncio.wait_for(
                asyncio.to_thread(ocr.classification_with_timeout, img_bytes, remaining),
                timeout=remaining,
            )
        except asyncio.TimeoutError:
            raise WorkerError(
                Outcome.UNKNOWN_ERROR, f"OCR 处理超时（>{OCR_TIMEOUT_SECS}s）"
            ) from None
        except Exception as exc:  # noqa: BLE001 — ddddocr/PIL 图片不可识别等，转为一句话
            raise WorkerError(
                Outcome.UNKNOWN_ERROR,
                f"无法识别该图片: {exc}。请使用清晰的标准图片（png/jpg），"
                "避免 webp/avif/截图边缘裁剪等 PIL 不支持的格式",
            ) from exc
        logger.info("[ocr] 识别完成，结果长度=%d", len(text or ""))
        return {"text": text}


__all__ = ["OcrCommandsMixin"]
