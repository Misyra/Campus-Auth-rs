<script setup lang="ts">
// Toast 通知（单例状态展示）。替代原 toast.html 的全局 toast 节点。

import { useToast } from "../../composables/useToast";
import IconApp from "./IconApp.vue";

const { toast } = useToast();
</script>

<template>
  <div
    v-if="toast.message"
    class="toast"
    :class="[toast.success ? 'success' : 'error', { leaving: toast.leaving }]"
    role="alert"
    aria-live="assertive"
  >
    <IconApp v-if="toast.success" name="check-circle" aria-hidden="true" />
    <IconApp v-else name="x-circle" aria-hidden="true" />
    <span>{{ toast.message }}</span>
  </div>
</template>
