"""使用模拟 procd / UCI 验证服务脚本，不访问真实路由器或启动主程序。"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SHELL = os.environ.get("OPENWRT_TEST_SHELL") or shutil.which("sh")
HARNESS = r'''
config_load() { :; }
config_get_bool() {
    read -r "$1" <<EOF
${TEST_ENABLED:-1}
EOF
}
config_get() {
    local value
    case "$3" in
        binary) value=/bin/sh ;;
        data_dir) value=$TEST_DATA_DIR ;;
        host) value=${TEST_HOST:-127.0.0.1} ;;
        port) value=${TEST_PORT:-50721} ;;
        *) value=$4 ;;
    esac
    read -r "$1" <<EOF
$value
EOF
}
procd_open_instance() { printf 'OPEN\n'; }
procd_close_instance() { printf 'CLOSE\n'; }
procd_set_param() { printf 'PARAM'; printf '|%s' "$@"; printf '\n'; }
procd_add_reload_trigger() { printf 'TRIGGER|%s\n' "$1"; }
readlink() {
    if [ -n "$TEST_RESOLVED_DATA" ]; then
        printf '%s\n' "$TEST_RESOLVED_DATA"
    else
        command readlink "$@"
    fi
}
. "$1"
start_service
'''


@unittest.skipUnless(SHELL, "需要 POSIX shell，Windows 可通过 OPENWRT_TEST_SHELL 指定 Git Bash")
class OpenWrtServiceTests(unittest.TestCase):
    """只操作隔离目录，验证启动参数与持久配置保护。"""

    def setUp(self):
        report_dir = ROOT / "docs" / "reports"
        report_dir.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="openwrt-service-", dir=report_dir)
        self.addCleanup(self.temp.cleanup)
        self.data_dir = Path(self.temp.name) / "data with spaces"
        # Git Bash 使用 /盘符/ 路径，保持与 OpenWrt 的绝对路径校验一致。
        self.shell_data_dir = self.data_dir.as_posix()
        if os.name == "nt":
            self.shell_data_dir = f"/{self.shell_data_dir[0].lower()}{self.shell_data_dir[2:]}"

    def run_service(self, **overrides):
        env = {**os.environ, "TEST_DATA_DIR": self.shell_data_dir, **overrides}
        return subprocess.run(
            [SHELL, "-c", HARNESS, "test-service", (ROOT / "openwrt/files/campus-auth.init").as_posix()],
            env=env, text=True, encoding="utf-8", capture_output=True, check=False,
        )

    def test_foreground_command_and_process_ownership(self):
        result = self.run_service()
        self.assertEqual(result.returncode, 0, result.stderr)
        command = next(line.split("|")[2:] for line in result.stdout.splitlines() if line.startswith("PARAM|command|"))
        self.assertEqual(command, [
            "/bin/sh", "--base-path", self.shell_data_dir, "--mode", "full",
            "--host", "127.0.0.1", "--port", "50721", "--no-browser", "--no-tray",
        ])
        self.assertIn("PARAM|env|CAMPUS_AUTH_PROCD=1", result.stdout)
        self.assertIn("PARAM|term_timeout|40", result.stdout)
        self.assertIn("PARAM|respawn|3600|5|5", result.stdout)
        settings = json.loads((self.data_dir / "config/settings.json").read_text())
        self.assertFalse(settings["global"]["logging"]["file_enabled"])
        self.assertFalse(settings["global"]["updater"]["auto_check_enabled"])

    def test_existing_config_is_preserved(self):
        config_dir = self.data_dir / "config"
        config_dir.mkdir(parents=True)
        path = config_dir / "settings.json"
        existing = '{"global":{"logging":{"file_enabled":true}},"custom":"keep"}\n'
        path.write_text(existing)
        self.assertEqual(self.run_service().returncode, 0)
        self.assertEqual(path.read_text(), existing)

    def test_disabled_service_creates_no_data(self):
        result = self.run_service(TEST_ENABLED="0")
        self.assertEqual(result.returncode, 0)
        self.assertNotIn("OPEN", result.stdout)
        self.assertFalse(self.data_dir.exists())

    def test_invalid_port_is_rejected(self):
        for port in ["0", "65536", "abc", "50721;touch bad"]:
            with self.subTest(port=port):
                result = self.run_service(TEST_PORT=port)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("OPEN", result.stdout)
        self.assertFalse(self.data_dir.exists())

    def test_lan_listen_address_is_passed_explicitly(self):
        result = self.run_service(TEST_HOST="192.168.1.1", TEST_PORT="50800")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("|--host|192.168.1.1|--port|50800|", result.stdout)

    def test_system_directory_is_rejected_after_symlink_resolution(self):
        # 只模拟解析结果，避免测试对宿主系统目录进行 mkdir / chmod。
        for path in ["/etc", "/etc/config", "/root", "/usr/share", "/proc/sys"]:
            with self.subTest(path=path):
                result = self.run_service(TEST_RESOLVED_DATA=path)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("OPEN", result.stdout)
                self.assertIn("专用数据目录", result.stderr)
                self.assertFalse((self.data_dir / "config/settings.json").exists())


if __name__ == "__main__":
    unittest.main()
