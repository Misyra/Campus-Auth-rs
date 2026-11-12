<script setup lang="ts">
/**
 * 表单字段帮助提示（`?` 圆标）：悬停 / 键盘聚焦即显示，点击可钉住。
 *
 * 钉住态解决两类真实场景：触屏与「边填边看」——脚本契约、占位符规则这类
 * 多段说明在鼠标移向输入框时会消失，只靠悬停等于逼用户背下来。
 *
 * 行为层由 Reka Tooltip 承担（此前是 `.field-help::after` 纯 CSS 气泡 + 手写
 * 钉住监听，见 docs/reports/full-review-2026-11-12.md「FieldHelp → Reka
 * Tooltip/Popover」）：悬停/聚焦的开启时序、指针在触发器与气泡间的 grace
 * area、Esc / 外部按下关闭、aria-describedby 关联、portal + floating-ui
 * 碰撞定位（自动翻边与越界收紧，取代原 syncAutoFlip 的实测逻辑）全部收口给库。
 *
 * 钉住态仍归本组件：受控 `open = 悬停 || 钉住`，库在钉住期间发来的关闭请求
 * （页面滚动、失焦）在 update:open 里驳回——「边填边看」要求气泡跨滚动存活；
 * Esc 与触发器之外的按下才真正解除（escapeKeyDown / pointerDownOutside 钩子）。
 *
 * 气泡外观在 form.css 的 `.field-help-tip`（沿用 tooltip-* token），内容是
 * portal 里的真实元素（role=tooltip），屏读器可经 aria-describedby 读取，
 * 不再依赖伪元素的 `attr(data-tip)`。
 */
import { computed, ref } from "vue";
import { TooltipContent, TooltipPortal, TooltipProvider, TooltipRoot, TooltipTrigger } from "reka-ui";

withDefaults(
  defineProps<{
    /** 气泡文本；换行用 \n */
    text: string;
    /** 右侧空间不足时优先把气泡翻到左侧（实际落位仍由库按碰撞实测决定） */
    flip?: boolean;
    /** 长文案放宽气泡宽度，否则会糊成一条竖直长条 */
    wide?: boolean;
  }>(),
  { flip: false, wide: false },
);

/** 钉住态：只有点击 / 回车会置真；解除靠再次点击、Esc、点气泡外 */
const pinned = ref(false);
/** 悬停 / 聚焦开启态：由库经 update:open 驱动 */
const hovered = ref(false);

const open = computed(() => hovered.value || pinned.value);

function syncOpen(value: boolean): void {
  if (value) {
    hovered.value = true;
  } else if (!pinned.value) {
    // 钉住期间驳回库的关闭请求（页面滚动 / 失焦），气泡钉在原地
    hovered.value = false;
  }
}

function togglePinned(): void {
  pinned.value = !pinned.value;
}

/** Esc / 气泡外按下：真正解除钉住。触发器上的按下已被库 preventDefault
    （disableClosingTrigger），不会走到这里——否则悬停中取消钉住会误关气泡 */
function unpin(): void {
  pinned.value = false;
}

function onPointerDownOutside(event: Event): void {
  if (!event.defaultPrevented) unpin();
}
</script>

<template>
  <!-- Provider 是 Root 的上下文前置（无默认注入值，缺它 Root 直接报错）；
       delay 0 维持迁移前「悬停即显」的节奏，skipDelay 无从谈起不设 -->
  <TooltipProvider>
    <TooltipRoot
      :open="open"
      :delay-duration="0"
      :ignore-non-keyboard-focus="true"
      :disable-closing-trigger="true"
      @update:open="syncOpen"
    >
      <TooltipTrigger as-child>
        <span
          class="field-help"
          :class="{ 'field-help--pinned': pinned }"
          tabindex="0"
          role="note"
          @click.stop="togglePinned"
          @keydown.enter.prevent="togglePinned"
          @keydown.space.prevent="togglePinned"
          @keydown.escape="unpin"
          >?</span
        >
      </TooltipTrigger>
      <TooltipPortal>
        <TooltipContent
          :side="flip ? 'left' : 'right'"
          :side-offset="10"
          :collision-padding="32"
          :aria-label="text"
          class="field-help-tip"
          :class="{ 'field-help-tip--wide': wide, 'field-help-tip--pinned': pinned }"
          @escape-key-down="unpin"
          @pointer-down-outside="onPointerDownOutside"
        >
          <span class="field-help-tip-body">{{ text }}</span>
        </TooltipContent>
      </TooltipPortal>
    </TooltipRoot>
  </TooltipProvider>
</template>
