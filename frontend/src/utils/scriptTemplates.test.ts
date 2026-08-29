import { describe, expect, it } from "vitest";
import { LOGIN_SCRIPT_TEMPLATE, NEW_SCRIPT_STUB } from "./scriptTemplates";

describe("script templates", () => {
  it("do not depend on undeclared third-party Python packages", () => {
    expect(NEW_SCRIPT_STUB).not.toContain("httpx");
    expect(LOGIN_SCRIPT_TEMPLATE).not.toContain("httpx");
    expect(LOGIN_SCRIPT_TEMPLATE).toContain("from urllib.request import Request, urlopen");
  });
});
