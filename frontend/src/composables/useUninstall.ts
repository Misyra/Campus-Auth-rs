/**
 * 卸载流程（单例）。
 *
 * 一条用户动作串起两个端点，**职责不重叠**：
 * 1. `POST /api/uninstall`       —— 清理 `base_path` 之外的系统残留（自启动 / 加密密钥
 *    目录 / Playwright 浏览器缓存）。进程内就能做，做完程序照常可用。
 * 2. `POST /api/uninstall/purge` —— 删程序本身并退出，交给 `campus-auth-helper --uninstall`
 *    （Windows 上运行中的 exe 删不掉自己），助手等主进程退出后执行，完成后弹系统提示框。
 *
 * 为什么抽成 composable 而不是留在 `AboutView`：这套流程有真实时序（确认 → 清理 →
 * 卸程序 → 界面失去后端），且每一步的失败语义**不同**——残留清理失败不该阻断卸载，
 * 卸程序失败必须说清"程序文件一个都没删"。这类"每步各自怎么失败"的逻辑放在模板里
 * 改一次错一次。
 */

import { computed, ref } from "vue";
import { uninstallApi } from "../api";
import type { UninstallDetectItem, UninstallStepResult, UninstallTarget } from "../api/types";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useConfirm } from "./useConfirm";

/** 卸载进度阶段（决定界面文案与按钮禁用） */
export type UninstallPhase = "idle" | "cleanup" | "purge" | "done";

const open = ref(false);
const detecting = ref(false);
const detectError = ref("");
const items = ref<UninstallDetectItem[]>([]);
const program = ref<UninstallTarget | null>(null);
const helper = ref<UninstallTarget | null>(null);
const data = ref<UninstallTarget[]>([]);
const blocked = ref<string | null>(null);
/** 「保留配置与任务」：默认不勾——默认真卸载是既定口径 */
const keepUserData = ref(false);
const phase = ref<UninstallPhase>("idle");
const cleanupResults = ref<UninstallStepResult[]>([]);
const cleanupMessage = ref("");
const error = ref("");
/** 取消失败：退出后仍可能被更新助手装回来（回执里要出声，否则用户以为卸干净了） */
const pendingUpdateLeft = ref(false);

const { confirm } = useConfirm();

/**
 * 拦下卸载的原因（守卫拒绝 或 卸载助手缺失），非 null 时按钮禁用。
 *
 * 助手缺失这一条是**前端补的**：后端 `detect` 只回报它是否在位，"要不要因此拦下"是界面
 * 策略。拦在这里的理由很具体——第二步 `purge` 必然失败（spawn 不出来），而第一步可能已经
 * 清掉了系统残留，用户白删一轮却什么都没卸掉。
 */
const blockReason = computed<string | null>(() => {
  if (blocked.value) return blocked.value;
  if (helper.value && !helper.value.exists) {
    return `卸载助手缺失（${helper.value.path}）：请重新解压完整发布包后再卸载`;
  }
  return null;
});

/** 正在执行（清理或启动卸载）：期间弹窗不可关闭，按钮禁用 */
const running = computed(() => phase.value === "cleanup" || phase.value === "purge");

/** 勾选「保留配置与任务」后**不再删除**的用户数据目录 */
const keptData = computed<UninstallTarget[]>(() =>
  keepUserData.value ? data.value.filter((d) => d.exists) : [],
);

/** 本次实际会删除的用户数据目录（未勾选时 = 存在的全部） */
const dataToDelete = computed<UninstallTarget[]>(() =>
  keepUserData.value ? [] : data.value.filter((d) => d.exists),
);

/** 计划删除的条目名（程序目录 + 数据目录），确认文案与清单渲染共用 */
function plannedDeletions(
  program: UninstallTarget | null,
  dataToDelete: readonly UninstallTarget[],
): string[] {
  const names: string[] = [];
  if (program) names.push(program.label);
  for (const d of dataToDelete) names.push(d.label);
  return names;
}

/**
 * 确认弹窗文案：**逐项点名**将删除的内容。
 *
 * 用户明确要求"删除时要说清会删哪些内容"。「清理残留并卸载程序」这种笼统说法会让
 * 人在按下按钮前不知道自己会失去什么（方案？脚本？定时任务？日志？），而这一步不可恢复。
 */
export function purgeConfirmMessage(
  program: UninstallTarget | null,
  dataToDelete: readonly UninstallTarget[],
  kept: readonly UninstallTarget[],
): string {
  const names = plannedDeletions(program, dataToDelete);
  const deleted = names.length > 0 ? `将永久删除：${names.join("、")}。` : "";
  const keptNote =
    kept.length > 0 ? `已按你的勾选保留 ${kept.length} 项用户数据。` : "";
  return (
    `${deleted}${keptNote}同时关闭开机自启动、删除加密密钥目录并清理 Playwright ` +
    "浏览器缓存。程序会立即退出，此操作不可恢复。"
  );
}

/** 本次将删除的条目总数（0 表示没有任何可删目标） */
const deleteCount = computed(
  () => plannedDeletions(program.value, dataToDelete.value).length,
);

/**
 * 计划删除的条目名（程序目录 + 数据目录）。
 *
 * 既供确认文案使用，也供"卸载已启动"回执使用：那时后端随时会消失，用户最后看到的
 * 那份清单必须与将要执行的删除一致。
 */
const deletionLabels = computed(() => plannedDeletions(program.value, dataToDelete.value));

function reset(): void {
  detecting.value = false;
  detectError.value = "";
  items.value = [];
  program.value = null;
  helper.value = null;
  data.value = [];
  blocked.value = null;
  keepUserData.value = false;
  phase.value = "idle";
  cleanupResults.value = [];
  cleanupMessage.value = "";
  error.value = "";
  pendingUpdateLeft.value = false;
}

/**
 * 打开弹窗：只探测清单，**不执行任何删除**。
 *
 * 探测与执行分开是有意的：用户要先看到"会删掉什么"（含具体路径）再决定，
 * 而不是点开就进入倒计时。
 */
async function openDialog(): Promise<void> {
  reset();
  open.value = true;
  detecting.value = true;
  try {
    const result = await uninstallApi.detect();
    items.value = result.items ?? [];
    program.value = result.program ?? null;
    helper.value = result.helper ?? null;
    data.value = result.data ?? [];
    blocked.value = result.blocked ?? null;
    if (blocked.value) {
      // 守卫拒绝（如该目录是源码仓库）：这不是错误而是"当前环境不支持卸载"，
      // 记一条日志便于排查用户为什么没有卸载按钮
      frontendLogger.warn("about", `卸载被守卫拒绝：${blocked.value}`);
    }
  } catch (e) {
    detectError.value = extractApiError(e, "检测失败");
  } finally {
    detecting.value = false;
  }
}

/**
 * 执行卸载（两步）。
 *
 * 失败语义刻意不对称：
 * - 第一步（系统残留）失败**不阻断**——那些是 best-effort 的环境清理，没理由因为
 *   清不掉浏览器缓存就不卸载程序；
 * - 第二步失败必须说清"程序文件一个都没删"，否则用户会以为卸载过了、直接去删目录。
 */
async function run(): Promise<void> {
  if (running.value || blockReason.value !== null || deleteCount.value === 0) return;

  const ok = await confirm({
    title: "确认卸载",
    message: purgeConfirmMessage(program.value, dataToDelete.value, keptData.value),
    confirmText: "卸载并退出",
    danger: true,
  });
  if (!ok) return;

  error.value = "";
  cleanupResults.value = [];
  cleanupMessage.value = "";

  // 第一步：系统残留（进程内可删，不依赖助手）。
  // `keepUserData` 必须传下去：勾了保留时这一步要**保留加密密钥目录**，否则被留下来的
  // config/ 里那些 ENC: 方案密码再也解不开——"保留配置与任务"就成了半句空话。
  phase.value = "cleanup";
  try {
    const cleaned = await uninstallApi.uninstall(keepUserData.value);
    cleanupResults.value = cleaned.results ?? [];
    cleanupMessage.value = cleaned.message ?? "";
  } catch (e) {
    frontendLogger.warn("about", "卸载第一步（清理系统残留）失败，继续执行删除程序", e);
    cleanupMessage.value = extractApiError(e, "系统残留清理失败（已跳过，继续卸载程序）");
  }

  // 第二步：删程序并退出。响应回来后本页面的后端随时会消失
  phase.value = "purge";
  try {
    const purged = await uninstallApi.purge(keepUserData.value);
    pendingUpdateLeft.value = purged.pending_update_left === true;
    phase.value = "done";
    frontendLogger.info(
      "about",
      `已启动卸载：${purged.install_dir}` +
        `（保留用户数据=${purged.kept_user_data}，取消待应用更新=${purged.cancelled_pending_update}）`,
    );
  } catch (e) {
    // "程序文件未被删除"必须写在正文里，**不能**当 `extractApiError` 的 fallback：
    // 后者只在异常没有 message 时才用它，有 message 时原样返回——那样用户只会看到
    // 一句"助手缺失"，完全无从判断程序到底删没删（这是最需要说清的一步）
    error.value = `启动卸载失败，程序文件未被删除：${extractApiError(e, "未知错误")}`;
    phase.value = "idle";
  }
}

/** 关闭弹窗（执行中拒绝关闭：卸载已经不可逆，半途关窗只会让人以为没生效） */
function closeDialog(): void {
  if (running.value) return;
  open.value = false;
}

export function useUninstall() {
  return {
    open,
    detecting,
    detectError,
    items,
    program,
    helper,
    data,
    blocked,
    blockReason,
    keepUserData,
    phase,
    running,
    cleanupResults,
    cleanupMessage,
    error,
    pendingUpdateLeft,
    keptData,
    dataToDelete,
    deleteCount,
    deletionLabels,
    openDialog,
    run,
    closeDialog,
  };
}
