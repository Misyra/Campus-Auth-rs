/**
 * getBinaryName 的单元测试（纯函数，无 DOM 依赖）。
 */
import { describe, it, expect } from "vitest";
import { getBinaryName } from "./file";

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
