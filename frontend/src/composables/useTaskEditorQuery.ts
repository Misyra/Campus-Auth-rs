/**
 * 任务页二级编辑态与地址栏 `?task=<id>` 的同步（三个面板共用）。
 *
 * 两态由 query 表达——刷新与深链直达编辑态、返回即列表、方案页带参跳进来即落编辑器。
 * 集中在此是因为三个面板此前各写一份，而**两个失配方向都没被处理**：
 *
 * 1. **query 指向的任务不存在**（被删 / 拼错 / 陈旧的分享链接）时，旧实现把「编辑态」
 *    判成「query 非空」，于是出现 `query 有值 + 草稿为空` 的组合：列表分支与编辑分支
 *    都不渲染 → 整页空白，没有提示、也没有返回入口，只能手改地址栏。
 * 2. **草稿被清掉而 query 还在**（删除当前编辑的任务、或关闭编辑器只清了参数没清草稿）
 *    时，刷新会再次踩到 1；反过来若只清草稿不清 query，"返回"看起来毫无反应。
 *
 * 现在的判据只有一条：**编辑态 = 草稿非空**（面板侧 `!!draft`）。query 是"意图"，
 * 解析不了就退回列表并清掉参数，绝不留一个指向不存在对象的地址。
 *
 * 未就绪（目录还没拉回来）时不判定"不存在"——冷启动深链时列表本来就是空的，
 * 把"还没拉完"说成"已被删除"会给错提示还抹掉用户的深链（见 `useTaskDirectory.loaded`）。
 */

import { computed, watch } from "vue";
import type { ComputedRef } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useTaskDirectory } from "./useTaskDirectory";
import { useToast } from "./useToast";

export interface TaskEditorQueryOptions {
  /** 本面板判据：该 id 是否存在于当前列表 */
  exists: (id: string) => boolean;
  /** 打开编辑器（面板注入自己的 `showXxxEditor`） */
  open: (id: string) => void | Promise<void>;
  /** 草稿当前 id（空串 = 未编辑）；草稿清空时据此把 query 一并撤下 */
  currentId: () => string;
  /**
   * 列表是否已就绪（未就绪时不判定"不存在"）。
   *
   * 默认取任务目录的 `loaded`（浏览器任务 / 直连任务 / 脚本三面板共用同一份混合列表）；
   * 定时任务的面板另有自己的列表（`GET /api/scheduler/jobs`），故注入自己的就绪标记。
   */
  ready?: () => boolean;
}

export interface TaskEditorQueryApi {
  /** query 里的任务 id（空串 = 无） */
  queryId: ComputedRef<string>;
  /** 立即按当前 query 解析一次（面板在目录拉取完成后调用） */
  resolve: () => void;
  /** 需要时打开某个任务的编辑器（点行 / 新建后进入）；已在该任务或正在打开则不动 */
  openIfNeeded: (id: string) => void;
  /** 进入编辑态：写 `?task=<id>`（history push，浏览器返回即列表） */
  syncQuery: (id: string) => void;
  /** 撤回 query 参数（无参数时不产生多余的历史记录） */
  clearQuery: () => void;
}

export function useTaskEditorQuery(options: TaskEditorQueryOptions): TaskEditorQueryApi {
  const route = useRoute();
  const router = useRouter();
  const { loaded } = useTaskDirectory();
  const { toastOnly } = useToast();
  /** 就绪门：未就绪时不判定存在性（默认任务目录，面板可注入自己的） */
  const ready = options.ready ?? (() => loaded.value);

  /**
   * 正在打开的任务 id。
   *
   * 点一行编辑会同时做两件事：写 query（`syncQuery`）与发起打开。query 变化本身
   * 也会触发解析，没有这个标记就会把同一次打开做两遍——两次详情请求、两次草稿赋值，
   * 谁后到谁生效（列表里连点两行时尤其明显）。
   */
  let opening = "";

  const queryId = computed(() =>
    typeof route.query.task === "string" ? route.query.task.trim() : "",
  );

  function clearQuery(): void {
    if (!route.query.task) return;
    const query = { ...route.query };
    delete query.task;
    void router.push({ query });
  }

  function syncQuery(id: string): void {
    void router.push({ query: { ...route.query, task: id } });
  }

  function openIfNeeded(id: string): void {
    if (!id) return;
    if (options.currentId() === id) return; // 已经在这个任务的编辑页里
    if (opening === id) return; // 同一次打开，别做第二遍
    if (!ready()) return; // 列表未就绪：不判定存在性（面板拉取后会再调 resolve）
    if (!options.exists(id)) {
      toastOnly(false, `找不到任务「${id}」，它可能已被删除`);
      clearQuery();
      return;
    }
    opening = id;
    void Promise.resolve(options.open(id)).finally(() => {
      if (opening === id) opening = "";
    });
  }

  function resolve(): void {
    const id = queryId.value;
    if (!id) return;
    openIfNeeded(id);
  }

  // query 变化（点行进入、浏览器前进后退、外部带参跳入）都要跟上
  watch(queryId, () => resolve());

  // 草稿被关掉 / 被删掉：地址栏不该继续指着一个没有打开的编辑器
  watch(
    () => options.currentId(),
    (id) => {
      if (!id) clearQuery();
    },
  );

  return { queryId, resolve, openIfNeeded, syncQuery, clearQuery };
}
