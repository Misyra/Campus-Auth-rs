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
        <IconApp name="grid" class="settings-card-icon" />
        <h2>任务概览</h2>
      </div>
      <div class="card-body">
        <div class="task-overview-compact">
          <div class="task-overview-left">
            <span class="task-overview-label">当前任务</span>
            <span class="task-overview-name">{{ activeTaskName || '未设置' }}</span>
          </div>
          <div class="task-overview-right">
            <button class="btn btn-primary btn-sm" type="button" @click="router.push({ name: 'tasks' })">管理任务</button>
          </div>
        </div>
        <div class="task-overview-actions">
          <button class="btn btn-secondary btn-sm" type="button" @click="t.importTask()">从文件导入</button>
          <button class="btn btn-secondary btn-sm" type="button" @click="repo.showRepoImport()">从仓库导入</button>
          <button class="btn btn-secondary btn-sm" type="button" @click="t.fetchTasks(true)">刷新列表</button>
          <a href="https://github.com/Misyra/campus-auth-tasks" target="_blank" rel="noopener" class="btn btn-ghost btn-sm">任务仓库 →</a>
        </div>
      </div>
    </section>

    <!-- 任务录制器 -->
    <section class="card">
      <div class="settings-card-header">
        <IconApp name="target" class="settings-card-icon" />
        <h2>任务录制器</h2>
      </div>
      <div class="card-body">
        <div class="task-recorder-section">
          <p class="task-recorder-desc">在登录页点选账号框、密码框、登录按钮等元素，自动生成任务步骤。</p>
          <div class="task-recorder-actions">
            <a href="/api/tools/task-recorder.user.js" class="btn btn-primary">
              <IconApp name="upload" class="icon-sm" />
              安装录制器脚本
            </a>
            <a href="/api/docs/task-writing-guide" download="task-writing-guide.md" class="btn btn-secondary">
              <IconApp name="file-text" class="icon-sm" />
              导出编写指南
            </a>
          </div>
          <div class="task-recorder-note">需先安装 <a href="https://www.tampermonkey.net/" target="_blank" rel="noopener">Tampermonkey</a> 扩展，再安装录制器脚本；在登录页点击浮动按钮开始录制。</div>
          <div class="task-recorder-note">编写规范见 <a href="/api/docs/task-writing-guide" target="_blank">任务编写指南</a> 与 <a href="/api/docs/task-manual" target="_blank">任务手册</a>。</div>
        </div>
      </div>
    </section>

    <!-- OCR 依赖 -->
    <section class="card settings-panel">
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
          <span v-else-if="ocrStatusError" class="ocr-status error">
            状态检测失败
            <button class="btn btn-sm btn-link" type="button" @click="refreshOcrStatus">重试</button>
          </span>
          <span v-else-if="ocrStatus.installed" class="ocr-status ok">已安装</span>
          <span v-else class="ocr-status none">未安装，请点击右上按钮安装</span>
        </div>
        <div v-if="ocrStatus.installed && ocrStatus.size_mb && ocrStatus.size_mb > 0" class="ocr-size-hint">当前占用约 {{ ocrStatus.size_mb }} MB</div>
      </div>
    </section>

    <!-- 验证码识别 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="layout-content" class="settings-card-icon" />
        <h2>验证码识别</h2>
      </div>
      <div class="card-body">
        <p class="ocr-description">选择本地验证码图片进行识别，用于验证 OCR 是否正常工作，需先安装 OCR 依赖。</p>
        <div v-if="!ocrStatus.installed" class="ocr-hint">请先安装 OCR 依赖，再进行识别。</div>
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
