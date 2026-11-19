<script setup lang="ts">
/**
 * 设置页共享说明块（schema 槽位）：登录/探测统一走方案出口 + 代理例外指引。
 *
 * 「网络检测」与「浏览器」两个分区都有同一段文案，抽成共享槽位避免两处漂移。
 */
import ProxyBypassHelp from "@/components/common/ProxyBypassHelp.vue";
</script>

<template>
  <div>
    <p class="hint">登录、重定向检测和网络探测统一使用方案中的“校园网出口”。<RouterLink to="/profiles">前往方案设置出口、网卡与 DNS</RouterLink></p>
    <ProxyBypassHelp />
  </div>
</template>
