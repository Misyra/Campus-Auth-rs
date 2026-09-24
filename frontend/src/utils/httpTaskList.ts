/**
 * 直连任务列表的纯派生数据：行摘要、搜索过滤、方案绑定索引。
 *
 * 抽成纯函数的理由：这三段都是「输入 → 输出」的映射，无状态、不碰 DOM，面板只负责
 * 渲染。前端没有组件测试环境（vite.config.ts 的 `environment: "node"`、无
 * @vue/test-utils），放在 utils 里这些判断才能被 vitest 直接覆盖，也让「某一行该显示
 * 什么、搜什么」有了唯一出处——A 版式把列表行从「名称 + 4 个图标按钮」换成「名称 +
 * 方法 chip + 地址摘要 + 绑定 pills」，这几项各自都可能悄悄算错。
 */

import type { ProfileSummary, TaskItem } from "@/api/types";

/** 列表行渲染所需的全部派生字段 */
export interface HttpTaskRow {
  id: string;
  /** 人读名称；空串时由渲染方回退显示 id */
  name: string;
  /** 描述：渲染成名称下方的副行（与浏览器任务 / 脚本列表同口径），空串则不渲染 */
  description: string;
  /** 请求方法（`TaskSummary.http_method`）；非直连/解析失败时为空串——不编造 GET */
  method: string;
  /** 请求地址模板（同时是搜索的匹配字段之一） */
  url: string;
  /**
   * 任务文件最近修改时间（UTC RFC3339，`TaskSummary.modified_at`）；读不到时为空串。
   *
   * 由本函数带出而非渲染方按 id 反查：列表行同时要名称、方法、地址、绑定与时间，
   * 分头取会让渲染层对每一行做一次 `find`（O(n²)），也把"行里有什么"这件事
   * 拆到两处。
   */
  modifiedAt: string;
  /** 绑定该任务的方案名（空数组 = 没有任何方案引用它） */
  boundProfiles: string[];
}

/** 从宽类型字段里取字符串（列表项带索引签名，`url` 之类字段的类型不是权威） */
function pickString(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

/**
 * 任务 ID → 绑定它的方案名索引。
 *
 * 数据源是 `ProfileSummary.active_http_task`（后端列表接口同步返回，无需额外请求）：
 * 直连任务与方案的引用关系只存这一个方向，故列表页要回答「这条任务被谁在用」只能
 * 反向聚合。按方案名排序是为了皮儿顺序稳定——对象键序取决于后端返回顺序，同一份
 * 配置刷新两次不该让 pill 换位置。
 */
export function buildHttpTaskBindingIndex(
  profiles: Record<string, ProfileSummary>,
): Map<string, string[]> {
  const index = new Map<string, string[]>();
  for (const profile of Object.values(profiles)) {
    const taskId = pickString(profile?.active_http_task);
    // 未绑定直连任务的方案不进索引（空/纯空白都算未绑定，与后端判定一致）
    if (!taskId) continue;
    const displayName = pickString(profile?.name) || pickString(profile?.id);
    index.set(taskId, [...(index.get(taskId) ?? []), displayName]);
  }
  for (const names of index.values()) names.sort();
  return index;
}

/**
 * 绑定索引是否可用。
 *
 * 方案列表为空时**不可用**（而非「所有任务都未绑定」）：`profiles` 在 `useUi.init`
 * 拉取完成前是空表，此时若照常渲染「未绑定」，用户会看到一条已被方案引用的任务被
 * 标成没人用——比不显示更坏。故空表时由渲染方整行不渲染绑定区。
 */
export function isBindingIndexReady(profiles: Record<string, ProfileSummary>): boolean {
  return Object.keys(profiles).length > 0;
}

/**
 * 任务列表 + 绑定索引 → 可渲染的行。
 *
 * 方法/地址直接取列表摘要自带字段（后端 `TaskSummary` 已带 `url` 与 `http_method`）：
 * 以前摘要里没有它们，面板只能对每条任务再补一次详情请求（N+1，且每次列表刷新都要
 * 重来）；现在列表接口一次就把行要显示的信息给全了。
 */
export function buildHttpTaskRows(
  tasks: TaskItem[],
  binding: Map<string, string[]>,
): HttpTaskRow[] {
  return tasks.map((task) => {
    const id = pickString(task.id);
    return {
      id,
      name: pickString(task.name),
      description: pickString(task.description),
      method: pickString(task.http_method),
      url: pickString(task.url),
      modifiedAt: pickString(task.modified_at),
      boundProfiles: [...(binding.get(id) ?? [])],
    };
  });
}

/**
 * 按关键字过滤行：匹配名称 / 任务 ID / 描述 / 请求地址，大小写不敏感。
 *
 * 描述与地址都进匹配面，是因为它们**看得见**（描述是名称格下的副行、地址在「请求」列）：
 * 界面上能读到的文字搜不到，用户会以为搜索坏了。
 *
 * 不匹配绑定方案名——这一列是「这条任务被谁在用」的答案，搜索的意图始终是
 * 「我要找那条任务」。四个字段用换行拼接避免跨字段误命中（"宿舍dorm" 不该因为
 * 名称以「宿舍」结尾、id 以 dorm 开头就算匹配）。
 */
export function filterHttpTaskRows(rows: HttpTaskRow[], query: string): HttpTaskRow[] {
  const keyword = query.trim().toLowerCase();
  if (!keyword) return rows;
  return rows.filter((row) =>
    `${row.name}\n${row.id}\n${row.description}\n${row.url}`.toLowerCase().includes(keyword),
  );
}
