/**
 * 任务仓库坐标的回归测试。
 *
 * 背景（真实缺陷）：任务页的「分享适配」按钮曾指向**主程序**仓库
 * `Misyra/Campus-Auth-rs`，而它的用途是"把你的登录任务分享给社区"——任务由独立仓库
 * `Misyra/campus-auth-tasks` 承载。点过去只会看到 Rust 源码，找不到任何可分享的任务。
 *
 * 本测试锁定：① 任务仓库坐标不与主程序仓库重合；② 索引地址确实指向任务仓库；
 * ③ 「分享适配」与「任务仓库 →」用同一个源，不会再次各写一份而漂移。
 *
 * 用「读源码断言」而非渲染组件：这两个入口分别在两个视图里，且不需要 DOM 就能
 * 表达"URL 指向哪个仓库"这一事实；比挂载组件更直接、更快、更不易假通过。
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  TASK_REPO_NAME,
  TASK_REPO_OWNER,
  TASK_REPO_SOURCES,
  TASK_REPO_URL,
  TASK_REPO_URL_GITEE,
  presetRepoIndexUrl,
} from "./constants";

/** 主程序仓库（**不应**被任务类入口引用） */
const APP_REPO = "https://github.com/Misyra/Campus-Auth-rs";

/** 选项表里全部预设索引地址（2 源 × 2 类 = 4 个） */
function allPresetIndexUrls(): string[] {
  return TASK_REPO_SOURCES.flatMap((s) =>
    s.indexUrls ? [s.indexUrls.browser, s.indexUrls.http] : [],
  );
}

describe("任务仓库坐标", () => {
  it("仓库主页指向任务仓库而非主程序仓库", () => {
    expect(TASK_REPO_URL).toBe(`https://github.com/${TASK_REPO_OWNER}/${TASK_REPO_NAME}`);
    expect(TASK_REPO_URL).not.toBe(APP_REPO);
    expect(TASK_REPO_NAME).not.toBe("Campus-Auth-rs");
  });

  it("四个索引地址都指向任务仓库（github 与 gitee 镜像同仓）", () => {
    // 索引按 (源, 类别) 两维给：两个源 × 两类任务
    const urls = allPresetIndexUrls();
    expect(urls).toHaveLength(4);
    for (const url of urls) {
      expect(url).toContain(`/${TASK_REPO_OWNER}/${TASK_REPO_NAME}/`);
      expect(url).not.toContain("/Campus-Auth-rs/");
    }
  });

  it("索引地址是可直接 GET 的 raw 地址（非仓库页面），两类任务的文件不同名", () => {
    expect(presetRepoIndexUrl("browser", "github")).toMatch(
      /^https:\/\/raw\.githubusercontent\.com\/.*\/index\.json$/,
    );
    expect(presetRepoIndexUrl("http", "github")).toMatch(
      /^https:\/\/raw\.githubusercontent\.com\/.*\/index\.http\.json$/,
    );
    expect(presetRepoIndexUrl("browser", "gitee")).toMatch(
      /^https:\/\/raw\.giteeusercontent\.com\/.*\/index\.gitee\.json$/,
    );
    expect(presetRepoIndexUrl("http", "gitee")).toMatch(
      /^https:\/\/raw\.giteeusercontent\.com\/.*\/index\.http\.gitee\.json$/,
    );
    // 两类任务不得共用同一个文件（那正是"混装"要根治的东西）
    expect(presetRepoIndexUrl("browser", "github")).not.toBe(presetRepoIndexUrl("http", "github"));
    expect(presetRepoIndexUrl("browser", "gitee")).not.toBe(presetRepoIndexUrl("http", "gitee"));
  });
});

describe("视图中的任务类入口", () => {
  const panel = readFileSync(
    resolve(__dirname, "../views/tasks/BrowserTasksPanel.vue"),
    "utf-8",
  );
  const envPage = readFileSync(
    resolve(__dirname, "../views/settings/TaskEnvironmentSettings.vue"),
    "utf-8",
  );

  it("「分享适配」不再硬编码任何 GitHub 地址，改用共享常量", () => {
    // 硬编码正是上次出错的原因：两处各写一份就会漂移
    expect(panel).toContain(':href="TASK_REPO_URL"');
    expect(panel).not.toMatch(/href="https:\/\/github\.com/);
  });

  it("「分享适配」不指向主程序仓库", () => {
    expect(panel).not.toContain(APP_REPO);
  });

  it("「任务仓库 →」同样使用共享常量", () => {
    expect(envPage).toContain(":href=\"TASK_REPO_URL\"");
    expect(envPage).not.toMatch(/href="https:\/\/github\.com\/Misyra/);
  });

  it("仓库导入的默认索引也来自共享常量（不再各写一份）", () => {
    const repoImport = readFileSync(
      resolve(__dirname, "../composables/useRepoImport.ts"),
      "utf-8",
    );
    expect(repoImport).toContain("presetRepoIndexUrl");
    expect(repoImport).not.toMatch(/https:\/\/raw\.githubusercontent\.com\/Misyra\/campus-auth-tasks/);
  });
});

describe("仓库来源选项表", () => {
  it("三个源齐全，且预设源的索引地址与共享常量一致", () => {
    expect(TASK_REPO_SOURCES.map((s) => s.id)).toEqual(["github", "gitee", "custom"]);
    const github = TASK_REPO_SOURCES.find((s) => s.id === "github")!;
    const gitee = TASK_REPO_SOURCES.find((s) => s.id === "gitee")!;
    // 表里另写一份字面量就会与常量漂移，故逐项对齐（地址按 (类别, 源) 两维取）
    expect(presetRepoIndexUrl("browser", "github")).toBe(github.indexUrls!.browser);
    expect(presetRepoIndexUrl("http", "github")).toBe(github.indexUrls!.http);
    expect(presetRepoIndexUrl("browser", "gitee")).toBe(gitee.indexUrls!.browser);
    expect(presetRepoIndexUrl("http", "gitee")).toBe(gitee.indexUrls!.http);
    expect(github.homeUrl).toBe(TASK_REPO_URL);
    expect(gitee.homeUrl).toBe(TASK_REPO_URL_GITEE);
    // 自定义源无预设地址（由用户手填）：取地址一律回空串，不抛错
    const custom = TASK_REPO_SOURCES.find((s) => s.id === "custom")!;
    expect(custom.indexUrls).toBeNull();
    expect(custom.homeUrl).toBe("");
    expect(presetRepoIndexUrl("browser", "custom")).toBe("");
    expect(presetRepoIndexUrl("http", "custom")).toBe("");
  });

  it("来源提示能把国内用户导向 Gitee，且 Gitee 侧说明了原因", () => {
    const gitee = TASK_REPO_SOURCES.find((s) => s.id === "gitee")!;
    const github = TASK_REPO_SOURCES.find((s) => s.id === "github")!;
    // Gitee 侧说明"为什么该选它"（按钮本身已写着 Gitee，不必重复名字）
    expect(gitee.hint).toContain("国内");
    // GitHub 侧必须给出出路：只说慢不给替代方案等于没有提示
    expect(github.hint).toContain("Gitee");
    expect(github.hint).toMatch(/慢|失败|卡/);
  });

  it("索引地址（raw JSON）与仓库主页（人类浏览）是两个不同的地址", () => {
    // 真实缺陷：把索引地址当作可读页面链接，点开是一屏 raw JSON
    for (const s of TASK_REPO_SOURCES) {
      if (!s.indexUrls) continue;
      for (const url of [s.indexUrls.browser, s.indexUrls.http]) {
        expect(url).not.toBe(s.homeUrl);
      }
      expect(s.homeUrl).toMatch(/^https:\/\/[^/]*gitee\.com\/|^https:\/\/github\.com\//);
      expect(s.homeUrl).not.toMatch(/raw\./);
    }
  });

  it("两个镜像的仓库主页同仓同名（Gitee 是 GitHub 的镜像）", () => {
    expect(TASK_REPO_URL_GITEE).toBe(`https://gitee.com/${TASK_REPO_OWNER}/${TASK_REPO_NAME}`);
    expect(TASK_REPO_URL_GITEE).toContain(TASK_REPO_NAME);
  });
});

describe("来源切换实现", () => {
  const repoImport = readFileSync(resolve(__dirname, "../composables/useRepoImport.ts"), "utf-8");

  it("索引地址经共享的 presetRepoIndexUrl 取，而非在函数里各写一份 raw 地址", () => {
    // 分支硬编码正是漂移的成因：新增源或新增类别时都会漏改一处
    expect(repoImport).toContain("presetRepoIndexUrl");
    expect(repoImport).not.toMatch(/raw\.githubusercontent\.com/);
    expect(repoImport).not.toMatch(/raw\.giteeusercontent\.com/);
  });

  it("切类别与切源都重取索引地址（否则会出现「在直连列表里拉浏览器索引」）", () => {
    // 地址同时取决于类别与源，任一处不重取就是静默错配
    expect(repoImport).toContain("applyPresetIndexUrl");
    const modal = readFileSync(resolve(__dirname, "../components/RepoImportModals.vue"), "utf-8");
    // 弹窗不再解释混合索引：拆分后文件即类别，那句提示成了噪音
    expect(modal).not.toContain("当前只显示直连任务条目");
  });

  it("「直接查看仓库」用仓库主页，不用索引地址", () => {
    const modal = readFileSync(resolve(__dirname, "../components/RepoImportModals.vue"), "utf-8");
    expect(modal).toContain("repo.sourceHomeUrl.value");
    // 此前是 :href="repo.repoImport.value.url"，即把 raw 索引地址当页面链接
    expect(modal).not.toMatch(/:href="repo\.repoImport\.value\.url"/);
  });

  it("来源选择器与选项表联动，模板里不再硬编码源按钮", () => {
    const modal = readFileSync(resolve(__dirname, "../components/RepoImportModals.vue"), "utf-8");
    expect(modal).toContain("v-for=\"opt in sourceOptions\"");
    expect(modal).not.toMatch(/selectRepoSource\('github'\)/);
  });
});
