<script setup lang="ts">
/**
 * 信息面板弹层外壳（Reka Popover headless），与 AppDropdownMenu 成对：
 * 动作菜单归 DropdownMenu（menuitem/方向键选择语义），通知历史这类
 * 「信息面板」归 Popover——子项不是 menuitem，menu 语义反而是错误播报
 * （对原报告「统一收口为 DropdownMenu」的修正，见 fixplaybook-E E6）。
 *
 * 行为层由 Reka 承担：Esc / 外部点击关闭、aria-haspopup=dialog +
 * aria-expanded/controls、关闭后焦点归还触发器。受控用法：
 * `:open` + `@update:open`（打开时刻的钩子——如通知未读清零——由宿主在
 * update:open 处理器里做）。
 *
 * 额外属性（如 aria-label）透传到 Content 元素上，不落在定位包装层。
 */
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from "reka-ui";

defineOptions({ inheritAttrs: false });

withDefaults(
  defineProps<{
    /** 受控开合（配合 @update:open 使用） */
    open: boolean;
    /** 弹出侧，默认触发器下方 */
    side?: "top" | "bottom" | "left" | "right";
    /** 与触发器的间距，默认 8px（原 CSS top: calc(100% + 8px) 的近值） */
    sideOffset?: number;
    /** 对齐，默认尾对齐（面板右缘贴触发器右缘，与原 right:0 视觉一致） */
    align?: "start" | "center" | "end";
  }>(),
  { side: "bottom", sideOffset: 8, align: "end" },
);

const emit = defineEmits<{ "update:open": [value: boolean] }>();
</script>

<template>
  <PopoverRoot :open="open" @update:open="emit('update:open', $event)">
    <PopoverTrigger as-child>
      <slot name="trigger" />
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent
        :side="side"
        :align="align"
        :side-offset="sideOffset"
        v-bind="$attrs"
      >
        <slot />
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
