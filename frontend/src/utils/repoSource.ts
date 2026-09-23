/**
 * 任务仓库条目的「来源仓库」标注（`RepoTask.source`）。
 *
 * 用途：收录的任务若是从别人的脚本/项目改编而来，索引里可以标出出处，导入弹窗的详情区
 * 渲染成可点击链接。地址来自**远端索引**（他人可写），因此这里只放行 `http(s)`：
 * 直接把索引里的字符串塞进 `href`，等于让仓库维护者能往用户的点击路径上放
 * `javascript:` 之类的东西。
 */

/** 可安全渲染的来源地址（非 http(s) / 空值一律返回空串，调用方据此不渲染链接） */
export function repoSourceUrl(raw?: string | null): string {
  const trimmed = (raw ?? "").trim();
  return /^https?:\/\/[^\s]+$/i.test(trimmed) ? trimmed : "";
}

/**
 * 来源链接的显示文字：去掉协议与 `www.`、去掉尾部斜杠。
 *
 * 详情栏只有一列宽度，`https://github.com/xxx/yyy` 这种前缀占掉半行却零信息量。
 */
export function repoSourceLabel(url: string): string {
  return url.replace(/^https?:\/\/(?:www\.)?/i, "").replace(/\/+$/, "");
}
