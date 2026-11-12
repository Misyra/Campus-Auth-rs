"""OCR 运行时：ddddocr 实例缓存、图片预处理与单次推理超时。

自 step_handlers.py 迁出（A 组重构）：OCR 逻辑原先横跨 step_handlers 与
playwright_worker 两个文件，归拢到单点便于测试与复用。本模块不依赖 Playwright。
"""

from __future__ import annotations

import logging
import threading
import time
from collections import OrderedDict
from io import BytesIO
from typing import Any

logger = logging.getLogger(__name__)

# OCR 缓存 key 的范围部分。字符范围会修改底层模型实例状态，必须纳入 key，
# 避免上一次任务的 ``set_ranges`` 污染后续未限制字符集的识别。
_OcrCacheKey = tuple[bool, str | int | None]

# OCR 缓存保存稳定的会话对象，上限 LRU 淘汰：每种合法自定义 char_range 各持有
# 一个模型实例（数十 MB/个），极端配置组合下必须有界（P2-6）。
_ocr_cache: "OrderedDict[_OcrCacheKey, _OcrSession]" = OrderedDict()
_ocr_lock = threading.Lock()
#: 缓存实例上限：超过即淘汰最久未使用的实例（下一次使用重新加载）。
_OCR_CACHE_MAX_ENTRIES = 4
#: 首次加载中的 key → 加载开始时刻（monotonic）。调用方超时不会终止已进入
#: 第三方 native 代码的加载线程（孤儿线程），标记让后续超时报错能区分
#: "模型正在首次加载"与"依赖不完整"，加载完成的日志也让恢复可见。
_ocr_load_started: dict[_OcrCacheKey, float] = {}


def _normalize_ocr_range(char_range: str | int | None) -> str | int | None:
    """把调用方传入的 char_range 归一为缓存 key 可用的范围值。

    空串/纯空白与"未指定"等价（``set_ranges("")`` 没有意义）；非 str/int 的脏
    数据（Rust 侧 JSON Value 可能传入数组/对象）退回默认范围，避免不可哈希值
    让缓存查找本身崩溃。
    """
    if isinstance(char_range, str):
        stripped = char_range.strip()
        return stripped or None
    if isinstance(char_range, int) and not isinstance(char_range, bool):
        return char_range
    return None

# 独立 OCR API 未提供更短截止时间时使用的默认 CPU 推理上限（秒）。浏览器步骤
# 由 step_handlers 传入步骤剩余预算，模型获取、DOM 与推理不会各拿一份完整预算。
OCR_TIMEOUT_SECS = 90


class _OcrSession:
    """可缓存的 OCR 会话，单次识别显式接收自己的超时预算。

    ``step_handlers`` 会把 ``classification`` 放入 ``asyncio.to_thread``。Python
    无法安全终止已经进入第三方 native/CPU 代码的线程，因此这里再用 daemon 线程
    包一层；单次预算耗尽时立即返回并淘汰该缓存会话，后续任务不会复用可能仍在
    工作的底层实例。
    """

    def __init__(self, instance: Any, key: tuple[bool, str | int | None]) -> None:
        self._instance = instance
        self._key = key

    def __getattr__(self, name: str) -> Any:
        """除受控 classification 外，其余属性透明委托给底层 ddddocr 实例。"""
        return getattr(self._instance, name)

    def classification(self, img_bytes: bytes) -> Any:
        """使用完整默认预算执行识别，兼容 ddddocr 的既有调用形式。"""
        return self.classification_with_timeout(img_bytes, float(OCR_TIMEOUT_SECS))

    def classification_with_timeout(
        self, img_bytes: bytes, inference_timeout_secs: float
    ) -> Any:
        """使用调用方本次显式提供的独立预算执行识别。"""
        inference_timeout_secs = max(0.0, float(inference_timeout_secs))
        if inference_timeout_secs <= 0:
            logger.warning("[ocr] 单次识别预算已耗尽，淘汰缓存会话: key=%s", self._key)
            _evict_ocr_session(self._key, self)
            raise TimeoutError("OCR 识别预算已耗尽")

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
        if not done.wait(inference_timeout_secs):
            logger.warning(
                "[ocr] 推理超过单次预算 %ss，淘汰缓存会话: key=%s",
                inference_timeout_secs,
                self._key,
            )
            _evict_ocr_session(self._key, self)
            raise TimeoutError(f"OCR 推理超过单次预算 {inference_timeout_secs:g}s")

        error = result.get("error")
        if error is not None:
            raise error
        return result.get("value")


def _evict_ocr_session(
    key: _OcrCacheKey, session: _OcrSession
) -> None:
    """仅当缓存仍指向指定会话时淘汰，避免误删并发创建的新实例。"""
    with _ocr_lock:
        if _ocr_cache.get(key) is session:
            _ocr_cache.pop(key, None)


def _get_ocr(old: bool, char_range: str | int | None = None) -> _OcrSession:
    """获取缓存 OCR 会话。

    模型不存在时抛出 ImportError，由调用方转换为 WorkerError。返回对象保持缓存身份
    稳定；实际 ddddocr 实例只在对应 ``old + char_range`` key 首次使用时创建。

    key 归一口径（P2-6）：空串/脏数据归一到默认档；``set_ranges`` 失败的取值
    同样归一到默认档——非法范围下实例保持模型默认字符集，与"未指定范围"共用
    一个实例，避免每种非法取值各自加载一份模型。
    """
    import ddddocr  # type: ignore

    normalized_range = _normalize_ocr_range(char_range)
    original_key: _OcrCacheKey = (old, normalized_range)
    key = original_key
    session = _ocr_cache.get(key)
    if session is not None:
        # LRU 触碰：命中即移至队尾（OrderedDict 保序，需持锁）。并发下该 key
        # 可能已被超时淘汰移除，存在性检查与 move_to_end 须在同一临界区内。
        with _ocr_lock:
            if key in _ocr_cache:
                _ocr_cache.move_to_end(key)
        return session
    # 在锁外登记加载标记：等待锁的并发调用与超时文案分流都据此判断
    # "正在首次加载"。锁保证构建本身仍只发生一次（B6 双检）。
    _ocr_load_started.setdefault(key, time.monotonic())
    try:
        with _ocr_lock:
            session = _ocr_cache.get(key)
            if session is None:
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
                        # 失败实例保持默认字符集：key 归一到默认档，与"未指定
                        # 范围"复用同一实例；默认档已有实例时丢弃刚建实例
                        key = (old, None)
                        existing = _ocr_cache.get(key)
                        if existing is not None:
                            session = existing
                if session is None:
                    session = _OcrSession(instance, key)
                    _ocr_cache[key] = session
                    # LRU 上限：逐项淘汰最久未使用，保证内存有界
                    while len(_ocr_cache) > _OCR_CACHE_MAX_ENTRIES:
                        _ocr_cache.popitem(last=False)
    except BaseException:
        # 构造失败/被取消也必须清标记：残留会让后续超时被误判为
        # "模型仍在首次加载"而永不建议重装依赖（key 可能已归一，两处都清）
        _ocr_load_started.pop(original_key, None)
        _ocr_load_started.pop(key, None)
        raise
    # 加载完成（含调用方已超时的孤儿线程收尾）：清除登记时刻的标记并写日志
    # 让恢复可见。key 可能已因 set_ranges 失败归一为默认档，两处都清（幂等）。
    load_started = _ocr_load_started.pop(original_key, None)
    if key != original_key:
        _ocr_load_started.pop(key, None)
    if load_started is not None:
        logger.info("[ocr] 模型加载完成（耗时 %.1fs）", time.monotonic() - load_started)

    return session


def ocr_load_in_progress(old: bool, char_range: str | int | None = None) -> bool:
    """指定 key 的模型是否正在首次加载中（供步骤超时文案分流）。

    与 ``_get_ocr`` 使用同一归一口径：空串/脏数据视为默认范围档。
    """
    normalized_range = _normalize_ocr_range(char_range)
    return (old, normalized_range) in _ocr_load_started


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
    except Exception as exc:  # noqa: BLE001 — 预处理失败不致命，回退原图
        logger.debug("OCR 图片预处理失败，回退原图: %s", exc)
        return img_bytes
