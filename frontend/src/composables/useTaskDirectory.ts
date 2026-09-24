/**
 * 任务目录（模块级单例）：任务 + 脚本混合列表的单一拉取源。
 *
 * GET /api/tasks 一次返回三类条目（浏览器任务 + 脚本 + 直连任务），
 * 此前 useTasks / useScripts 各发一次请求各过滤一半，useUi.init 与每次 WS 重连
 * 都会发出两个一模一样的请求。本模块收敛为：
 * 单次拉取 + 单一 5 秒守卫 + 单一失败计数 + task_type/type 归一化函数，
 * useTasks / useScripts / useHttpTasks 直接消费本模块维护的过滤视图。
 *
 * 注意：视图不能是纯 computed——拖拽排序需要原地 splice 重排本地列表
 * （顺序由 persistOrder 持久化到后端，下次拉取时以服务端顺序重建），
 * 因此这里持有三个物化的响应式数组，在每次拉取后统一填充。
 */

import { ref } from "vue";
import type { TaskItem, Script } from "../api/types";
import { tasksApi } from "../api";
import { frontendLogger } from "../utils/logger";
import { createFetchGuard, createFirstFailNotifier } from "../utils/guards";
import { useToast } from "./useToast";

/**
 * 归一化任务类型。
 * 列表项可能携带 task_type（后端列表序列化字段）或 type（配置内嵌字段），兼容判断只留这一份。
 * 仅本模块内部使用，不再经 useTaskDirectory 返回对象对外暴露。
 */
function normalizeTaskType(item: TaskItem): string {
  return String(item.task_type || item.type || "");
}

const browserTasks = ref<TaskItem[]>([]);
const scripts = ref<Script[]>([]);
const httpTasks = ref<TaskItem[]>([]);

/**
 * 目录是否已成功拉取过一次。
 *
 * 存在的理由：`?task=<id>` 深链要在"列表里没有这个 id"时判定任务不存在并清掉参数，
 * 而冷启动时列表**本来就**是空的——把"还没拉完"当成"不存在"会给出一句错误的
 * 提示并抹掉用户的深链。故判据必须是"已拉取过"而不是"列表非空"。
 */
const loaded = ref(false);
const fetchGuard = createFetchGuard(5000);
const firstFail = createFirstFailNotifier();
const { toastOnly } = useToast();

/**
 * 拉取世代号：只有**最新**那一发的响应才允许写列表。
 *
 * 存在的理由：落盘后刷新（`force`）与 5 秒守卫内的自然刷新可以重叠，两个响应到达顺序
 * 不保证——较旧的那份快照后到会覆盖新列表，于是"刚刚还在列表里的任务"凭空消失，
 * `draftReconcile` 判定它已被删除 → 编辑器自行关闭，那半秒内未落盘的改动直接丢。
 * （`docs/plan-next.md` 早先把这列为"存疑未验证"，2026-09-24 复核确认机制成立。）
 */
let fetchEpoch = 0;

/** 单次拉取混合列表并填充浏览器任务 / 脚本 / 直连任务三个过滤视图（force 语义与其他 fetch 一致） */
async function fetchDirectory(force = false): Promise<void> {
  if (!fetchGuard.shouldFetch(force)) return;
  const mine = ++fetchEpoch;
  try {
    const data = await tasksApi.list();
    // 已有更新的拉取接管：这份是过期快照，丢弃（写进去会让列表倒退）
    if (mine !== fetchEpoch) return;
    if (Array.isArray(data)) {
      const browser = data.filter((t) => {
        const tt = normalizeTaskType(t);
        // 类型缺失视为浏览器任务（与后端 TaskKind 默认行为一致）
        return tt === "" || tt === "browser";
      });
      const script = data.filter((t) => {
        const tt = normalizeTaskType(t);
        // Shell 类型已移除：历史 shell 数据由后端拒绝加载，不会到达此处
        return tt === "script";
      });
      const httpTaskList = data.filter((t) => normalizeTaskType(t) === "http");
      browserTasks.value.splice(0, browserTasks.value.length, ...browser);
      scripts.value.splice(0, scripts.value.length, ...script);
      httpTasks.value.splice(0, httpTasks.value.length, ...httpTaskList);
      loaded.value = true;
    }
    fetchGuard.markSuccess();
    firstFail.trackRecovery();
  } catch (error) {
    // 过期那一发失败不必出声：接管它的那次拉取会给出自己的结论（否则会连弹两次）
    if (mine !== fetchEpoch) return;
    frontendLogger.error("tasks", "获取任务/脚本列表失败", error);
    // F3：首次失败 toast 通知，后续失败保持静默（log-only）
    if (firstFail.trackFailure()) {
      toastOnly(false, "加载任务/脚本列表失败");
    }
  }
}

/**
 * 三类任务 id 的并集。
 *
 * 取号必须对**整个目录**唯一，不能只看自己那一类：后端 `save_task` 写盘时会删掉
 * **另外两个桶**里同 id 的文件（那是给"用户主动改任务类型"清残留用的）。各面板若
 * 只看自己的列表取号，浏览器任务与直连任务都会从 `untitled_1` 起算，于是"新建第二类
 * 的第一个任务并改一下"就会静默删掉另一类那份文件。副本的 `_copy` 后缀同理。
 */
export function allTaskIds(): Set<string> {
  return new Set([
    ...browserTasks.value.map((t) => t.id),
    ...scripts.value.map((s) => s.id),
    ...httpTasks.value.map((t) => t.id),
  ]);
}

export function useTaskDirectory() {
  return { browserTasks, scripts, httpTasks, loaded, fetchDirectory, allTaskIds };
}
