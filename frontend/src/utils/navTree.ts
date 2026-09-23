/**
 * 侧栏二级导航的数据源与激活判定（纯函数 + 常量，便于单测）。
 *
 * 「任务」一级项在左侧主侧栏里展开一层子项，取代原先任务页内的竖排二级导航卡：
 * 那张卡在 2280px 视口下离左边缘 400px 有余、四周全是留白，像被丢在中间的孤岛，
 * 而正文又与顶栏页标题不在同一列上。收进侧栏后，导航与它归属的一级项长在一起，
 * 任务页只剩正文（`.tasks-page` 只负责限宽，见 styles/pages/tasks.css）。
 *
 * 子项定义放这里而不是 TasksView：消费方有两个——AppSidebar（展开渲染）与
 * TasksView（侧栏在 ≤768px 只剩图标、子项文字放不下时，页面内退化为横向 pill 行）。
 * 一份数据两个渲染出口，避免两处硬编码同一份路由表后各自漂移。
 */

import type { IconName } from "@/components/common/IconApp.vue";

export interface NavChild {
  /** 稳定标识：激活判定按它比较（不是路由名，路由名可能被深度链接改名） */
  id: string;
  /** 导航短标签：侧栏宽度有限，长标签会把文字压成省略号（全称见 title） */
  label: string;
  /** 路由名：与既有深链一致，禁止改名（`/tasks/http` 等由 router 定义） */
  name: string;
  /** 悬停 title：全称 + 一句说明，鼠标悬停即可看全，不为宽度牺牲信息 */
  title: string;
  /** IconApp 图标名：类型来自 IconApp，图标被改名时此处编译期即报错 */
  icon: IconName;
}

/** 「任务」分组的子项（顺序即侧栏顺序，也是窄屏 pill 行的顺序） */
export const TASK_NAV_CHILDREN: readonly NavChild[] = [
  {
    id: "browser",
    label: "浏览器任务",
    name: "tasks-browser",
    title: "浏览器任务：浏览器自动化步骤序列",
    icon: "chrome",
  },
  {
    id: "http",
    label: "直连任务",
    name: "tasks-http",
    title: "直连任务：直接向校园网网关发登录请求，免 Python 与浏览器",
    icon: "globe",
  },
  {
    id: "scripts",
    label: "脚本",
    name: "tasks-scripts",
    title: "脚本：定时执行的辅助脚本",
    icon: "terminal",
  },
  {
    id: "scheduled",
    label: "定时任务",
    name: "tasks-scheduled",
    title: "定时任务：按时间或启动时机自动执行",
    icon: "calendar",
  },
  {
    // 标签写「AI 生成」而非全称：侧栏宽度有限，全称放进 title 与顶栏页标题
    id: "ai",
    label: "AI 生成",
    name: "tasks-ai",
    title: "AI 生成浏览器任务：用自然语言描述生成浏览器任务",
    icon: "sparkles",
  },
];

/**
 * 当前路由名命中的子项 id。
 *
 * 按 `route.name` 精确匹配而不是解析路径末段：`/tasks` 落地时经 redirect 后
 * `route.name` 已是 `tasks-browser`（不是父级的 `tasks`），而 `/tasks/ai` 这类
 * 末段一旦将来再嵌套就解析不准。不属于该分组时返回 `null`，调用方据此表达
 * 「本组无激活项」。
 */
export function activeChildId(
  children: readonly NavChild[],
  routeName: string | null | undefined,
): string | null {
  if (!routeName) return null;
  return children.find((child) => child.name === routeName)?.id ?? null;
}
