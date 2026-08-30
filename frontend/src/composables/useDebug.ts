/**
 * 调试会话状态管理（单例）。
 * 替代原 debugTaskMethods，与 DebugPanel.vue 配合使用。
 */

import { reactive, ref } from "vue";
import type { DebugSession, DebugStepResult } from "../api/types";
import { debugApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useToast } from "./useToast";

/** 空白调试会话状态 */
const emptySession = (): DebugSession => ({
  running: false,
  task_id: null,
  current_step: 0,
  total_steps: 0,
  steps: [],
  results: [],
  screenshot_url: null,
});

const session = reactive<DebugSession>(emptySession());
const loading = ref(false);
const visible = ref(false);
const _resultMap = ref<Map<number, DebugStepResult>>(new Map());

const { toastOnly } = useToast();

/** 根据 API 返回的会话数据同步本地状态 */
function syncSession(data: DebugSession): void {
  // start/step 响应的 screenshot_url 恒为 null（截图只经 WS 事件推送），
  // 直接 Object.assign 会用 null 抹掉 WS 先行写入的截图 URL，面板永远"暂无截图"
  const prevScreenshot = session.screenshot_url;
  Object.assign(session, data);
  session.screenshot_url = data.screenshot_url || prevScreenshot || null;
  // 仅当负载带 `results` 时才重建结果表：debug_run_all 返回的是 StructuredResult
  //（无 results），若在此强制清空会让 WebSocket step_progress 已写入的步骤结果
  // 全部丢失，调试面板显示为“空”。保留已有结果，避免误清空。
  if (Array.isArray(data.results)) {
    _resultMap.value = new Map(data.results.map((r) => [r.step_index, r]));
  }
}

/** 启动调试会话 */
async function startDebug(taskId: string): Promise<void> {
  loading.value = true;
  try {
    // 新会话不带旧截图：screenshot_url 若保留上一场的残留 URL（文件已被
    // 停止流程清理），面板会显示裂图；置空等 WS 推送本场首张截图
    session.screenshot_url = null;
    const data = await debugApi.start(taskId);
    syncSession(data);
    visible.value = true;
    frontendLogger.info("debug", `开始调试任务 ${taskId}`);
  } catch (error) {
    const msg = extractApiError(error, "启动调试失败");
    frontendLogger.error("debug", "启动调试失败: " + msg);
    toastOnly(false, msg);
  } finally {
    loading.value = false;
  }
}

/** 执行下一步 */
async function nextStep(): Promise<void> {
  loading.value = true;
  try {
    const data = await debugApi.next();
    syncSession(data);
  } catch (error) {
    const msg = extractApiError(error, "执行步骤失败");
    frontendLogger.error("debug", "执行步骤失败: " + msg);
    toastOnly(false, msg);
  } finally {
    loading.value = false;
  }
}

/** 执行全部步骤 */
async function runAll(): Promise<void> {
  loading.value = true;
  try {
    const data = await debugApi.runAll();
    syncSession(data);
  } catch (error) {
    const msg = extractApiError(error, "执行全部失败");
    frontendLogger.error("debug", "执行全部失败: " + msg);
    toastOnly(false, msg);
  } finally {
    loading.value = false;
  }
}

/** 停止调试 */
async function stopDebug(): Promise<void> {
  try {
    const data = await debugApi.stop();
    frontendLogger.info("debug", "调试已停止");
    toastOnly(true, data?.message || "调试已停止");
  } catch (error) {
    frontendLogger.error("debug", "停止调试失败", error);
    toastOnly(false, "停止调试失败");
  } finally {
    // 无论 API 成功失败都重置本地状态；顺带取消未触发的详情补全定时器
    clearDetailRefill();
    syncSession(emptySession());
    visible.value = false;
  }
}

/** 恢复服务端仍活跃、但前端已"失忆"的调试会话（页面刷新/换页后本地状态丢失）。
 *
 * 调试会话是 Worker 侧持久状态：不恢复的话界面上没有任何停止入口，
 * 登录等命令会一直撞上"Worker 忙: 调试会话进行中"。恢复为"进行中"骨架
 * 面板（详情由 refillSessionDetails 退避补全），可立即停止。
 */
async function restoreIfActive(): Promise<void> {
  if (session.running || visible.value) return;
  try {
    const st = await debugApi.status();
    if (!st?.active) return;
    if (st.screenshot_url) session.screenshot_url = st.screenshot_url;
    // Worker 返回完整会话（步骤/结果）时整体恢复；拿不到时（典型场景："执行全部"
    // 正在执行，Worker 命令队列被占住，详情查询 5s 超时）退化为骨架并排队补全
    if (st.session && Array.isArray(st.session.steps)) {
      syncSession(st.session);
    } else {
      session.running = true;
      refillSessionDetails();
    }
    visible.value = true;
    frontendLogger.info("debug", "检测到服务端仍活跃的调试会话，已恢复面板");
  } catch {
    // 查询失败不阻塞启动（后端未升级/网络问题），静默忽略
  }
}

// 骨架会话详情补全：恢复时"执行全部"在 Worker 内部循环执行、占住命令队列，
// 详情查询只能排队直到整个执行结束，立即重试必然超时；按退避等它跑完，
// 任意一次查询成功即整体补全步骤列表（总预算约 95s，覆盖常见任务时长）。
const DETAIL_REFILL_DELAYS = [5_000, 10_000, 20_000, 30_000, 30_000];
let refillTimer: ReturnType<typeof setTimeout> | undefined;

function clearDetailRefill(): void {
  if (refillTimer) {
    clearTimeout(refillTimer);
    refillTimer = undefined;
  }
}

function refillSessionDetails(attempt = 0): void {
  if (attempt >= DETAIL_REFILL_DELAYS.length) return;
  clearDetailRefill();
  refillTimer = setTimeout(() => {
    void (async () => {
      // 面板已关/会话已停止/步骤已就位则无需补全
      if (!visible.value || !session.running || session.steps.length > 0) return;
      try {
        const st = await debugApi.status();
        if (!st?.active) {
          // 会话在骨架期间被外部结束：清理面板残留
          syncSession(emptySession());
          visible.value = false;
          return;
        }
        if (st.session && Array.isArray(st.session.steps) && st.session.steps.length > 0) {
          syncSession(st.session);
          return;
        }
      } catch {
        // 查询失败（Worker 忙/未启动）走下一轮退避
      }
      refillSessionDetails(attempt + 1);
    })();
  }, DETAIL_REFILL_DELAYS[attempt]);
}

/** 获取指定步骤的执行结果 */
function getStepResult(index: number): DebugStepResult | null {
  return _resultMap.value.get(index) ?? null;
}

/** 获取指定步骤的执行状态 */
function getStepStatus(index: number): "success" | "failed" | "running" | "current" | "pending" {
  const result = getStepResult(index);
  if (result) {
    // F5：running 标记优先——进行中步骤不再因 success=false 误显失败
    if (result.running) return "running";
    return result.success ? "success" : "failed";
  }
  if (index === session.current_step) return "current";
  return "pending";
}

/** 处理来自 WebSocket 的调试截图事件 */
function handleScreenshot(data: { url?: string; step_index?: number; description?: string }): void {
  if (data?.url) {
    session.screenshot_url = data.url;
    frontendLogger.info("debug", `收到调试截图: ${data.url}`);
  }
}

/** 截图加载失败（残留 URL 指向已清理文件等）时清空，回退占位文案 */
function clearScreenshot(): void {
  session.screenshot_url = null;
}

/** 处理来自 WebSocket 的调试步骤进度事件 */
function handleStepProgress(data: {
  step_index: number;
  total_steps?: number;
  description?: string;
  step_type?: string;
}): void {
  if (typeof data?.step_index !== "number") return;
  session.current_step = data.step_index;
  if (typeof data.total_steps === "number") session.total_steps = data.total_steps;
  const result: DebugStepResult = {
    step_index: data.step_index,
    // F5：success 仅作占位，真实成败由 syncSession 覆盖；running 标记才是进行中判定
    success: false,
    running: true,
    message: data.description || "步骤进行中",
  };
  // F6：两个分支统一先重建引用再写入，保证 _resultMap 引用变化一致，
  // 消除对 current_step 副作用的隐式依赖
  _resultMap.value = new Map(_resultMap.value);
  const existing = _resultMap.value.get(data.step_index);
  if (existing) {
    existing.success = result.success;
    existing.running = true;
    existing.message = result.message;
  } else {
    _resultMap.value.set(data.step_index, result);
  }
  frontendLogger.info(
    "debug",
    `调试步骤进度: ${data.step_index}/${data.total_steps ?? "?"}${data.description ? " - " + data.description : ""}`,
  );
}

export function useDebug() {
  return {
    session,
    loading,
    visible,
    startDebug,
    nextStep,
    runAll,
    stopDebug,
    getStepResult,
    getStepStatus,
    handleScreenshot,
    handleStepProgress,
    clearScreenshot,
    restoreIfActive,
  };
}
