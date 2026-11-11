<script setup lang="ts">
// 确认对话框（替代原生 confirm()）。读取单例 confirmState，点击后调用 resolveConfirm。
// 行为层同 Modal 走 Reka Dialog：Esc=取消、遮罩点击=取消、Tab 循环、关闭后焦点
// 归还触发器、body 滚动锁由库承担；role 保持 alertdialog（确认框语义强于普通
// dialog，经由 attrs 覆盖 reka 的 role="dialog"）。
// 危险操作先聚焦取消，避免无意按 Enter 即执行危险操作；普通操作聚焦确认按钮——
// 阻止 reka 打开时"聚焦首个可聚焦元素"的默认行为后自行定位。
// 注意：confirmState 是 reactive 对象（非 ref），此处不能写 .value。

import { ref } from "vue";
import { DialogContent, DialogDescription, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from "reka-ui";
import IconApp from "@/components/common/IconApp.vue";
import { useConfirm } from "../../composables/useConfirm";

const { confirmState, resolveConfirm } = useConfirm();

const cancelBtn = ref<HTMLButtonElement | null>(null);
const confirmBtn = ref<HTMLButtonElement | null>(null);

function onOpenAutoFocus(event: Event): void {
  event.preventDefault();
  (confirmState.danger ? cancelBtn.value : confirmBtn.value)?.focus();
}

// reka 的各 dismiss 路径（Esc / 遮罩点击）统一汇为 update:open(false)；
// 确认框无"否决关闭"场景，一律按取消结算（与迁移前 Esc/遮罩点击语义一致）
function onOpenChange(value: boolean): void {
  if (!value) resolveConfirm(false);
}
</script>

<template>
  <DialogRoot :open="confirmState.visible" @update:open="onOpenChange">
    <DialogPortal>
      <Transition name="modal-fade">
        <DialogOverlay v-if="confirmState.visible" force-mount as-child>
          <div class="modal-overlay modal-overlay--confirm" tabindex="-1">
            <DialogContent
              as-child
              force-mount
              role="alertdialog"
              aria-modal="true"
              @open-auto-focus="onOpenAutoFocus"
            >
              <div class="confirm-dialog" :class="{ danger: confirmState.danger }">
                <DialogTitle as="h3" class="confirm-title">{{ confirmState.title }}</DialogTitle>
                <DialogDescription as="p" class="confirm-message" :class="{ 'confirm-message--tight': confirmState.changes.length }">{{ confirmState.message }}</DialogDescription>
                <div v-if="confirmState.sections.length" class="confirm-sections">
                  <section v-for="section in confirmState.sections" :key="section.title" class="confirm-section">
                    <h4>{{ section.title }}</h4>
                    <p>{{ section.text }}</p>
                  </section>
                </div>
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
                  <button ref="cancelBtn" class="btn btn-secondary" @click="resolveConfirm(false)">{{ confirmState.cancelText }}</button>
                  <button
                    ref="confirmBtn"
                    class="btn"
                    :class="confirmState.danger ? 'btn-danger' : 'btn-primary'"
                    @click="resolveConfirm(true)"
                  >
                    {{ confirmState.confirmText }}
                  </button>
                </div>
              </div>
            </DialogContent>
          </div>
        </DialogOverlay>
      </Transition>
    </DialogPortal>
  </DialogRoot>
</template>
