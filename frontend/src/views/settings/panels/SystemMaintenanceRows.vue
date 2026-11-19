<script setup lang="ts">
/** 设置 · 维护操作面板体（schema 槽位）：定时自重启下拉与配置热重载动作（带确认与结果提示）。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import { computed, ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useConfirm } from "@/composables/useConfirm";
import { configApi } from "@/api";

const config = useConfig();
const { confirm } = useConfirm();

const autoRestartOptions = [
  { value: "0", label: "不启用" },
  { value: "6", label: "每 6 小时" },
  { value: "12", label: "每 12 小时" },
  { value: "24", label: "每 24 小时" },
  { value: "48", label: "每 48 小时" },
  { value: "168", label: "每 168 小时（每周）" },
];
const autoRestartHours = computed<string>({
  get: () => String(config.config.app_settings.auto_restart_hours ?? 0),
  set: (v) => { config.config.app_settings.auto_restart_hours = Number(v); },
});

const reloading = ref(false);
const reloadMsg = ref("");
async function reloadConfig() {
  if (config.dirty.value) {
    const ok = await confirm({
      title: "放弃未保存修改",
      message: "重新加载会用磁盘配置覆盖当前未保存的设置，是否继续？",
      confirmText: "放弃并重新加载",
      danger: true,
    });
    if (!ok) return;
  }
  reloading.value = true;
  reloadMsg.value = "";
  try {
    await configApi.reload();
    await config.fetchConfig();
    if (config.configLoadFailed.value) {
      reloadMsg.value = "后端已重新加载，但表单刷新失败，请重试";
      return;
    }
    reloadMsg.value = "配置已重新加载";
  } catch (e: unknown) {
    reloadMsg.value = "重新加载失败：" + ((e as Error).message || "未知错误");
  } finally { reloading.value = false; }
}
</script>

<template>
  <div>
    <SettingsRow description="按运行时长周期性重启本程序，以回收内存">
      <template #label>
        <label for="settings-auto-restart">定时自重启</label>
        <FieldHelp text="按运行时长周期性重启本程序，以回收内存。先启动新进程再退出旧进程，修改即时生效。" />
      </template>
      <CustomSelect v-model="autoRestartHours" :options="autoRestartOptions" aria-label="定时自重启" />
    </SettingsRow>
    <SettingsRow description="从磁盘重新读取配置文件并应用，无需重启">
      <template #label>
        <!-- 行标题：该行控件是自带完整文本的动作按钮「重新加载配置」，标题
             与按钮名不同，强行 for 关联会改写按钮的可访问名（误报豁免） -->
        <!-- eslint-disable-next-line vuejs-accessibility/label-has-for -->
        <label>配置热重载</label>
        <FieldHelp text="从磁盘重新读取配置文件并应用，无需重启。日常修改请使用下方的保存按钮。" />
      </template>
      <template #description>
        <span v-if="reloadMsg" class="hint">{{ reloadMsg }}</span>
      </template>
      <button type="button" class="btn btn-secondary btn-sm" @click="reloadConfig" :disabled="reloading">
        {{ reloading ? "加载中..." : "重新加载配置" }}
      </button>
    </SettingsRow>
  </div>
</template>
