<script setup lang="ts">
/**
 * 浏览器品牌图标：按 channel 取 `public/icons/` 下的品牌 SVG，
 * 未收录的渠道（如自定义浏览器）回退到通用的线条地球图标。
 *
 * `设置 · 浏览器` 的卡片与首次启动向导的浏览器选择共用，避免两处各写一条
 * v-if 链后渠道收录不同步（新增渠道图标时只改这里一处）。
 */
defineProps<{ /** 浏览器渠道标识（browser_channel / GET /api/browsers 的 channel） */ channel: string; /** 渲染尺寸（宽高一致），默认 32 */ size?: number }>();
</script>

<template>
  <img
    v-if="channel === 'chromium'"
    src="/icons/chromium.svg"
    :width="size ?? 32"
    :height="size ?? 32"
    alt="chromium"
  />
  <img v-else-if="channel === 'msedge'" src="/icons/edge.svg" :width="size ?? 32" :height="size ?? 32" alt="edge" />
  <img v-else-if="channel === 'chrome'" src="/icons/chrome.svg" :width="size ?? 32" :height="size ?? 32" alt="chrome" />
  <img v-else-if="channel === 'firefox'" src="/icons/firefox.svg" :width="size ?? 32" :height="size ?? 32" alt="firefox" />
  <img v-else-if="channel === 'webkit'" src="/icons/webkit.svg" :width="size ?? 32" :height="size ?? 32" alt="webkit" />
  <svg
    v-else
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    :width="size ?? 32"
    :height="size ?? 32"
  >
    <circle cx="12" cy="12" r="10" />
    <text x="12" y="16" text-anchor="middle" font-size="10" fill="currentColor">W</text>
  </svg>
</template>
