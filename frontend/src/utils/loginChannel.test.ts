/**
 * 登录渠道展示映射的单元测试。
 * 标签与 outcome 文案是列表卡、直连面板、后续引导向导共用的事实源，
 * 出现漂移会导致同一渠道在不同页面叫法不一致。
 */
import { describe, expect, it } from "vitest";
import {
  certPolicyFromValue,
  certPolicyHint,
  certPolicyToValue,
  channelNeedsRuntimeEnvironment,
  HTTP_CERT_POLICY_OPTIONS,
  HTTP_CRYPTO_BUILTINS,
  HTTP_METHOD_OPTIONS,
  httpConfigGaps,
  httpTaskOptions,
  httpTestOutcomeHint,
  httpTestOutcomeLabel,
  isCredentialExposedViaGet,
  loginChannelHint,
  loginChannelIcon,
  loginChannelLabel,
  loginChannelShortLabel,
  SCRIPT_LOGIN_CONTRACT_NOTE,
  SCRIPT_LOGIN_ENV_VARS,
  scriptTaskOptions,
} from "./loginChannel";

// ============ 直连 HTTPS 证书策略（三态） ============

describe("certPolicy 三态映射", () => {
  it("null/undefined 均为「跟随全局」，与浏览器渠道同口径", () => {
    // 校园网门户多为自签名证书，全局默认忽略；未设置时必须落到该口径，
    // 否则「浏览器能登、直连报证书错误」这个不一致会重新出现
    expect(certPolicyFromValue(null)).toBe("follow");
    expect(certPolicyFromValue(undefined)).toBe("follow");
  });

  it("显式布尔值映射到对应策略", () => {
    expect(certPolicyFromValue(true)).toBe("ignore");
    expect(certPolicyFromValue(false)).toBe("strict");
  });

  it("往返转换保持语义（follow 必须落回 null 而非 false）", () => {
    // follow→false 会让「未设置」变成「显式严格校验」，自签门户从此登不上；
    // 这条断言锁定的正是该回归
    for (const value of [null, undefined, true, false] as const) {
      expect(certPolicyToValue(certPolicyFromValue(value))).toBe(value ?? null);
    }
  });

  it("三种策略各有选项与说明文案", () => {
    for (const option of HTTP_CERT_POLICY_OPTIONS) {
      expect(certPolicyHint(option.value).length).toBeGreaterThan(0);
    }
    expect(HTTP_CERT_POLICY_OPTIONS.map((o) => o.value)).toEqual([
      "follow",
      "ignore",
      "strict",
    ]);
  });

  it("严格策略的说明点明代价（自签门户会失败）", () => {
    expect(certPolicyHint("strict")).toContain("证书错误");
    expect(certPolicyHint("ignore")).toContain("截获");
  });
});

describe("loginChannelLabel", () => {
  it("浏览器、直连与脚本各自有稳定中文标签", () => {
    expect(loginChannelLabel("browser")).toBe("浏览器自动化");
    expect(loginChannelLabel("http")).toBe("直连请求");
    expect(loginChannelLabel("script")).toBe("自定义脚本");
  });

  it("未知/缺失值回落浏览器（存量方案未写该字段的场景）", () => {
    expect(loginChannelLabel(undefined)).toBe("浏览器自动化");
    expect(loginChannelLabel("")).toBe("浏览器自动化");
  });

  it("紧凑标签与完整标签对 http 一致、对 browser 更短", () => {
    expect(loginChannelShortLabel("http")).toBe("直连请求");
    expect(loginChannelShortLabel("browser")).toBe("浏览器");
    expect(loginChannelShortLabel("script")).toBe("脚本");
  });

  it("每个渠道都有徽标图标与悬停说明", () => {
    // 图标名必须落在 IconApp 注册表里（返回类型已收窄，这里是运行时兜底）
    expect(loginChannelIcon("browser")).toBe("chrome");
    expect(loginChannelIcon("http")).toBe("globe");
    expect(loginChannelIcon("script")).toBe("code");
    for (const channel of ["browser", "http", "script"] as const) {
      expect(loginChannelHint(channel).length).toBeGreaterThan(0);
    }
  });
});

describe("脚本登录渠道的契约文案", () => {
  it("环境变量清单与后端 login::script_login 一一对应", () => {
    // 后端改名而这里没跟着改，用户会照界面写一个永远读到空值的脚本，且不报错
    expect([...SCRIPT_LOGIN_ENV_VARS]).toEqual([
      "CAMPUS_USERNAME",
      "CAMPUS_PASSWORD",
      "CAMPUS_ISP",
      "CAMPUS_AUTH_URL",
    ]);
  });

  it("契约说明讲清四件事：谁来跑、凭据从哪来、怎么算成功、输出去哪", () => {
    for (const key of SCRIPT_LOGIN_ENV_VARS) {
      expect(SCRIPT_LOGIN_CONTRACT_NOTE).toContain(key);
    }
    expect(SCRIPT_LOGIN_CONTRACT_NOTE).toContain("退出码 0");
    expect(SCRIPT_LOGIN_CONTRACT_NOTE).toContain("网络验证");
    expect(SCRIPT_LOGIN_CONTRACT_NOTE).toContain("***");
  });

  it("脚本任务下拉首项是「未绑定」——脚本渠道没有兜底任务", () => {
    const options = scriptTaskOptions([{ id: "checkin", name: "每日签到" }]);
    expect(options[0]).toEqual({ value: "", label: "未绑定（脚本登录不可用）" });
    expect(options[1]).toEqual({ value: "checkin", label: "每日签到" });
    // 直连侧同一口径：两个进程内渠道在"未绑定即不可用"上没有分歧
    expect(httpTaskOptions([])[0].value).toBe("");
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
  // 仪表盘据此抑制「环境未就绪」横幅：直连请求与自定义脚本都在 Rust 进程内
  // 完成登录（前者发 HTTP、后者起子进程），不拉起 Python Worker 与浏览器；
  // 浏览器自动化必须依赖该环境
  it("直连请求不需要运行环境", () => {
    expect(channelNeedsRuntimeEnvironment("http")).toBe(false);
  });

  it("自定义脚本不需要运行环境（起的是本地子进程，不是 Worker）", () => {
    expect(channelNeedsRuntimeEnvironment("script")).toBe(false);
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

describe("httpTestOutcomeHint", () => {
  // 标签说明"发生了什么"，hint 说明"该改哪里"——后者是用户从失败走向成功的
  // 唯一指引，缺了它用户只会反复重试同一个错配置
  it("每个可映射 outcome 都给出针对性的下一步", () => {
    const hinted = [
      "success",
      "invalid_credential",
      "assertion_failed",
      "network_error",
      "unknown_error",
      "cancelled",
    ].map((o) => httpTestOutcomeHint(o));
    for (const hint of hinted) {
      expect(hint.length).toBeGreaterThan(10);
    }
    // 不同结论必须给出不同指引（复制粘贴导致文案串位是最容易犯的错）
    expect(new Set(hinted).size).toBe(hinted.length);
  });

  it("「响应恒 200」门户的坑在未命中成功标识时被点名", () => {
    expect(httpTestOutcomeHint("assertion_failed")).toContain("200");
  });

  it("未知/缺失 outcome 回落通用指引而非空白", () => {
    expect(httpTestOutcomeHint(undefined)).toBeTruthy();
    expect(httpTestOutcomeHint("navigation_timeout")).toBeTruthy();
  });
});

describe("httpConfigGaps", () => {
  it("三项齐备时无缺口", () => {
    expect(
      httpConfigGaps({ username: "20230001", password: "pw", url: "http://10.0.0.1/login" }),
    ).toEqual([]);
  });

  it("按「账号 → 密码 → 请求地址」顺序列出缺口（与向导步骤顺序一致）", () => {
    expect(httpConfigGaps({})).toEqual(["账号", "密码", "请求地址"]);
    expect(httpConfigGaps({ username: "u" })).toEqual(["密码", "请求地址"]);
    expect(httpConfigGaps({ username: "u", password: "p" })).toEqual(["请求地址"]);
  });

  it("已保存方案不把空密码算作缺口（后端按 profile_id 回退本机已保存凭据）", () => {
    expect(
      httpConfigGaps(
        { username: "u", password: "", url: "http://10.0.0.1/login" },
        { hasSavedProfile: true },
      ),
    ).toEqual([]);
    // 同一输入在未保存方案下必须报缺密码——否则用户点测试只会拿到后端的 400
    expect(
      httpConfigGaps(
        { username: "u", password: "", url: "http://10.0.0.1/login" },
        { hasSavedProfile: false },
      ),
    ).toEqual(["密码"]);
  });

  it("纯空白按缺失处理", () => {
    expect(httpConfigGaps({ username: "  ", password: "\t", url: " " })).toEqual([
      "账号",
      "密码",
      "请求地址",
    ]);
  });
});

describe("isCredentialExposedViaGet", () => {
  it("GET 且地址含 {password} 时判为暴露", () => {
    expect(isCredentialExposedViaGet("GET", "http://10.0.0.1/login?p={password}")).toBe(true);
  });

  it("POST 不暴露（凭据在请求体里）", () => {
    expect(isCredentialExposedViaGet("POST", "http://10.0.0.1/login?p={password}")).toBe(false);
  });

  it("GET 但地址不含密码时不误报", () => {
    expect(isCredentialExposedViaGet("GET", "http://10.0.0.1/login?u={username}")).toBe(false);
    expect(isCredentialExposedViaGet("GET", undefined)).toBe(false);
    expect(isCredentialExposedViaGet(undefined, "http://10.0.0.1/?p={password}")).toBe(false);
  });
});

describe("HTTP_CRYPTO_BUILTINS", () => {
  // 前端向用户宣告的可用函数清单必须与执行器 register_builtins 一致；
  // 少列会让用户以为函数不存在（改动脚本绕远路），多列会让脚本报错
  it("与执行器注册的全局函数一一对应", () => {
    expect([...HTTP_CRYPTO_BUILTINS]).toEqual([
      "md5(text)",
      "sha1(text)",
      "sha256(text)",
      "hmac_sha256(key, data)",
      "base64_encode(text)",
      "base64_decode(text)",
      "hex_encode(text)",
      "url_encode(text)",
      "now_ms()",
    ]);
  });
});
