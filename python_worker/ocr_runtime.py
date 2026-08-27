"""OCR 运行时：ddddocr 实例缓存、图片预处理与共享超时预算。

自 step_handlers.py 迁出（A 组重构）：OCR 逻辑原先横跨 step_handlers 与
playwright_worker 两个文件，归拢到单点便于测试与复用。本模块不依赖 Playwright。
"""

from __future__ import annotations

import logging
import threading
import time
from io import BytesIO
from typing import Any

logger = logging.getLogger(__name__)

# OCR 实例缓存：字符范围会修改模型实例状态，必须纳入 key，避免上一次任务的
# ``set_ranges`` 污染后续未限制字符集的识别。
_ocr_cache: dict[tuple[bool, str | int | None], Any] = {}
_ocr_lock = threading.Lock()

# OCR 模型获取 + CPU 推理共享总预算（秒）。
# step_handlers 仍负责 DOM 等待与步骤级 timeout；这里专门防止冷启动 90s 后
# 推理阶段再额外获得完整 90s，导致单次 OCR 的 CPU 阶段上限翻倍。
OCR_TIMEOUT_SECS = 90


class _OcrSession:
    """单次 OCR 调用会话，为推理保留模型获取后的剩余预算。

    ``step_handlers`` 会把 ``classification`` 放入 ``asyncio.to_thread``。Python
    无法安全终止一个已经进入第三方 native/CPU 代码的线程，因此这里再用 daemon
    线程包一层：到达共享预算后立即把控制权交还调用方，并淘汰对应缓存实例。
    即使底层识别稍后才自行结束，后续任务也不会复用这个可能仍在工作的实例。
    """

    def __init__(
        self,
        instance: Any,
        key: tuple[bool, str | int | None],
        inference_timeout_secs: float,
    ) -> None:
        self._instance = instance
        self._key = key
        self._inference_timeout_secs = max(0.0, inference_timeout_secs)

    def classification(self, img_bytes: bytes):
        """在本次剩余共享预算内执行识别。"""
        if self._inference_timeout_secs <= 0:
            _evict_ocr_instance(self._key, self._instance)
            raise TimeoutError("OCR 模型获取已耗尽共享预算")

        done = threading.Event()
        result: dict[str, Any] = {}

        def run() -> None:
            try:
                result["value"] = self._instance.classification(img_bytes)
            except Exception as exc:  # noqa: BLE001 — 原样回传第三方识别错误
                result["error"] = exc
            finally:
                done.set()

        worker = threading.Thread(target=run, name="ocr-classification", daemon=True)
        worker.start()
        if not done.wait(self._inference_timeout_secs):
            _evict_ocr_instance(self._key, self._instance)
            raise TimeoutError(
                f"OCR 模型获取与推理超过共享预算 {OCR_TIMEOUT_SECS}s"
            )

        error = result.get("error")
        if error is not None:
            raise error
        return result.get("value")


def _evict_ocr_instance(
    key: tuple[bool, str | int | None], instance: Any
) -> None:
    """仅当缓存仍指向指定实例时淘汰，避免误删并发创建的新实例。"""
    with _ocr_lock:
        if _ocr_cache.get(key) is instance:
            _ocr_cache.pop(key, None)


def _get_ocr(old: bool, char_range: str | int | None = None):
    """获取缓存实例并创建一次调用级 OCR 会话。

    模型不存在时抛出 ImportError，由调用方转换为 WorkerError。模型获取实际耗时会
    从 ``OCR_TIMEOUT_SECS`` 中扣除，返回会话的 ``classification`` 只能使用剩余预算。
    """
    started = time.monotonic()
    import ddddocr  # type: ignore

    # Rust 侧历史配置用 JSON Value 承载；若脏数据传入数组/对象，退回默认范围，
    # 避免不可哈希值让缓存查找本身崩溃。
    normalized_range = char_range if isinstance(char_range, (str, int)) else None
    key = (old, normalized_range)
    instance = _ocr_cache.get(key)
    if instance is None:
        with _ocr_lock:
            instance = _ocr_cache.get(key)
            if instance is None:
                instance = ddddocr.DdddOcr(old=old, show_ad=False)
                if normalized_range is not None:
                    try:
                        instance.set_ranges(normalized_range)
                    except Exception as exc:  # noqa: BLE001 — 非法范围退回模型默认字符集
                        logger.warning(
                            "[ocr] set_ranges(%s) 失败，使用默认范围: %s",
                            normalized_range,
                            exc,
                        )
                _ocr_cache[key] = instance

    elapsed = time.monotonic() - started
    return _OcrSession(instance, key, OCR_TIMEOUT_SECS - elapsed)


def _preprocess_ocr_image(img_bytes: bytes) -> bytes:
    """将验证码图片规整为 ddddocr 友好的 RGB 字节流。

    站点验证码多为彩色/带噪点，而 Playwright 截图是 **RGBA**（含透明通道）。
    ddddocr 的 ``classification`` 对 alpha 通道敏感，直接将 RGBA 喂入常导致
    误识别（例如透明区域被当作黑色边角字符）。这里统一：

    - RGBA / LA / 带透明度的调色板图：透明白底合成转 RGB；
    - 其余（P/CMYK/YCbCr 等）：直接转 RGB；
    - 解析失败（webp/avif 等 PIL 不支持的格式）则退回原图，交由 ddddocr 自行处理。

    不做强制缩放：ddddocr 内部已做归一化，保留原分辨率反而保真。
    """
    try:
        from PIL import Image  # type: ignore
    except ImportError:
        return img_bytes
    try:
        img = Image.open(BytesIO(img_bytes))
        if img.mode in ("RGBA", "LA") or (img.mode == "P" and "transparency" in img.info):
            background = Image.new("RGB", img.size, (255, 255, 255))
            alpha = img.convert("RGBA").split()[-1]
            background.paste(img.convert("RGBA"), mask=alpha)
            img = background
        else:
            img = img.convert("RGB")
        out = BytesIO()
        img.save(out, format="PNG")
        return out.getvalue()
    except Exception:  # noqa: BLE001 — 预处理失败不致命，回退原图
        return img_bytes
