<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const config = useConfig();
const { busy, autostart } = useStatus();

const loginActionOptions: SelectOption[] = [
  { value: "monitor", label: "开始检测" },
  { value: "login_once", label: "登录一次后退出" },
  { value: "none", label: "无操作" },
];
const startupActionHint = computed(() => {
  switch (config.config.app_settings.startup_action) {
    case "monitor": return "启动后开始持续检测，断线自动重连";
    case "login_once": return "启动后执行一次登录，成功后自动退出程序";
    default: return "启动后不执行任何操作";
  }
});

const autostartModeOptions: SelectOption[] = [
  { value: "full", label: "完整模式" },
  { value: "lightweight", label: "轻量模式" },
];
const runtimeModeHint = computed(() =>
  config.config.app_settings.runtime_mode === "lightweight"
    ? "仅运行后台检测，不启动 Web 控制台"
    : "保留 Web 控制台，可查看状态与手动操作",
);

const logLevelOptions: SelectOption[] = [
  { value: "TRACE", label: "TRACE" },
  { value: "DEBUG", label: "DEBUG" },
  { value: "INFO", label: "INFO" },
  { value: "WARN", label: "WARN" },
  { value: "ERROR", label: "ERROR" },
];
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--cols2">
    <!-- 启动与运行 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="power" class="settings-card-icon" />
        <h2>启动与运行</h2>
      </div>
      <div class="card-body">
        <div class="form-group">
          <div class="field-label-row"><label for="settings-startup-action">启动后执行</label><FieldHelp text="程序启动后自动执行的操作。" /></div>
          <CustomSelect v-model="config.config.app_settings.startup_action" :options="loginActionOptions" />
          <span class="hint">{{ startupActionHint }}</span>
        </div>
        <div class="form-group">
          <div class="field-label-row"><label>运行模式</label><FieldHelp text="完整模式保留 Web 控制台；轻量模式仅后台检测。切换后重启生效。" /></div>
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
              <div class="field-label-row"><label for="settings-log-retention">日志保留天数</label><FieldHelp text="日志和失败截图按天归档，超过设定天数自动清理。" /></div>
              <input id="settings-log-retention" v-model.number="config.config.logging.retention_days" type="number" min="1" max="365" />
            </div>
          </div>
          <div class="toggle-group">
            <div class="toggle-with-help">
              <label class="toggle toggle-help-inline"><input type="checkbox" v-model="config.config.logging.file_enabled" /><span class="toggle-slider"></span><span class="toggle-label">启用文件日志</span></label>
            </div>
          </div>
        </div>
        <div>
          <div class="form-group">
            <div class="field-label-row"><label>全局日志级别</label><FieldHelp text="低于该级别的日志将被过滤。选择后即时热更新。" /></div>
            <CustomSelect :model-value="config.config.logging.level" :options="logLevelOptions" @update:model-value="config.setLogLevel($event as string)" />
          </div>
        </div>
      </div>
    </section>

    <!-- 占位均衡：日志设置较矮时由界面行为补齐，见 system.css -->
    <!-- 界面行为 -->
    <section class="card settings-panel">
      <div class="settings-card-header">
        <IconApp name="monitor" class="settings-card-icon" />
        <h2>界面行为</h2>
      </div>
      <div class="card-body">
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline"><input type="checkbox" v-model="config.config.app_settings.auto_start_browser" /><span class="toggle-slider"></span><span class="toggle-label">启动时打开控制台</span></label>
            <FieldHelp text="启用后，程序启动时自动打开 Web 控制台。" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline"><input type="checkbox" v-model="config.config.app_settings.task_notification" /><span class="toggle-label">任务通知</span></label>
            <FieldHelp text="关键事件完成时弹出系统通知。" />
          </div>
        </div>
        <div class="toggle-group">
          <div class="toggle-with-help">
            <label class="toggle toggle-help-inline"><input type="checkbox" v-model="config.config.app_settings.show_tray" /><span class="toggle-slider"></span><span class="toggle-label">显示系统托盘图标</span></label>
            <FieldHelp text="关闭后无托盘图标，仅可通过 Web 控制台操作。修改后重启生效。" />
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
