/**
 * 任务目录（模块级单例）：任务 + 脚本混合列表的单一拉取源。
 *
 * GET /api/tasks 与 /api/scripts 返回完全相同的混合列表（浏览器任务 + 脚本），
 * 此前 useTasks / useScripts 各发一次请求各过滤一半，useUi.init 与每次 WS 重连
 * 都会发出两个一模一样的请求。本模块收敛为：
 * 单次拉取 + 单一 5 秒守卫 + 单一失败计数 + task_type/type 归一化函数，
 * useTasks / useScripts 直接消费本模块维护的两个过滤视图。
 *
 * 注意：视图不能是纯 computed——拖拽排序需要原地 splice 重排本地列表
 * （顺序由 persistOrder 持久化到后端，下次拉取时以服务端顺序重建），
 * 因此这里持有两个物化的响应式数组，在每次拉取后统一填充。
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
 */
export function normalizeTaskType(item: TaskItem): string {
  return String(item.task_type || item.type || "");
}

const browserTasks = ref<TaskItem[]>([]);
const scripts = ref<Script[]>([]);
const fetchGuard = createFetchGuard(5000);
const firstFail = createFirstFailNotifier();
const { toastOnly } = useToast();

/** 单次拉取混合列表并填充浏览器任务 / 脚本两个过滤视图（force 语义与其他 fetch 一致） */
async function fetchDirectory(force = false): Promise<void> {
  if (!fetchGuard.shouldFetch(force)) return;
  try {
    const data = await tasksApi.list();
    if (Array.isArray(data)) {
      const browser = data.filter((t) => {
        const tt = normalizeTaskType(t);
        // 类型缺失视为浏览器任务（与后端 TaskKind 默认行为一致）
        return tt === "" || tt === "browser";
      });
      const script = data.filter((t) => {
        const tt = normalizeTaskType(t);
        return tt === "script" || tt === "shell";
      });
      browserTasks.value.splice(0, browserTasks.value.length, ...browser);
      scripts.value.splice(0, scripts.value.length, ...script);
    }
    fetchGuard.markSuccess();
    firstFail.trackRecovery();
  } catch (error) {
    frontendLogger.error("tasks", "获取任务/脚本列表失败", error);
    // F3：首次失败 toast 通知，后续失败保持静默（log-only）
    if (firstFail.trackFailure()) {
      toastOnly(false, "加载任务/脚本列表失败");
    }
  }
}

export function useTaskDirectory() {
  return { browserTasks, scripts, fetchDirectory, normalizeTaskType };
}
