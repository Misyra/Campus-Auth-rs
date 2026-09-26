/**
 * 登录渠道（浏览器自动化 / 直连请求 / 自定义脚本）展示映射。
 *
 * 单一事实源：方案列表卡徽标、直连面板标题、后续设置页与引导向导的分流文案
 * 均由此派生，避免同一枚举在多处各写一套中文标签而漂移。
 */

import type { HttpLoginMethod, HttpSuccessCheck, LoginChannel } from "../api/types";

/** 登录渠道 → 用户可见标签 */
export function loginChannelLabel(channel: LoginChannel | string | undefined): string {
  if (channel === "http") return "直连请求";
  if (channel === "script") return "自定义脚本";
  return "浏览器自动化";
}

/** 登录渠道 → 列表卡等紧凑场景的短标签 */
export function loginChannelShortLabel(channel: LoginChannel | string | undefined): string {
  if (channel === "http") return "直连请求";
  if (channel === "script") return "脚本";
  return "浏览器";
}

/**
 * 登录渠道 → 徽标图标名（`IconApp` 的 name）。
 *
 * 与标签同处一地：换了图标名或加了渠道，列表卡与其它入口不必各自去猜。
 * 返回类型收窄成字面量联合而非 `string`——`IconApp` 的 `name` 是注册表键的联合，
 * 返回 `string` 会在每个宿主处编译不过。
 */
export function loginChannelIcon(
  channel: LoginChannel | string | undefined,
): "chrome" | "globe" | "code" {
  if (channel === "http") return "globe";
  if (channel === "script") return "code";
  return "chrome";
}

/**
 * 直连测试结果 → 人类可读状态。
 *
 * 只映射直连路径实际可能产生的 outcome（后端 `Outcome` 全集的子集）：
 * 浏览器专属的 navigation_timeout / selector_failed / captcha_failed 不会出现，
 * 未命中时统一回落「测试未通过」。
 */
export function httpTestOutcomeLabel(outcome: string | undefined): string {
  switch (outcome) {
    case "success":
      return "请求判定成功";
    case "invalid_credential":
      return "门户拒绝凭据";
    case "assertion_failed":
      return "未命中成功标识";
    case "network_error":
      return "请求未送达";
    case "cancelled":
      return "测试已取消";
    case "unknown_error":
      return "配置或脚本错误";
    default:
      return "测试未通过";
  }
}

/** 直连请求方法选项（CustomSelect 消费） */
export const HTTP_METHOD_OPTIONS: Array<{ value: HttpLoginMethod; label: string }> = [
  { value: "GET", label: "GET" },
  { value: "POST", label: "POST" },
];

/**
 * 直连 HTTPS 证书策略的下拉取值。
 *
 * 用字符串而非裸布尔承载三态：`null`（未设置=跟随全局）与 `false`（显式严格校验）
 * 语义完全不同，若把布尔直接塞进 `<option value>`，未设置会被渲染成字符串
 * `"null"` 与"关闭"混淆。取名另加一层映射，新增策略时改动点集中在此。
 */
export type HttpCertPolicy = "follow" | "ignore" | "strict";

/** 证书策略选项（CustomSelect 消费） */
export const HTTP_CERT_POLICY_OPTIONS: Array<{ value: HttpCertPolicy; label: string }> = [
  { value: "follow", label: "跟随全局设置（默认）" },
  { value: "ignore", label: "忽略证书错误" },
  { value: "strict", label: "严格校验证书" },
];

/** 直连成败判定方式选项（CustomSelect 消费；取值与后端 `HttpSuccessCheck` 同源） */
export const HTTP_SUCCESS_CHECK_OPTIONS: Array<{ value: HttpSuccessCheck; label: string }> = [
  { value: "response", label: "响应关键字（默认）" },
  { value: "network", label: "网络检测" },
];

/**
 * 方案的 `http_ignore_https_errors`（三态布尔）→ 下拉值。
 *
 * `null`/`undefined` 均为"未设置"，即跟随全局 `browser.ignore_https_errors`
 * （默认 true）——与浏览器渠道同口径，自签证书门户才不会被直连渠道挡在门外。
 */
export function certPolicyFromValue(value: boolean | null | undefined): HttpCertPolicy {
  if (value === true) return "ignore";
  if (value === false) return "strict";
  return "follow";
}

/** 下拉值 → 方案字段（`follow` 落回 `null`，保持"未设置"可跨版本跟随全局） */
export function certPolicyToValue(policy: HttpCertPolicy): boolean | null {
  if (policy === "ignore") return true;
  if (policy === "strict") return false;
  return null;
}

/** 证书策略说明：讲清"实际会怎样"与"什么时候该改"，避免用户盲目选严格后门户登不上 */
export function certPolicyHint(policy: HttpCertPolicy): string {
  switch (policy) {
    case "ignore":
      return "始终不校验证书：自签名门户可用，但请求携带的明文凭据可能被中间人截获。";
    case "strict":
      return "始终校验证书：安全性更高，但自签名 / 过期证书的门户会直接报证书错误。";
    default:
      return "跟随「设置 · 浏览器」的证书设置（默认忽略），与浏览器自动化的判定一致。";
  }
}

/**
 * 测试结果的「下一步该怎么办」提示。
 *
 * 与 {@link httpTestOutcomeLabel} 配对：标签说明**发生了什么**，本函数说明
 * **该改哪里**。没有它时用户只看到「未命中成功标识」这类结论，不知道该动哪个字段
 * （实测最常卡住的正是 assertion_failed：门户响应恒为 200，判定完全依赖关键字）。
 */
export function httpTestOutcomeHint(outcome: string | undefined): string {
  switch (outcome) {
    case "success":
      // 文案保持"上下文中立"：测试入口有两个（直连任务编辑器 / 方案编辑器），
      // 说「保存方案」会让任务页的用户去点一个不存在的按钮
      return "请求已按预期判定成功。保存后，方案里选中这个直连任务即会走直连，无需 Python 与浏览器。";
    case "invalid_credential":
      return "门户明确拒绝了这次请求：先确认账号密码正确；若门户要求密码加密或附加签名字段，请在直连任务的「凭据变换脚本」里按门户逻辑生成。";
    case "assertion_failed":
      return "请求送达了，但没在响应里找到成功标识。请核对「成功关键字」是否与门户真实响应一致；若该门户响应总是 HTTP 200，必须填写成功与失败关键字，否则错误凭据也会被当成成功。";
    case "network_error":
      return "请求没能送达门户。确认电脑已连上校园网、地址可被本机访问；校园网网关多为内网地址，需处于同一网络内。";
    case "unknown_error":
      return "配置或脚本执行出错，请求没有发出。结果里的「脚本错误」给出具体原因，修正后重试。";
    case "cancelled":
      return "测试被取消，可重新发送。";
    default:
      return "测试未通过。请对照下方的请求内容与响应片段逐项检查。";
  }
}

/**
 * 请求地址 / 请求头 / 请求体可用的模板占位符。
 *
 * 前五类是执行器内置字段，其余为凭据变换脚本 `transform()` 的返回字段
 * （脚本返回同名键会覆盖内置值）。契约见 `src/login/http_login.rs` 的
 * `run_once`（内置 vars）与 `substitute`（未知占位符原样保留）。
 */
export const HTTP_TEMPLATE_PLACEHOLDERS = [
  "{username}",
  "{password}",
  "{isp}",
  "{auth_url}",
  "{local_ip}",
  "{local_mac}",
] as const;

/**
 * 凭据变换脚本可用的内置函数（与执行器 `register_builtins` 注册的全局函数一一对应）。
 *
 * 这里是纯计算函数，无网络与文件访问；沙箱另有递归深度 64、循环 10 万次与
 * 500ms 墙钟上限。**新增内置函数时必须同步此表**，否则面板与向导会漏报，
 * 用户只能靠试错发现（`loginChannel.test.ts` 锁定该清单）。
 */
export const HTTP_CRYPTO_BUILTINS = [
  "md5(text)",
  "sha1(text)",
  "sha256(text)",
  "hmac_sha256(key, data)",
  "base64_encode(text)",
  "base64_decode(text)",
  "hex_encode(text)",
  "url_encode(text)",
  "now_ms()",
] as const;

/**
 * `transform()` 的 `ctx` 参数字段（与执行器的 `JsValue::from_json` 构造一一对应）。
 *
 * `isp` 为方案的运营商字段原样透传（预设「移动/联通/电信」或自定义关键字，
 * 未选择为空串），门户侧的表示法（Dr.COM 的 @cmcc 后缀等）由脚本自行映射；
 * `page` 为认证页原文（抓取失败时为空串）；`local_ip` / `local_mac` 为本机主用
 * 接口地址，取不到时为空串，脚本必须容忍——eportal / Dr.COM 类门户的字段密钥
 * 由来源 IP 推导，没有它就只能退回从 `page` 里找补。
 */
export const HTTP_CRYPTO_CTX_FIELDS = [
  "username",
  "password",
  "isp",
  "auth_url",
  "page",
  "local_ip",
  "local_mac",
] as const;

/** 请求头模板示例（表单 placeholder 与向导预填共用，避免两处文案漂移） */
export const HTTP_HEADERS_EXAMPLE =
  "Content-Type: application/x-www-form-urlencoded\nReferer: {auth_url}";

/** 请求体模板示例（POST 场景） */
export const HTTP_BODY_EXAMPLE = "username={username}&password={password}";

/**
 * 凭据变换脚本骨架：直接可跑的最小 transform（向导「脚本」步可一键填入） */
export const HTTP_CRYPTO_SCRIPT_SKELETON = `function transform(ctx) {
  // 返回对象的字段可被 {字段名} 占位符引用
  return {
    password: md5(ctx.password),
  };
}`;

/**
 * MAC 形态转换示例：{local_mac} 固定为小写冒号分隔（aa:bb:cc:dd:ee:ff），
 * 门户常要求裸十六进制 / 大写 / 连字符形态，直接引用会提交错误格式。
 * 注入占位符说明与 ctx 字段说明两处（`http-template-help` 与脚本卡）。
 */
export const HTTP_MAC_FORMAT_NOTE = `{local_mac} 固定为小写冒号分隔（如 aa:bb:cc:dd:ee:ff）。门户要求裸十六进制 / 大写 / 连字符时，请在脚本里转换后引用，如：ctx.local_mac.replace(/:/g, "") → aabbccddeeff、toUpperCase() → 大写。eportal / Dr.COM 类门户靠来源 IP 防串号，多数场景 MAC 提交空值即可。`;

/**
 * 直连配置的必填缺口清单（向导据此决定能否进入下一步 / 发送测试）。
 *
 * 与后端校验口径对齐，但**更早失败**：后端 `HttpTaskTestBody` 的校验在发请求前
 * 才返回 400，用户点一次「发送测试请求」才知道缺什么；向导逐步收敛到当前字段。
 *
 * 注意 `password` 只在「新建/未保存方案」时才算必填——已保存方案可留空由后端
 * 回退本机已保存凭据（见 `POST /api/http-tasks/test` 的 profile_id 回退），
 * 此时提示用户手填密码是错的。
 */
export function httpConfigGaps(draft: {
  url?: string;
  username?: string;
  password?: string;
}, options?: { hasSavedProfile?: boolean }): string[] {
  const gaps: string[] = [];
  if (!(draft.username ?? "").trim()) gaps.push("账号");
  if (!options?.hasSavedProfile && !(draft.password ?? "").trim()) gaps.push("密码");
  if (!(draft.url ?? "").trim()) gaps.push("请求地址");
  return gaps;
}

/**
 * 请求地址是否含 `{password}` 且为 GET。
 *
 * GET 把凭据放进查询串，网关/代理/系统日志可能记录完整地址；程序只保证自身
 * 日志脱敏。向导据此给出「能用 POST 就优先 POST」的针对性提示。
 */
export function isCredentialExposedViaGet(
  method: string | undefined,
  url: string | undefined,
): boolean {
  return method === "GET" && (url ?? "").includes("{password}");
}

/**
 * 该登录渠道是否需要 Python / 浏览器运行环境。
 *
 * 直连请求与自定义脚本都在 Rust 进程内完成登录（前者发 HTTP、后者起本地子进程），
 * 不拉起 Python Worker 与 Playwright，因此环境未就绪（python/worker/playwright
 * 任一缺失）对它们没有任何影响——仪表盘据此抑制「环境未就绪」横幅，避免免
 * Python/浏览器的用户被无意义的提示长期打扰。浏览器自动化需要该环境。
 *
 * 未知取值一律按"需要"处理（保守侧）：漏报会让人以为环境无关而卡在假登录上。
 */
export function channelNeedsRuntimeEnvironment(
  channel: LoginChannel | string | undefined,
): boolean {
  return channel !== "http" && channel !== "script";
}

/**
 * 浏览器任务下拉的选项构造与绑定显示。
 *
 * 回归背景：下拉曾有一个空值项「使用内置默认任务」，它指向的其实就是任务列表里
 * 的 `default`（播种名「通用登录」）——同一件事出现两个条目，用户无从选择；
 * 而 `default` 那条又不带任何说明，看不出它就是"内置默认"。
 *
 * 两个口径（单测直接覆盖这两个函数，避免组件与测试各写一份而漂移）：
 * 1. 选项只来自任务列表，`default` 那条加「（内置默认）」后缀承袭原语义；
 * 2. 绑定显示走"未绑定 → 显示 default"，但**读取绝不写回草稿**——否则编辑器
 *    一打开就与服务端不一致，立刻显示「未保存」。
 */

/** 任务下拉选项（default 一条加「内置默认」标注） */
export function browserTaskOptions(
  tasks: Array<{ id: string; name?: string }>,
  defaultTaskId: string,
): Array<{ value: string; label: string }> {
  return tasks.map((t) => ({
    value: t.id,
    label: t.id === defaultTaskId ? `${t.name || t.id}（内置默认）` : t.name || t.id,
  }));
}

/**
 * 直连任务下拉选项。
 *
 * 与浏览器任务不同，首项是显式的「未绑定」：直连没有可内置的兜底任务（门户地址
 * 因人而异），空值必须能被表达出来，否则用户没法把一个方案从直连任务上摘下来。
 * 未绑定的后果是直连登录直接失败，故文案讲清而不是留个空项。
 */
export function httpTaskOptions(
  tasks: Array<{ id: string; name?: string }>,
): Array<{ value: string; label: string }> {
  return [
    { value: "", label: "未绑定（直连登录不可用）" },
    ...tasks.map((t) => ({ value: t.id, label: t.name || t.id })),
  ];
}

/**
 * 脚本任务下拉选项。
 *
 * 与直连同一口径：首项是显式的「未绑定」，因为脚本渠道也没有可内置的兜底任务——
 * 登录逻辑只能由用户自己写。未绑定的后果是脚本登录直接失败，故文案讲清而不是留空项。
 */
export function scriptTaskOptions(
  tasks: Array<{ id: string; name?: string }>,
): Array<{ value: string; label: string }> {
  return [
    { value: "", label: "未绑定（脚本登录不可用）" },
    ...tasks.map((t) => ({ value: t.id, label: t.name || t.id })),
  ];
}

/** 渠道徽标的悬停说明（列在网络匹配标签旁，一句话讲清这个渠道怎么登） */
export function loginChannelHint(channel: LoginChannel | string | undefined): string {
  if (channel === "http") return "直连请求：在程序内发登录请求，不启动浏览器";
  if (channel === "script") return "自定义脚本：由绑定的脚本任务完成登录，不启动浏览器";
  return "浏览器自动化：按任务步骤操作登录页";
}

/**
 * 脚本登录渠道注入的环境变量（与后端 `login::script_login` 的 `LOGIN_ENV_*` 同源）。
 *
 * `loginChannel.test.ts` 钉住这份清单：后端改名而前端没跟着改，用户会照着
 * 界面上写好的变量名写出一个永远读到空值的脚本，且没有任何报错。
 */
export const SCRIPT_LOGIN_ENV_VARS = [
  "CAMPUS_USERNAME",
  "CAMPUS_PASSWORD",
  "CAMPUS_ISP",
  "CAMPUS_AUTH_URL",
] as const;

/**
 * 脚本登录的契约说明（面板 `?` 气泡与任务页脚本指南共用，避免两处文案漂移）。
 */
export const SCRIPT_LOGIN_CONTRACT_NOTE =
  `程序起本地子进程执行该脚本任务的脚本，把登录整个交给它，不启动浏览器与 Python Worker。\n\n` +
  `脚本从环境变量取凭据：${SCRIPT_LOGIN_ENV_VARS.map((v) => v).join(" / ")}` +
  `（脚本任务本身不做 {{USERNAME}} 这类模板替换）。\n\n` +
  `退出码 0 = 本次尝试成功，程序随后仍会做一次登录后网络验证来确认真登上了；` +
  `非 0 = 本次尝试失败，按方案的重试策略重发，重试预算耗尽才判失败。\n\n` +
  `超时取脚本任务自己的「超时」设置；脚本的 stdout/stderr 末尾会写进登录历史，` +
  `其中出现的密码会被抹成 ***。`;

/**
 * 绑定值 → 下拉显示的 value。
 *
 * 未绑定（空串）时显示为内置默认任务：两者对登录等价——后端 `resolve_task_choice`
 * 在 `active_task` 为空与为 `default` 时解析结果相同（都回退内置任务）。
 */
export function taskBindingDisplay(activeTask: string, defaultTaskId: string): string {
  return activeTask || defaultTaskId;
}
