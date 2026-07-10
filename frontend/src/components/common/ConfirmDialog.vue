<script setup lang="ts">
// 确认对话框（替代原生 confirm()）。读取单例 confirmState，点击后调用 resolveConfirm。

import { useConfirm } from "../../composables/useConfirm";

const { confirmState, resolveConfirm } = useConfirm();
</script>

<template>
  <Teleport to="body">
    <div v-if="confirmState.visible" class="modal-overlay confirm-overlay" @click.self="resolveConfirm(false)">
      <div class="confirm-dialog" :class="{ danger: confirmState.danger }" role="alertdialog" aria-modal="true">
        <h3 class="confirm-title">{{ confirmState.title }}</h3>
        <p class="confirm-message">{{ confirmState.message }}</p>
        <div class="confirm-actions">
          <button class="btn btn-secondary" @click="resolveConfirm(false)">{{ confirmState.cancelText }}</button>
          <button
            class="btn"
            :class="confirmState.danger ? 'btn-danger' : 'btn-primary'"
            @click="resolveConfirm(true)"
          >
            {{ confirmState.confirmText }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
