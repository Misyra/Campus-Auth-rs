<script setup lang="ts">
/**
 * 表单字段帮助提示（`?` 圆标）：悬停 / 键盘聚焦即显示，点击可钉住。
 *
 * 钉住态解决两类真实场景：触屏与「边填边看」——脚本契约、占位符规则这类
 * 多段说明在鼠标移向输入框时会消失，只靠悬停等于逼用户背下来。
 *
 * 气泡本体仍是 styles/components/form.css 里的 `.field-help::after` 纯 CSS 实现，
 * 内容取自 `data-tip`，因此只支持纯文本：换行用 `\n`（气泡按 `pre-line` 渲染）。
 */
import { onBeforeUnmount, ref, watch } from "vue";

const props = withDefaults(
  defineProps<{
    /** 气泡文本；换行用 \n */
    text: string;
    /** 右侧空间不足时把气泡翻到左侧 */
    flip?: boolean;
    /** 长文案放宽气泡宽度，否则会糊成一条竖直长条 */
    wide?: boolean;
  }>(),
  { flip: false, wide: false },
);

/** 钉住态：只有点击会置真，悬停显示由 CSS `:hover` 负责 */
const pinned = ref(false);
const root = ref<HTMLElement | null>(null);

/**
 * 气泡定位：默认从触发点向右展开。
 *
 * 窄屏下这一步是必需的——触发点靠右时整块气泡会跑到视口外，说明文字等于丢失
 * （`--flip` 只解决静态可判的右列字段，触发点在页面里的水平位置得实测）。
 * 顺序：右侧放得下 → 向右；右侧放不下而左侧更宽 → 翻到左侧；
 * 两侧都放不下 → 留在较宽一侧，并把宽度压到该侧实际可用值。
 */
const autoFlip = ref(false);

function syncAutoFlip(): void {
  const el = root.value;
  if (!el) return;
  const rect = el.getBoundingClientRect();
  const wanted = props.wide ? 460 : 320;
  const rightRoom = window.innerWidth - rect.right - 26;
  const leftRoom = rect.left - 26;
  const flipToLeft = rightRoom < wanted && leftRoom > rightRoom;
  autoFlip.value = flipToLeft;
  const room = flipToLeft ? leftRoom : rightRoom;
  if (room >= wanted) {
    el.style.removeProperty("--tip-max");
  } else {
    // 200px 下限：别把气泡压成一条竖线（宁可纵向变长，仍在视口内）
    el.style.setProperty("--tip-max", `${Math.max(200, room)}px`);
  }
}

function togglePinned(): void {
  syncAutoFlip();
  pinned.value = !pinned.value;
}

function closeOnPointerDown(event: PointerEvent): void {
  // 气泡是 root 的伪元素，点它命中的仍是 root，不会误关
  if (!root.value?.contains(event.target as Node)) pinned.value = false;
}

function closeOnEscape(event: KeyboardEvent): void {
  if (event.key === "Escape") pinned.value = false;
}

/**
 * 全局监听只在钉住期间挂载：本组件实例以数十计，常驻监听白占开销。
 */
watch(pinned, (open) => {
  if (open) {
    document.addEventListener("pointerdown", closeOnPointerDown);
    document.addEventListener("keydown", closeOnEscape);
  } else {
    document.removeEventListener("pointerdown", closeOnPointerDown);
    document.removeEventListener("keydown", closeOnEscape);
  }
});

// 卸载时兜底摘除，避免路由离开后监听留在 document 上
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", closeOnPointerDown);
  document.removeEventListener("keydown", closeOnEscape);
});
</script>

<template>
  <span
    ref="root"
    class="field-help"
    :class="{
      'field-help--flip': flip || autoFlip,
      'field-help--wide': wide,
      'field-help--pinned': pinned,
    }"
    tabindex="0"
    role="note"
    :data-tip="text"
    @mouseenter="syncAutoFlip"
    @focus="syncAutoFlip"
    @click.stop="togglePinned"
    @keydown.enter.prevent="togglePinned"
    @keydown.space.prevent="togglePinned"
    @keydown.escape="pinned = false"
    >?</span
  >
</template>
