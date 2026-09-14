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
