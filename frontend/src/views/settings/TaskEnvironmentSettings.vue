<script setup lang="ts">
/** 设置 · 任务与环境页：任务概览与录制器入口 + Python 环境状态、OCR 依赖与验证码识别测试。
 * 原「任务」「环境」两个 Tab 合并而来：任务定义与它依赖的运行环境同域，且两页均为动作/状态卡、无配置保存栏。 */
import IconApp from "@/components/common/IconApp.vue";
import { ref, computed, watch, onMounted, onActivated, onUnmounted } from "vue";
import { useRouter } from "vue-router";
import { useTasks } from "@/composables/useTasks";
import { useRepoImport } from "@/composables/useRepoImport";
import { useStatus } from "@/composables/useStatus";
import { useEnvironment } from "@/composables/useEnvironment";
import { ocrApi } from "@/api";
import { extractApiError } from "@/api/client";
import { pickFile } from "@/utils/file";
import { useToast } from "@/composables/useToast";

const { busy } = useStatus();
const { envStatus, envLoading, envError, refreshEnv, bootstrapEnv } = useEnvironment();
const { toastOnly } = useToast();
const t = useTasks();
const repo = useRepoImport();
const router = useRouter();

onMounted(() => { void t.fetchTasks(); });
onMounted(() => { void refreshEnv(); });
onActivated(() => { void refreshEnv(); });

const activeTaskName = computed(() => {
  const id = t.activeTaskId.value;
  const task = t.tasks.value.find((tk) => tk.id === id);
  return task?.name || id;
});

const envReady = computed(() => Boolean(envStatus.value?.capability_ready));

/** Python 环境卡折叠态：就绪自动折叠、异常/初始化中自动展开；头部可手动切换，下次状态变化仍自动跟随 */
const envCollapsed = ref(false);
watch(envReady, (ready) => { envCollapsed.value = ready; }, { immediate: true });
const envStageLabel = computed(() => {
  const s = envStatus.value?.stage;
  if (!s || s === "Done" || s === "Idle") return "";
  const map: Record<string, string> = {
    Checking: "检查运行环境",
    DownloadingUv: "下载 uv",
    SyncingVenv: "同步虚拟环境",
    VerifyingWorker: "验证 Worker",
    ApplyingOcr: "对齐 OCR 偏好",
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
    <!-- Python 环境：可折叠卡，环境就绪自动收起、异常或初始化中自动展开 -->
    <section class="card settings-panel settings-panel--wide env-collapse-card" :class="{ 'env-collapsed': envCollapsed }">
      <div
        class="settings-card-header env-collapse-header"
        role="button"
        tabindex="0"
        :aria-expanded="!envCollapsed"
        @click="envCollapsed = !envCollapsed"
        @keydown.enter.prevent="envCollapsed = !envCollapsed"
        @keydown.space.prevent="envCollapsed = !envCollapsed"
      >
        <IconApp name="terminal" class="settings-card-icon" />
        <h2>Python 环境</h2>
        <span v-if="!envLoading && envStatus" class="badge badge--sm" :class="envReady ? 'badge--success' : 'badge--warn'">{{ envReady ? '已就绪' : '未就绪' }}</span>
        <span v-if="envStageLabel" class="badge badge--sm">{{ envStageLabel }}<template v-if="envStatus?.progress?.percent != null"> {{ envStatus.progress.percent }}%</template></span>
        <IconApp name="chevron-down" class="env-collapse-chevron" />
      </div>
      <div v-show="!envCollapsed" class="card-body">
        <p class="hint env-lead">自动登录需要 Python 环境、认证核心和可用浏览器三项就绪。每次启动前都会自动检测，缺失的组件会先自动安装再继续；验证码识别（OCR）是可选功能，按需安装即可。</p>
        <div class="env-status-row">
          <span v-if="envLoading" class="hint">检测中…</span>
          <template v-else-if="envError && !envStatus"> <span class="env-error">{{ envError }}</span> <button class="btn btn-sm btn-link" type="button" @click="void refreshEnv()">重试</button> </template>
          <template v-else-if="envStatus">
            <div class="env-checklist" aria-label="环境组件状态">
              <div class="env-check-item" :class="envStatus.uv_ready ? 'ready' : 'missing'">
                <span class="env-check-dot"></span><span class="env-check-name">uv</span><span class="env-check-value">{{ envStatus.uv_ready ? '可用' : '未就绪' }}</span>
              </div>
              <div class="env-check-item" :class="envStatus.python_ready ? 'ready' : 'missing'">
                <span class="env-check-dot"></span><span class="env-check-name">Python</span><span class="env-check-value">{{ envStatus.python_ready ? '可运行' : '未就绪' }}</span>
              </div>
              <div class="env-check-item" :class="envStatus.worker_ready && envStatus.manifest_current ? 'ready' : 'missing'">
                <span class="env-check-dot"></span><span class="env-check-name">认证核心</span><span class="env-check-value">{{ envStatus.worker_ready && envStatus.manifest_current ? '已验证' : (envStatus.worker_ready ? '需要同步' : '未就绪') }}</span>
              </div>
              <div class="env-check-item" :class="envStatus.playwright_ready || envStatus.system_browser_ready ? 'ready' : 'missing'">
                <span class="env-check-dot"></span><span class="env-check-name">浏览器</span><span class="env-check-value">{{ envStatus.playwright_ready ? 'Chromium 可用' : (envStatus.system_browser_ready ? '系统浏览器可用' : '未就绪') }}</span>
              </div>
              <div class="env-check-item" :class="!envStatus.ocr_enabled ? 'optional' : (envStatus.ocr_ready ? 'ready' : 'missing')">
                <span class="env-check-dot"></span><span class="env-check-name">OCR</span><span class="env-check-value">{{ !envStatus.ocr_enabled ? '未启用（可选）' : (envStatus.ocr_ready ? '已就绪' : '未就绪') }}</span>
              </div>
            </div>
          </template>
          <span v-else class="hint">暂未获取到环境状态</span>
        </div>
        <p v-if="envStatus?.progress?.message" class="hint">{{ envStatus.progress.message }}</p>
        <p v-if="envStatus?.last_error" class="hint env-error env-preline">{{ envStatus.last_error }}</p>
        <p v-if="envError && envStatus" class="hint env-error">{{ envError }}</p>
        <div class="env-card-actions">
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
      </div>
    </section>

    <!-- 任务概览 -->
    <section class="card settings-panel task-overview-card">
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
    <section class="card settings-panel">
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
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="file-check" class="settings-card-icon" />
        <h2>OCR 依赖</h2>
      </div>
      <div class="card-body">
        <p class="ocr-description">OCR 用于自动识别验证码图片，仅在任务中使用 <code>ocr</code> 步骤时才需要安装，约占用 120MB 磁盘空间。</p>
        <div class="ocr-status-row">
          <span v-if="ocrStatusLoading" class="ocr-status detecting">检测中…</span>
          <span v-else-if="ocrStatusError" class="ocr-status error">状态检测失败 <button class="btn btn-sm btn-link" type="button" @click="refreshOcrStatus">重试</button></span>
          <span v-else-if="ocrStatus.installed" class="ocr-status ok">已安装</span>
          <span v-else class="ocr-status none">未安装</span>
        </div>
        <div v-if="ocrStatus.installed && ocrStatus.size_mb && ocrStatus.size_mb > 0" class="ocr-size-hint">当前占用约 {{ ocrStatus.size_mb }} MB</div>
        <div class="env-card-actions">
          <button v-if="!ocrStatus.installed" class="btn btn-primary btn-sm" type="button" @click="installOcr" :disabled="busy.ocr">
            {{ busy.ocr ? '安装中...' : '安装 OCR 依赖' }}
          </button>
          <button v-else class="btn btn-danger-ghost btn-sm" type="button" @click="uninstallOcr" :disabled="busy.ocr">
            {{ busy.ocr ? '卸载中...' : '卸载 OCR 依赖' }}
          </button>
        </div>
      </div>
    </section>

    <!-- 验证码识别 -->
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
