/**
 * 「来源仓库」标注的回归测试。
 *
 * 两条要守住的性质：① 只有 http(s) 能进 `href`（索引是远端数据，他人可写）；
 * ② 导入弹窗真的用了这个判据（读源码断言，与本仓 `taskRepo.test.ts` 同一套手法：
 * 详情区的渲染不值得为它挂一次组件）。
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { repoSourceLabel, repoSourceUrl } from "./repoSource";

describe("repoSourceUrl", () => {
  it("放行 http/https（含大小写与路径、查询串）", () => {
    for (const url of [
      "https://github.com/heragehome/haust-auto-login",
      "http://example.com/x/y",
      "HTTPS://GitHub.com/Owner/Repo",
    ]) {
      expect(repoSourceUrl(url)).toBe(url);
    }
  });

  it("拦下非 http(s) 协议与空值（索引是远端数据，不能直接进 href）", () => {
    for (const raw of [
      "javascript:alert(1)",
      "data:text/html;base64,PHNjcmlwdD4=",
      "file:///etc/passwd",
      "  ",
      "github.com/Misyra/campus-auth-tasks",
      "https://",
      null,
      undefined,
    ]) {
      expect(repoSourceUrl(raw)).toBe("");
    }
  });

  it("两侧空白被清掉（索引里手写 JSON 常带空格）", () => {
    expect(repoSourceUrl("  https://github.com/a/b  ")).toBe("https://github.com/a/b");
  });
});

describe("repoSourceLabel", () => {
  it("去掉协议、www. 与尾部斜杠（详情栏只有一列宽）", () => {
    expect(repoSourceLabel("https://github.com/heragehome/haust-auto-login")).toBe(
      "github.com/heragehome/haust-auto-login",
    );
    expect(repoSourceLabel("http://www.example.com/a/")).toBe("example.com/a");
  });
});

describe("导入弹窗的详情区", () => {
  const modal = readFileSync(
    resolve(__dirname, "../components/RepoImportModals.vue"),
    "utf-8",
  );

  it("用 repoSourceUrl 判可用性，不把索引里的原串直接绑到 href", () => {
    expect(modal).toContain("repoSourceUrl(");
    expect(modal).not.toMatch(/:href="repo\.repoImport\.value\.selected\.source/);
  });

  it("外链带 noopener（详情区点开原仓库）", () => {
    expect(modal).toMatch(/repo-detail-source[\s\S]{0,400}rel="noopener"/);
  });
});
