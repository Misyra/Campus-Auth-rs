<script setup lang="ts">
/** 设置 · 日志设置面板的非常规行（schema 槽位）：日志级别即时热更新 + 导出诊断日志动作。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import { ref } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useToast } from "@/composables/useToast";
import { systemApi } from "@/api";
import { extractApiError } from "@/api/client";
import { downloadBlob } from "@/utils/file";

const config = useConfig();
const { toastOnly } = useToast();

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
  <div>
    <SettingsRow help="低于该级别的日志将被过滤。选择后即时热更新。">
      <template #label>
        <label for="settings-log-level">全局日志级别</label>
      </template>
      <CustomSelect
        :model-value="config.config.logging.level"
        :options="logLevelOptions"
        trigger-id="settings-log-level"
        @update:model-value="config.setLogLevel($event as string)"
      />
    </SettingsRow>
    <SettingsRow label="导出诊断日志" description="打包运行日志、登录历史与脱敏环境摘要，不含密码，可用于反馈问题">
      <button
        class="btn btn-secondary btn-sm"
        type="button"
        :disabled="exportingLogs"
        @click="handleExportLogs"
        title="打包运行日志与登录历史，用于反馈问题"
      >
        {{ exportingLogs ? "导出中..." : "导出日志压缩包" }}
      </button>
    </SettingsRow>
  </div>
</template>
