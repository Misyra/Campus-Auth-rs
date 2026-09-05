<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted, ref, onActivated } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useEnvironment } from "@/composables/useEnvironment";
import { autostartApi, configApi } from "@/api";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const config = useConfig();
const { busy, autostart } = useStatus();
const { envStatus, envLoading, envError, refreshEnv, bootstrapEnv } = useEnvironment();

// 启动后动作选项
const loginActionOptions: SelectOption[] = [
  { value: "monitor", label: "开始监测" },
  { value: "login_once", label: "登录一次后退出" },
  { value: "none", label: "无操作" },
];
const startupActionHint = computed(() => {
  switch (config.config.app_settings.startup_action) {
    case "monitor": return "启动后开始持续监测，断线自动重连";
    case "login_once": return "启动后执行一次登录，成功后自动退出程序";
    default: return "启动后不执行任何操作";
  }
});

// 运行模式
const autostartModeOptions: SelectOption[] = [
  { value: "full", label: "完整模式" },
  { value: "lightweight", label: "轻量模式" },
];
const runtimeModeHint = computed(() =>
  config.config.app_settings.runtime_mode === "lightweight"
    ? "仅运行后台监测，不启动 Web 控制台"
    : "保留 Web 控制台，可查看状态与手动操作",
);

// 日志级别
const logLevelOptions: SelectOption[] = [
  { value: "TRACE", label: "TRACE" },
  { value: "DEBUG", label: "DEBUG" },
  { value: "INFO", label: "INFO" },
  { value: "WARN", label: "WARN" },
  { value: "ERROR", label: "ERROR" },
];

// Python 环境卡片
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

// 配置热重载
const reloading = ref(false);
const reloadMsg = ref("");
async function reloadConfig() {
  reloading.value = true;
  reloadMsg.value = "";
  try {
    await configApi.reload();
    reloadMsg.value = "配置已重新加载";
  } catch (e: unknown) {
    reloadMsg.value = "重新加载失败：" + ((e as Error).message || "未知错误");
  } finally {
    reloading.value = false;
  }
}
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--cols2">
    <!-- Python 环境（uv sync + Chromium）：状态行较宽，独占整行 -->
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
        <p class="hint env-lead">
          自动登录与 OCR 依赖该环境。首次使用需初始化一次（约 1–10 分钟），缺失时会自动补装。
        </p>
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

    <!-- 日志设置 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="file-text" class="settings-card-icon" />
        <h2>日志设置</h2>
      </div>
      <div class="card-body settings-grid-2col">
        <div>
          <div class="form-row">
            <div class="form-group">
              <div class="field-label-row">
                <label for="settings-log-retention">日志保留天数</label>
                <FieldHelp text="日志和失败截图按天归档，超过设定天数自动清理。" />
              </div>
              <input id="settings-log-retention" v-model.number="config.config.logging.retention_days" type="number" min="1" max="365" />
            </div>
          </div>
          <div class="toggle-group">
            <div class="toggle-with-help">
              <label class="toggle toggle-help-inline">
                <input type="checkbox" v-model="config.config.logging.file_enabled" />
                <span class="toggle-slider"></span>
                <span class="toggle-label">启用文件日志</span>
              </label>
            </div>
          </div>
        </div>
        <div>
          <div class="form-group">
            <div class="field-label-row">
              <label>全局日志级别</label>
              <FieldHelp text="低于该级别的日志将被过滤。选择后即时热更新。" />
            </div>
            <CustomSelect :model-value="config.config.logging.level" :options="logLevelOptions" @update:model-value="config.setLogLevel($event as string)" />
          </div>
        </div>
      </div>
    </section>

    <!-- 启动与运行 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="power" class="settings-card-icon" />
        <h2>启动与运行</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-startup-action">启动后执行</label>
            <FieldHelp text="程序启动后自动执行的操作。" />
          </div>
          <CustomSelect v-model="config.config.app_settings.startup_action" :options="loginActionOptions" />
          <span class="hint">{{ startupActionHint }}</span>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label>运行模式</label>
            <FieldHelp text="完整模式保留 Web 控制台；轻量模式仅后台监测。切换后重启生效。" />
          </div>
          <CustomSelect v-model="config.config.app_settings.runtime_mode" :options="autostartModeOptions" />
          <span class="hint">{{ runtimeModeHint }}</span>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" :checked="autostart.enabled" @change="config.toggleAutostart(!autostart.enabled)" :disabled="busy.autostart" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">开机自启动</span>
              <span v-if="autostart.method !== '-'" class="autostart-method-badge">{{ autostart.method }}</span>
            </label>
            <FieldHelp text="开机登录后自动启动本程序，注册方式显示于开关右侧。" />
          </div>
        </div>
      </div>
    </section>

    <!-- 界面行为 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="monitor" class="settings-card-icon" />
        <h2>界面行为</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.app_settings.auto_start_browser" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">启动时打开控制台</span>
            </label>
            <FieldHelp text="启用后，程序启动时自动打开 Web 控制台。" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.app_settings.task_notification" />
              <span class="toggle-label">任务通知</span>
            </label>
            <FieldHelp text="关键事件完成时弹出系统通知。" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.app_settings.show_tray" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">显示系统托盘图标</span>
            </label>
            <FieldHelp text="关闭后无托盘图标，仅可通过 Web 控制台操作。修改后重启生效。" />
          </div>
        </div>
      </div>
    </section>

    <!-- 网络、端口与代理 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="globe" class="settings-card-icon" />
        <h2>网络、端口与代理</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-app-port">控制台端口</label>
            <FieldHelp text="Web 控制台的监听端口。修改后重启生效，默认 50721。" />
          </div>
          <input id="settings-app-port" v-model.number="config.config.app_settings.port" type="number" min="1024" max="65535" />
          <span class="hint">本机访问地址一般为 http://127.0.0.1:端口</span>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.updater.use_proxy" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">使用代理下载更新</span>
            </label>
            <FieldHelp text="仅影响版本更新检查、下载与任务仓库。监测流量的代理设置见“监测”页。" />
          </div>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-proxy-url">代理地址</label>
            <FieldHelp text="完整的 HTTP 代理地址，如 http://127.0.0.1:7890。仅在启用后生效。" />
          </div>
          <input
            id="settings-proxy-url"
            v-model="config.config.updater.proxy_url"
            type="text"
            placeholder="http://127.0.0.1:7890"
            spellcheck="false"
            :disabled="!config.config.updater.use_proxy"
          />
        </div>
      </div>
    </section>

    <!-- 维护操作：内容窄，独占整行避免右列空洞 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="sliders" class="settings-card-icon" />
        <h2>维护操作</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-auto-restart">定时自重启</label>
            <FieldHelp text="按运行时长周期性重启本程序，以回收内存。先启动新进程再退出旧进程，修改即时生效。" />
          </div>
          <select
            id="settings-auto-restart"
            v-model.number="config.config.app_settings.auto_restart_hours"
          >
            <option :value="0">不启用</option>
            <option :value="6">每 6 小时</option>
            <option :value="12">每 12 小时</option>
            <option :value="24">每 24 小时</option>
            <option :value="48">每 48 小时</option>
            <option :value="168">每 168 小时</option>
          </select>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label>配置热重载</label>
            <FieldHelp text="从磁盘重新读取配置文件并应用，无需重启。日常修改请使用下方的保存按钮。" />
          </div>
          <button class="btn btn-secondary btn-sm" @click="reloadConfig" :disabled="reloading">
            {{ reloading ? "加载中..." : "重新加载配置" }}
          </button>
          <span v-if="reloadMsg" class="hint">{{ reloadMsg }}</span>
        </div>
      </div>
    </section>
  </div>
</template>
