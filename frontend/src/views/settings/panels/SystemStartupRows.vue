<script setup lang="ts">
/** 设置 · 启动与运行面板体（schema 槽位）：启动动作/服务模式下拉与开机自启动开关（带注册状态联动）。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import { IS_OPENWRT } from "@/utils/platform";
import { autostartDescription } from "@/utils/autostart";
import { computed, onMounted } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useStatus } from "@/composables/useStatus";

const config = useConfig();
const { busy, autostart, fetchAutostart } = useStatus();

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
  ...(!IS_OPENWRT ? [{ value: "lightweight", label: "轻量模式" }] : []),
];
const runtimeModeHint = computed(() =>
  config.config.app_settings.runtime_mode === "lightweight"
    ? "仅运行后台检测，不启动 Web 控制台"
    : "保留 Web 控制台，可查看状态与手动操作",
);

onMounted(() => {
  // 自启动已无全局轮询（后台不再常驻查询）：进入设置页时拉取一次，
  // 之后靠开关/配置预设操作后的刷新（toggleAutostart / applyRunMode）保持新鲜
  void fetchAutostart();
});
</script>

<template>
  <div>
    <SettingsRow>
      <template #label>
        <label for="settings-startup-action">启动后执行</label>
        <FieldHelp text="程序启动后自动执行的操作。" />
      </template>
      <template #description>
        <span class="hint">{{ startupActionHint }}</span>
      </template>
      <CustomSelect v-model="config.config.app_settings.startup_action" :options="loginActionOptions" aria-label="启动后执行" />
    </SettingsRow>
    <SettingsRow>
      <template #label>
        <label>服务模式</label>
        <FieldHelp text="完整模式保留 Web 控制台；轻量模式仅后台检测。切换后重启生效。" />
      </template>
      <template #description>
        <span class="hint">{{ runtimeModeHint }}</span>
      </template>
      <CustomSelect v-model="config.config.app_settings.runtime_mode" :options="autostartModeOptions" aria-label="服务模式" />
    </SettingsRow>
    <SettingsRow :description="autostartDescription(autostart)">
      <template #label>
        开机自启动
        <span v-if="autostart.method !== '-'" class="badge badge--sm badge--mono">{{ autostart.method }}</span>
        <FieldHelp text="Windows 状态来自系统注册与启动审批检查。移动程序后请重新启用修复；程序不会自动覆盖任务管理器的禁用选择。" />
      </template>
      <button v-if="!autostart.enabled && (autostart.registered || autostart.error)" type="button" class="btn btn-sm btn-secondary" :disabled="busy.autostart" @click="config.toggleAutostart(false)">取消注册</button>
      <label class="toggle setting-row-switch">
        <input
          type="checkbox"
          role="switch"
          :aria-checked="autostart.enabled"
          :checked="autostart.enabled"
          @change="config.toggleAutostart(!autostart.enabled)"
          :disabled="busy.autostart"
        />
        <span class="toggle-slider"></span>
        <span class="sr-only">开机自启动</span>
      </label>
      <template v-if="autostart.registered && !autostart.enabled" #details>
        <p class="text-muted">当前程序：{{ autostart.expected_path }}</p>
        <p class="text-muted">已注册路径：{{ autostart.registry_path || "无法解析" }}</p>
      </template>
    </SettingsRow>
  </div>
</template>
