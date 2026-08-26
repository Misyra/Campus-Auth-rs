<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { ref, computed, onMounted, onActivated } from "vue";
import { useRouter } from "vue-router";
import { useTasks } from "@/composables/useTasks";
import { useRepoImport } from "@/composables/useRepoImport";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { ocrApi } from "@/api";
import { pickFile } from "@/utils/file";

const t = useTasks();
const repo = useRepoImport();
const router = useRouter();
const { busy } = useStatus();
const { toastOnly } = useToast();

const ocrStatus = ref<{ installed: boolean; declared?: boolean; size_mb?: number }>({
  installed: false,
});
const ocrStatusLoading = ref(false);
const ocrStatusError = ref(false);

/** 拉取 OCR 依赖安装状态（每次进入本页都会调用，确保状态实时） */
async function refreshOcrStatus(): Promise<void> {
  ocrStatusLoading.value = true;
  ocrStatusError.value = false;
  try {
    ocrStatus.value = await ocrApi.fetchStatus();
  } catch {
    // 不静默吞掉错误：标记为检测失败，UI 提示可重试，避免停留在过期的「未安装」
    ocrStatusError.value = true;
  } finally {
    ocrStatusLoading.value = false;
  }
}

// 进入页面即检测 OCR 依赖安装状态（onMounted 首次挂载 + onActivated 每次重新进入）
onMounted(refreshOcrStatus);
onActivated(refreshOcrStatus);

const activeTaskName = computed(() => {
  const id = t.activeTaskId.value;
  const task = t.tasks.value.find((tk) => tk.id === id);
  return task?.name || id;
});

async function installOcr() {
  busy.ocr = true;
  try {
    await ocrApi.install();
    // 安装为后台异步，轮询直到 installed=true 或超时，装完自动刷新状态（无需再点一次）
    const installed = await refreshOcrUntilInstalled();
    if (!installed) {
      // G22：5 分钟超时退出时给出提示——安装可能仍在后台进行，并非失败
      toastOnly(false, "OCR 安装耗时较长，仍在后台安装中，可稍后回到本页查看状态");
    }
  } catch {
    // G22：不静默吞掉安装异常——复用 ocrStatusError 标记 + toast 报错，用户可感知并重试
    ocrStatusError.value = true;
    toastOnly(false, "OCR 依赖安装失败，请查看后端日志后重试");
  } finally {
    busy.ocr = false;
  }
}

/** 轮询 /api/ocr/status，直到 installed 为 true 或到达超时（最大 5 分钟）；返回最终是否已安装 */
async function refreshOcrUntilInstalled(): Promise<boolean> {
  const deadline = Date.now() + 5 * 60 * 1000;
  while (Date.now() < deadline) {
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
    // G22：卸载失败同样不静默，toast 报错提示用户
    toastOnly(false, "OCR 依赖卸载失败，请查看后端日志后重试");
  } finally {
    busy.ocr = false;
  }
}

// 验证码识别：选择本地图片 → 转 base64 → 调用后端 OCR
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
      // 去掉 data:image/png;base64, 前缀，仅保留纯 base64
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
    ocrError.value = String(e);
  } finally {
    busy.ocrRec = false;
  }
}
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--task">
    <!-- 任务概览 -->
    <section class="card task-overview-card">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/>
        </svg>
        <h2>任务概览</h2>
      </div>
      <div class="card-body">
        <div class="task-overview-compact">
          <div class="task-overview-left">
            <span class="task-overview-label">当前任务</span>
            <span class="task-overview-name">{{ activeTaskName }}</span>
          </div>
          <div class="task-overview-right">
            <button class="btn btn-primary btn-sm" type="button" @click="router.push({ name: 'tasks' })">管理任务</button>
          </div>
        </div>
        <div class="task-overview-actions">
          <button class="btn btn-secondary btn-sm" type="button" @click="t.importTask()">从文件导入</button>
          <button class="btn btn-secondary btn-sm" type="button" @click="repo.showRepoImport()">从仓库导入</button>
          <button class="btn btn-secondary btn-sm" type="button" @click="t.fetchTasks(true)">刷新列表</button>
          <a href="https://github.com/Misyra/campus-auth-tasks" target="_blank" rel="noopener" class="btn btn-secondary btn-sm">分享任务</a>
        </div>
      </div>
    </section>

    <!-- 任务录制器 -->
    <section class="card">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/>
        </svg>
        <h2>任务录制器</h2>
      </div>
      <div class="card-body">
        <div class="task-recorder-section">
          <p class="task-recorder-desc">任务录制器是一个浏览器脚本，可在登录页面上点击账号框、密码框、验证码等位置，自动生成配置。</p>
          <div class="task-recorder-actions">
            <a href="/api/tools/task-recorder.user.js" class="btn btn-primary">
              <IconApp name="upload" class="icon-sm" />
              安装录制器脚本
            </a>
            <a href="/api/docs/task-writing-guide" class="btn btn-secondary">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="icon-sm">
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/>
                <polyline points="14 2 14 8 20 8"/>
                <line x1="16" y1="13" x2="8" y2="13"/>
                <line x1="16" y1="17" x2="8" y2="17"/>
                <polyline points="10 9 9 9 8 9"/>
              </svg>
              导出编写指南
            </a>
          </div>
          <div class="task-recorder-note">需要 <a href="https://www.tampermonkey.net/" target="_blank" rel="noopener">Tampermonkey</a> 浏览器扩展支持。安装后在登录页面点击浮动按钮即可开始录制。</div>
          <div class="task-recorder-note">详细文档请查看 <a href="/api/docs/task-writing-guide" target="_blank">任务编写指南</a> 和 <a href="/api/docs/task-manual" target="_blank">任务手册</a></div>
        </div>
      </div>
    </section>

    <!-- OCR 依赖 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><path d="M9 15l2 2 4-4"/>
        </svg>
        <h2>OCR 依赖</h2>
        <button v-if="!ocrStatus.installed" class="btn btn-primary btn-sm" type="button" @click="installOcr" :disabled="busy.ocr">
          {{ busy.ocr ? '安装中...' : '安装 OCR 依赖' }}
        </button>
        <button v-else class="btn btn-danger-ghost btn-sm" type="button" @click="uninstallOcr" :disabled="busy.ocr">
          {{ busy.ocr ? '卸载中...' : '卸载 OCR 依赖' }}
        </button>
      </div>
      <div class="card-body">
        <p class="ocr-description">OCR 用于自动识别验证码图片。仅在任务中使用 <code>ocr</code> 步骤时才需要安装。安装后会占用约 120MB 磁盘空间。</p>
        <div class="ocr-status-row">
          <span v-if="ocrStatusLoading" class="ocr-status detecting">检测中…</span>
          <span v-else-if="ocrStatusError" class="ocr-status error">
            状态检测失败
            <button class="btn btn-sm btn-link" type="button" @click="refreshOcrStatus">重试</button>
          </span>
          <span v-else-if="ocrStatus.installed" class="ocr-status ok">已安装（进入本页会自动检测）</span>
          <span v-else class="ocr-status none">未安装</span>
        </div>
        <div v-if="ocrStatus.installed && ocrStatus.size_mb && ocrStatus.size_mb > 0" class="ocr-size-hint">当前占用约 {{ ocrStatus.size_mb }} MB</div>
      </div>
    </section>

    <!-- 验证码识别 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18"/><path d="M9 21V9"/><path d="M7 13h2M13 13h4"/>
        </svg>
        <h2>验证码识别</h2>
      </div>
      <div class="card-body">
        <p class="ocr-description">选择一张本地验证码图片，调用 OCR 引擎识别其中的文本。需先安装 OCR 依赖。</p>
        <div v-if="!ocrStatus.installed" class="ocr-hint">请先在上方安装 OCR 依赖。</div>
        <div v-else class="ocr-recognize">
          <div class="ocr-pick-row">
            <button class="btn btn-secondary btn-sm" type="button" @click="pickOcrImage" :disabled="busy.ocrRec">
              选择图片
            </button>
            <span v-if="ocrImageName" class="ocr-filename">{{ ocrImageName }}</span>
          </div>
          <img v-if="ocrImagePreview" :src="ocrImagePreview" alt="验证码预览" class="ocr-preview" />
          <button class="btn btn-primary btn-sm" type="button" @click="recognizeOcr" :disabled="busy.ocrRec || !ocrImageFile">
            {{ busy.ocrRec ? '识别中...' : '开始识别' }}
          </button>
          <div v-if="ocrResult" class="ocr-result">
            <span class="ocr-result-label">识别结果：</span>
            <code class="ocr-result-text">{{ ocrResult }}</code>
          </div>
          <div v-if="ocrError" class="ocr-error">识别失败：{{ ocrError }}</div>
        </div>
      </div>
    </section>
  </div>
</template>
