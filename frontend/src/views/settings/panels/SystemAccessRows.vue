<script setup lang="ts">
/** 设置 · 访问与安全面板体（schema 槽位）：远程访问引导密码（密文展示 + 显隐切换）。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import { ref } from "vue";
import { useConfig } from "@/composables/useConfig";

const config = useConfig();

/** 管理凭据输入框的明文显隐（默认密文展示，防旁观泄露） */
const showManagementPassword = ref(false);
</script>

<template>
  <div>
    <!-- 说明收进 ? 气泡：help 为详细说明（含环境变量注入路径），description 为行短说明 -->
    <SettingsRow
      help="远程打开界面时，引导页要求输入本密码以换取访问凭据。建议 ≥8 字符；Docker 部署可经环境变量 CAMPUS_AUTH_MANAGEMENT_PASSWORD 注入（此时此处留空即可）。随下方保存按钮一并提交。"
      description="仅远程（非回环）访问时作为界面引导凭据，本机经托盘打开界面无需此密码"
    >
      <template #label>
        <label for="settings-management-password">管理凭据（远程访问引导密码）</label>
      </template>
      <div class="management-password-row">
        <input
          id="settings-management-password"
          v-model="config.config.app_settings.management_password"
          :type="showManagementPassword ? 'text' : 'password'"
          placeholder="留空表示未设置"
          autocomplete="new-password"
          spellcheck="false"
        />
        <button
          type="button"
          class="btn btn-ghost btn-sm"
          :aria-pressed="showManagementPassword"
          @click="showManagementPassword = !showManagementPassword"
        >
          {{ showManagementPassword ? "隐藏" : "显示" }}
        </button>
      </div>
    </SettingsRow>
  </div>
</template>
