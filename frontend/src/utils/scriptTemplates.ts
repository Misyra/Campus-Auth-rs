/** Python 脚本模板只允许依赖 Python 标准库（用户环境未声明任何第三方包）。 */
export const NEW_SCRIPT_STUB = `#!/usr/bin/env python3
"""自定义脚本

用于定时执行的辅助动作（打卡、签到等）。退出码 0 表示成功。
如需发送 HTTP 请求，可直接使用 Python 标准库 urllib.request。
`;

export const LOGIN_SCRIPT_TEMPLATE = `#!/usr/bin/env python3
"""自定义脚本示例

脚本用于每日签到一类的辅助动作，退出码 0 表示成功（非 0 会记为失败）。
如需登录校园网，请使用方案里的「直连请求」，无需编写脚本。
模板只使用 Python 标准库，避免依赖应用运行环境未声明的第三方包。
"""

CHECKIN_URL = "http://10.0.0.1/checkin"

from urllib.request import urlopen

with urlopen(CHECKIN_URL, timeout=30) as response:
    print(f"HTTP {response.status}")
`;
