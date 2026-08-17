<script setup lang="ts">
// 表单字段帮助提示：悬停/聚焦显示 data-tip 气泡（样式见 styles/components/form.css 的 .field-help）
defineProps<{ text: string }>();
</script>

<template>
  <span class="field-help" tabindex="0" role="note" :data-tip="text">?</span>
</template>
