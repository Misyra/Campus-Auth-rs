/**
 * 更新弹窗的全局状态（单例）。
 *
 * 三条触发路径共用同一个弹窗：
 * 1. 启动自动检查（`useUi.autoCheckUpdateOnStartup`）——命中直接弹，不再只发 toast；
 * 2. 后端周期检查命中（`status.update_available` 由假转真，经 `initAutoOpen` 注册的
 *    watch 触发）——此前这条路径前端完全没有提示；
 * 3. 设置页「立即检查」与「查看更新日志」入口（`openWith`）。
 *
 * 数据直接复用 `GET /api/check-update`：其 `notes` 就是 GitHub Release 正文
 * （后端 `manifest_from_github_release` 取的 `body` 字段），`release_date` 为
 * `published_at`，故本功能不需要任何新接口。
 */

import { computed, reactive, watch } from "vue";

import { systemApi } from "../api";
import type { UpdateInfo } from "../api/types";
import { frontendLogger } from "../utils/logger";
import { useConfirm } from "./useConfirm";
import { useStatus } from "./useStatus";

/** 已点过「稍后提醒」的版本号：同一版本不再自动弹窗（手动打开不受影响） */
const SNOOZE_KEY = "campus-auth.update-dialog-snoozed";

function readSnoozedVersion(): string {
  try {
    return localStorage.getItem(SNOOZE_KEY) ?? "";
  } catch {
    // 隐私模式等 localStorage 不可用场景：退化为"每次都会弹"，不影响主流程
    return "";
  }
}

function writeSnoozedVersion(version: string): void {
  try {
    localStorage.setItem(SNOOZE_KEY, version);
  } catch {
    /* 同上：写失败只影响本次记忆，不该中断关闭动作 */
  }
}

const state = reactive({
  /** 弹窗是否可见（点击遮罩/ESC/关闭按钮都会走 `dismiss`） */
  open: false,
  /** 正在拉取检查结果 */
  loading: false,
  /** 正在下载并应用更新 */
  applying: false,
  /** 本次检查结果；`null` 表示尚未拉取 */
  info: null as UpdateInfo | null,
  /** 检查/应用失败的用户可读文案（检查结果自带的 `error` 也会落到这里） */
  error: "",
});

/** 并发去重：启动检查与周期检查可能同时命中，避免重复请求与重复弹窗 */
let inFlight: Promise<UpdateInfo | null> | null = null;

/** 最近一次成功检查的时间戳（毫秒）：手动入口据此决定复用缓存还是重拉 */
let fetchedAt = 0;

/** 手动打开时复用缓存的时限：超过即重拉，避免弹窗永远停在旧结论 */
const OPEN_REUSE_MS = 60_000;

async function fetchInfo(): Promise<UpdateInfo | null> {
  if (inFlight) return inFlight;
  inFlight = (async () => {
    state.loading = true;
    state.error = "";
    try {
      const info = await systemApi.checkUpdate();
      state.info = info;
      fetchedAt = Date.now();
      if (info?.error) state.error = info.error;
      return info;
    } catch (e: unknown) {
      state.error = (e as Error).message || "检查更新失败";
      return null;
    } finally {
      state.loading = false;
    }
  })();
  try {
    return await inFlight;
  } finally {
    inFlight = null;
  }
}

/**
 * 手动打开弹窗（设置页「查看更新日志」）。
 *
 * 没有结果、或结果已超过 [`OPEN_REUSE_MS`] 就重拉：弹窗可能长时间开着，期间后端
 * 周期检查已经发现新版本，一直复用旧结果会让这里始终显示"已是最新"。
 */
async function openDialog(): Promise<void> {
  state.open = true;
  if (!state.info || Date.now() - fetchedAt > OPEN_REUSE_MS) await fetchInfo();
}

/** 用调用方已有的检查结果打开弹窗（设置页两个入口，不重复请求） */
function openWith(info: UpdateInfo | null): void {
  if (info) state.info = info;
  state.error = info?.error ?? "";
  state.open = true;
}

/**
 * 自动路径：检查更新，命中且未被「稍后提醒」过才弹窗。
 *
 * 弹窗已打开时直接返回：启动检查与周期检查命中同一次更新时只弹一次。
 */
async function checkAndMaybeOpen(): Promise<void> {
  if (state.open) return;
  const info = await fetchInfo();
  if (!info?.has_update) return;
  const latest = info.latest ?? "";
  if (latest && latest === readSnoozedVersion()) {
    frontendLogger.debug("update", `v${latest} 已点过稍后提醒，跳过自动弹窗`);
    return;
  }
  frontendLogger.info("update", `发现新版本 v${latest}，已弹出更新说明`);
  state.open = true;
}

/** 重新拉取检查结果（弹窗内「重试」）：先丢弃上次结果，不走"已有结果不重拉"的短路 */
async function refresh(): Promise<void> {
  state.info = null;
  await fetchInfo();
}

/** 关闭弹窗（点击遮罩/ESC/关闭按钮）：不记忆版本，下次命中仍会弹 */
function dismiss(): void {
  state.open = false;
}

/** 「稍后提醒」：记住当前最新版本，之后不再自动弹；手动打开仍可查看 */
function snooze(): void {
  const latest = state.info?.latest ?? "";
  if (latest) writeSnoozedVersion(latest);
  state.open = false;
  frontendLogger.info("update", `已推迟 v${latest} 的更新提醒`);
}

/** 更新暂存完成后的重启询问（与设置页两条更新路径同一交互） */
async function offerRestart(): Promise<void> {
  const { confirm } = useConfirm();
  const ok = await confirm({
    title: "更新已就绪",
    message: "更新已下载完成，是否立即重启应用以生效？",
    confirmText: "立即重启",
  });
  if (!ok) return;
  try {
    await systemApi.restart();
  } catch {
    if (state.info) state.info.message = "更新已就绪，但自动重启失败，请手动重启应用";
  }
}

/** 立即更新：固定本次已确认的版本（防服务端重查导致版本漂移） */
async function applyUpdate(): Promise<void> {
  if (state.applying) return;
  state.applying = true;
  state.error = "";
  try {
    const info = state.info;
    const pin =
      info?.latest && info.url && typeof info.sha256 === "string"
        ? { version: info.latest, url: info.url, sha256: info.sha256 }
        : undefined;
    const data = await systemApi.update(pin);
    state.info = {
      has_update: false,
      message: (data.message as string) || "更新已就绪，重启后生效",
    };
    await offerRestart();
  } catch (e: unknown) {
    state.error = (e as Error).message || "更新失败";
  } finally {
    state.applying = false;
  }
}

/** 下载进度（WS 状态快照的 update_progress，下载期间有值） */
const progress = computed(() => useStatus().status.update_progress ?? null);

/**
 * 在 App 挂载时调用一次：注册"周期检查命中"的自动弹窗。
 *
 * 立即判一次当前值：状态快照可能在挂载前就已带着 `update_available=true`
 * （watch 只在变化时触发，漏判会让本条路径静默失效）。
 */
function initAutoOpen(): void {
  const { status } = useStatus();
  if (status.update_available) void checkAndMaybeOpen();
  watch(
    () => status.update_available,
    (available) => {
      if (available) void checkAndMaybeOpen();
    },
  );
}

export function useUpdateDialog() {
  return {
    state,
    progress,
    openDialog,
    openWith,
    checkAndMaybeOpen,
    initAutoOpen,
    dismiss,
    snooze,
    refresh,
    applyUpdate,
  };
}
