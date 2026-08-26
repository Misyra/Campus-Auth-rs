"""OCR 运行时：ddddocr 实例缓存、图片预处理与识别超时常量（WorkerCore 拆分）。

自 step_handlers.py 迁出（A 组重构）：OCR 逻辑原先横跨 step_handlers 与
playwright_worker 两个文件，归拢到单点便于测试与复用。本模块不依赖 Playwright。
"""

from __future__ import annotations

from io import BytesIO
from typing import Any

# OCR 实例缓存：字符范围会修改模型实例状态，必须纳入 key，避免上一次任务的
# ``set_ranges`` 污染后续未限制字符集的识别。
_ocr_cache: dict[tuple[bool, str | int | None], Any] = {}

# OCR 单次识别超时（秒）：覆盖模型加载 + CPU 推理，防止卡死导致任务无限阻塞
OCR_TIMEOUT_SECS = 90


def _get_ocr(old: bool, char_range: str | int | None = None):
    """获取（并缓存）ddddocr 实例。

    模型不存在时抛出 ImportError，由调用方转换为 WorkerError。
    """
    import ddddocr  # type: ignore

    # Rust 侧历史配置用 JSON Value 承载；若脏数据传入数组/对象，退回默认范围，
    # 避免不可哈希值让缓存查找本身崩溃。
    normalized_range = char_range if isinstance(char_range, (str, int)) else None
    key = (old, normalized_range)
    instance = _ocr_cache.get(key)
    if instance is None:
        instance = ddddocr.DdddOcr(old=old, show_ad=False)
        if normalized_range is not None:
            try:
                instance.set_ranges(normalized_range)
            except Exception as exc:  # noqa: BLE001 — 非法范围退回模型默认字符集
                logger.warning("[ocr] set_ranges(%s) 失败，使用默认范围: %s", normalized_range, exc)
        _ocr_cache[key] = instance
    return instance


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

