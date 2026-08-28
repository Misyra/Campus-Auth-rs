/** Python script templates must only rely on the Python standard library. */
export const NEW_SCRIPT_STUB = `#!/usr/bin/env python3
"""自定义登录脚本"""

# 如需发送 HTTP 请求，可直接使用 Python 标准库 urllib.request。
`;

export const LOGIN_SCRIPT_TEMPLATE = `#!/usr/bin/env python3
"""自定义登录脚本示例

脚本只需发送登录请求，登录是否成功由系统网络检测自动判断。
模板只使用 Python 标准库，避免依赖应用运行环境未声明的第三方包。
"""

LOGIN_URL = "http://10.0.0.1/login"
USERNAME = "your_username"
PASSWORD = "your_password"
ISP = "cmcc"

from urllib.parse import urlencode
from urllib.request import Request, urlopen

payload = urlencode({
    "username": USERNAME,
    "password": PASSWORD,
    "operator": ISP,
}).encode("utf-8")
request = Request(LOGIN_URL, data=payload, method="POST")
with urlopen(request, timeout=30) as response:
    print(f"HTTP {response.status}")
`;
