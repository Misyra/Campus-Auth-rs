/**
 * 登录渠道展示映射的单元测试。
 * 标签与 outcome 文案是列表卡、直连面板、后续引导向导共用的事实源，
 * 出现漂移会导致同一渠道在不同页面叫法不一致。
 */
import { describe, expect, it } from "vitest";
import {
  channelNeedsRuntimeEnvironment,
  HTTP_METHOD_OPTIONS,
  httpTestOutcomeLabel,
  loginChannelLabel,
  loginChannelShortLabel,
} from "./loginChannel";

describe("loginChannelLabel", () => {
  it("浏览器与直连各自有稳定中文标签", () => {
    expect(loginChannelLabel("browser")).toBe("浏览器自动化");
    expect(loginChannelLabel("http")).toBe("直连请求");
  });

  it("未知/缺失值回落浏览器（存量方案未写该字段的场景）", () => {
    expect(loginChannelLabel(undefined)).toBe("浏览器自动化");
    expect(loginChannelLabel("")).toBe("浏览器自动化");
  });

  it("紧凑标签与完整标签对 http 一致、对 browser 更短", () => {
    expect(loginChannelShortLabel("http")).toBe("直连请求");
    expect(loginChannelShortLabel("browser")).toBe("浏览器");
  });
});

describe("httpTestOutcomeLabel", () => {
  it("直连路径可能产生的 outcome 均有专门文案", () => {
    expect(httpTestOutcomeLabel("success")).toBe("请求判定成功");
    expect(httpTestOutcomeLabel("invalid_credential")).toBe("门户拒绝凭据");
    expect(httpTestOutcomeLabel("assertion_failed")).toBe("未命中成功标识");
    expect(httpTestOutcomeLabel("network_error")).toBe("请求未送达");
    expect(httpTestOutcomeLabel("unknown_error")).toBe("配置或脚本错误");
  });

  it("未映射或缺失的 outcome 回落通用文案而非空白", () => {
    expect(httpTestOutcomeLabel(undefined)).toBe("测试未通过");
    expect(httpTestOutcomeLabel("navigation_timeout")).toBe("测试未通过");
  });
});

describe("HTTP_METHOD_OPTIONS", () => {
  it("仅含 GET/POST 且值与后端枚举字面量一致", () => {
    expect(HTTP_METHOD_OPTIONS.map((o) => o.value)).toEqual(["GET", "POST"]);
  });
});

describe("channelNeedsRuntimeEnvironment", () => {
  // 仪表盘据此抑制「环境未就绪」横幅：直连请求不拉起 Python Worker
  // 与浏览器，环境缺失对它无影响；浏览器自动化必须依赖该环境
  it("直连请求不需要运行环境", () => {
    expect(channelNeedsRuntimeEnvironment("http")).toBe(false);
  });

  it("浏览器自动化需要运行环境", () => {
    expect(channelNeedsRuntimeEnvironment("browser")).toBe(true);
  });

  it("缺失/未知值按需要环境处理（与默认渠道 browser 一致，宁多提示不静默漏提示）", () => {
    expect(channelNeedsRuntimeEnvironment(undefined)).toBe(true);
    expect(channelNeedsRuntimeEnvironment("")).toBe(true);
    expect(channelNeedsRuntimeEnvironment("something-else")).toBe(true);
  });
});
