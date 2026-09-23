<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
// 侧边导航栏（替代原 sidebar.html）。
// 路由驱动高亮。导航按「配置对象」组织而非功能清单——每项对应唯一的数据归属：
//   仪表盘 = 状态总览
//   方案   = ProfileData（账号/认证/匹配规则/登录方式，保存走 /api/profiles/{id}）
//   任务   = tasks/（浏览器任务/直连任务/脚本/定时/AI 生成，保存走 /api/tasks|/api/scripts）
//   设置   = GlobalConfig（检测/浏览器/环境/系统/网络/外观，全局保存栏）
//   关于
//
// 二级导航（「任务」分组的五个子项）：此前它是任务页页内的一张竖排导航卡，在宽屏上
// 离左边缘很远、四周全是留白，像被丢在页面中间的一张卡片，且正文与顶栏页标题不在
// 同一列上。现在改为在左侧栏本层展开子项——导航与它归属的一级项长在一起，任务页
// 只剩正文。子项数据在 `utils/navTree.ts`（窄屏 pill 行兜底共用同一份）。
//
// 「设置」的六个 Tab 仍留在设置页内的横向页签：它不产生"孤岛"，且窄屏下横排页签
// 比侧栏展开更省纵向空间，故不做同款收编。

import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useStatus } from "@/composables/useStatus";
import { useUi } from "@/composables/useUi";
import { activeChildId, TASK_NAV_CHILDREN } from "@/utils/navTree";

const route = useRoute();
const router = useRouter();
const { status, busy } = useStatus();
const { quitApp } = useUi();

/** 任务页各子路由的名字都以 tasks 开头；设置页同理用 settings 前缀判定高亮 */
const onTasks = computed(() => String(route.name).startsWith("tasks"));
const onSettings = computed(() => String(route.name).startsWith("settings"));

/** 「任务」分组当前激活的子项 id（不在该分组时为 null） */
const activeTaskChild = computed(() => activeChildId(TASK_NAV_CHILDREN, String(route.name)));

/** 「任务」分组的展开态：**默认收起**，只在用户点「任务」这一行时展开。
 *
 *  默认收起是用户明确要求：侧栏默认保持精简，不要一进来就撑开五条子项。
 *  因此也**不按路由自动展开**——从别的页面点进任务区同样保持收起，侧栏由用户自己
 *  按需展开（展开态在会话内记忆，切页不会自己收回去）。
 *
 *  刻意不落 localStorage：侧栏是本应用唯一通往这五个页面的入口，一次忘记的折叠若被
 *  持久化，下次冷启动整个任务区就被收进一个 caret 后面，属发现性陷阱。组件在 App
 *  生命周期内常驻，故会话内记忆已足够。 */
const tasksExpanded = ref(false);

function toggleTasks(): void {
  tasksExpanded.value = !tasksExpanded.value;
}

function navigate(name: string): void {
  router.push({ name });
}
</script>

<template>
  <nav class="sidebar">
    <div class="sidebar-header">
      <div class="logo">
        <span class="logo-icon logo-mark" role="img" aria-label="认证喵 Campus-Auth"></span>
        <span class="logo-text">认证喵</span>
      </div>
    </div>

    <div class="nav-links">
      <button class="nav-item" :class="{ active: route.name === 'dashboard' }" @click="navigate('dashboard')" title="仪表盘">
        <IconApp name="grid" class="nav-icon" />
        <span>仪表盘</span>
      </button>

      <button class="nav-item" :class="{ active: route.name === 'profiles' }" @click="navigate('profiles')" title="配置方案（账号、认证地址、匹配规则、登录方式）">
        <IconApp name="wifi" class="nav-icon" />
        <span>方案</span>
      </button>

      <!-- 任务：一级项本身不指向页面，而是这一组子项的展开/收起开关（整行可点，
           caret 是它的状态指示器而非第二个按钮）。此前一级项"点主体=跳浏览器任务、
           点 caret=展开"，看似两全，实际是"点了主体页面就跳走、想展开子项却得瞄准
           那个小箭头"——用户要的是文件夹语义：点一下，子项开合，页不动。
           子项各自指向页面，`/tasks` 的 redirect 与深链仍落在 `tasks-browser`。 -->
      <div class="nav-group" :class="{ 'nav-group--open': tasksExpanded }">
        <button
          class="nav-item nav-item--group"
          :class="{ active: onTasks }"
          :aria-expanded="tasksExpanded"
          aria-controls="nav-tasks-children"
          :title="tasksExpanded ? '任务（浏览器任务 / 直连任务 / 脚本 / 定时任务 / AI 生成）— 点击收起' : '任务（浏览器任务 / 直连任务 / 脚本 / 定时任务 / AI 生成）— 点击展开'"
          @click="toggleTasks"
        >
          <IconApp name="file-text" class="nav-icon" />
          <span>任务</span>
          <IconApp name="chevron-down" class="nav-caret" :class="{ 'nav-caret--collapsed': !tasksExpanded }" />
        </button>

        <div v-show="tasksExpanded" id="nav-tasks-children" class="nav-children">
          <button
            v-for="child in TASK_NAV_CHILDREN"
            :key="child.id"
            type="button"
            class="nav-child"
            :class="{ active: activeTaskChild === child.id }"
            :aria-current="activeTaskChild === child.id ? 'page' : undefined"
            :title="child.title"
            @click="navigate(child.name)"
          >
            {{ child.label }}
          </button>
        </div>
      </div>

      <button class="nav-item" :class="{ active: onSettings }" @click="navigate('settings')" title="设置（检测 / 浏览器 / 任务与环境 / 系统 / 网络与更新 / 外观）">
        <IconApp name="settings" class="nav-icon" />
        <span>设置</span>
      </button>

      <button class="nav-item" :class="{ active: route.name === 'about' }" @click="navigate('about')" title="关于">
        <IconApp name="info" class="nav-icon" />
        <span>关于</span>
      </button>
    </div>

    <div class="sidebar-footer">
      <div class="status-badge" :class="status.monitoring ? 'online' : 'offline'">
        <span class="status-dot"></span>
        {{ status.monitoring ? "运行中" : "已停止" }}
      </div>
      <button class="btn quit-btn" @click="quitApp" :disabled="busy.monitor" title="退出应用">
        <IconApp name="log-out" />
        退出
      </button>
    </div>
  </nav>
</template>
