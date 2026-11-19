<script setup lang="ts">
/** 设置 · 验证码识别（OCR）面板体（schema 槽位）：依赖装卸与本地图片识别测试。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { ref, computed, onMounted, onActivated, onUnmounted } from "vue";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { ocrApi } from "@/api";
import { extractApiError } from "@/api/client";
import { pickFile } from "@/utils/file";

const { busy } = useStatus();
const { toastOnly } = useToast();

const ocrStatus = ref<{
  installed: boolean;
  declared?: boolean;
  size_mb?: number;
  runtime_ocr?: boolean | null;
}>({ installed: false });
const ocrStatusLoading = ref(false);
const ocrStatusError = ref(false);

/** 运行时能力说明：区分「依赖已装入环境」与「Worker 已加载」（O-3）
 *
 * `runtime_ocr` 为 null 是常态——Worker 按需懒加载，未运行时没有运行时数据；
 * 只有明确为 false 才表示 Worker 已跑但 OCR 不可用（真正需要排查的情形）。
 */
const runtimeOcrLabel = computed(() => {
  const runtime = ocrStatus.value.runtime_ocr;
  if (runtime === true) return "识别核心已加载，可直接使用";
  if (runtime === false) return "识别核心未加载：请点下方「安装 OCR 依赖」后重试";
  return "识别核心将在首次使用时自动加载（认证核心当前未运行）";
});

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
  } catch (e) {
    // 透出后端具体原因（如「有任务正在执行，请稍后再试」），而非笼统的失败提示
    toastOnly(false, extractApiError(e, "OCR 依赖卸载失败，请查看后端日志后重试"));
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
  <div>
    <SettingsRow label="OCR 依赖">
      <template #description>
        <p class="ocr-description">
          仅在任务中使用
          <code>ocr</code>
          步骤时需要，约占用 120MB 磁盘空间。
        </p>
        <div class="ocr-status-row">
          <span v-if="ocrStatusLoading" class="ocr-status detecting">检测中…</span>
          <span v-else-if="ocrStatusError" class="ocr-status error">
            状态检测失败
            <button class="btn btn-sm btn-link" type="button" @click="refreshOcrStatus">重试</button>
          </span>
          <span v-else-if="ocrStatus.installed" class="ocr-status ok">已安装</span>
          <span v-else class="ocr-status none">未安装</span>
        </div>
        <div v-if="ocrStatus.installed && ocrStatus.size_mb && ocrStatus.size_mb > 0" class="ocr-size-hint">
          当前占用约
          {{ ocrStatus.size_mb }}
          MB
        </div>
        <div v-if="ocrStatus.installed" class="ocr-size-hint">{{ runtimeOcrLabel }}</div>
      </template>
      <button
        v-if="!ocrStatus.installed"
        class="btn btn-primary btn-sm"
        type="button"
        @click="installOcr"
        :disabled="busy.ocr"
      >
        {{ busy.ocr ? '安装中...' : '安装 OCR 依赖' }}
      </button>
      <button
        v-else
        class="btn btn-danger-ghost btn-sm"
        type="button"
        @click="uninstallOcr"
        :disabled="busy.ocr"
      >
        {{ busy.ocr ? '卸载中...' : '卸载 OCR 依赖' }}
      </button>
    </SettingsRow>
    <SettingsRow label="验证码识别测试" description="选择本地验证码图片，验证 OCR 是否正常工作">
      <div v-if="!ocrStatus.installed" class="ocr-hint">安装依赖后可在这里测试。</div>
      <template v-else>
        <div class="ocr-pick-row">
          <button class="btn btn-secondary btn-sm" type="button" @click="pickOcrImage" :disabled="busy.ocrRec">
            选择图片
          </button>
          <span v-if="ocrImageName" class="ocr-filename" :title="ocrImageName">{{ ocrImageName }}</span>
          <span v-else class="hint">未选择图片</span>
        </div>
        <div v-if="ocrImagePreview" class="ocr-preview-wrap">
          <img :src="ocrImagePreview" alt="验证码预览" class="ocr-preview" />
        </div>
        <button
          class="btn btn-primary ocr-recognize-btn"
          type="button"
          :disabled="busy.ocrRec || !ocrImageFile"
          @click="recognizeOcr"
        >
          {{ busy.ocrRec ? '识别中...' : '开始识别' }}
        </button>
        <div v-if="ocrResult" class="ocr-result">
          <span>识别结果：</span>
          <code class="ocr-result-text">{{ ocrResult }}</code>
        </div>
        <div v-if="ocrError" class="ocr-error">识别失败：{{ ocrError }}</div>
      </template>
    </SettingsRow>
  </div>
</template>
