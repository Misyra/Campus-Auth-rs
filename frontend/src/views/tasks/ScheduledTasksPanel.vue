<script setup lang="ts">
/** 定时任务面板：**列表页 + 二级编辑页**（与浏览器任务 / 直连任务 / 脚本同构）。
 *
 * 列表态是整页表格（名称 / 类型 / 触发 / 目标 / 超时 / 最近结果 / 启用 / 操作），
 * 点行或「新建定时任务」进入编辑态；编辑态独占全宽、面包屑返回，两态由 `?task=<id>`
 * 表达（刷新与深链直达编辑态、返回即列表）。字段变更 debounce 静默落盘，页头状态字
 * 说明保存进展，缺口未补齐时改口并拦住落盘（见 `utils/scheduledDraft`）。
 *
 * 此前是「表格 + 弹窗」：同一个任务页里另外三个子页都是二级编辑页，定时任务却要开弹窗、
 * 填完点保存——同一个页面的四套交互，用户得记住自己现在在哪一套里。
 *
 * 版式走 `styles/pages/tasks.css` 的 `tsk-*` 共享组，本页只写自己的列宽与单元格结构。
 */
import IconApp from "@/components/common/IconApp.vue";
import FieldHelp from "@/components/common/FieldHelp.vue";
import ToggleSwitch from "@/components/common/ToggleSwitch.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import Modal from "@/components/common/Modal.vue";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useScheduledTasks } from "@/composables/useScheduledTasks";
import { useTaskDirectory } from "@/composables/useTaskDirectory";
import { useTaskEditorQuery } from "@/composables/useTaskEditorQuery";
import { autosaveLabel } from "@/utils/autosave";
import { switchDraftTargetKind, switchDraftTrigger, scheduledTriggerLabel } from "@/utils/scheduledDraft";
import { formatMtime, formatScheduleTime, formatTimestamp } from "@/utils/formatters";
import type { ScheduledTask } from "@/api/types";

const st = useScheduledTasks();
// 目标候选池与「目标是否已不存在」都从任务目录取（与另外三个面板同一份混合列表）
const { browserTasks, scripts } = useTaskDirectory();

// ===== 两态：编辑态 = 草稿非空，`?task=<id>` 只是意图 =====
const currentId = computed(() => st.scheduledTaskDraft.value?.id ?? "");
const draft = computed(() => st.scheduledTaskDraft.value);
const isEditing = computed(() => !!draft.value);

const { resolve, openIfNeeded, syncQuery, clearQuery } = useTaskEditorQuery({
  exists: (id) => st.scheduledTasks.value.some((t) => t.id === id),
  open: (id) => st.showScheduledTaskEditor(id),
  currentId: () => currentId.value,
  // 定时任务有自己的列表（GET /api/scheduler/jobs）：未拉完时不能判定"任务不存在"
  ready: () => st.scheduledLoaded.value,
});

function openEditor(taskId: string): void {
  syncQuery(taskId);
  openIfNeeded(taskId);
}

/** 返回列表：先撤 query（浏览器返回即列表），再关编辑器（在途改动补一发保存） */
async function closeEditor(): Promise<void> {
  clearQuery();
  await st.closeScheduledTaskEditor();
}

/** 新建：草稿不落盘，名称与目标补齐后由第一次自动保存创建（中途放弃不留垃圾任务） */
function onNewScheduledTask(): void {
  clearQuery();
  st.createScheduledDraft();
}

/**
 * 新建草稿第一次落盘后补上 `?task=<id>`：落盘前地址栏不能指向它（磁盘上还没有，
 * 刷新会得到「找不到任务」），落盘后它就该和"点行进入"一样可刷新可分享。
 */
watch(st.isNewScheduledDraft, (isNew) => {
  const id = currentId.value;
  if (!isNew && id) syncQuery(id);
});

// ===== 编辑态派生 =====

/** 自动保存状态字（缺口优先，见 utils/autosave；新建未落盘时也不能说"改动自动保存"） */
const autosaveText = computed(() =>
  autosaveLabel(st.autosaveState.value, st.draftGapsNow.value, st.isNewScheduledDraft.value),
);

/** 未落盘的草稿不可运行 / 不可查看历史（磁盘上还没有这个任务） */
const canRun = computed(() => !st.isNewScheduledDraft.value && !!currentId.value);

/** 列表里的这条任务（编辑页侧栏要读它的「今日成功次数」等后端回填字段） */
const currentTask = computed(() => st.scheduledTasks.value.find((t) => t.id === currentId.value) ?? null);

// 类型仅用于切换目标下拉：保存不上传 task_type，后端从 target 推导
const taskTypeOptions: SelectOption[] = [
  { value: "browser", label: "浏览器任务" },
  { value: "script", label: "自定义脚本" },
];

// 触发方式：定时执行（每日 HH:MM）/ 启动后执行（软件每次启动后自动执行）
const triggerOptions: SelectOption[] = [
  { value: "cron", label: "定时执行" },
  { value: "startup", label: "启动后执行" },
];

const scriptTargetOptions = computed<SelectOption[]>(() =>
  scripts.value.map((s) => ({ value: s.id, label: s.name })),
);

const browserTargetOptions = computed<SelectOption[]>(() =>
  browserTasks.value.map((t) => ({ value: t.id, label: t.name })),
);

/** 切换任务类型时清空目标（见 switchDraftTargetKind） */
function onTaskTypeChange(value: string): void {
  const d = st.scheduledTaskDraft.value;
  if (d) switchDraftTargetKind(d, value === "script" ? "script" : "browser");
}

/** 触发方式收窄为合法枚举值（CustomSelect 发的是 string） */
function onTriggerChange(value: string): void {
  const d = st.scheduledTaskDraft.value;
  if (d) switchDraftTrigger(d, value);
}

/** 原生 time 控件的值 → 草稿里的 {hour, minute} */
function onTimeChange(event: Event): void {
  const value = (event.target as HTMLInputElement).value;
  const d = st.scheduledTaskDraft.value;
  if (!value || !d) return;
  const [hour, minute] = value.split(":").map(Number);
  d.schedule.hour = hour;
  d.schedule.minute = minute;
}

/**
 * 下次执行提示（编辑页侧栏）。
 *
 * 后端不暴露"下次执行时间"，而每日表达式（`分 时 * * *`）本身就是确定的，故本地按
 * 当前时刻推算一次——用户改了时间立刻能看到"是今天还是明天"。
 */
const nextRunHint = computed(() => {
  const d = draft.value;
  if (!d) return "";
  if (d.trigger === "startup") {
    const today = currentTask.value?.startup_runs_today ?? 0;
    const cap = d.max_runs_per_day || 1;
    return `每次程序启动后延迟 ${d.startup_delay_secs} 秒执行；今日已成功 ${today}/${cap} 次`;
  }
  const now = new Date();
  const minutesNow = now.getHours() * 60 + now.getMinutes();
  const minutesTarget = d.schedule.hour * 60 + d.schedule.minute;
  const hhmm = formatScheduleTime(d.schedule);
  return minutesTarget > minutesNow ? `今天 ${hhmm}` : `明天 ${hhmm}`;
});

// ===== 列表派生 =====

/** 目标任务显示名：优先任务名（列表里记的是 id，直接显示 id 要用户自己去别的页面对照） */
function targetLabel(task: ScheduledTask): string {
  const pool = task.task_type === "script" ? scripts.value : browserTasks.value;
  return pool.find((t) => t.id === task.target_id)?.name || task.target_id || "（未选择）";
}

/**
 * 目标是否已不存在（任务被删/改了 id）。
 *
 * 后端只在保存时拦死引用，列表里得自己认：否则这一行看起来一切正常，直到触发时
 * 才失败，用户还得去日志里找原因。
 */
function targetMissing(task: ScheduledTask): boolean {
  if (!task.target_id) return true;
  const pool = task.task_type === "script" ? scripts.value : browserTasks.value;
  return !pool.some((t) => t.id === task.target_id);
}

/** 最近结果徽标：成功 / 失败 / 尚未执行 */
function lastRunBadge(task: ScheduledTask): { cls: string; text: string } {
  if (!task.last_run) return { cls: "badge badge--sm", text: "尚未执行" };
  const ok = task.last_result?.startsWith("[success]");
  return ok
    ? { cls: "badge badge--sm badge--success", text: "成功" }
    : { cls: "badge badge--sm badge--warn", text: "失败" };
}

// ===== 行尾 ⋯ 菜单（与任务页三个面板同口径：同一时刻只开一个） =====
const openMenuId = ref("");

function toggleRowMenu(taskId: string): void {
  openMenuId.value = openMenuId.value === taskId ? "" : taskId;
}

function closeRowMenu(): void {
  openMenuId.value = "";
}

function runRowAction(action: () => unknown): void {
  closeRowMenu();
  void action();
}

function onDocumentPointerDown(): void {
  closeRowMenu();
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === "Escape") closeRowMenu();
}

watch(openMenuId, (open) => {
  if (open) {
    document.addEventListener("pointerdown", onDocumentPointerDown);
    document.addEventListener("keydown", onDocumentKeydown);
  } else {
    document.removeEventListener("pointerdown", onDocumentPointerDown);
    document.removeEventListener("keydown", onDocumentKeydown);
  }
});

// 切走时菜单还开着的话，watch 的清理分支不会执行（pre-flush watcher 在卸载时不再跑），
// 必须在 onBeforeUnmount 里直接摘掉 document 监听
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocumentPointerDown);
  document.removeEventListener("keydown", onDocumentKeydown);
  closeRowMenu();
});

const NOTICE_HELP =
  "定时任务按计划执行「浏览器任务」或「脚本」：定时执行是每天固定时间，启动后执行是程序每次启动后自动跑一次。\n\n" +
  "直连任务不在这里调度——它的验证入口是任务编辑器里的「发送测试请求」。";

onMounted(async () => {
  await st.loadScheduledTasks();
  // 列表就绪后再消费 ?task=<id>：未就绪时不判定"任务不存在"（见 useTaskEditorQuery）
  resolve();
});
</script>

<template>
  <!-- ==================== 列表态 ==================== -->
  <div v-if="!isEditing" class="tsk-list-page scheduled-tasks-page">
    <div class="tsk-toolbar">
      <h2 class="tsk-title">
        定时任务
        <FieldHelp :text="NOTICE_HELP" wide />
      </h2>
      <button type="button" class="btn btn-sm btn-primary" @click="onNewScheduledTask">
        <IconApp name="plus" class="icon-sm" />
        新建定时任务
      </button>
    </div>

    <div class="card tsk-table-card">
      <table class="tsk-table">
        <colgroup>
          <col class="sch-col-name" />
          <col class="sch-col-type" />
          <col class="sch-col-trigger" />
          <col class="sch-col-target" />
          <col class="sch-col-timeout" />
          <col class="sch-col-result" />
          <col class="sch-col-enabled" />
          <col class="tsk-col-actions" />
        </colgroup>
        <thead>
          <tr>
            <th>名称</th>
            <th class="sch-cell-type">类型</th>
            <th class="sch-cell-trigger">触发</th>
            <th class="sch-cell-target">目标</th>
            <th class="sch-cell-timeout">超时</th>
            <th class="sch-cell-result">最近结果</th>
            <th class="sch-cell-enabled">启用</th>
            <th class="tsk-actions">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!st.scheduledTasks.value.length">
            <td colspan="8">
              <div class="empty-state">
                <IconApp name="calendar" :stroke-width="1.5" />
                <span>暂无定时任务</span>
                <span class="empty-desc">让「浏览器任务」或「脚本」在每天固定时间、或程序启动后自动跑一次</span>
                <div class="empty-actions">
                  <button type="button" class="btn btn-sm btn-primary" @click="onNewScheduledTask">
                    <IconApp name="plus" />新建定时任务
                  </button>
                </div>
              </div>
            </td>
          </tr>
          <tr
            v-for="task in st.scheduledTasks.value"
            :key="task.id"
            :class="{ 'is-off': !task.enabled }"
            @click="openEditor(task.id)"
          >
            <td>
              <span class="tsk-name">{{ task.name || task.id }}</span>
              <span v-if="task.description" class="tsk-name-sub">{{ task.description }}</span>
            </td>
            <td class="sch-cell-type">
              <span class="badge badge--sm" :class="'badge-' + task.task_type">{{ st.formatTaskType(task.task_type) }}</span>
            </td>
            <td class="sch-cell-trigger">
              <span class="sch-main sch-mono">
                {{ scheduledTriggerLabel(task) }}
              </span>
              <span v-if="task.schedule_invalid" class="sch-sub text-danger" title="cron 表达式解析失败，该任务已启用但永远不会触发，请编辑修正">表达式无效</span>
              <span v-else-if="task.trigger === 'startup'" class="sch-sub">
                今日成功 {{ task.startup_runs_today ?? 0 }}/{{ task.max_runs_per_day || 1 }}
              </span>
            </td>
            <td class="sch-cell-target" :title="task.target_id">
              <span class="sch-main">{{ targetLabel(task) }}</span>
              <span v-if="targetMissing(task)" class="sch-sub text-danger">目标已不存在</span>
            </td>
            <td class="sch-cell-timeout">
              <span class="sch-mono-muted">{{ task.timeout ? task.timeout + 's' : '—' }}</span>
            </td>
            <td class="sch-cell-result">
              <span :class="lastRunBadge(task).cls">{{ lastRunBadge(task).text }}</span>
              <span class="sch-sub">{{ task.last_run ? formatMtime(task.last_run) : '' }}</span>
            </td>
            <td class="sch-cell-enabled" @click.stop>
              <ToggleSwitch
                :model-value="task.enabled !== false"
                :disabled="st.togglingIds.has(task.id)"
                @update:model-value="st.toggleScheduledTask(task.id)"
              />
            </td>
            <td class="tsk-actions" @click.stop>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="st.runningIds.has(task.id) ? '运行中…' : `立即运行：${task.name || task.id}`"
                :disabled="st.runningIds.has(task.id)"
                @click="st.runScheduledTask(task.id)"
              >
                <IconApp :name="st.runningIds.has(task.id) ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: st.runningIds.has(task.id) }" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="`编辑：${task.name || task.id}`"
                @click="openEditor(task.id)"
              >
                <IconApp name="pencil" class="icon-sm" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon-only"
                :title="`更多操作：${task.name || task.id}`"
                aria-haspopup="menu"
                :aria-expanded="openMenuId === task.id"
                @pointerdown.stop
                @click.stop="toggleRowMenu(task.id)"
              >
                <IconApp name="more-vertical" class="icon-sm" />
              </button>
              <div v-if="openMenuId === task.id" class="tsk-menu" role="menu" @pointerdown.stop>
                <button type="button" role="menuitem" :disabled="st.runningIds.has(task.id)" @click="runRowAction(() => st.runScheduledTask(task.id))">
                  {{ st.runningIds.has(task.id) ? '运行中…' : '立即运行' }}
                </button>
                <button type="button" role="menuitem" @click="runRowAction(() => st.loadScheduledTaskHistory(task.id))">查看历史</button>
                <button type="button" role="menuitem" @click="runRowAction(() => openEditor(task.id))">编辑</button>
                <button type="button" role="menuitem" class="tsk-menu-danger" @click="runRowAction(() => st.deleteScheduledTask(task.id))">删除</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-if="st.scheduledTasks.value.length" class="tsk-foot">
        <span>共 {{ st.scheduledTasks.value.length }} 个定时任务</span>
        <span class="tsk-spacer"></span>
        <span>点行进入编辑 · 改动自动保存 · 开关控制启用</span>
      </div>
    </div>
  </div>

  <!-- ==================== 编辑态（二级页） ==================== -->
  <div v-else-if="draft" class="tsk-editor-page">
    <div class="tsk-crumb">
      <button type="button" class="tsk-crumb-back" @click="closeEditor">
        <IconApp name="chevron-down" class="icon-sm tsk-crumb-icon" />
        返回定时任务
      </button>
      <span class="tsk-crumb-sep">/</span>
      <span class="tsk-crumb-here">{{ draft.name || '新建定时任务' }}</span>
    </div>

    <div class="tsk-editor-head">
      <h2 class="tsk-editor-title">{{ draft.name || '新建定时任务' }}</h2>
      <span :class="autosaveText.cls">{{ autosaveText.text }}</span>
      <span class="tsk-spacer"></span>
      <button
        v-if="canRun"
        type="button"
        class="btn btn-sm"
        :disabled="st.runningIds.has(currentId)"
        title="立即运行一次这个定时任务"
        @click="st.runScheduledTask(currentId)"
      >
        <IconApp :name="st.runningIds.has(currentId) ? 'refresh' : 'play'" class="icon-sm" :class="{ spin: st.runningIds.has(currentId) }" />
        {{ st.runningIds.has(currentId) ? '运行中…' : '运行' }}
      </button>
      <button v-if="canRun" type="button" class="btn btn-sm" title="查看这个定时任务的执行历史" @click="st.loadScheduledTaskHistory(currentId)">
        <IconApp name="clock" class="icon-sm" />
        执行历史
      </button>
      <button type="button" class="btn btn-sm btn-danger" :title="canRun ? '删除定时任务' : '放弃这个还没保存的新建任务'" @click="st.deleteScheduledTask(currentId)">
        <IconApp name="trash" class="icon-sm" />
        {{ canRun ? '删除' : '放弃' }}
      </button>
    </div>

    <!-- 缺口提示：自动保存被缺口拦住时状态字已经改口，这里把"缺什么"说全 -->
    <div v-if="st.draftGapsNow.value.length" class="tsk-gaps">
      <IconApp name="alert-triangle" class="icon-sm" />
      <span>
        还缺 {{ st.draftGapsNow.value.join('、') }}，补齐前改动不会保存
        <template v-if="st.isNewScheduledDraft.value">（新建的定时任务补齐后才会创建）</template>
      </span>
    </div>

    <div class="tsk-grid">
      <!-- 主列 -->
      <div class="tsk-main">
        <div class="card">
          <div class="card-header"><h3>基本信息</h3></div>
          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <label for="scheduled-task-name">任务名称</label>
                <input id="scheduled-task-name" v-model="draft.name" type="text" placeholder="输入任务名称" />
              </div>
              <div class="form-group">
                <label for="scheduled-task-desc">描述</label>
                <input id="scheduled-task-desc" v-model="draft.description" type="text" placeholder="可选" />
              </div>
            </div>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h3>任务配置</h3></div>
          <div class="card-body">
            <div class="form-row">
              <div class="form-group">
                <label for="scheduled-task-type">任务类型</label>
                <CustomSelect
                  :model-value="draft.task_type"
                  :options="taskTypeOptions"
                  @update:model-value="onTaskTypeChange"
                />
              </div>
              <div class="form-group">
                <label for="scheduled-task-target">{{ draft.task_type === 'script' ? '选择脚本' : '选择浏览器任务' }}</label>
                <CustomSelect v-if="draft.task_type === 'script'" v-model="draft.target_id" :options="scriptTargetOptions" />
                <CustomSelect v-else v-model="draft.target_id" :options="browserTargetOptions" />
                <span class="hint">{{ draft.task_type === 'script' ? '在「任务 → 脚本」页创建和管理脚本' : '浏览器任务会自动打开网页并执行登录等自动化操作' }}</span>
              </div>
            </div>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h3>执行设置</h3></div>
          <div class="card-body">
            <!-- 触发方式与它这一行的参数并列：定时执行多一个「执行时间」夹在中间，
                 启动后执行没有它（三个并列短字段另起一行）。 -->
            <div class="form-row form-row--flex">
              <div class="form-group flex-1">
                <label>触发方式</label>
                <CustomSelect
                  :model-value="draft.trigger"
                  :options="triggerOptions"
                  @update:model-value="onTriggerChange"
                />
              </div>
              <!-- 定时执行：每日固定时间触发 -->
              <div v-if="draft.trigger === 'cron'" class="form-group flex-1">
                <label for="scheduled-task-time">执行时间</label>
                <input id="scheduled-task-time" class="sch-time-input" type="time"
                  :value="formatScheduleTime(draft.schedule)"
                  @input="onTimeChange($event as InputEvent)" />
                <span class="hint">每天到这个时间执行一次</span>
                <!-- 非每日表达式（*/5 之类）在表单里表达不了：说清保存会改写调度语义 -->
                <span v-if="draft._originalCronInvalid" class="hint text-danger">
                  原表达式「{{ draft._originalCron }}」不是每日时间格式。不动上面的时间就保持原样；
                  一旦改了「执行时间」，保存后会按上方时间改为每日执行
                </span>
              </div>
              <div class="form-group flex-1">
                <label for="scheduled-task-timeout">超时（秒）</label>
                <input id="scheduled-task-timeout" v-model.number="draft.timeout" type="number" min="5" max="3600" />
              </div>
            </div>
            <!-- 启动后执行：软件每次启动后自动执行，仅成功计入每日次数 -->
            <div v-if="draft.trigger !== 'cron'" class="form-row form-row--flex">
              <div class="form-group flex-1">
                <label for="scheduled-task-max-runs">每天最多成功</label>
                <input id="scheduled-task-max-runs" v-model.number="draft.max_runs_per_day" type="number" min="1" max="99" />
                <span class="hint">次 · 执行成功才计入，当日达到上限后自动跳过</span>
              </div>
              <div class="form-group flex-1">
                <label for="scheduled-task-retries">失败重试</label>
                <input id="scheduled-task-retries" v-model.number="draft.max_retries" type="number" min="0" max="10" />
                <span class="hint">次 · 每次间隔 1 分钟</span>
              </div>
              <div class="form-group flex-1">
                <label for="scheduled-task-delay">延迟执行（秒）</label>
                <input id="scheduled-task-delay" v-model.number="draft.startup_delay_secs" type="number" min="0" max="86400" />
                <span class="hint">启动后先等待，便于网络就绪</span>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 侧栏 -->
      <div class="tsk-side">
        <div class="card">
          <div class="card-header"><h3>执行与状态</h3></div>
          <div class="card-body tsk-side-body">
            <!-- 启停开关与列表里那一列改的是同一个字段（走自动保存），文案说明它对"按计划执行"的作用 -->
            <ToggleSwitch
              v-model="draft.enabled"
              label="启用"
              :description="draft.enabled ? '已启用，按计划执行' : '已停用，不会按计划执行（立即运行不受影响）'"
            />
            <dl class="tsk-kv">
              <dt>触发</dt>
              <dd><span class="tsk-side-hint">{{ draft.trigger === 'startup' ? '程序每次启动后执行' : `每天 ${formatScheduleTime(draft.schedule)}` }}</span></dd>
              <dt>{{ draft.trigger === 'startup' ? '今日成功' : '下次执行' }}</dt>
              <dd><span class="tsk-side-hint">{{ nextRunHint }}</span></dd>
              <dt>超时</dt>
              <dd><span class="tsk-side-hint">{{ draft.timeout }} 秒 · 超过即判定失败</span></dd>
              <dt>任务 ID</dt>
              <dd><span class="tsk-side-hint sch-mono">{{ draft.id }}</span></dd>
              <dt v-if="canRun">最近结果</dt>
              <dd v-if="canRun">
                <span class="tsk-side-hint">
                  {{ currentTask?.last_run ? `${currentTask.last_result?.startsWith('[success]') ? '成功' : '失败'} · ${formatMtime(currentTask.last_run)}` : '尚未执行' }}
                </span>
              </dd>
            </dl>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h3>快速上手</h3></div>
          <div class="card-body tsk-side-body">
            <p class="tsk-side-hint">定时任务只调度<strong>「浏览器任务」或「脚本」</strong>，它本身不含登录参数。</p>
            <p class="tsk-side-hint">直连任务不在这里调度——它的验证入口是任务编辑器里的「发送测试请求」。</p>
            <p class="tsk-side-hint">改动自动保存；名称与目标没补齐前不会落盘，新建任务因此不会在磁盘上留下半成品。</p>
            <p class="tsk-side-hint">立即运行只是「排入执行」，成败与耗时见「执行历史」。</p>
          </div>
        </div>
      </div>
    </div>
  </div>

  <!-- 执行历史弹窗：条目结构与「仪表盘」的登录历史同构（状态图标 + 信息列），
       直接复用 dashboard.css 的 `.history-*` 词汇，本页不另起一套。 -->
  <Modal :open="!!st.selectedScheduledTaskId.value" title="执行历史" size="lg" @close="st.closeScheduledTaskHistory()">
    <div v-if="st.scheduledTaskHistoryLoading.value" class="loading-state">
      <div class="spinner"></div><p>加载中...</p>
    </div>
    <div v-else-if="!st.scheduledTaskHistory.value.length" class="empty-state">
      <p>暂无执行记录</p>
    </div>
    <div v-else class="history-list">
      <div
        v-for="(record, index) in st.scheduledTaskHistory.value"
        :key="index"
        class="history-item"
        :class="record.success ? 'success' : 'failed'"
      >
        <div class="history-status">
          <IconApp :name="record.success ? 'check-circle' : 'x-circle'" />
        </div>
        <div class="history-info">
          <div class="history-row">
            <span class="history-time">{{ formatTimestamp(record.run_at) }}</span>
            <span v-if="record.duration != null" class="history-duration">{{ record.duration }}s</span>
          </div>
          <div class="history-row">
            <span :class="record.success ? 'history-profile' : 'history-error'" :title="record.message">{{ record.message }}</span>
          </div>
        </div>
      </div>
    </div>
    <template #footer>
      <button class="btn btn-secondary" @click="st.closeScheduledTaskHistory()">关闭</button>
    </template>
  </Modal>
</template>
