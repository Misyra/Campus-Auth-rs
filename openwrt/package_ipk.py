"""为 opkg 固件生成 IPK；APK 固件必须使用对应 OpenWrt SDK。"""

import argparse
import gzip
import hashlib
import io
from pathlib import Path
import re
import tarfile
import tomllib

from verify_elf import ARMV7_CPUS, TARGETS, verify_binary


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ARCH = {
    "x86_64-unknown-linux-musl": "x86_64",
    "aarch64-unknown-linux-musl": "aarch64_generic",
    "armv7-unknown-linux-musleabihf": "arm_cortex-a7_neon-vfpv4",
}


def archive(entries):
    """使用固定时间、root 属主和显式权限创建可复现的 tar.gz。"""
    result = io.BytesIO()
    with gzip.GzipFile(fileobj=result, mode="wb", mtime=0, filename="") as zipped:
        with tarfile.open(fileobj=zipped, mode="w", format=tarfile.GNU_FORMAT) as tar:
            for name, (content, mode) in sorted(entries.items()):
                info = tarfile.TarInfo("./" + name)
                info.size = len(content)
                info.mode = mode
                info.uid = info.gid = 0
                tar.addfile(info, io.BytesIO(content))
    return result.getvalue()


def package(output, name, version, architecture, depends, data, scripts=None, conffiles=None):
    """按 OpenWrt ipkg-build 的三层 tar 格式组装安装包并生成摘要。"""
    control = (
        f"Package: {name}\nVersion: {version}\nArchitecture: {architecture}\n"
        f"Depends: {depends}\nInstalled-Size: {sum(len(item[0]) for item in data.values())}\n"
        "Section: net\nPriority: optional\nLicense: AGPL-3.0-only\n"
        "Maintainer: Campus-Auth 项目\nDescription: 认证喵校园网认证\n"
    )
    entries = {"control": (control.encode(), 0o644)}
    for script_name, text in (scripts or {}).items():
        entries[script_name] = (text.encode(), 0o755)
    if conffiles:
        entries["conffiles"] = (("\n".join(conffiles) + "\n").encode(), 0o644)
    content = archive({
        "debian-binary": (b"2.0\n", 0o644),
        "control.tar.gz": (archive(entries), 0o644),
        "data.tar.gz": (archive(data), 0o644),
    })
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    path = output / f"{name}_{version}_{architecture}.ipk"
    path.write_bytes(content)
    path.with_suffix(".ipk.sha256").write_text(
        f"{hashlib.sha256(content).hexdigest()}  {path.name}\n", encoding="utf-8", newline="\n",
    )
    return path


def luci_data():
    """从原生 SDK 包目录收集插件，保持两种打包入口的文件一致。"""
    base = ROOT / "openwrt/package/luci-app-campus-auth"
    entries = {}
    for folder, prefix in [("htdocs", "www/"), ("root", "")]:
        for path in (base / folder).rglob("*"):
            if path.is_file():
                content = path.read_text(encoding="utf-8").encode()
                entries[prefix + path.relative_to(base / folder).as_posix()] = (content, 0o644)
    return entries


def main():
    """必须提供实际静态主程序，不能生成占位的认证服务包。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--architecture", help="以设备 opkg print-architecture 为准")
    parser.add_argument("--output", type=Path, default=ROOT / "openwrt/dist")
    parser.add_argument("--readelf", default="readelf")
    args = parser.parse_args()
    arch = args.architecture or DEFAULT_ARCH[args.target]
    if not re.fullmatch(r"[a-z0-9_-]+", arch):
        parser.error("架构名只能包含小写字母、数字、下划线与连字符")
    expected_prefix = {62: "x86_64", 183: "aarch64_", 40: "arm_"}[TARGETS[args.target][1]]
    if not arch.startswith(expected_prefix):
        parser.error("包架构名与 Rust 目标不符，不允许伪装为 all 或其它 CPU")
    if args.target == "armv7-unknown-linux-musleabihf":
        cpu = arch.removeprefix("arm_").split("_")[0]
        if cpu not in ARMV7_CPUS:
            parser.error("ARM 包的 CPU 子型号须为已支持的 ARMv7 / ARMv8 CPU")
    verify_binary(args.binary, args.target, args.readelf)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"] + "-1"
    data = {"usr/bin/campus-auth": (args.binary.read_bytes(), 0o755)}
    for name, path, mode in [
        ("campus-auth.init", "etc/init.d/campus-auth", 0o755),
        ("campus-auth.config", "etc/config/campus-auth", 0o600),
        ("campus-auth.keep", "lib/upgrade/keep.d/campus-auth", 0o644),
    ]:
        data[path] = ((ROOT / "openwrt/files" / name).read_text(encoding="utf-8").encode(), mode)
    # 显式启用开机启动由 LuCI / init.d 管理；升级不会重新开启已关闭的开机启动。
    scripts = {
        "postinst": '#!/bin/sh\n[ -n "$IPKG_INSTROOT" ] && exit 0\n/etc/init.d/campus-auth start\n',
        "prerm": '#!/bin/sh\n[ -n "$IPKG_INSTROOT" ] && exit 0\n/etc/init.d/campus-auth stop\n[ "${PKG_UPGRADE:-0}" = 1 ] || [ "$1" = upgrade ] || /etc/init.d/campus-auth disable\nexit 0\n',
    }
    print(package(args.output, "campus-auth", version, arch, "ca-bundle, ip-full", data,
                  scripts, ["/etc/config/campus-auth"]))
    print(package(args.output, "luci-app-campus-auth", version, "all", "campus-auth, luci-base, rpcd-mod-file",
                  luci_data(), {"postinst": '#!/bin/sh\n[ -n "$IPKG_INSTROOT" ] && exit 0\n/etc/init.d/rpcd restart\n/etc/init.d/uhttpd reload\nexit 0\n'}))


if __name__ == "__main__":
    main()
