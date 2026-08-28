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


def _function_body(source: str, name: str) -> str:
    """提取简单 function 声明附近的源码片段，足够用于静态契约断言。"""
    marker = f"function {name}("
    start = source.find(marker)
    assert start >= 0, f"缺少函数 {name}"
    return source[start : start + 5000]


def test_recorder_javascript_syntax() -> None:
    """userscript 必须能被当前 Node 解析，避免发布语法损坏的录制器。"""
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


def test_recorder_button_group_uses_existing_selector_field() -> None:
    source = _source()
    # getElementInfo() 只提供 selectors 数组，不提供 bestSelector；按钮组不得读取不存在字段。
    assert "groupContainerInfo.bestSelector" not in source


def test_recorder_redacts_sensitive_dom_before_prompt() -> None:
    source = _source()
    get_info = _function_body(source, "getElementInfo")

    assert "sanitizeDomHtml" in source
    assert "SENSITIVE_ATTR_RE" in source
    # 元素属性摘要不能收集 value；token/session/csrf 等 data-* 也必须经过敏感属性过滤。
    assert '"value"' not in get_info[:1800]
    assert "isSensitiveAttributeName" in get_info[:2200]
    # 录制步骤和页面公共上下文都不能再直接把 raw HTML 塞进 AI prompt。
    assert "elementHTML: el.outerHTML" not in source
    assert "elementParentContext: el.parentElement ? el.parentElement.innerHTML" not in source
    assert "findStepContainer(el)?.innerHTML.substring" not in source
    assert "common.innerHTML.substring" not in source
    # 页面/iframe/图片 URL 必须先去掉 query/hash，避免 token 被提示词带出。
    assert 'url = sanitizeAttributeValue("href", url)' in source
    assert 'sanitizeAttributeValue("src", frame.src)' in source


def test_recorder_smart_detect_filters_non_login_inputs() -> None:
    source = _source()
    # 智能检测必须显式区分登录表单、账号、验证码和搜索输入，不能把任意 text input 当 USERNAME。
    assert "isLikelyLoginForm" in source
    assert "isLikelyUsernameInput" in source
    assert "isLikelyCaptchaInput" in source
    assert "isLikelySearchInput" in source
    # OTP/短信验证码也属于验证码类输入，不能因位于 login form 中而回退成 username。
    assert "|otp|" in source
    assert "one[-_ ]?time[-_ ]?code" in source
    assert "短信码" in source


def test_recorder_smart_detect_listeners_are_not_duplicated_on_reopen() -> None:
    source = _source()
    # createPanel 会在面板关闭后重建；全局 input/change 监听必须有一次性注册护栏。
    assert "_smartDetectListenersAttached" in source


def test_recorder_smart_detect_listeners_do_nothing_when_inactive() -> None:
    source = _source()
    # 一次性 document 监听会跨面板生命周期存在，因此回调必须同时检查 active + recording。
    assert source.count("if (!state.active || !state.recording) return;") >= 2
    deactivate = _function_body(source, "deactivate")
    assert "state.currentStepType = null" in deactivate
