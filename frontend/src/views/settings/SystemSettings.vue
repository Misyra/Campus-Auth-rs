<script setup lang="ts">
/** 设置 · 系统页：启动与运行、日志设置及界面行为配置 */
import IconApp from "@/components/common/IconApp.vue";
import { computed, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";
import { useToast } from "@/composables/useToast";
import { useRunMode } from "@/composables/useRunMode";
import { useConfirm } from "@/composables/useConfirm";
import { RUN_MODE_PRESETS } from "@/utils/runMode";
import type { RunModeId } from "@/utils/runMode";
import { systemApi } from "@/api";
import { extractApiError } from "@/api/client";
import { downloadBlob } from "@/utils/file";
import CustomSelect from "@/components/common/CustomSelect.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const config = useConfig();
const { busy, autostart } = useStatus();
const { toastOnly } = useToast();
const { confirm } = useConfirm();
const { currentMode, applying: modeApplying, applyRunMode, diffFor } = useRunMode();

/** 「自定义」不是可点的选项，只作为当前态的展示——它由手动改过设置自然落入 */
const modeOptions = computed(() =>
  RUN_MODE_PRESETS.map((p) => ({ id: p.id, label: p.label, description: p.description })),
);

/** 当前模式的说明文案（含自定义） */
const modeHint = computed(() => {
  if (currentMode.value === "custom") {
    return "检测到你手动调整过相关设置，已不在任何预设内。可点上方模式一键回到常规配置。";
  }
  return RUN_MODE_PRESETS.find((p) => p.id === currentMode.value)?.description ?? "";
});

/**
 * 切换模式：先弹确认并列出**将要改动的项**。
 *
 * 不做"静默应用"：这个动作会写多处设置、还会真实注册/取消开机自启，用户有权在
 * 动手前看到究竟改了什么。改动清单以结构化 `changes` 传入（而非拼成一段文本）——
 * 确认框据此分列着色，多项时仍能逐行扫读。
 */
async function switchMode(id: Exclude<RunModeId, "custom">): Promise<void> {
  if (id === currentMode.value) return;
  const preset = RUN_MODE_PRESETS.find((p) => p.id === id);
  if (!preset) return;
  const changes = diffFor(id);
  const ok = await confirm({
    title: `切换到${preset.label}`,
    message: changes.length
      ? "将应用以下改动："
      : `当前设置已与${preset.label}一致，无需改动。`,
    changes: changes.map((c) => ({ label: c.label, from: c.from, to: c.to })),
    confirmText: "应用",
  });
  if (ok !== true) return;
  await applyRunMode(id);
}

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

const exportingLogs = ref(false);

/** 导出日志压缩包（运行日志 + 登录历史 + 脱敏环境摘要），供随 bug 反馈上传 */
async function handleExportLogs(): Promise<void> {
  if (exportingLogs.value) return;
  exportingLogs.value = true;
  try {
    const blob = await systemApi.exportLogs();
    const stamp = new Date().toISOString().slice(0, 19).replace(/[-:T]/g, "");
    downloadBlob(blob, `campus-auth-logs-${stamp}.zip`, "application/zip");
    toastOnly(true, "日志压缩包已导出");
  } catch (e) {
    toastOnly(false, extractApiError(e as Error, "导出日志压缩包失败"));
  } finally {
    exportingLogs.value = false;
  }
}
</script>

<template>
  <div class="settings-panel-grid settings-panel-grid--cols2">
    <!-- 运行模式：一组设置的命名组合，一键在「稳定跑」与「看得见、好排查」之间切换 -->
    <section class="card settings-panel settings-panel--wide">
      <div class="settings-card-header">
        <IconApp name="sliders" class="settings-card-icon" />
        <h2>运行模式</h2>
        <span class="badge badge--sm" :class="currentMode === 'custom' ? 'badge--warn' : 'badge--success'">
          当前：{{ currentMode === "custom" ? "自定义" : (RUN_MODE_PRESETS.find((p) => p.id === currentMode)?.label ?? "") }}
        </span>
      </div>
      <div class="card-body">
        <div class="run-mode-options">
          <button
            v-for="opt in modeOptions" :key="opt.id"
            type="button"
            class="run-mode-option"
            :class="{ active: currentMode === opt.id }"
            :disabled="modeApplying"
            :aria-pressed="currentMode === opt.id"
            @click="switchMode(opt.id)"
          >
            <span class="run-mode-option-head">
              <span class="run-mode-option-label">{{ opt.label }}</span>
              <IconApp v-if="currentMode === opt.id" name="check" class="icon-sm" />
            </span>
            <span class="run-mode-option-desc">{{ opt.description }}</span>
          </button>
          <!-- 自定义态：由手动改动自然落入，不可点选，只作说明 -->
          <div class="run-mode-option run-mode-option--custom" :class="{ active: currentMode === 'custom' }">
            <span class="run-mode-option-head">
              <span class="run-mode-option-label">自定义</span>
              <IconApp v-if="currentMode === 'custom'" name="check" class="icon-sm" />
            </span>
            <span class="run-mode-option-desc">手动调整过下方任一设置后自动进入此状态。</span>
          </div>
        </div>
        <span class="hint run-mode-hint">{{ modeHint }}</span>
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
              <span v-if="autostart.method !== '-'" class="badge badge--sm badge--mono">{{ autostart.method }}</span>
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
      <div class="card-footer log-export-footer">
        <p class="hint">导出日志压缩包：打包运行日志、登录历史与脱敏环境摘要（不含密码），可随 bug 反馈一并上传。</p>
        <button
          class="btn btn-secondary btn-sm"
          type="button"
          :disabled="exportingLogs"
          @click="handleExportLogs"
          title="打包运行日志与登录历史，用于反馈问题"
        >
          {{ exportingLogs ? "导出中..." : "导出日志压缩包" }}
        </button>
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
            <label class="toggle toggle-help-inline"><input type="checkbox" v-model="config.config.app_settings.task_notification" /><span class="toggle-slider"></span><span class="toggle-label">任务通知</span></label>
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
