<script setup lang="ts">
/**
 * 设置 · 运行环境面板（chromeless 整面板槽位）：可折叠卡，环境就绪自动收起、异常或初始化中自动展开。
 *
 * 折叠开关长在卡头（role=button），整面板自绘 chrome，不走 schema 渲染器的统一卡头。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import IconApp from "@/components/common/IconApp.vue";
import EnvironmentInstallDialog from "@/components/common/EnvironmentInstallDialog.vue";
import { ref, computed, watch, onMounted, onActivated } from "vue";
import { useStatus } from "@/composables/useStatus";
import { useEnvironment } from "@/composables/useEnvironment";
import { environmentChecklist } from "@/utils/environmentStatus";

const { busy } = useStatus();
const { envStatus, envLoading, envError, refreshEnv } = useEnvironment();

const envReady = computed(() => Boolean(envStatus.value?.capability_ready));

/** 组件清单（uv/Python/认证核心/浏览器/OCR）由共享工具派生，本页只负责渲染 */
const envChecks = computed(() => (envStatus.value ? environmentChecklist(envStatus.value) : []));

/** 运行环境卡折叠态：就绪自动折叠、异常/初始化中自动展开；头部可手动切换，下次状态变化仍自动跟随 */
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

/**
 * 初始化弹窗开关：「初始化环境 / 重新同步」统一走 EnvironmentInstallDialog
 * （阶段进度 + 下载源测速选择 + 日志尾部），不再只靠 toast 等结果。
 */
const installDialogOpen = ref(false);

onMounted(() => { void refreshEnv(); });
onActivated(() => { void refreshEnv(); });
</script>

<template>
  <!-- 运行环境：可折叠卡，环境就绪自动收起、异常或初始化中自动展开 -->
  <section
    class="settings-panel env-collapse-card"
    :class="{ 'env-collapsed': envCollapsed }"
  >
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
      <h2>运行环境</h2>
      <span
        v-if="!envLoading && envStatus"
        class="badge badge--sm"
        :class="envReady ? 'badge--success' : 'badge--warn'"
      >
        {{ envReady ? '已就绪' : '未就绪' }}
      </span>
      <span v-if="envStageLabel" class="badge badge--sm">
        {{ envStageLabel }}
        <template v-if="envStatus?.progress?.percent != null">{{ envStatus.progress.percent }}%</template>
      </span>
      <IconApp name="chevron-down" class="env-collapse-chevron" />
    </div>
    <div v-show="!envCollapsed" class="card-body">
      <p class="hint env-lead">
        自动登录需要 Python 环境、认证核心和可用浏览器三项就绪。每次启动前都会自动检测，缺失的组件会先自动安装再继续；验证码识别（OCR）是可选功能，按需安装即可。
      </p>
      <div class="env-status-row">
        <span v-if="envLoading" class="hint">检测中…</span>
        <template v-else-if="envError && !envStatus">
          <span class="env-error">{{ envError }}</span>
          <button class="btn btn-sm btn-link" type="button" @click="void refreshEnv()">重试</button>
        </template>
        <template v-else-if="envStatus">
          <!-- 清单口径来自 utils/environmentStatus（与仪表盘横幅、关于页共用） -->
          <div class="env-checklist" aria-label="环境组件状态">
            <div
              v-for="c in envChecks"
              :key="c.key"
              class="env-check-item"
              :class="c.optional ? 'optional' : c.ready ? 'ready' : 'missing'"
            >
              <span class="env-check-dot"></span>
              <span class="env-check-name">{{ c.name }}</span>
              <span class="env-check-value">{{ c.value }}</span>
            </div>
          </div>
        </template>
        <span v-else class="hint">暂未获取到环境状态</span>
      </div>
      <p v-if="envStatus?.progress?.message" class="hint">{{ envStatus.progress.message }}</p>
      <p v-if="envStatus?.last_error" class="hint env-error env-preline">{{ envStatus.last_error }}</p>
      <p v-if="envError && envStatus" class="hint env-error">{{ envError }}</p>
      <SettingsRow label="同步运行环境" description="安装缺失组件或重新同步现有依赖">
        <button
          v-if="envReady"
          type="button"
          class="btn btn-secondary btn-sm"
          :disabled="busy.env"
          @click="installDialogOpen = true"
          title="重新同步运行环境（Python 依赖与浏览器）"
        >
          <IconApp v-if="busy.env" name="refresh" class="spin" />
          {{ busy.env ? "同步中..." : "重新同步" }}
        </button>
        <button
          v-else
          type="button"
          class="btn btn-primary btn-sm"
          :disabled="busy.env"
          @click="installDialogOpen = true"
          title="初始化运行环境（uv sync + 浏览器）"
        >
          <IconApp v-if="busy.env" name="refresh" class="spin" />
          {{ busy.env ? "初始化中..." : "初始化环境" }}
        </button>
      </SettingsRow>
    </div>
  </section>
  <!-- 初始化弹窗（阶段进度 / 下载源选择 / 日志），Teleport 与滚动锁由 Modal 承担 -->
  <EnvironmentInstallDialog
    :open="installDialogOpen"
    auto-start
    @close="installDialogOpen = false"
  />
</template>
