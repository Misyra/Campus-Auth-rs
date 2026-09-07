<script setup lang="ts">
/** 设置 · 环境页：Python 环境初始化、OCR 依赖安装与验证码识别测试 */
import IconApp from "@/components/common/IconApp.vue";
import { ref, computed, onMounted, onActivated, onUnmounted } from "vue";
import { useStatus } from "@/composables/useStatus";
import { useEnvironment } from "@/composables/useEnvironment";
import { ocrApi } from "@/api";
import { extractApiError } from "@/api/client";
import { pickFile } from "@/utils/file";
import { useToast } from "@/composables/useToast";

const { busy } = useStatus();
const { envStatus, envLoading, envError, refreshEnv, bootstrapEnv } = useEnvironment();
const { toastOnly } = useToast();

onMounted(() => { void refreshEnv(); });
onActivated(() => { void refreshEnv(); });

const envReady = computed(() => Boolean(envStatus.value?.capability_ready));
const envStageLabel = computed(() => {
  const s = envStatus.value?.stage;
  if (!s || s === "Done" || s === "Idle") return "";
  const map: Record<string, string> = {
    DownloadingUv: "下载 uv",
    SyncingVenv: "同步虚拟环境",
    InstallingPlaywright: "安装浏览器",
    Error: "失败",
  };
  return map[s] ?? s;
});

const ocrStatus = ref<{ installed: boolean; declared?: boolean; size_mb?: number }>({ installed: false });
const ocrStatusLoading = ref(false);
const ocrStatusError = ref(false);

async function refreshOcrStatus(): Promise<void> {
  ocrStatusLoading.value = true;
  ocrStatusError.value = false;
  try {
    ocrStatus.value = await ocrApi.fetchStatus();
  } catch {
    ocrStatusError.value = true;
  } finally {
    ocrStatusLoading.value = false;
  }
}

onMounted(refreshOcrStatus);
onActivated(refreshOcrStatus);

async function installOcr() {
  busy.ocr = true;
  try {
    await ocrApi.install();
    const installed = await refreshOcrUntilInstalled();
    if (!installed) toastOnly(false, "OCR 安装耗时较长，仍在后台安装中，可稍后回到本页查看状态");
  } catch {
    ocrStatusError.value = true;
    toastOnly(false, "OCR 依赖安装失败，请查看后端日志后重试");
  } finally {
    busy.ocr = false;
  }
}

let ocrPollStopped = false;
onUnmounted(() => {
  ocrPollStopped = true;
  if (ocrImagePreview.value) URL.revokeObjectURL(ocrImagePreview.value);
});

/** 安装后持续轮询 OCR 状态直到就绪：1.5s 间隔、5 分钟兜底截止（后端安装超时靠此收敛）；
 *  页面卸载（ocrPollStopped）即停止轮询，单次失败不中断（网络抖动忽略继续下一轮）。 */
async function refreshOcrUntilInstalled(): Promise<boolean> {
  const deadline = Date.now() + 5 * 60 * 1000;
  while (Date.now() < deadline) {
    if (ocrPollStopped) return ocrStatus.value.installed;
    try {
      const status = await ocrApi.fetchStatus();
      ocrStatus.value = status;
      if (status.installed) return true;
    } catch { /* 网络抖动忽略，继续轮询 */ }
    await new Promise((r) => setTimeout(r, 1500));
  }
  return ocrStatus.value.installed;
}

async function uninstallOcr() {
  busy.ocr = true;
  try {
    await ocrApi.uninstall();
    await refreshOcrStatus();
  } catch {
    toastOnly(false, "OCR 依赖卸载失败，请查看后端日志后重试");
  } finally {
    busy.ocr = false;
  }
}

const ocrImageFile = ref<File | null>(null);
const ocrImageName = ref("");
const ocrImagePreview = ref("");
const ocrResult = ref("");
const ocrError = ref("");

function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = reader.result as string;
      const base64 = dataUrl.includes(",") ? dataUrl.split(",")[1] : dataUrl;
      resolve(base64);
    };
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

async function pickOcrImage() {
  const file = await pickFile("image/*");
  if (!file) return;
  ocrImageFile.value = file;
  ocrImageName.value = file.name;
  if (ocrImagePreview.value) URL.revokeObjectURL(ocrImagePreview.value);
  ocrImagePreview.value = URL.createObjectURL(file);
}

async function recognizeOcr() {
  const file = ocrImageFile.value;
  if (!file) return;
  busy.ocrRec = true;
  ocrResult.value = "";
  ocrError.value = "";
  try {
    const base64 = await readFileAsBase64(file);
    const res = await ocrApi.recognize({ image_base64: base64 });
    ocrResult.value = res.text ?? "";
  } catch (e) {
    ocrError.value = extractApiError(e as Error, "识别失败");
  } finally {
    busy.ocrRec = false;
  }
}
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--cols2 environment-page">
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="terminal" class="settings-card-icon" />
        <h2>Python 环境</h2>
        <button
          v-if="envReady"
          class="btn btn-secondary btn-sm"
          :disabled="busy.env"
          @click="void bootstrapEnv()"
          title="重新同步 Python 虚拟环境与浏览器"
        >
          <IconApp v-if="busy.env" name="refresh" class="spin" />
          {{ busy.env ? "同步中..." : "重新同步" }}
        </button>
        <button
          v-else
          class="btn btn-primary btn-sm"
          :disabled="busy.env"
          @click="void bootstrapEnv()"
          title="初始化 Python 虚拟环境（uv sync）"
        >
          <IconApp v-if="busy.env" name="refresh" class="spin" />
          {{ busy.env ? "初始化中..." : "初始化 Python 环境" }}
        </button>
      </div>
      <div class="card-body">
        <p class="hint env-lead">自动登录与 OCR 依赖该环境。首次使用需初始化一次（约 1–10 分钟），缺失时会自动补装。</p>
        <div class="env-status-row">
          <span v-if="envLoading" class="hint">检测中…</span>
          <template v-else-if="envError && !envStatus"> <span class="env-error">{{ envError }}</span> <button class="btn btn-sm btn-link" type="button" @click="void refreshEnv()">重试</button> </template>
          <template v-else>
            <span v-if="envReady" class="env-pill env-pill--ok">已就绪</span>
            <span v-else class="env-pill env-pill--warn">未就绪</span>
            <span v-if="envStatus?.playwright_ready" class="env-pill">Chromium 已安装</span>
            <span v-if="envStageLabel" class="env-pill">{{ envStageLabel }}<template v-if="envStatus?.progress?.percent != null"> {{ envStatus.progress.percent }}%</template></span>
          </template>
        </div>
        <p v-if="envStatus?.progress?.message" class="hint">{{ envStatus.progress.message }}</p>
        <p v-if="envStatus?.last_error" class="hint env-error env-preline">{{ envStatus.last_error }}</p>
        <p v-if="envError && envStatus" class="hint env-error">{{ envError }}</p>
      </div>
    </section>

    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="file-check" class="settings-card-icon" />
        <h2>OCR 依赖</h2>
        <button v-if="!ocrStatus.installed" class="btn btn-primary btn-sm" type="button" @click="installOcr" :disabled="busy.ocr">
          {{ busy.ocr ? '安装中...' : '安装 OCR 依赖' }}
        </button>
        <button v-else class="btn btn-danger-ghost btn-sm" type="button" @click="uninstallOcr" :disabled="busy.ocr">
          {{ busy.ocr ? '卸载中...' : '卸载 OCR 依赖' }}
        </button>
      </div>
      <div class="card-body">
        <p class="ocr-description">OCR 用于自动识别验证码图片，仅在任务中使用 <code>ocr</code> 步骤时才需要安装，约占用 120MB 磁盘空间。</p>
        <div class="ocr-status-row">
          <span v-if="ocrStatusLoading" class="ocr-status detecting">检测中…</span>
          <span v-else-if="ocrStatusError" class="ocr-status error">状态检测失败 <button class="btn btn-sm btn-link" type="button" @click="refreshOcrStatus">重试</button></span>
          <span v-else-if="ocrStatus.installed" class="ocr-status ok">已安装</span>
          <span v-else class="ocr-status none">未安装，请点击右上按钮安装</span>
        </div>
        <div v-if="ocrStatus.installed && ocrStatus.size_mb && ocrStatus.size_mb > 0" class="ocr-size-hint">当前占用约 {{ ocrStatus.size_mb }} MB</div>
      </div>
    </section>

    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="layout-content" class="settings-card-icon" />
        <h2>验证码识别</h2>
      </div>
      <div class="card-body">
        <p class="ocr-description">选择本地验证码图片进行识别，用于验证 OCR 是否正常工作，需先安装 OCR 依赖。</p>
        <div v-if="!ocrStatus.installed" class="ocr-hint">请先安装 OCR 依赖，再进行识别。</div>
        <template v-else>
          <div class="ocr-pick-row">
            <button class="btn btn-secondary btn-sm" type="button" @click="pickOcrImage" :disabled="busy.ocrRec">选择图片</button>
            <span v-if="ocrImageName" class="ocr-filename" :title="ocrImageName">{{ ocrImageName }}</span>
            <span v-else class="hint">未选择图片</span>
          </div>
          <div v-if="ocrImagePreview" class="ocr-preview-wrap">
            <img :src="ocrImagePreview" alt="验证码预览" class="ocr-preview" />
          </div>
          <button class="btn btn-primary ocr-recognize-btn" type="button" :disabled="busy.ocrRec || !ocrImageFile" @click="recognizeOcr">
            {{ busy.ocrRec ? '识别中...' : '开始识别' }}
          </button>
          <div v-if="ocrResult" class="ocr-result"><span class="ocr-result-label">识别结果：</span><code class="ocr-result-text">{{ ocrResult }}</code></div>
          <div v-if="ocrError" class="ocr-error">识别失败：{{ ocrError }}</div>
        </template>
      </div>
    </section>
  </div>
</template>
