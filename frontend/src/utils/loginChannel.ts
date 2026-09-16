/**
 * 登录渠道（浏览器自动化 / 直连请求）展示映射。
 *
 * 单一事实源：方案列表卡徽标、直连面板标题、后续设置页与引导向导的分流文案
 * 均由此派生，避免同一枚举在多处各写一套中文标签而漂移。
 */

import type { HttpLoginMethod, LoginChannel } from "../api/types";

/** 登录渠道 → 用户可见标签 */
export function loginChannelLabel(channel: LoginChannel | string | undefined): string {
  return channel === "http" ? "直连请求" : "浏览器自动化";
}

/** 登录渠道 → 列表卡等紧凑场景的短标签 */
export function loginChannelShortLabel(channel: LoginChannel | string | undefined): string {
  return channel === "http" ? "直连请求" : "浏览器";
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
 * 测试结果的「下一步该怎么办」提示。
 *
 * 与 {@link httpTestOutcomeLabel} 配对：标签说明**发生了什么**，本函数说明
 * **该改哪里**。没有它时用户只看到「未命中成功标识」这类结论，不知道该动哪个字段
 * （实测最常卡住的正是 assertion_failed：门户响应恒为 200，判定完全依赖关键字）。
 */
export function httpTestOutcomeHint(outcome: string | undefined): string {
  switch (outcome) {
    case "success":
      return "请求已按预期判定成功。点「保存方案」后，自动登录就会走直连，无需 Python 与浏览器。";
    case "invalid_credential":
      return "门户明确拒绝了这次请求：先确认账号密码正确；若门户要求密码加密或附加签名字段，请在下方「凭据变换脚本」里按门户逻辑生成。";
    case "assertion_failed":
      return "请求送达了，但没在响应里找到成功标识。请核对「成功关键字」是否与门户真实响应一致；若该门户响应总是 HTTP 200，必须填写成功与失败关键字，否则错误凭据也会被当成成功。";
    case "network_error":
      return "请求没能送达门户。确认电脑已连上校园网、地址可被本机访问；校园网网关多为内网地址，需处于同一网络内。";
    case "unknown_error":
      return "配置或脚本执行出错，请求没有发出。下方「脚本错误」给出具体原因，修正后重试。";
    case "cancelled":
      return "测试被取消，可重新发送。";
    default:
      return "测试未通过。请对照下方的请求内容与响应片段逐项检查。";
  }
}

/**
 * 请求地址 / 请求头 / 请求体可用的模板占位符。
 *
 * 前四类是执行器内置字段，其余为凭据变换脚本 `transform()` 的返回字段
 * （脚本返回同名键会覆盖内置值）。契约见 `src/login/http_login.rs` 的
 * `run_once`（内置 vars）与 `substitute`（未知占位符原样保留）。
 */
export const HTTP_TEMPLATE_PLACEHOLDERS = [
  "{username}",
  "{password}",
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
 * `page` 为认证页原文（抓取失败时为空串）；`local_ip` / `local_mac` 为本机主用
 * 接口地址，取不到时为空串，脚本必须容忍——eportal / Dr.COM 类门户的字段密钥
 * 由来源 IP 推导，没有它就只能退回从 `page` 里找补。
 */
export const HTTP_CRYPTO_CTX_FIELDS = [
  "username",
  "password",
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
 * 与后端校验口径对齐，但**更早失败**：后端 `HttpLoginTestBody` 的校验在发请求前
 * 才返回 400，用户点一次「发送测试请求」才知道缺什么；向导逐步收敛到当前字段。
 *
 * 注意 `password` 只在「新建/未保存方案」时才算必填——已保存方案可留空由后端
 * 回退本机已保存凭据（见 `POST /api/profiles/http-login-test` 的 profile_id 回退），
 * 此时提示用户手填密码是错的。
 */
export function httpConfigGaps(draft: {
  http_url?: string;
  username?: string;
  password?: string;
}, options?: { hasSavedProfile?: boolean }): string[] {
  const gaps: string[] = [];
  if (!(draft.username ?? "").trim()) gaps.push("账号");
  if (!options?.hasSavedProfile && !(draft.password ?? "").trim()) gaps.push("密码");
  if (!(draft.http_url ?? "").trim()) gaps.push("请求地址");
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
 * 直连请求在 Rust 进程内完成登录，不拉起 Python Worker 与 Playwright，
 * 因此环境未就绪（python/worker/playwright 任一缺失）对它没有任何影响——
 * 仪表盘据此抑制「环境未就绪」横幅，避免免 Python/浏览器的用户被无意义的
 * 提示长期打扰。浏览器自动化需要该环境。
 */
export function channelNeedsRuntimeEnvironment(
  channel: LoginChannel | string | undefined,
): boolean {
  return channel !== "http";
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
 * 绑定值 → 下拉显示的 value。
 *
 * 未绑定（空串）时显示为内置默认任务：两者对登录等价——后端 `resolve_task_choice`
 * 在 `active_task` 为空与为 `default` 时解析结果相同（都回退内置任务）。
 */
export function taskBindingDisplay(activeTask: string, defaultTaskId: string): string {
  return activeTask || defaultTaskId;
}
