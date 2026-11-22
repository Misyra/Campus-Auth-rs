<script setup lang="ts">
/** 工具栏「社区任务」入口：打开任务仓库导入弹窗，首次使用时在按钮下方挂一条引导气泡。
 *
 * 此前是 TasksView 顶部的常驻宣传横幅：一张大卡片强推一个低频入口，在只有一条
 * 默认任务的列表上喧宾夺主。入口降级进各面板工具栏后，曝光量随之下降，故补一个
 * 引导气泡兜底首次触达：只在「还没有任何自己的任务」时出现（浏览器列表仅剩内置
 * default、HTTP 与脚本为空），点按钮或点 ✕ 都算已引导，写入 localStorage 永久记忆。
 *
 * 引导气泡是纯展示的 role="note"（不需要菜单/弹层交互语义），用锚定定位挂在
 * 按钮下方即可，不引入 Reka 弹层组件。
 */
import { computed, ref } from "vue";
import IconApp from "./IconApp.vue";
import { useRepoImport } from "@/composables/useRepoImport";
import { useTaskDirectory } from "@/composables/useTaskDirectory";

const repo = useRepoImport();
const { browserTasks, httpTasks, scripts, loaded } = useTaskDirectory();

const GUIDE_DISMISS_KEY = "community-tasks-guide-dismissed";

/** 读引导记忆：隐私模式等存储不可用时按「未引导」处理，静默降级（与写入侧对齐） */
function readDismissed(): boolean {
  try {
    return localStorage.getItem(GUIDE_DISMISS_KEY) === "1";
  } catch {
    return false;
  }
}
const dismissed = ref(readDismissed());

/** 任务目录尚未拉完时不展示，避免数据未到就闪一下引导 */
const onlyDefaultTasks = computed(
  () =>
    loaded.value &&
    httpTasks.value.length === 0 &&
    scripts.value.length === 0 &&
    browserTasks.value.every((task) => task.id === "default"),
);

const showGuide = computed(() => !dismissed.value && onlyDefaultTasks.value);

function dismissGuide(): void {
  dismissed.value = true;
  try {
    localStorage.setItem(GUIDE_DISMISS_KEY, "1");
  } catch {
    // 隐私模式等存储不可用时静默：至少本次会话内不再出现
  }
}

function openRepo(): void {
  // 点按钮本身就说明引导已起效，顺手收掉气泡
  dismissGuide();
  repo.showRepoImport();
}
</script>

<template>
  <span class="cte-anchor">
    <button type="button" class="btn btn-sm" title="浏览社区任务库：导入他人分享的登录流程" @click="openRepo">
      <IconApp name="globe-grid" class="icon-sm" />
      社区任务
    </button>
    <span v-if="showGuide" class="cte-guide" role="note">
      <span class="cte-guide-text">第一次用？社区任务库里有现成的校园网登录流程，搜索你的学校一键导入。</span>
      <button type="button" class="cte-guide-close" aria-label="关闭引导" title="知道了" @click="dismissGuide">
        <IconApp name="close" />
      </button>
    </span>
  </span>
</template>

<style scoped>
.cte-anchor {
  position: relative;
  display: inline-flex;
}

/* 引导气泡：锚定按钮右下方，指向按钮的箭头贴右上角。
   底色用 --bg-modal（不透明）而非 --bg-card——后者在两主题下都是半透明
   （壁纸功能让卡片透出底图），气泡又直接浮在内容上，会"看不清文字"。 */
.cte-guide {
  position: absolute;
  top: calc(100% + 8px);
  right: 0;
  width: max-content;
  max-width: 280px;
  padding: 10px 30px 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--bg-modal);
  color: var(--text-primary);
  font-size: var(--text-sm);
  line-height: 1.6;
  text-align: left;
  white-space: normal;
  box-shadow: var(--shadow-float);
  z-index: var(--z-dropdown);
}

.cte-guide::before {
  content: "";
  position: absolute;
  top: -5px;
  right: 22px;
  width: 8px;
  height: 8px;
  transform: rotate(45deg);
  background: var(--bg-modal);
  border-left: 1px solid var(--border);
  border-top: 1px solid var(--border);
}

.cte-guide-close {
  position: absolute;
  top: 6px;
  right: 6px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  padding: 0;
  border: 0;
  border-radius: var(--radius-full);
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
}

.cte-guide-close svg {
  width: 12px;
  height: 12px;
}

.cte-guide-close:hover {
  color: var(--text-primary);
  background: var(--bg-hover);
}
</style>
