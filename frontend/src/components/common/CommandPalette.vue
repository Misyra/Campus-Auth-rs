<script setup lang="ts">
/**
 * 命令面板（Ctrl+K）。焦点陷阱 / Esc / 遮罩点击 / 滚动锁 / 关闭后焦点归还
 * 全部由 reka Dialog 承担（与 Modal.vue 同一迁移口径）；本组件只做搜索过滤
 * 与键盘选择。开合状态与快捷键见 useCommandPalette（首屏只含这一层），
 * 命令集构建见 commandRegistry（随本组件一起懒加载）；入口为顶栏搜索按钮
 * （AppTopbar 的 .topbar-search），本组件由 App.vue defineAsyncComponent 懒加载，
 * 不进首屏 chunk。样式见 styles/components/command-palette.css。
 */
import { computed, nextTick, ref, watch } from "vue";
import { DialogContent, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from "reka-ui";
import IconApp from "./IconApp.vue";
import { useCommandPalette } from "@/composables/useCommandPalette";
import { buildCommandItems, filterCommands, type PaletteCommand } from "@/composables/commandRegistry";

const palette = useCommandPalette();
const query = ref("");
const activeIndex = ref(0);
const inputEl = ref<HTMLInputElement | null>(null);
const items = ref<PaletteCommand[]>([]);

// 命令集在每次打开时重建：「启动/停止监测」动态文案与登录中可用性即时取值；
// 查询词与选中项复位，避免残留上一次的过滤现场。
// immediate 必需：面板是懒加载 chunk，首次打开时 open 已为 true 才挂载，
// 非 immediate 的 watch 永远收不到这次"打开"，命令集为空（首个打开恒「无匹配命令」）。
watch(palette.open, (open) => {
  if (!open) return;
  items.value = buildCommandItems();
  query.value = "";
  activeIndex.value = 0;
}, { immediate: true });
const filtered = computed(() => filterCommands(items.value, query.value));
// 换查询词回到第一个结果：选中态跟随输入而不是停留在旧的条目上
watch(query, () => {
  activeIndex.value = 0;
});

/** 分组视图：注册顺序下同组连续，entry.index 保留在扁平过滤结果中的位置 */
const groups = computed(() => {
  const result: { group: string; entries: { command: PaletteCommand; index: number }[] }[] = [];
  filtered.value.forEach((command, index) => {
    const last = result[result.length - 1];
    if (last && last.group === command.group) last.entries.push({ command, index });
    else result.push({ group: command.group, entries: [{ command, index }] });
  });
  return result;
});

const activeDomId = computed(() => {
  const command = filtered.value[activeIndex.value];
  return command ? `command-palette-item-${command.id}` : undefined;
});

function itemDomId(index: number): string | undefined {
  const command = filtered.value[index];
  return command ? `command-palette-item-${command.id}` : undefined;
}

function onInputKeydown(event: KeyboardEvent): void {
  const total = filtered.value.length;
  if (!total) return;
  if (event.key === "ArrowDown") {
    event.preventDefault();
    activeIndex.value = (activeIndex.value + 1) % total;
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    activeIndex.value = (activeIndex.value - 1 + total) % total;
  } else if (event.key === "Home") {
    event.preventDefault();
    activeIndex.value = 0;
  } else if (event.key === "End") {
    event.preventDefault();
    activeIndex.value = total - 1;
  } else if (event.key === "Enter") {
    event.preventDefault();
    run(filtered.value[activeIndex.value]);
  }
}

function run(command: PaletteCommand | undefined): void {
  if (!command) return;
  // 先关面板再执行：导航/弹层类命令不与面板遮罩叠加，执行失败也有 toast 兜底
  palette.closePalette();
  command.run();
}

// 选中项过长列表时保持可见；happy-dom 无 scrollIntoView，可选调用
watch(activeIndex, async () => {
  await nextTick();
  document.getElementById(itemDomId(activeIndex.value) ?? "")?.scrollIntoView?.({ block: "nearest" });
});

// reka 默认聚焦首个可聚焦元素；显式落点到搜索框，保证「打开即打字」
function onOpenAutoFocus(event: Event): void {
  event.preventDefault();
  inputEl.value?.focus();
}
</script>

<template>
  <DialogRoot :open="palette.open.value" @update:open="(value) => { if (!value) palette.closePalette(); }">
    <DialogPortal>
      <Transition name="command-palette-fade">
        <DialogOverlay v-if="palette.open.value" force-mount as-child>
          <div class="command-palette-overlay">
            <DialogContent
              as-child
              force-mount
              aria-modal="true"
              :aria-describedby="undefined"
              @open-auto-focus="onOpenAutoFocus"
            >
              <div class="command-palette-panel">
                <DialogTitle class="sr-only">命令面板</DialogTitle>
                <div class="command-palette-input-row">
                  <IconApp name="search" class="command-palette-search-icon" />
                  <input
                    ref="inputEl"
                    v-model="query"
                    class="command-palette-input"
                    type="text"
                    placeholder="搜索命令…"
                    aria-label="搜索命令"
                    role="combobox"
                    aria-autocomplete="list"
                    aria-controls="command-palette-list"
                    aria-expanded="true"
                    :aria-activedescendant="activeDomId"
                    @keydown="onInputKeydown"
                  />
                  <kbd class="command-palette-kbd">Esc</kbd>
                </div>
                <div id="command-palette-list" class="command-palette-list" role="listbox" aria-label="命令列表">
                  <div v-if="!filtered.length" class="command-palette-empty">无匹配命令</div>
                  <div v-for="group in groups" :key="group.group" role="group" :aria-label="group.group">
                    <div class="command-palette-group-label">{{ group.group }}</div>
                    <!-- aria-expanded 恒为 true：面板整体在 palette.open 下渲染，
                         combobox 展开态与输入框存在性等价。选项的键盘激活由输入框
                         activedescendant 模式统一处理（↑↓/Enter），本元素不接收
                         焦点，click 仅指针路径——click-events 在此为误报 -->
                    <!-- eslint-disable-next-line vuejs-accessibility/click-events-have-key-events -->
                    <div
                      v-for="entry in group.entries"
                      :id="`command-palette-item-${entry.command.id}`"
                      :key="entry.command.id"
                      class="command-palette-item"
                      role="option"
                      tabindex="-1"
                      :aria-selected="entry.index === activeIndex"
                      :class="{ active: entry.index === activeIndex }"
                      @click="run(entry.command)"
                      @pointermove="activeIndex = entry.index"
                    >
                      <IconApp v-if="entry.command.icon" :name="entry.command.icon" class="command-palette-item-icon" />
                      <span class="command-palette-item-label">{{ entry.command.label }}</span>
                    </div>
                  </div>
                </div>
                <div class="command-palette-hints">
                  <span><kbd>↑</kbd><kbd>↓</kbd> 选择</span>
                  <span><kbd>↵</kbd> 执行</span>
                  <span><kbd>Esc</kbd> 关闭</span>
                </div>
              </div>
            </DialogContent>
          </div>
        </DialogOverlay>
      </Transition>
    </DialogPortal>
  </DialogRoot>
</template>
