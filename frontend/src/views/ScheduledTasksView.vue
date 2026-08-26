<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { computed, onMounted } from "vue";
import { useScheduledTasks } from "@/composables/useScheduledTasks";
import { useScripts } from "@/composables/useScripts";
import { useTasks } from "@/composables/useTasks";
import ToggleSwitch from "@/components/common/ToggleSwitch.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import Modal from "@/components/common/Modal.vue";
import type { SelectOption } from "@/components/common/CustomSelect.vue";

const st = useScheduledTasks();
const { scripts } = useScripts();
const { tasks: browserTasks } = useTasks();

onMounted(() => { void st.loadScheduledTasks(); });

const scheduledTaskTypeOptions: SelectOption[] = [
  { value: "browser", label: "浏览器任务" },
  { value: "script", label: "自定义脚本" },
  { value: "shell", label: "Shell 命令" },
];

const scriptTargetOptions = computed<SelectOption[]>(() =>
  scripts.value.map((s) => ({ value: s.id, label: s.name })),
);

const browserTargetOptions = computed<SelectOption[]>(() =>
  browserTasks.value.map((t) => ({ value: t.id, label: t.name })),
);
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
          <p>暂无定时任务</p>
          <p class="hint">定时任务会在指定时间自动执行脚本或浏览器任务</p>
        </div>
        <div v-else class="task-list">
          <div v-for="task in st.scheduledTasks.value" :key="task.id" class="task-item hover-lift scheduled-task-item" :class="{ disabled: !task.enabled }">
            <div class="task-info">
              <h3>{{ task.name }}</h3>
              <p class="task-desc">
                <span class="scheduled-task-type" :class="'badge-' + task.task_type">{{ st.formatTaskType(task.task_type) }}</span>
                <span v-if="task.target_id"> · {{ task.target_id }}</span>
                · 每天 {{ task.cron }}
                <span v-if="task.schedule_invalid" class="text-danger" title="cron 表达式解析失败，该任务已启用但永远不会触发，请编辑修正"> · 表达式无效</span>
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
        <div class="form-row">
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
        <div class="form-row">
          <div class="form-group" style="min-width:140px">
            <label for="scheduled-task-type">任务类型</label>
            <CustomSelect v-model="st.scheduledTaskForm.value.task_type" :options="scheduledTaskTypeOptions" />
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
        <div class="form-row">
          <div class="form-group">
            <label for="scheduled-task-time">执行时间</label>
            <input id="scheduled-task-time" type="time"
              :value="st.formatScheduleTime(st.scheduledTaskForm.value.schedule)"
              @input="st.onTimeChange($event as InputEvent)" />
          </div>
          <div class="form-group">
            <label for="scheduled-task-timeout">超时（秒）</label>
            <input id="scheduled-task-timeout" v-model.number="st.scheduledTaskForm.value.timeout" type="number" min="5" max="3600" />
          </div>
        </div>
      </div>
      <template #footer>
        <button class="btn btn-secondary" @click="st.closeScheduledTaskModal()">取消</button>
        <button class="btn btn-primary" @click="st.saveScheduledTask()" :disabled="st.scheduledTaskFormLoading.value">
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
            <span class="history-time">{{ record.run_at.replace('T', ' ').substring(0, 19) }}</span>
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
