<script setup lang="ts">
/** 定时任务页：cron 调度计划的增删改查与启停 */
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted } from "vue";
import { useScheduledTasks } from "@/composables/useScheduledTasks";
import { useScripts } from "@/composables/useScripts";
import { useTasks } from "@/composables/useTasks";
import ToggleSwitch from "@/components/common/ToggleSwitch.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import Modal from "@/components/common/Modal.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";
import { formatTimestamp } from "@/utils/formatters";

const st = useScheduledTasks();
const { scripts } = useScripts();
const { tasks: browserTasks } = useTasks();

onMounted(() => { void st.loadScheduledTasks(); });

// 类型仅用于切换目标下拉：保存不上传 task_type，后端从 target 推导
const scheduledTaskTypeOptions: SelectOption[] = [
  { value: "browser", label: "浏览器任务" },
  { value: "script", label: "自定义脚本" },
];

// 触发方式：定时执行（每日 HH:MM）/ 启动后执行（软件每次启动后自动执行）
const scheduledTaskTriggerOptions: SelectOption[] = [
  { value: "cron", label: "定时执行" },
  { value: "startup", label: "启动后执行" },
];

const scriptTargetOptions = computed<SelectOption[]>(() =>
  scripts.value.map((s) => ({ value: s.id, label: s.name })),
);

const browserTargetOptions = computed<SelectOption[]>(() =>
  browserTasks.value.map((t) => ({ value: t.id, label: t.name })),
);

/** 切换任务类型时清空目标：旧类型的 target_id 残留会被保存成另一类型的目标（或死引用） */
function onTaskTypeChange(value: string): void {
  if (st.scheduledTaskForm.value.task_type === value) return;
  st.scheduledTaskForm.value.task_type = value;
  st.scheduledTaskForm.value.target_id = "";
}

/** 触发方式收窄为合法枚举值（CustomSelect 发的是 string） */
function onTriggerChange(value: string): void {
  st.scheduledTaskForm.value.trigger = value === "startup" ? "startup" : "cron";
}

/** 保存前把当前类型下合法的目标 id 集合交给 composable 做死引用校验 */
function onSaveClick(): void {
  const options =
    st.scheduledTaskForm.value.task_type === "script"
      ? scriptTargetOptions.value
      : browserTargetOptions.value;
  void st.saveScheduledTask(options.map((o) => o.value));
}
</script>

<template>
  <div class="page-content scheduled-tasks-page">
    <!-- 任务列表 -->
    <div class="card">
      <div class="card-header">
        <h2>定时任务</h2>
        <button class="btn btn-sm btn-primary" @click="st.openCreateScheduledTask()">
          <IconApp name="plus" class="icon-sm" />
          新建定时任务
        </button>
      </div>
      <div class="card-body">
        <div v-if="!st.scheduledTasks.value.length" class="empty-state">
          <IconApp name="calendar" :stroke-width="1.5" />
          <span class="empty-title">暂无定时任务</span>
          <span class="empty-desc">定时或启动后自动执行脚本与浏览器任务</span>
          <div class="empty-actions">
            <button class="btn btn-sm btn-primary" type="button" @click="st.openCreateScheduledTask()">
              <IconApp name="plus" />新建定时任务
            </button>
          </div>
        </div>
        <div v-else class="task-list">
          <div v-for="task in st.scheduledTasks.value" :key="task.id" class="task-item hover-lift scheduled-task-item" :class="{ disabled: !task.enabled }">
            <div class="task-info">
              <h3>{{ task.name }}</h3>
              <p class="task-desc">
                <span class="scheduled-task-type" :class="'badge-' + task.task_type">{{ st.formatTaskType(task.task_type) }}</span>
                <span v-if="task.target_id"> · {{ task.target_id }}</span>
                <template v-if="task.trigger === 'startup'">
                  · 启动后执行<span v-if="typeof task.startup_runs_today === 'number'"> · 今日成功 {{ task.startup_runs_today }}/{{ task.max_runs_per_day || 1 }}</span>
                </template>
                <template v-else>
                  · 每天 {{ task.cron }}
                  <span v-if="task.schedule_invalid" class="text-danger" title="cron 表达式解析失败，该任务已启用但永远不会触发，请编辑修正"> · 表达式无效</span>
                </template>
                <span v-if="task.timeout"> · 超时 {{ task.timeout }}s</span>
                <span v-if="task.last_run">
                  · 上次: <span :class="task.last_result?.startsWith('[success]') ? 'text-success' : 'text-danger'">{{ task.last_result?.startsWith('[success]') ? '成功' : '失败' }}</span>
                </span>
              </p>
            </div>
            <div class="task-actions">
              <button class="btn btn-sm" @click="st.runScheduledTask(task.id)" :disabled="st.runningIds.has(task.id)" title="手动执行">
                {{ st.runningIds.has(task.id) ? '运行中...' : '运行' }}
              </button>
              <button class="btn btn-sm" @click="st.loadScheduledTaskHistory(task.id)" title="查看执行历史">查看历史</button>
              <button class="btn btn-sm" @click="st.openEditScheduledTask(task)" title="编辑">编辑</button>
              <button class="btn btn-sm btn-danger" @click="st.deleteScheduledTask(task.id)">删除</button>
            </div>
            <div class="task-toggle">
              <ToggleSwitch
                :model-value="task.enabled !== false"
                :disabled="st.togglingIds.has(task.id)"
                @update:model-value="st.toggleScheduledTask(task.id)"
              />
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 新建/编辑弹窗 -->
    <Modal :open="st.showScheduledTaskModal.value" :title="st.editingScheduledTask.value ? '编辑定时任务' : '新建定时任务'" @close="st.closeScheduledTaskModal()">
      <div class="form-section">
        <div class="form-section-title">基本信息</div>
        <div class="form-row form-row--flex">
          <div class="form-group flex-1">
            <label for="scheduled-task-name">任务名称</label>
            <input id="scheduled-task-name" v-model="st.scheduledTaskForm.value.name" type="text" placeholder="输入任务名称" />
          </div>
          <div class="form-group flex-1">
            <label for="scheduled-task-desc">描述</label>
            <input id="scheduled-task-desc" v-model="st.scheduledTaskForm.value.description" type="text" placeholder="可选" />
          </div>
        </div>
      </div>
      <div class="form-section">
        <div class="form-section-title">任务配置</div>
        <div class="form-row form-row--flex">
          <div class="form-group form-group--min140">
            <label for="scheduled-task-type">任务类型</label>
            <CustomSelect
              :model-value="st.scheduledTaskForm.value.task_type"
              :options="scheduledTaskTypeOptions"
              @update:model-value="onTaskTypeChange"
            />
          </div>
          <div class="form-group flex-1">
            <label for="scheduled-task-target">{{ st.scheduledTaskForm.value.task_type === 'script' ? '选择脚本' : '选择浏览器任务' }}</label>
            <CustomSelect v-if="st.scheduledTaskForm.value.task_type === 'script'" v-model="st.scheduledTaskForm.value.target_id" :options="scriptTargetOptions" />
            <CustomSelect v-else v-model="st.scheduledTaskForm.value.target_id" :options="browserTargetOptions" />
            <span class="hint">{{ st.scheduledTaskForm.value.task_type === 'script' ? '在「自定义脚本」页面创建和管理脚本' : '浏览器任务会自动打开网页并执行登录等自动化操作' }}</span>
          </div>
        </div>
      </div>
      <div class="form-section">
        <div class="form-section-title">执行设置</div>
        <div class="form-row form-row--flex">
          <div class="form-group">
            <label>触发方式</label>
            <CustomSelect
              :model-value="st.scheduledTaskForm.value.trigger"
              :options="scheduledTaskTriggerOptions"
              @update:model-value="onTriggerChange"
            />
          </div>
          <div class="form-group">
            <label for="scheduled-task-timeout">超时（秒）</label>
            <input id="scheduled-task-timeout" v-model.number="st.scheduledTaskForm.value.timeout" type="number" min="5" max="3600" />
          </div>
        </div>
        <!-- 定时执行：每日固定时间触发 -->
        <div v-if="st.scheduledTaskForm.value.trigger === 'cron'" class="form-row form-row--flex">
          <div class="form-group">
            <label for="scheduled-task-time">执行时间</label>
            <input id="scheduled-task-time" type="time"
              :value="st.formatScheduleTime(st.scheduledTaskForm.value.schedule)"
              @input="st.onTimeChange($event as InputEvent)" />
            <span v-if="st.originalCronInvalid.value" class="hint text-danger">
              原表达式「{{ st.originalCron.value }}」不是每日时间格式，保存后将按上方时间改为每日执行
            </span>
          </div>
        </div>
        <!-- 启动后执行：软件每次启动后自动执行，仅成功计入每日次数 -->
        <div v-else class="form-row form-row--flex">
          <div class="form-group">
            <label for="scheduled-task-max-runs">每天最多成功</label>
            <input id="scheduled-task-max-runs" v-model.number="st.scheduledTaskForm.value.max_runs_per_day" type="number" min="1" max="99" />
            <span class="hint">次 · 执行成功才计入，当日达到上限后自动跳过</span>
          </div>
          <div class="form-group">
            <label for="scheduled-task-retries">失败重试</label>
            <input id="scheduled-task-retries" v-model.number="st.scheduledTaskForm.value.max_retries" type="number" min="0" max="10" />
            <span class="hint">次 · 每次间隔 1 分钟</span>
          </div>
          <div class="form-group">
            <label for="scheduled-task-delay">延迟执行（秒）</label>
            <input id="scheduled-task-delay" v-model.number="st.scheduledTaskForm.value.startup_delay_secs" type="number" min="0" max="86400" />
            <span class="hint">启动后先等待，便于网络就绪</span>
          </div>
        </div>
      </div>
      <template #footer>
        <button class="btn btn-secondary" @click="st.closeScheduledTaskModal()">取消</button>
        <button class="btn btn-primary" @click="onSaveClick()" :disabled="st.scheduledTaskFormLoading.value">
          {{ st.scheduledTaskFormLoading.value ? '保存中...' : '保存' }}
        </button>
      </template>
    </Modal>

    <!-- 执行历史弹窗 -->
    <Modal :open="!!st.selectedScheduledTaskId.value" title="执行历史" size="lg" @close="st.closeScheduledTaskHistory()">
      <div v-if="st.scheduledTaskHistoryLoading.value" class="loading-state">
        <div class="spinner"></div><p>加载中...</p>
      </div>
      <div v-else-if="!st.scheduledTaskHistory.value.length" class="empty-state">
        <p>暂无执行记录</p>
      </div>
      <div v-else class="history-list">
        <div v-for="(record, index) in st.scheduledTaskHistory.value" :key="index" class="history-item" :class="record.success ? 'success' : 'failed'">
          <div class="history-header">
            <span class="history-status" :class="record.success ? 'success' : 'failed'">
              {{ record.success ? '成功' : '失败' }}
            </span>
            <span class="history-time">{{ formatTimestamp(record.run_at) }}</span>
            <span v-if="record.duration != null" class="history-duration">{{ record.duration }}s</span>
          </div>
          <div class="history-message">{{ record.message }}</div>
        </div>
      </div>
      <template #footer>
        <button class="btn btn-secondary" @click="st.closeScheduledTaskHistory()">关闭</button>
      </template>
    </Modal>
  </div>
</template>
