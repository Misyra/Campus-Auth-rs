# HTTP 登录使用指南

HTTP 登录由 Rust 主进程按任务中的步骤向校园网门户发请求；纯计算脚本由同一程序的临时子进程执行，超时即结束该进程。无需浏览器或 Python Worker。它适合能通过 HTTP 接口完成认证的门户；需要图形验证码或复杂页面交互时，使用浏览器自动化。

## 创建与绑定任务

1. 打开「任务 → HTTP 登录」，新建任务，填写名称和门户的认证页面地址。任务文件位于 `<base>/tasks/http/<id>.json`。
2. 新建任务默认有两步：先用计算步骤把本机 IP 原样保存为 `ip`，再填写登录请求。门户需要本机 IP 时，可在登录请求的地址、请求头或请求内容中写 `{ip}`；不需要时可删除示例计算步骤。一个任务至少需要一个请求步骤，最多 16 步。
3. 在「结果判断」中选择哪一步的响应作为依据，设置成功方式、成功标识与失败标识。
4. 填写测试账号、测试密码，点击「发送测试请求」，在逐步骤结果中核对请求和响应。
5. 到「方案」页选择登录方式「HTTP 登录」，绑定刚才的任务。账号和密码保存在方案中，任务文件不包含凭据。

同一个任务可供多个账号的方案复用。任务中的认证页面地址留空时，会使用方案的认证地址。HTTP 登录没有内置兜底任务；未绑定任务的方案无法使用该渠道。

旧版任务仍按原来的“退出登录请求 → 凭据变换脚本 → 前置请求 → 登录请求”执行。编辑器会将这些内容显示成等价的有序步骤；**首次调整步骤后**，才自动保存为 `schema_version: 2` 的新流程。

## 请求流程

点击「添加请求步骤」或「添加计算步骤」，通过上移、下移调整执行顺序。点击步骤摘要可展开或收起详细字段，「全部收起」可快速查看完整流程；新建步骤会自动展开。步骤 ID 稳定，用于结果判断；修改展示名称或排序不会改变引用。

### HTTP 请求

每个请求步骤可设置 GET/POST、完整 URL、请求头和请求内容。POST 且填写请求内容但没有 `Content-Type` 时，程序会补 `application/x-www-form-urlencoded`；未设置 `User-Agent` 时会补浏览器标识。每行请求头使用 `名称: 值` 格式。

请求地址、头和内容都可使用 `{username}`、`{password}`、`{isp}`、`{auth_url}`、`{local_ip}`、`{local_mac}`，以及前面步骤提取或计算出的 `{变量名}`。替换值**不会自动 URL 编码**；账号密码含 `&`、`+`、空格等字符时，可在计算步骤用 `url_encode()` 生成编码字段。GET 地址中的密码可能被网关或代理记录，门户支持时优先使用 POST。

每个请求步骤可添加多条取值规则，分别给出来源和变量名。来源包括 `json:字段路径`（支持 JSONP）、`header:头名`、`url:参数名`（最终 URL）、`redirect:参数名`（`Location` 的查询参数）、`html:输入框名`、`regex:含捕获组的表达式`。读取 `redirect:` 前应勾选“停在重定向响应”。取值失败默认停止本次登录。测试结果只展示生成的变量名，不回传值；从**下一步**开始，请求里用 `{challenge}`，计算脚本里用 `ctx.vars.challenge`。旧任务的单字段 `extract` 仍可读取。

「请求失败时」默认停止登录。对于清理旧会话等尽力而为的动作，可选「记录后继续」，并视需要设置请求后等待 0～30 秒。被结果判断引用的请求不能忽略失败。每步响应中的 HTTP 状态和取值错误都会显示在测试结果里。

同一次登录的请求共用一个 HTTP 客户端和仅在本次尝试中存在的 Cookie 存储。请求按顺序执行，连接池会尝试复用连接；具体 TCP 连接由 HTTP 客户端与门户共同决定。跨源重定向会清除原请求的自定义请求头和内容，HTTPS 降级跳转会被拒绝。

### 计算字段

计算步骤用于密码摘要、签名、challenge 响应等纯计算。脚本定义 `function transform(ctx)` 并返回对象；返回的字符串、数字、布尔字段成为后续步骤的占位符。

```javascript
function transform(ctx) {
  return {
    signature: hmac_sha256(ctx.password, ctx.vars.challenge),
    encoded_user: url_encode(ctx.username),
  };
}
```

`ctx.vars` 包含当前可用的占位符，包括前面请求提取的变量。`ctx.username`、`ctx.password`、`ctx.isp`、`ctx.auth_url`、`ctx.local_ip`、`ctx.local_mac` 继续可用。测试请求启用抓取认证页时，`ctx.page` 是认证页面原文，抓取失败时为空串。

可用函数：`md5`、`sha1`、`sha256`、`hmac_md5`、`hmac_sha256`、`srun_info`、`shu_ruijie_password`、`base64_encode`、`base64_decode`、`hex_encode`、`url_encode`、`now_ms`。深澜 srun_bx1 中，`hmac_md5(challenge, password)` 的 challenge 是密钥，`srun_info(info_json, challenge)` 生成带 `{SRBX1}` 前缀的值；完整任务见任务仓库的 `srun-bx1-template`。`shu_ruijie_password(password, mac)` 仅复现上海大学锐捷部署的固定公钥和 `password>mac` 规则，其他学校使用前必须核对页面公钥和实际请求。脚本在无网络和文件访问能力的沙箱中运行，含子进程启动在内的单次上限为 2 秒；超时会结束计算子进程。独立的「自定义脚本」登录渠道仍适合需要外部解释器、文件或其他非 HTTP 能力的门户。

## 结果判断

流程执行完毕后，程序读取「依据哪个请求步骤」所选步骤的响应。失败标识优先于成功标识：

| 配置 | 行为 |
|------|------|
| 响应命中失败标识 | 按「命中失败标识后」归类为凭据错误、需要人工操作，或按方案策略重试 |
| 成功依据为“响应”，成功标识非空 | 响应命中成功标识才是候选成功 |
| 成功依据为“响应”，成功标识为空 | HTTP 2xx 是候选成功 |
| 成功依据为“网络检测” | 不看成功标识和状态码，发送完成后由登录后联网检测判定；失败标识仍生效 |

候选成功还会经过登录后网络验证。编辑器中的「发送测试请求」只执行 HTTP 流程，不改变自动登录状态；若采用网络检测，测试结果会提示“尚未验证”，需要在方案中运行正式登录确认。

## 任务格式示例

任务仓库的共享索引仍使用 `type: "http"`；新流程的任务文件带 `schema_version: 2`。以下示例不含账号密码：

```json
{
  "type": "http",
  "schema_version": 2,
  "task_id": "portal_http",
  "name": "门户 HTTP 登录",
  "auth_url": "http://portal.example.com/",
  "steps": [
    {
      "id": "challenge",
      "name": "获取令牌",
      "kind": "request",
      "method": "GET",
      "url": "http://portal.example.com/api/challenge",
      "extract": "json:data.token",
      "extract_as": "token"
    },
    {
      "id": "login",
      "name": "提交登录",
      "kind": "request",
      "method": "POST",
      "url": "http://portal.example.com/api/login",
      "body": "username={username}&password={password}&token={token}"
    }
  ],
  "result_step_id": "login",
  "success_check": "network"
}
```

旧版应用不能执行新流程任务。新格式故意将旧的顶层 `url` 留空，使旧应用在保存或导入时明确拒绝它，避免只执行最后一个请求而给出错误结论。

## 测试与安全提示

- 测试结果按步骤显示状态、耗时、请求和响应片段；账号、密码、提取值与脚本产出会脱敏。`Set-Cookie` 响应头不展示具体值。
- 从任务仓库导入 HTTP 登录任务前，核对**每个请求步骤的目标域名**。任务中的模板可以把方案凭据发送到所写的地址。
- 请求地址仅接受 HTTP/HTTPS。HTTPS 证书设置可跟随全局策略、显式忽略错误或严格校验；不应在未确认门户身份时忽略证书错误。
- 每个请求读取的响应体最多 64 KiB；新流程总时限 120 秒，请求步骤单次最多 20 秒。步骤配置和脚本也有大小限制，超出时保存会被拒绝。
- 请求不走系统代理。需要浏览器 Cookie、页面交互、验证码识别或无法用上述步骤表达的流程时，选择浏览器自动化。
- 页面出现滑块、短信或扫码确认时，可把该提示设为失败标识并选择“需要人工验证，停止重试”；浏览器任务可用 `manual_check` 显式检测。不能靠重新请求自动完成用户操作。
