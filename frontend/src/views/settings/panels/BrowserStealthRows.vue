<script setup lang="ts">
/** 设置 · 反自动化检测行（schema 槽位）：开关 + 启用后展开的自定义脚本输入与内置脚本回填。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { useConfig } from "@/composables/useConfig";
import { useToast } from "@/composables/useToast";
import { configApi } from "@/api";
import { extractApiError } from "@/api/client";
import { frontendLogger } from "@/utils/logger";

const config = useConfig();
const { toastOnly } = useToast();

async function loadDefaultStealthScript() {
  try {
    const data = await configApi.fetchStealthScript();
    config.config.browser.stealth_custom_script = data.script;
  } catch (error) {
    // FE2-6：失败给用户可见反馈，避免误以为脚本已填入
    frontendLogger.error("browser", "获取内置脚本失败", error);
    toastOnly(false, `获取内置脚本失败：${extractApiError(error, "未知错误")}`);
  }
}
</script>

<template>
  <div>
    <SettingsRow>
      <template #label>
        反自动化检测
        <FieldHelp text="隐藏自动化特征。仅在被登录页拦截时启用。" />
      </template>
      <template #description>
        <p id="settings-stealth-description">隐藏自动化特征；脚本留空使用内置版本，仅在需要自定义时填写</p>
      </template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.browser.stealth_mode" />
        <span class="toggle-slider"></span>
        <span class="sr-only">反自动化检测</span>
      </label>
      <template v-if="config.config.browser.stealth_mode" #details>
        <div class="setting-row-control form-group">
          <textarea
            v-model="config.config.browser.stealth_custom_script"
            rows="6"
            placeholder="留空使用内置脚本"
            class="textarea--mono"
            aria-label="自定义反检测脚本"
            aria-describedby="settings-stealth-description"
          ></textarea>
          <button type="button" class="btn btn-sm btn-secondary" @click="loadDefaultStealthScript()">填入内置脚本</button>
        </div>
      </template>
    </SettingsRow>
  </div>
</template>
