"""任务录制器静态契约回归测试。"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
RECORDER = ROOT / "resources" / "tools" / "task-recorder.user.js"


def _source() -> str:
    return RECORDER.read_text(encoding="utf-8")


def test_recorder_javascript_syntax() -> None:
    """userscript 必须至少能被当前 Node 解析，避免发布语法损坏的录制器。"""
    node = shutil.which("node")
    if node is None:
        pytest.skip("当前环境未安装 Node.js")
    result = subprocess.run(
        [node, "--check", str(RECORDER)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr or result.stdout


def test_recorder_metadata_version_matches_runtime_version() -> None:
    source = _source()
    metadata = re.search(r"^//\s*@version\s+(\S+)", source, re.MULTILINE)
    runtime = re.search(r'const\s+VERSION\s*=\s*"([^"]+)"', source)
    assert metadata is not None
    assert runtime is not None
    assert metadata.group(1) == runtime.group(1)


def test_recorder_keeps_selector_engines_and_prompt_redaction() -> None:
    source = _source()
    assert "selectorForPlayback" in source
    assert "sanitizeDomHtml" in source
    assert 'return `text=${JSON.stringify(candidate.value)}`' in source
    assert 'return `xpath=${candidate.value}`' in source


def test_recorder_button_group_does_not_use_missing_best_selector() -> None:
    source = _source()
    # getElementInfo() 返回 selectors 数组，不存在 bestSelector 字段。
    assert "groupContainerInfo.bestSelector" not in source
