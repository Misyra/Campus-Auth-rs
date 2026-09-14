import { describe, expect, it } from "vitest";
import { LOGIN_SCRIPT_TEMPLATE, NEW_SCRIPT_STUB } from "./scriptTemplates";

describe("script templates", () => {
  it("do not depend on undeclared third-party Python packages", () => {
    expect(NEW_SCRIPT_STUB).not.toContain("httpx");
    expect(LOGIN_SCRIPT_TEMPLATE).not.toContain("httpx");
    // 只允许 Python 标准库：示例用 urllib.request
    expect(LOGIN_SCRIPT_TEMPLATE).toContain("from urllib.request import urlopen");
  });

  it("脚本模板不再宣称是登录手段，改为引导到直连请求", () => {
    // 脚本不能参与登录（加载时仅嵌入浏览器任务，脚本以空步骤执行），
    // 模板若继续以「登录脚本」定位，会把用户带进必然失败的路径
    expect(LOGIN_SCRIPT_TEMPLATE).not.toContain("登录脚本");
    expect(LOGIN_SCRIPT_TEMPLATE).toContain("直连请求");
    expect(NEW_SCRIPT_STUB).not.toContain("登录脚本");
  });
});
