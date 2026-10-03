"""检查 IPK 内容、权限和 ELF 拒绝条件，不把模拟二进制视为真实编译产物。"""

import io
import json
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "openwrt"))
import package_ipk
import prepare_sdk
import verify_elf


def unpack(content):
    """在内存中读取包，避免测试解压覆盖实际文件。"""
    with tarfile.open(fileobj=io.BytesIO(content), mode="r:gz") as tar:
        return {item.name.removeprefix("./"): (tar.extractfile(item).read(), item.mode)
                for item in tar.getmembers() if item.isfile()}


class PackageTests(unittest.TestCase):
    """验证包管理契约和配置保留，不执行设备安装操作。"""

    def setUp(self):
        folder = ROOT / "docs/reports"
        folder.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="openwrt-package-", dir=folder)
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)

    def test_ipk_layout_modes_and_reproducibility(self):
        data = {"usr/bin/campus-auth": (b"fixture", 0o755), "etc/config/campus-auth": (b"keep", 0o600)}
        path = package_ipk.package(self.base, "campus-auth", "5.1.1-1", "x86_64", "ca-bundle, ip-full", data,
                                   {"postinst": "#!/bin/sh\nexit 0\n"}, ["/etc/config/campus-auth"])
        original = path.read_bytes()
        outer = unpack(original)
        self.assertEqual(set(outer), {"debian-binary", "control.tar.gz", "data.tar.gz"})
        self.assertEqual(outer["debian-binary"][0], b"2.0\n")
        control = unpack(outer["control.tar.gz"][0])
        self.assertIn(b"Architecture: x86_64\n", control["control"][0])
        self.assertEqual(control["conffiles"][0], b"/etc/config/campus-auth\n")
        self.assertEqual(control["postinst"][1], 0o755)
        self.assertEqual(unpack(outer["data.tar.gz"][0]), data)
        with tarfile.open(fileobj=io.BytesIO(outer["data.tar.gz"][0]), mode="r:gz") as tar:
            self.assertTrue(all(item.uid == item.gid == item.mtime == 0 for item in tar.getmembers()))
        package_ipk.package(self.base, "campus-auth", "5.1.1-1", "x86_64", "ca-bundle, ip-full", data,
                            {"postinst": "#!/bin/sh\nexit 0\n"}, ["/etc/config/campus-auth"])
        self.assertEqual(path.read_bytes(), original)

    def test_luci_paths_and_acl_are_restricted(self):
        data = package_ipk.luci_data()
        self.assertIn("www/luci-static/resources/view/campus-auth.js", data)
        menu = json.loads(data["usr/share/luci/menu.d/luci-app-campus-auth.json"][0])
        self.assertEqual(menu["admin/services/campus-auth"]["action"]["path"], "campus-auth")
        acl = json.loads(data["usr/share/rpcd/acl.d/luci-app-campus-auth.json"][0])["luci-app-campus-auth"]
        self.assertEqual(acl["read"]["uci"], ["campus-auth"])
        self.assertEqual(acl["read"]["ubus"]["file"], ["exec"])
        self.assertEqual(set(acl["read"]["file"]), {"/etc/init.d/campus-auth enabled"})
        self.assertEqual(set(acl["write"]["file"]), {
            f"/etc/init.d/campus-auth {action}" for action in ["start", "stop", "restart", "enable", "disable"]
        })
        self.assertNotIn("*", json.dumps(acl))

    def test_sdk_assembly_uses_canonical_files_and_never_overwrites(self):
        output = prepare_sdk.prepare(self.base / "sdk-package")
        for name in ["campus-auth.init", "campus-auth.config", "campus-auth.keep"]:
            self.assertEqual((output / "campus-auth/files" / name).read_text(encoding="utf-8"),
                             (ROOT / "openwrt/files" / name).read_text(encoding="utf-8"))
        self.assertTrue((output / "campus-auth/files/verify_elf.py").is_file())
        self.assertTrue((output / "luci-app-campus-auth/Makefile").is_file())
        self.assertIn("/root/.campus_network_auth/", (output / "campus-auth/files/campus-auth.keep").read_text(encoding="utf-8"))
        with self.assertRaises(ValueError):
            prepare_sdk.prepare(output)

    def test_versions_follow_main_program(self):
        version = package_ipk.tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
        for package in ["campus-auth", "luci-app-campus-auth"]:
            self.assertIn(f"PKG_VERSION:={version}\n",
                          (ROOT / "openwrt/package" / package / "Makefile").read_text(encoding="utf-8"))

    def test_cli_packages_each_supported_architecture_and_keeps_data_unowned(self):
        binary = self.base / "fixture-binary"
        binary.write_bytes(b"fixture")
        for target, architecture in package_ipk.DEFAULT_ARCH.items():
            with self.subTest(target=target), patch.object(package_ipk, "verify_binary"), patch.object(sys, "argv", [
                "package_ipk.py", "--binary", str(binary), "--target", target, "--output", str(self.base),
            ]), patch("builtins.print"):
                package_ipk.main()
                path = next(self.base.glob(f"campus-auth_*_{architecture}.ipk"))
                outer = unpack(path.read_bytes())
                data = unpack(outer["data.tar.gz"][0])
                self.assertEqual(data["usr/bin/campus-auth"][1], 0o755)
                self.assertEqual(data["etc/config/campus-auth"][1], 0o600)
                self.assertFalse(any(name.startswith("etc/campus-auth/") or name.startswith("root/") for name in data))
                scripts = unpack(outer["control.tar.gz"][0])
                self.assertIn(b"PKG_UPGRADE", scripts["prerm"][0])
                self.assertNotIn(b" disable", scripts["postinst"][0])
                self.assertNotIn(b" enable", scripts["postinst"][0])

    def elf_header(self, elf_class=2, machine=62, flags=0):
        header = bytearray(64)
        header[:7] = b"\x7fELF" + bytes([elf_class, 1, 1])
        struct.pack_into("<HH", header, 16, 2, machine)
        struct.pack_into("<I", header, 36 if elf_class == 1 else 48, flags)
        path = self.base / "mock-elf"
        path.write_bytes(header)
        return path

    def test_invalid_elf_and_wrong_architecture_are_rejected_before_readelf(self):
        path = self.elf_header()
        with patch.object(verify_elf.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "架构"):
                verify_elf.verify_binary(path, "aarch64-unknown-linux-musl")
            path.write_bytes(b"not ELF")
            with self.assertRaises(ValueError):
                verify_elf.verify_binary(path, "x86_64-unknown-linux-musl")
            run.assert_not_called()

    def test_soft_float_arm_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "硬浮点"):
            verify_elf.verify_binary(self.elf_header(1, 40), "armv7-unknown-linux-musleabihf")

    def test_sdk_rejects_old_arm_cpu(self):
        with self.assertRaisesRegex(ValueError, "ARM CPU"):
            verify_elf.sdk_target("arm", "arm1176jzf-s")
        self.assertEqual(verify_elf.sdk_target("arm", "cortex-a7"), "armv7-unknown-linux-musleabihf")

    def test_dynamic_linking_is_rejected(self):
        for marker in ["INTERP", "NEEDED"]:
            with self.subTest(marker=marker):
                with patch.object(verify_elf.subprocess, "run", return_value=verify_elf.subprocess.CompletedProcess([], 0, marker)):
                    with self.assertRaisesRegex(ValueError, "动态"):
                        verify_elf.verify_binary(self.elf_header(), "x86_64-unknown-linux-musl")

    def test_static_elf_checks_both_headers_and_dynamic_section(self):
        with patch.object(verify_elf.subprocess, "run", return_value=verify_elf.subprocess.CompletedProcess([], 0, "")) as run:
            verify_elf.verify_binary(self.elf_header(), "x86_64-unknown-linux-musl")
            self.assertEqual([call.args[0][1] for call in run.call_args_list], ["-l", "-d"])


if __name__ == "__main__":
    unittest.main()
