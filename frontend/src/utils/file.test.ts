/**
 * getBinaryName 与 shareFileName 的单元测试（纯函数，无 DOM 依赖）。
 */
import { describe, it, expect } from "vitest";
import { getBinaryName, isProfileSharePayload, shareFileName, unwrapSharePayload } from "./file";

describe("getBinaryName", () => {
  it("空路径回退 Python", () => {
    expect(getBinaryName("")).toBe("Python");
  });

  it("去目录与常见扩展名", () => {
    expect(getBinaryName("C:\\tools\\python.exe")).toBe("python");
    expect(getBinaryName("/usr/bin/python3")).toBe("python3");
    expect(getBinaryName("pwsh.cmd")).toBe("pwsh");
    expect(getBinaryName("run.sh")).toBe("run");
    expect(getBinaryName("script.bat")).toBe("script");
  });

  it("保留多点文件名主体与无扩展名", () => {
    expect(getBinaryName("C:/a/my.tool.exe")).toBe("my.tool");
    expect(getBinaryName("uv")).toBe("uv");
  });
});

describe("shareFileName", () => {
  it("保留中文名，便于用户区分文件", () => {
    expect(shareFileName("宿舍移动", "dorm")).toBe("campus-auth-profile-宿舍移动.json");
  });

  it("剔除 Windows 非法文件名字符（否则下载直接失败）", () => {
    const name = shareFileName('a/b\\c:d*e?f"g<h>i|j', "x");
    expect(name).toBe("campus-auth-profile-a-b-c-d-e-f-g-h-i-j.json");
    // 逐个确认没漏掉任何非法字符
    for (const ch of ['/', "\\", ":", "*", "?", '"', "<", ">", "|"]) {
      expect(name.slice("campus-auth-profile-".length, -".json".length)).not.toContain(ch);
    }
  });

  it("空白折叠且不留首尾连字符", () => {
    expect(shareFileName("  my   dorm  ", "x")).toBe("campus-auth-profile-my-dorm.json");
    expect(shareFileName("...", "x")).toBe("campus-auth-profile-profile.json");
  });

  it("名为空时回退到 ID，再空则回退固定名", () => {
    expect(shareFileName("", "dorm")).toBe("campus-auth-profile-dorm.json");
    expect(shareFileName("  ", "  ")).toBe("campus-auth-profile-profile.json");
  });

  it("超长名截断到 60 字符，避免超出文件系统上限", () => {
    const name = shareFileName("x".repeat(200), "x");
    const body = name.slice("campus-auth-profile-".length, -".json".length);
    expect(body).toHaveLength(60);
  });

  it("控制字符被清除（防止写入非法文件名）", () => {
    const name = shareFileName("a\u0000b\u001fc", "x");
    expect(name).toBe("campus-auth-profile-a-b-c.json");
  });
});

describe("isProfileSharePayload / unwrapSharePayload", () => {
  const valid = { campus_auth_profile: 1, profile: { name: "宿舍移动" } };

  it("认本应用导出的顶层形态", () => {
    expect(isProfileSharePayload(valid)).toBe(true);
  });

  it("认 { data: ... } API 信封包裹（与后端 parse_share_payload 同口径）", () => {
    expect(isProfileSharePayload({ data: valid })).toBe(true);
    expect(unwrapSharePayload({ data: valid })).toEqual(valid);
  });

  it("缺格式标记或 profile 一律拒绝（不做猜字段的宽松解析）", () => {
    expect(isProfileSharePayload({ profile: { name: "x" } })).toBe(false);
    expect(isProfileSharePayload({ campus_auth_profile: 1 })).toBe(false);
    expect(isProfileSharePayload({ campus_auth_profile: "1", profile: {} })).toBe(false);
  });

  it("非对象与数组一律拒绝", () => {
    for (const bad of [null, undefined, "str", 42, [], [valid], [[valid]]]) {
      expect(isProfileSharePayload(bad)).toBe(false);
      expect(unwrapSharePayload(bad)).toBeNull();
    }
  });

  it("profile 为数组时拒绝（结构不符）", () => {
    expect(isProfileSharePayload({ campus_auth_profile: 1, profile: [] })).toBe(false);
  });
});
