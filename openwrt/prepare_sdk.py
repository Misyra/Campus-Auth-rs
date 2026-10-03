"""将软件包定义与公共部署文件组装成可复制到 OpenWrt SDK 的目录。"""

import argparse
from pathlib import Path
import shutil
import tempfile


ROOT = Path(__file__).resolve().parents[1]


def prepare(destination):
    """只在新目录内组装，防止覆盖 SDK 或旧构建产物。"""
    destination = Path(destination).resolve()
    if destination.exists():
        raise ValueError("输出目录已存在，请选择新目录")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".campus-auth-sdk-", dir=destination.parent) as temp:
        stage = Path(temp) / "package"
        shutil.copytree(ROOT / "openwrt/package", stage)
        files = stage / "campus-auth/files"
        files.mkdir()
        for name in ("campus-auth.init", "campus-auth.config", "campus-auth.keep"):
            # Windows 检出目录可能已有 CRLF，设备脚本始终转为 LF。
            text = (ROOT / "openwrt/files" / name).read_text(encoding="utf-8")
            (files / name).write_text(text, encoding="utf-8", newline="\n")
        shutil.copyfile(ROOT / "openwrt/verify_elf.py", files / "verify_elf.py")
        stage.rename(destination)
    return destination


def main():
    """输出完整 SDK 包目录，不修改 SDK 的配置或 feeds。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        print(prepare(args.destination))
    except (ValueError, OSError) as error:
        parser.exit(1, f"SDK 包组装失败：{error}\n")


if __name__ == "__main__":
    main()
