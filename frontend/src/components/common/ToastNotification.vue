<script setup lang="ts">
// Toast 通知（单例状态展示）。替代原 toast.html 的全局 toast 节点。
//
// aria-live 区域必须先于内容变化存在才有播报保证：若 role="alert" 挂在每条
// 消息新建的节点上，屏幕阅读器会把变化当成节点增删而非 live 区域更新而漏播。
// 容器因此常驻（空态零尺寸、无消息体不可交互），消息体在容器内进出——离场
// 动画走内容级 .leaving class，live 节点全程不重建（见 ToastNotification.test.ts）。

import { useToast } from "../../composables/useToast";
import IconApp from "./IconApp.vue";

const { toast } = useToast();
</script>

<template>
  <div class="toast-live" role="alert" aria-live="assertive">
    <div
      v-if="toast.message"
      class="toast"
      :class="[toast.success ? 'success' : 'error', { leaving: toast.leaving }]"
    >
      <IconApp v-if="toast.success" name="check-circle" aria-hidden="true" />
      <IconApp v-else name="x-circle" aria-hidden="true" />
      <span>{{ toast.message }}</span>
    </div>
  </div>
</template>
