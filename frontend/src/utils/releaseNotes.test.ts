/**
 * 更新日志 Markdown 渲染器的单元测试。
 *
 * 重点是**安全**：渲染结果会经 `v-html` 插进弹窗，正文来自远端仓库，
 * 因此原生标签必须被转义、危险协议链接必须降级为纯文本。
 */
import { describe, expect, it } from "vitest";

import { escapeHtml, renderInline, renderReleaseNotes, safeHref, stripTemplateTitle } from "./releaseNotes";

describe("escapeHtml / safeHref", () => {
  it("转义全部 HTML 敏感字符", () => {
    expect(escapeHtml(`<a href="x">'&'</a>`)).toBe(
      "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;",
    );
  });

  it("链接白名单只放行 http / https / mailto", () => {
    expect(safeHref("https://github.com/Misyra/Campus-Auth-rs")).toBe(
      "https://github.com/Misyra/Campus-Auth-rs",
    );
    expect(safeHref("http://example.com/x")).toBe("http://example.com/x");
    expect(safeHref("mailto:a@b.c")).toBe("mailto:a@b.c");
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("data:text/html;base64,xx")).toBeNull();
    expect(safeHref("/relative/path")).toBeNull();
  });
});

describe("renderInline", () => {
  it("加粗 / 行内代码 / 斜体", () => {
    expect(renderInline("**加粗** 与 `code` 与 *斜*")).toBe(
      "<strong>加粗</strong> 与 <code>code</code> 与 <em>斜</em>",
    );
  });

  it("Markdown 链接与裸链接都渲染成新窗口链接", () => {
    expect(renderInline("[发布页](https://github.com/x/y)")).toBe(
      '<a href="https://github.com/x/y" target="_blank" rel="noopener noreferrer">发布页</a>',
    );
    expect(renderInline("见 https://github.com/x/y 说明")).toBe(
      '见 <a href="https://github.com/x/y" target="_blank" rel="noopener noreferrer">https://github.com/x/y</a> 说明',
    );
  });

  it("危险协议的链接降级为纯文本（不产生 a 标签、不残留协议名）", () => {
    const html = renderInline("[点我](javascript:alert(1))");
    expect(html).not.toContain("<a");
    expect(html).not.toContain("javascript");
    expect(html).toContain("点我");
  });

  it("正文里的原生标签按纯文本输出（XSS 防线）", () => {
    const html = renderInline('<img src=x onerror="alert(1)">');
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img");
  });
});

describe("renderReleaseNotes 块级结构", () => {
  it("标题整体下沉三级，子弹列表按无序列表渲染", () => {
    const html = renderReleaseNotes(
      "## v5.0.1（2026-09-19）\n\n### 修复\n\n- **能退出了**：登录成功后会退出\n- 另一项",
    );
    expect(html).toBe(
      "<h5>v5.0.1（2026-09-19）</h5>" +
        "<h6>修复</h6>" +
        "<ul><li><strong>能退出了</strong>：登录成功后会退出</li><li>另一项</li></ul>",
    );
  });

  it("有序列表 / 引用 / 分隔线 / 围栏代码块", () => {
    expect(renderReleaseNotes("1. 第一步\n2. 第二步")).toBe(
      "<ol><li>第一步</li><li>第二步</li></ol>",
    );
    expect(renderReleaseNotes("> 注意这里有坑")).toBe("<blockquote>注意这里有坑</blockquote>");
    expect(renderReleaseNotes("---")).toBe("<hr>");
    expect(renderReleaseNotes("```\ncampus-auth --version\n```")).toBe(
      "<pre><code>campus-auth --version</code></pre>",
    );
  });

  it("列表与段落之间正确闭合，段内换行保留为 <br>", () => {
    expect(renderReleaseNotes("- 甲\n- 乙\n\n正文一行\n续行")).toBe(
      "<ul><li>甲</li><li>乙</li></ul><p>正文一行<br>续行</p>",
    );
  });

  it("空输入返回空串（发布未填说明时不产生空标签）", () => {
    expect(renderReleaseNotes("")).toBe("");
    expect(renderReleaseNotes(null)).toBe("");
    expect(renderReleaseNotes(undefined)).toBe("");
  });
});

/**
 * 真实发布说明样例：`GET /api/repos/.../releases/latest` 返回的 v5.0.2 正文原文
 * （由 `release.yml` 从 `docs/updatelog.md` 提取章节 + 追加平台运行说明生成）。
 *
 * 用真实正文而非手写样例，是为了锁住"发布流程改了模板后弹窗渲染不会退化"。
 */
const REAL_RELEASE_BODY = [
  "## 更新日志",
  "",
  "## v5.0.2（2026-09-20）",
  "",
  "### 修复",
  "",
  "- **应用内更新下载完成后无法生效**：此前点「立即更新」下载完成后，若在弹窗中选择「立即重启」，程序会用旧版本生成一个替代进程再退出——替代进程恰好锁住主程序文件，导致更新助手替换失败（日志出现「替换失败: 另一个程序正在使用此文件」），重启后仍是旧版本，且已下载的更新包被清理、需重新下载。现在带待应用更新重启时不再生成替代进程，由更新助手完成替换后直接用新版本启动。手动选择安装包更新与定时自动重启场景一并修复；即便助手替换意外失败，也会保留已下载的更新包，下次启动自动重试应用，无需重新下载。",
  "",
  "## 平台运行说明",
  "",
  "- **Windows**：解压后直接运行 campus-auth.exe（GUI 程序，无控制台窗口，自动打开浏览器 Web 控制台）",
  "- **macOS**：浏览器下载的二进制带 quarantine 属性，Gatekeeper 会拦截，首次运行前先执行 `xattr -cr campus-auth`（或对解压后的整个目录 `xattr -cr .`）；解压用 `tar -xzf`",
  "- **Linux**：二进制动态链接 GTK3 / libayatana-appindicator / librsvg（托盘），最小系统需先安装运行时库，Debian/Ubuntu：`sudo apt install libgtk-3-0 libayatana-appindicator3-1 librsvg2-2`；解压用 `tar -xzf`，执行 `./campus-auth`",
  "- **Linux ARM64**：同上依赖；构建于 ubuntu-24.04（glibc 较新），老发行版（Debian 11/Ubuntu 20.04 及更早）可能因 glibc 过旧无法运行，建议 Debian 12+/Ubuntu 22.04+；树莓派 OS（64 位）可直接运行",
  "- **Docker**：预构建多架构镜像为 `ghcr.io/misyra/campus-auth-rs:v5.0.2`，`prerelease` 指向最新测试版，正式版更新 `latest`",
  "- 各平台 .sha256 为对应压缩包的 SHA256 校验文件",
  "",
].join("\n");

describe("stripTemplateTitle", () => {
  it("去掉发布流程生成的「## 更新日志」模板标题", () => {
    expect(stripTemplateTitle(REAL_RELEASE_BODY)).toMatch(/^## v5\.0\.2/);
  });

  it("没有模板标题时原样返回，不误伤用户正文", () => {
    expect(stripTemplateTitle("## v1.0.0（2026-01-01）")).toBe("## v1.0.0（2026-01-01）");
    expect(stripTemplateTitle(null)).toBe("");
  });
});

describe("真实发布说明渲染", () => {
  const html = renderReleaseNotes(stripTemplateTitle(REAL_RELEASE_BODY));

  it("版本与大节下沉为 h5、小节下沉为 h6", () => {
    expect(html.startsWith("<h5>v5.0.2（2026-09-20）</h5>")).toBe(true);
    expect(html).toContain("<h6>修复</h6>");
    expect(html).toContain("<h5>平台运行说明</h5>");
  });

  it("两段列表各成一块，平台说明 6 条一项不少", () => {
    expect(html.match(/<ul>/g)?.length).toBe(2);
    expect(html.match(/<li>/g)?.length).toBe(7);
    expect(html).toContain("<strong>应用内更新下载完成后无法生效</strong>");
    expect(html).toContain("<strong>Linux ARM64</strong>");
  });

  it("行内代码正确闭合，长命令不丢内容", () => {
    expect(html).toContain("<code>xattr -cr campus-auth</code>");
    expect(html).toContain("<code>ghcr.io/misyra/campus-auth-rs:v5.0.2</code>");
    expect(html).toContain("<code>sudo apt install libgtk-3-0 libayatana-appindicator3-1 librsvg2-2</code>");
  });

  it("正文没有被包成段落，且不产生任何原生标签泄漏", () => {
    expect(html).not.toContain("<p>");
    expect(html).not.toContain("&lt;");
  });
});
