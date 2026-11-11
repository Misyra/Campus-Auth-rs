<script setup lang="ts">
/**
 * 行操作菜单的行为层外壳（Reka DropdownMenu headless）。
 *
 * 此前 4 个任务面板各自手写一份「开关状态 + 文档级 pointerdown/keydown 监听 +
 * 卸载清理」（每处 ~38 行），且只支持 Esc，方向键 / Home/End / typeahead /
 * 焦点归还全部缺失。收口后这些行为由 Reka 承担：Esc 与外部点击关闭、菜单项
 * 方向键导航与 typeahead、关闭后焦点归还触发器、aria-haspopup/expanded/controls
 * 全套语义由库输出（触发器不再手写 aria-*）。
 *
 * 视觉保持现状：Content 默认挂 tasks.css 的 `.tsk-menu`（三面板共享菜单样式）；
 * 定位改由 floating-ui 按触发器计算（portal 出表格，横向滚动时菜单跟随触发器，
 * 窄屏自动翻转避让），`.tsk-menu` 里旧的绝对定位三行已随之删除。
 *
 * 菜单项见 AppDropdownMenuItem：以 button 渲染以命中现有 `.tsk-menu button`
 * 样式，选中后菜单自动关闭，宿主不再手写 closeRowMenu。
 */
import { DropdownMenuContent, DropdownMenuPortal, DropdownMenuRoot, DropdownMenuTrigger } from "reka-ui";

withDefaults(
  defineProps<{
    /** 弹出侧，默认触发器下方 */
    side?: "top" | "bottom" | "left" | "right";
    /** 与触发器的间距，默认 6px（原绝对定位 right:8px 的近值） */
    sideOffset?: number;
    /** 对齐，默认尾对齐（菜单右缘贴触发器右缘，与原 right:8px 视觉一致） */
    align?: "start" | "center" | "end";
    /** Content 附加类；默认样式即 .tsk-menu，换皮时从这里覆盖 */
    contentClass?: string;
  }>(),
  { side: "bottom", sideOffset: 6, align: "end", contentClass: "" },
);

const emit = defineEmits<{ "update:open": [value: boolean] }>();
</script>

<template>
  <DropdownMenuRoot :modal="true" @update:open="emit('update:open', $event)">
    <DropdownMenuTrigger as-child>
      <slot name="trigger" />
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent
        :side="side"
        :align="align"
        :side-offset="sideOffset"
        class="tsk-menu"
        :class="contentClass"
      >
        <slot />
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
