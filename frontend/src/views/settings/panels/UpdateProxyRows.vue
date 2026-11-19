<script setup lang="ts">
/** 设置 · 下载与任务仓库代理子分区（schema 槽位）：开关 + 启用后展开的代理地址输入。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import { useConfig } from "@/composables/useConfig";

const config = useConfig();
</script>

<template>
  <div class="settings-subsection">
    <h3>下载与任务仓库代理</h3>
    <SettingsRow description="仅影响版本更新检查、下载与任务仓库">
      <template #label>
        使用代理
        <FieldHelp text="仅影响版本更新检查、下载与任务仓库。检测流量的代理设置见“检测”页。" />
      </template>
      <label class="toggle setting-row-switch">
        <input type="checkbox" v-model="config.config.updater.use_proxy" />
        <span class="toggle-slider"></span>
        <span class="sr-only">使用代理</span>
      </label>
    </SettingsRow>
    <!-- description-id + input aria-describedby：格式说明经编程关联可随控件朗读
         （FieldHelp 的 tooltip 只在悬停/聚焦 `?` 时可达） -->
    <SettingsRow
      v-if="config.config.updater.use_proxy"
      description="完整的 HTTP 代理地址，如 http://127.0.0.1:7890"
      description-id="settings-proxy-url-desc"
    >
      <template #label>
        <label for="settings-proxy-url">代理地址</label>
        <FieldHelp text="完整的 HTTP 代理地址，如 http://127.0.0.1:7890。" />
      </template>
      <input
        id="settings-proxy-url"
        v-model="config.config.updater.proxy_url"
        type="text"
        placeholder="http://127.0.0.1:7890"
        spellcheck="false"
        aria-describedby="settings-proxy-url-desc"
      />
    </SettingsRow>
  </div>
</template>
