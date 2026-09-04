/**
 * 无浏览器文案握手测试：后端 `browser::NO_BROWSER_MESSAGE`（及登录侧离线兜底
 * 后缀 `；当前无可用浏览器，…`）必须命中 `isNoBrowserMessage`，否则手动登录
 * 的弹窗引导会静默退化成普通通知。改动任一侧文案时先改这里，再同步另一侧。
 */
import { describe, it, expect } from "vitest";
import { isNoBrowserMessage } from "./client";

/** 与后端 `browser::NO_BROWSER_MESSAGE` 逐字同步 */
const BACKEND_NO_BROWSER_MESSAGE =
  "当前无可用浏览器，请下载 Chromium（设置 · 浏览器页可一键安装）";

describe("isNoBrowserMessage", () => {
  it("识别后端无浏览器文案与离线兜底后缀", () => {
    expect(isNoBrowserMessage(BACKEND_NO_BROWSER_MESSAGE)).toBe(true);
    expect(
      isNoBrowserMessage(
        `环境未就绪，自动初始化失败: uv 下载失败；${BACKEND_NO_BROWSER_MESSAGE.slice(2)}`,
      ),
    ).toBe(true);
  });

  it("拒绝普通登录失败文案与非字符串", () => {
    expect(isNoBrowserMessage("配置不完整: username, password")).toBe(false);
    expect(isNoBrowserMessage("auth_url 不可达: http://127.0.0.1:9/")).toBe(false);
    expect(isNoBrowserMessage("登录成功")).toBe(false);
    expect(isNoBrowserMessage("")).toBe(false);
    expect(isNoBrowserMessage(null)).toBe(false);
    expect(isNoBrowserMessage(undefined)).toBe(false);
    expect(isNoBrowserMessage(42)).toBe(false);
  });
});
