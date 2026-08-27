# Campus-Auth 浏览器任务编写指南

本文以当前 Rust `TaskConfig` / `StepConfig` 与 Python Worker 的实际执行语义为准。任务使用 JSON 描述，由 Playwright 按顺序执行。

## 1. 最小任务结构

```json
{
  "name": "校园网登录",
  "url": "{{LOGIN_URL}}",
  "timeout": 30000,
  "navigation_wait": 1.0,
  "step_delay": 0.5,
  "variables": {},
  "steps": [
    {
      "id": "fill_username",
      "type": "input",
      "selector": "#username",
      "value": "{{USERNAME}}"
    },
    {
      "id": "fill_password",
      "type": "input",
      "selector": "#password",
      "value": "{{PASSWORD}}"
    },
    {
      "id": "submit",
      "type": "click",
      "selector": "button[type='submit']"
    }
  ]
}
```

## 2. 顶层字段

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `name` | `"未命名任务"` | 显示名称 |
| `description` | `""` | 任务说明 |
| `url` | `""` | 初始登录页；非空时执行器会先自动导航 |
| `timeout` | `30000` | 任务级总超时，毫秒；由 Rust Bridge 统一兜底 |
| `navigation_wait` | `1.0` | 初始导航完成后的额外等待，秒 |
| `step_delay` | `0.5` | 相邻步骤之间的等待，秒 |
| `reveal_hidden` | `false` | 是否在执行前揭示隐藏输入元素 |
| `variables` | `{}` | 任务自定义模板变量 |
| `success_condition` | `""` | 指定一个 `store_as` 变量作为最终成功条件 |
| `steps` | `[]` | 按顺序执行的步骤列表 |
| `metadata` | 可选 | 自定义元数据，执行器不解释 |
| `on_success` / `on_failure` | 可选 | 保留的结果处理配置 |

`url` 是初始导航地址，不代表任务中不能再次跳转。多页面认证可以继续使用 `goto` / `navigate` 步骤。

## 3. 步骤公共字段

```json
{
  "id": "step_id",
  "type": "click",
  "description": "可选描述",
  "timeout": 10000,
  "required": true,
  "frame": "loginFrame"
}
```

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `id` | `""` | 步骤标识；正式任务应提供稳定且唯一的 ID |
| `type` | 必填 | 步骤类型 |
| `description` | `""` | 日志和调试界面显示的描述 |
| `timeout` | 浏览器默认值 | 单步超时，毫秒 |
| `required` | **`true`** | 步骤失败时是否终止任务；这是当前 Rust/Python 的真实默认值 |
| `frame` | 无 | frame name、`url=片段` 或 iframe/frame CSS 选择器 |

未知字段会作为扩展字段保留，并由对应处理器按需读取，例如 `goto.wait_until`、`screenshot.full_page`、`click_select.select_delay`。

## 4. 变量系统

模板格式为：

```text
{{VARIABLE_NAME}}
```

常用系统变量：

- `{{USERNAME}}`：当前 Profile 的账号
- `{{PASSWORD}}`：当前 Profile 的密码
- `{{ISP}}`：当前 Profile 的运营商
- `{{LOGIN_URL}}`：当前认证地址

变量优先级从低到高为：

1. 任务 `variables`
2. 系统保留登录变量（`USERNAME` / `PASSWORD` / `ISP` / `LOGIN_URL`）
3. 前序步骤通过 `store_as` 产生的运行时结果

因此任务自定义变量不能伪造或覆盖当前 Profile 的真实登录凭据；运行时结果则可以在后续步骤中覆盖同名模板值。

模板不仅支持 `selector` / `value`，也会解析 `frame`、`option_selector`、`target_selector`、`path`、`pattern`、脚本内容，以及扩展字段中的嵌套字符串，例如：

```json
{
  "id": "goto_next",
  "type": "goto",
  "url": "https://example.edu/next?ticket={{ticket}}"
}
```

`store_as` 保存原生 JavaScript/OCR 结果；引用到字符串模板时才会稳定转换：`null -> ""`、布尔值 -> `true/false`、对象/数组 -> JSON 字符串。

## 5. input — 输入文本

```json
{
  "id": "fill_username",
  "type": "input",
  "selector": "input[name='DDDDD'], #username",
  "value": "{{USERNAME}}",
  "clear": true
}
```

- `selector`：目标输入框。
- `value`：输入文本。
- `clear`：默认 `true`，先清空再填写。
- 普通填写失败时，执行器会保留一部分单步 timeout 给 attached/JavaScript 降级路径，而不是让第一次 `fill` 吃掉全部预算。
- `reveal_hidden=true` 只应作为特殊门户的兼容手段，不建议默认开启。

## 6. click — 点击

```json
{
  "id": "click_login",
  "type": "click",
  "selector": "button[type='submit'], input[name='0MKKey']"
}
```

点击支持多个候选选择器。候选只会在**顶层逗号**处分割，因此下面这些合法 CSS 不会被错误拆开：

```text
:is(.login,.submit)
[data-value='a,b']
```

普通 Playwright 点击失败后，会在剩余预算内尝试 attached + `dispatch_event("click")` 降级。

历史任务中的纯文本 selector 仍有兼容回退，但新任务建议显式使用 `text="登录"`、`text=登录` 或稳定 CSS。

## 7. select — 原生 `<select>`

```json
{
  "id": "select_isp",
  "type": "select",
  "selector": "select[name='isp']",
  "value": "{{ISP}}",
  "required": false
}
```

匹配顺序：

1. option `value` 精确匹配；
2. option 显示文本精确匹配；
3. 显示文本唯一子串匹配。

`value` 为空时直接跳过。没有唯一匹配时：`required=true` 失败，`required=false` 跳过。不要依赖旧文档中“默认会忽略失败”的说法——`required` 默认是 `true`。

## 8. click_select — 自定义下拉框 / 按钮组

```json
{
  "id": "select_isp_custom",
  "type": "click_select",
  "selector": ".service-selector",
  "option_selector": ".service-options",
  "value": "{{ISP}}",
  "select_delay": 300,
  "required": false
}
```

语义：

1. 点击 `selector` 展开选项；
2. 可选等待 `select_delay` 毫秒，默认 500；
3. 在 `option_selector` 范围内按 `value` 文本寻找唯一选项；
4. 点击匹配项。

`option_selector` **只是搜索范围，不是最终要点击的值**。触发器点击、展开等待和选项点击共用同一个步骤 timeout 预算。

## 9. wait / sleep / wait_for_selector

### 等元素出现

```json
{
  "id": "wait_form",
  "type": "wait",
  "selector": "#login-form",
  "timeout": 10000
}
```

`wait` 有 `selector` 时等待元素进入 `visible` 状态。

### 固定等待

新任务应使用 `sleep`：

```json
{
  "id": "wait_animation",
  "type": "sleep",
  "duration": 800
}
```

为兼容历史任务，`wait` **没有 selector** 时仍按 `duration` 做固定等待；但不要在新任务中继续依赖这种双重语义。

`wait_for_selector` 仍作为显式别名支持，语义与带 selector 的 `wait` 一致。

## 10. wait_url — 等待 URL

```json
{
  "id": "wait_redirect",
  "type": "wait_url",
  "pattern": "success|welcome",
  "timeout": 10000
}
```

`pattern` 是正则表达式。非法正则会直接报配置/执行错误，而不是静默等待到超时。

## 11. eval / evaluate — 执行 JavaScript

推荐使用 `eval` 或 `evaluate`：

```json
{
  "id": "read_ticket",
  "type": "eval",
  "script": "() => document.querySelector('#ticket')?.textContent || null",
  "store_as": "ticket",
  "timeout": 5000
}
```

兼容别名：`eval`、`evaluate`、`custom_js`、`custom`。`code` 仍可作为 `script` 的历史别名。

`timeout` 会真正约束该步骤；若脚本长期不返回，Worker 会中断该页面以避免悬挂。`store_as` 保留结果原生类型。

## 12. goto / navigate — 页面导航

两种类型都受支持：

```json
{
  "id": "goto_sso",
  "type": "goto",
  "url": "https://sso.example.edu/login",
  "wait_until": "domcontentloaded",
  "timeout": 15000
}
```

URL 可写在扩展字段 `url`，历史任务也可使用 `value` 或 `selector`。推荐新任务使用 `url`。

`wait_until` 支持：`load`、`domcontentloaded`、`networkidle`、`commit`。

任务顶层 `url` 已经负责第一次自动导航，因此单页登录通常不需要再写第一条 `goto`；多页 SSO/认证流程则可以正常使用。

## 13. screenshot — 截图

```json
{
  "id": "capture_result",
  "type": "screenshot",
  "path": "result.png",
  "full_page": true
}
```

调试截图保存在 Worker 的调试目录。Web 调试面板不会接触 Worker 的绝对本地路径；服务端会校验文件名、固定目录、文件类型和大小，再通过已鉴权 WebSocket 内联图片。

## 14. upload_file — 上传文件

```json
{
  "id": "upload_cert",
  "type": "upload_file",
  "selector": "input[type='file']",
  "path": "C:/path/to/file.txt"
}
```

新任务使用 `path`。历史任务把路径写在 `value` 中仍兼容。

## 15. ocr — 验证码识别

```json
{
  "id": "ocr_captcha",
  "type": "ocr",
  "selector": "#captchaImage",
  "target_selector": "#captchaInput",
  "store_as": "captcha_text",
  "char_range": "0123456789",
  "required": true
}
```

- `selector`：验证码图片元素。
- `target_selector`：可选；识别后自动填写的输入框。
- `store_as`：可选；保存识别结果，供后续模板或脚本引用。
- `old`：是否使用 ddddocr 旧模型，默认 `false`。
- `char_range`：可选字符范围。

OCR 依赖是可选能力；未安装时普通非 OCR 浏览器任务仍可运行。

数学验证码可采用 OCR + eval 链：

```json
[
  {
    "id": "ocr_math",
    "type": "ocr",
    "selector": "#captcha",
    "store_as": "captcha_expr"
  },
  {
    "id": "calc_math",
    "type": "eval",
    "script": "() => { const s = '{{captcha_expr}}'.replace(/[^0-9+\\-*/().]/g, ''); try { return Function('return (' + s + ')')(); } catch { return null; } }",
    "store_as": "captcha_answer"
  },
  {
    "id": "fill_captcha",
    "type": "input",
    "selector": "#captchaInput",
    "value": "{{captcha_answer}}"
  }
]
```

仅对可信、受控的简单算术字符串这样处理；不要把任意页面文本直接拼入脚本执行。

## 16. assert_text — 文本断言

```json
{
  "id": "assert_success",
  "type": "assert_text",
  "value": "登录成功",
  "timeout": 5000
}
```

等待页面正文包含指定文本；超时归类为断言失败。

## 17. Frame / iframe

每个步骤都可使用 `frame`：

```json
{
  "id": "fill_iframe_user",
  "type": "input",
  "frame": "loginFrame",
  "selector": "#username",
  "value": "{{USERNAME}}"
}
```

支持三种格式：

```text
loginFrame                  # frame name
url=/portal/login           # URL 包含指定片段
iframe#login-frame          # iframe/frame CSS selector
```

name 或 URL 匹配到多个 frame 时会失败，避免静默操作错误页面。

## 18. success_condition

当任务配置：

```json
{
  "success_condition": "login_success"
}
```

执行器会读取同名 `store_as` 结果作为最终成功判定。例如：

```json
{
  "id": "check_success",
  "type": "eval",
  "script": "() => document.body.innerText.includes('登录成功')",
  "store_as": "login_success"
}
```

布尔 `false`、`null`、数值 `0`、空字符串，以及字符串 `"false"` / `"0"` / `"no"` / `"off"` 会按失败值处理；不要故意把布尔结果转成含糊字符串。

## 19. 选择器建议

优先级建议：

1. 稳定 `id`
2. 稳定 `name`
3. `data-testid` / `data-*`
4. 明确的属性组合
5. Playwright text selector
6. 结构性 CSS
7. XPath 作为最后选择

示例：

```text
#username
input[name='DDDDD']
button[data-action='login']
text="登录"
xpath=//button[contains(., '登录')]
```

避免只依赖自动生成、每次刷新变化的 class 或过长 DOM 路径。

## 20. required 的使用原则

`required` 默认是 **true**。

应该保持 `true`：

- 用户名、密码输入
- 登录按钮
- 必须完成的跳转
- 成功状态校验

适合设为 `false`：

- 某些学校才存在的运营商选择器
- 可有可无的协议勾选
- 非关键提示框关闭按钮

不要为了“任务不报错”把所有步骤都设为 `false`，那会把真实页面变更隐藏成假成功。

## 21. 完整示例

```json
{
  "name": "示例校园网",
  "description": "账号密码 + 可选运营商 + 成功判定",
  "url": "{{LOGIN_URL}}",
  "timeout": 30000,
  "navigation_wait": 1.0,
  "step_delay": 0.3,
  "steps": [
    {
      "id": "fill_username",
      "type": "input",
      "selector": "#username",
      "value": "{{USERNAME}}"
    },
    {
      "id": "fill_password",
      "type": "input",
      "selector": "#password",
      "value": "{{PASSWORD}}"
    },
    {
      "id": "select_isp",
      "type": "select",
      "selector": "#isp",
      "value": "{{ISP}}",
      "required": false
    },
    {
      "id": "submit",
      "type": "click",
      "selector": "button[type='submit']"
    },
    {
      "id": "check_success",
      "type": "eval",
      "script": "() => document.body.innerText.includes('登录成功') || document.body.innerText.includes('已连接')",
      "store_as": "login_success"
    }
  ],
  "success_condition": "login_success"
}
```

## 22. 快速排错

### 任务总是找不到元素

- 检查是否在 iframe 中；必要时加 `frame`。
- 动态页面增大 `navigation_wait`。
- 不要把固定延时误写成带错误 selector 的 `wait`。
- 优先重新确认稳定 CSS，而不是不断拉长 timeout。

### 运营商选择找不到

- 原生 `<select>` 用 `select`。
- div/span 自定义下拉用 `click_select`。
- `option_selector` 是选项搜索范围，不是运营商值。
- 页面确实可能没有运营商选择时设 `required:false`。

### 后续步骤拿不到 OCR/eval 结果

确认前一步存在 `store_as`，后一步使用相同名称：

```text
store_as: "ticket"
{{ticket}}
```

运行时结果优先于静态变量。

### 想固定等待一段时间

使用 `sleep + duration`。`wait` 无 selector 的固定等待只为兼容旧任务保留。

### 想在任务中主动跳转

直接使用 `goto` 或 `navigate`。旧文档中“系统不识别 navigate”的说法已过时。
