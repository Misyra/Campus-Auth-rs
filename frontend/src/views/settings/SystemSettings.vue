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

// 启动操作选项
const loginActionOptions: SelectOption[] = [
  { value: "monitor", label: "开始监控" },
  { value: "login_once", label: "自动登录，成功后退出" },
  { value: "none", label: "无操作" },
];
const startupActionHint = computed(() => {
  switch (config.config.app_settings.startup_action) {
    case "monitor": return "启动后自动开启网络监测，确保持续在线";
    case "login_once": return "启动后执行一次登录，成功后自动退出程序";
    default: return "启动后不执行任何操作";
  }
});

// 运行模式
const autostartModeOptions: SelectOption[] = [
  { value: "full", label: "完整模式（Web 界面 + 后台监控）" },
  { value: "lightweight", label: "轻量模式（仅后台监控，降低内存占用）" },
];

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
    DownloadingMinGit: "下载 Git",
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
  <div class="settings-panel-grid">
    <!-- Python 环境（uv sync + Chromium） -->
    <section class="card settings-panel">
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
        <p class="hint" style="margin:0 0 0.5rem 0">
          初始化项目内 Python 虚拟环境（<code>uv sync</code>）并安装 Chromium，约需 1–10 分钟。
          手动登录若环境缺失会自动触发；此按钮用于网络中断后的手动修复。
        </p>
        <div class="env-status-row" style="display:flex;flex-wrap:wrap;gap:0.5rem;align-items:center">
          <span v-if="envLoading" class="hint">检测中…</span>
          <template v-else-if="envError && !envStatus"> <span style="color:var(--error)">{{ envError }}</span> <button class="btn btn-sm btn-link" type="button" @click="void refreshEnv()">重试</button> </template>
          <template v-else>
            <span v-if="envReady" class="autostart-method-badge" style="background:var(--success-bg, #dcfce7);color:var(--success, #15803d)">已就绪</span>
            <span v-else class="autostart-method-badge" style="background:var(--warning-bg, #fef3c7);color:var(--warning, #92400e)">未就绪</span>
            <span v-if="envStatus?.playwright_ready" class="autostart-method-badge">Chromium 已安装</span>
            <span v-if="envStageLabel" class="autostart-method-badge">{{ envStageLabel }}<template v-if="envStatus?.progress?.percent != null"> {{ envStatus.progress.percent }}%</template></span>
          </template>
        </div>
        <p v-if="envStatus?.progress?.message" class="hint" style="margin-top:0.35rem">{{ envStatus.progress.message }}</p>
        <p v-if="envStatus?.last_error" class="hint" style="margin-top:0.35rem;color:var(--error)">{{ envStatus.last_error }}</p>
        <p v-if="envError && envStatus" class="hint" style="color:var(--error)">{{ envError }}</p>
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

    <!-- 启动行为 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="power" class="settings-card-icon" />
        <h2>启动行为</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-startup-action">启动后执行</label>
            <FieldHelp text="选择程序启动后自动执行的操作。" />
          </div>
          <CustomSelect v-model="config.config.app_settings.startup_action" :options="loginActionOptions" />
          <span class="hint">{{ startupActionHint }}</span>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" :checked="autostart.enabled" @change="config.toggleAutostart(!autostart.enabled)" :disabled="busy.autostart" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">开机自启动</span>
              <span v-if="autostart.method !== '-'" class="autostart-method-badge">{{ autostart.method }}</span>
            </label>
          </div>
        </div>
      </div>
    </section>

    <!-- 运行模式 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="sun" class="settings-card-icon" />
        <h2>运行模式</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label>运行模式</label>
            <FieldHelp text="轻量模式仅后台监控，不启动 Web 界面；完整模式启动 Web 管理界面。" />
          </div>
          <CustomSelect v-model="config.config.app_settings.runtime_mode" :options="autostartModeOptions" />
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
              <input type="checkbox" :checked="!config.config.app_settings.auto_start_browser" @change="config.config.app_settings.auto_start_browser = !($event.target as HTMLInputElement).checked" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">静默启动（不自动打开浏览器）</span>
            </label>
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.app_settings.task_notification" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">任务通知</span>
            </label>
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.app_settings.show_tray" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">显示系统托盘图标</span>
              <span class="hint">关闭后程序仅在 Web 控制台运行，无桌面图标（重启生效）</span>
            </label>
          </div>
        </div>
      </div>
    </section>

    <!-- 网络与端口 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="server" class="settings-card-icon" />
        <h2>网络与端口</h2>
      </div>
      <div class="card-body">
        <div class="form-row">
          <div class="form-group">
            <div class="field-label-row">
              <label for="settings-app-port">控制台端口</label>
              <FieldHelp text="Web 控制台的监听端口。修改后需要重启服务器。默认 50721。" />
            </div>
            <input id="settings-app-port" v-model.number="config.config.app_settings.port" type="number" min="1024" max="65535" />
          </div>
        </div>
      </div>
    </section>

    <!-- 更新与代理 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="globe" class="settings-card-icon" />
        <h2>更新与代理</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline">
              <input type="checkbox" v-model="config.config.updater.use_proxy" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">使用代理下载更新</span>
              <FieldHelp text="启用后更新检查/下载与仓库任务下载走下方代理地址（如 Clash 默认 http://127.0.0.1:7890）；未启用时跟随系统代理。网络检测的代理行为在监测设置中单独控制。" />
            </label>
          </div>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-proxy-url">代理地址</label>
            <FieldHelp text="完整的 HTTP 代理地址（http:// 或 https:// 开头），如 http://127.0.0.1:7890，也支持局域网内其他机器上的代理。仅在启用“使用代理下载更新”后生效。" />
          </div>
          <input
            id="settings-proxy-url"
            v-model="config.config.updater.proxy_url"
            type="text"
            placeholder="http://127.0.0.1:7890"
            spellcheck="false"
            :disabled="!config.config.updater.use_proxy"
          />
          <span class="hint">更新与仓库任务下载共用此代理；网络检测默认不走代理（监测设置可调）。</span>
        </div>
      </div>
    </section>

    <!-- 维护操作 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="download" class="settings-card-icon" />
        <h2>维护操作</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row">
            <label for="settings-auto-restart">定时自重启</label>
            <FieldHelp text="按运行时长周期性地优雅重启本程序（重新打开浏览器会短暂断开），用于回收长期运行累积的内存。计时基准是本次运行的总时长，修改后无需手动重启即生效。" />
          </div>
          <select
            id="settings-auto-restart"
            v-model.number="config.config.app_settings.auto_restart_hours"
          >
            <option :value="0">不启用</option>
            <option :value="6">每 6 小时</option>
            <option :value="12">每 12 小时</option>
            <option :value="24">每 24 小时</option>
            <option :value="48">每 2 天</option>
            <option :value="168">每 7 天</option>
          </select>
          <span class="hint">重启采用与手动重启相同的优雅关闭流程，不会丢失配置；到点时会先启动新进程再退出旧进程。</span>
        </div>
        <div class="form-group">
          <div class="field-label-row">
            <label>配置热重载</label>
            <FieldHelp text="重新从磁盘读取配置文件，无需重启服务即可应用变更。" />
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
