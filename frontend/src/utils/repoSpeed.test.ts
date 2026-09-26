/**
 * 仓库双源测速的单元测试。
 * pickFastestSource：成功优先于失败、取最快、全失败返回 null、并列时保持稳定；
 * measureRepoSources：双源并行、各源自带 (类别, 源) 预设索引地址、失败/非数组
 * 按失败处理（结果进 error，不抛），成功时把索引条目带回给向导复用。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { presetRepoIndexUrl } from "./constants";
import type { RepoSourceTiming } from "./repoSpeed";

const { fetchIndexMock } = vi.hoisted(() => ({ fetchIndexMock: vi.fn() }));

vi.mock("../api", () => ({
  repoApi: { fetchIndex: fetchIndexMock },
}));

const { measureRepoSources, pickFastestSource } = await import("./repoSpeed");

function timing(source: string, ms: number | null, error = ""): RepoSourceTiming {
  return { source: source as never, ms, error, tasks: [] };
}

beforeEach(() => {
  fetchIndexMock.mockReset();
});

describe("pickFastestSource", () => {
  it("无结果或全失败返回 null（向导据此提示离线并回退默认任务）", () => {
    expect(pickFastestSource([])).toBeNull();
    expect(pickFastestSource([timing("github", null, "超时"), timing("gitee", null, "超时")])).toBeNull();
  });

  it("成功者优先：一个源失败时选唯一成功的那个", () => {
    expect(pickFastestSource([timing("github", null, "超时"), timing("gitee", 500)])).toBe("gitee");
  });

  it("双源都成功取最快", () => {
    expect(pickFastestSource([timing("github", 1200), timing("gitee", 300)])).toBe("gitee");
    expect(pickFastestSource([timing("github", 200), timing("gitee", 900)])).toBe("github");
  });

  it("耗时相同保持先到者（结果顺序稳定，不因并列抖动）", () => {
    expect(pickFastestSource([timing("github", 500), timing("gitee", 500)])).toBe("github");
  });
});

describe("measureRepoSources", () => {
  it("并行测两个预设镜像源，各用 (类别, 源) 自己的索引地址，成功带回条目", async () => {
    const githubTasks = [{ id: "a", name: "A", url: "https://example.com/a.json" }];
    fetchIndexMock.mockImplementation((url: string) =>
      url === presetRepoIndexUrl("browser", "github")
        ? Promise.resolve(githubTasks)
        : Promise.resolve([]),
    );

    const results = await measureRepoSources("browser");

    expect(results.map((r) => r.source)).toEqual(["github", "gitee"]);
    expect(fetchIndexMock).toHaveBeenCalledTimes(2);
    expect(fetchIndexMock).toHaveBeenCalledWith(presetRepoIndexUrl("browser", "github"));
    expect(fetchIndexMock).toHaveBeenCalledWith(presetRepoIndexUrl("browser", "gitee"));
    expect(results[0].tasks).toEqual(githubTasks);
    expect(results[0].error).toBe("");
    expect(results[0].ms).not.toBeNull();
  });

  it("直连类别用 index.http 那一份索引（类别 × 源两维不可混）", async () => {
    fetchIndexMock.mockResolvedValue([]);
    await measureRepoSources("http");
    expect(fetchIndexMock).toHaveBeenCalledWith(presetRepoIndexUrl("http", "github"));
    expect(fetchIndexMock).toHaveBeenCalledWith(presetRepoIndexUrl("http", "gitee"));
  });

  it("单源失败不抛：结果进 error，另一源照常返回", async () => {
    fetchIndexMock.mockImplementation((url: string) =>
      url === presetRepoIndexUrl("browser", "github")
        ? Promise.reject(new Error("连接超时"))
        : Promise.resolve([]),
    );

    const results = await measureRepoSources("browser");

    expect(results[0].ms).toBeNull();
    expect(results[0].error).not.toBe("");
    expect(results[1].ms).not.toBeNull();
    expect(results[1].error).toBe("");
  });

  it("非数组响应按失败处理（不把脏数据当索引交给向导过滤）", async () => {
    fetchIndexMock.mockResolvedValue({ tasks: [] } as never);
    const results = await measureRepoSources("browser");
    expect(results[0].ms).toBeNull();
    expect(results[0].error).toContain("JSON 数组");
  });
});
