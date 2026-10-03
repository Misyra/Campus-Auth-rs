"""校验路由器二进制的架构、浮点 ABI 与静态链接属性。"""

import argparse
import os
from pathlib import Path
import struct
import subprocess


TARGETS = {
    "x86_64-unknown-linux-musl": (2, 62),
    "aarch64-unknown-linux-musl": (2, 183),
    "armv7-unknown-linux-musleabihf": (1, 40),
}
SDK_TARGETS = {
    "x86_64": "x86_64-unknown-linux-musl",
    "aarch64": "aarch64-unknown-linux-musl",
    "arm": "armv7-unknown-linux-musleabihf",
}
ARMV7_CPUS = {f"cortex-a{number}" for number in (5, 7, 8, 9, 12, 15, 17, 53, 57, 72)}


def sdk_target(arch, cpu):
    """32 位 ARM 额外限制 CPU，防止将 ARMv7 程序交给 ARMv5 / ARMv6 SDK。"""
    if arch == "arm" and cpu not in ARMV7_CPUS:
        raise ValueError("SDK 的 ARM CPU 不在已支持的 ARMv7 / ARMv8 列表中")
    return SDK_TARGETS[arch]


def verify_binary(binary, target, readelf="readelf"):
    """拒绝错架构、软浮点 ARM 和依赖动态库的产物。"""
    with Path(binary).open("rb") as stream:
        header = stream.read(64)
    if len(header) < 52 or header[:4] != b"\x7fELF" or header[5] != 1:
        raise ValueError("主程序必须是完整的小端 ELF 文件")
    elf_class, machine = TARGETS[target]
    if header[4] != elf_class or struct.unpack_from("<H", header, 18)[0] != machine:
        raise ValueError("ELF 架构与构建目标不符")
    if elf_class == 2 and len(header) < 64:
        raise ValueError("64 位 ELF 头不完整")
    if struct.unpack_from("<H", header, 16)[0] not in (2, 3):
        raise ValueError("ELF 文件不是可执行程序")
    if machine == 40 and not (struct.unpack_from("<I", header, 36)[0] & 0x400):
        raise ValueError("ARMv7 程序必须使用硬浮点 ABI")
    env = {**os.environ, "LC_ALL": "C"}
    for flag, forbidden in [("-l", "INTERP"), ("-d", "NEEDED")]:
        result = subprocess.run(
            [readelf, flag, str(binary)], env=env, text=True, capture_output=True, check=True,
        )
        if forbidden in result.stdout:
            raise ValueError("二进制包含动态加载器或动态库依赖，拒绝打包")


def main():
    """为开发机打包脚本和 SDK 提供统一检查入口。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--target", choices=TARGETS)
    group.add_argument("--sdk-arch", choices=SDK_TARGETS)
    parser.add_argument("--sdk-cpu", default="")
    parser.add_argument("--readelf", default="readelf")
    args = parser.parse_args()
    try:
        target = args.target or sdk_target(args.sdk_arch, args.sdk_cpu)
        verify_binary(args.binary, target, args.readelf)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"ELF 校验失败：{error}\n")


if __name__ == "__main__":
    main()
