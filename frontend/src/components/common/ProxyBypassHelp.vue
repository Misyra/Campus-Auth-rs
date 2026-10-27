<script setup lang="ts">
/** 网络出口帮助入口，完整步骤在文档站维护。 */
import { DOCS } from "@/utils/constants";
</script>

<template>
  <p class="proxy-bypass-help">
    <span>使用 Clash / TUN 时，请确认校园网出口与学校 DNS。</span>
    <a :href="DOCS.networkEgress" target="_blank" rel="noopener noreferrer">查看配置指南 ↗</a>
  </p>
</template>

<style scoped>
.proxy-bypass-help {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  margin: var(--space-sm) 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: 1.6;
}
</style>
