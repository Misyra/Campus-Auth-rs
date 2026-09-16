<script setup lang="ts">
// 确认对话框（替代原生 confirm()）。读取单例 confirmState，点击后调用 resolveConfirm。
// 键盘可达性对齐 Modal：Esc=取消、Enter=确认、打开时聚焦确认按钮、Tab 循环。

import { nextTick, watch } from "vue";
import IconApp from "@/components/common/IconApp.vue";
import { useConfirm } from "../../composables/useConfirm";
import { lockBodyScroll, unlockBodyScroll } from "../../composables/useBodyScrollLock";

const { confirmState, resolveConfirm } = useConfirm();

// Tab 循环（与 Modal.onTrapKeydown 同策略）：仅两个按钮，循环聚焦
function onTrapKeydown(e: KeyboardEvent): void {
  if (e.key !== "Tab") return;
  const buttons = dialogButtons();
  if (buttons.length < 2) return;
  const first = buttons[0];
  const last = buttons[buttons.length - 1];
  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
}

function dialogButtons(): HTMLElement[] {
  return Array.from(
    document.querySelectorAll<HTMLElement>(".confirm-dialog .confirm-actions button"),
  );
}

// 打开时聚焦确认按钮（危险操作也聚焦确认，配合红色样式强化感知）+ 锁定背景滚动
// 注意：confirmState 是 reactive 对象（非 ref），此处不能写 .value，
// 否则 getter 求值抛 TypeError，上报为 watcher getter（生产构建 runtime-2）异常
// FE2-3：滚动锁走全局计数（此前无条件清空 body.overflow，会提前解锁叠加中的 Modal）
watch(
  () => confirmState.visible,
  async (val) => {
    if (!val) {
      unlockBodyScroll();
      return;
    }
    lockBodyScroll();
    await nextTick();
    const buttons = dialogButtons();
    buttons[buttons.length - 1]?.focus();
  },
  { immediate: true },
);
</script>

<template>
  <Teleport to="body">
    <Transition name="modal-fade">
      <div
        v-if="confirmState.visible"
        class="modal-overlay modal-overlay--confirm"
        tabindex="-1"
      @click.self="resolveConfirm(false)"
      @keydown="onTrapKeydown"
      @keydown.esc.prevent="resolveConfirm(false)"
      @keydown.enter.prevent="resolveConfirm(true)"
    >
      <div class="confirm-dialog" :class="{ danger: confirmState.danger }" role="alertdialog" aria-modal="true" aria-labelledby="confirm-dialog-title">
        <h3 id="confirm-dialog-title" class="confirm-title">{{ confirmState.title }}</h3>
        <p class="confirm-message" :class="{ 'confirm-message--tight': confirmState.changes.length }">{{ confirmState.message }}</p>
        <!-- 结构化改动清单：字段名与旧/新值分列着色。旧值弱化、新值强调，
             使"会变成什么"一眼可见（拼接成整段文本做不到这点） -->
        <ul v-if="confirmState.changes.length" class="confirm-changes">
          <li v-for="c in confirmState.changes" :key="c.label" class="confirm-change">
            <span class="confirm-change-label">{{ c.label }}</span>
            <span class="confirm-change-values">
              <span class="confirm-change-from">{{ c.from }}</span>
              <IconApp name="arrow-right" class="confirm-change-arrow" aria-hidden="true" />
              <span class="confirm-change-to">{{ c.to }}</span>
            </span>
          </li>
        </ul>
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
    </Transition>
  </Teleport>
</template>
