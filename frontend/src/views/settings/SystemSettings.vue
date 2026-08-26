<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { autostartApi, configApi } from "@/api";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const config = useConfig();
const { busy, autostart } = useStatus();

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
    <!-- 日志设置 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"/><polyline points="14 2 14 8 20 8"/>
        </svg>
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
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/>
        </svg>
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
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="12" cy="12" r="3"/><path d="M12 1v2m0 18v2M4.22 4.22l1.42 1.42m12.72 12.72 1.42 1.42M1 12h2m18 0h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42"/>
        </svg>
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
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>
        </svg>
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
        <svg class="settings-card-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="2" width="20" height="8" rx="2" ry="2"/><rect x="2" y="14" width="20" height="8" rx="2" ry="2"/><line x1="6" y1="6" x2="6.01" y2="6"/><line x1="6" y1="18" x2="6.01" y2="18"/>
        </svg>
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

    <!-- 维护操作 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="download" class="settings-card-icon" />
        <h2>维护操作</h2>
      </div>
      <div class="card-body">
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
