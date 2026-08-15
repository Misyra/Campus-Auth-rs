<script setup lang="ts">
/**
 * 调试面板组件。
 * 用于单步/全量调试浏览器任务，展示步骤执行结果与截图预览。
 *
 * 使用 useDebug composable 管理会话状态，通过 Modal 弹出。
 */

import { computed } from "vue";
import { useDebug } from "@/composables/useDebug";
import Modal from "./common/Modal.vue";

const { session, loading, visible, nextStep, runAll, stopDebug, getStepStatus, getStepResult } =
  useDebug();

/** 当前步骤索引 */
const currentStep = computed(() => session.current_step);

/** 总步骤数 */
const totalSteps = computed(() => session.total_steps);

/** 是否已执行完毕 */
const isDone = computed(() => !session.running && session.steps.length > 0 && session.current_step >= session.total_steps);

/** 状态图标映射 */
function statusIcon(status: string): string {
  switch (status) {
    case "success":
      return "step-check";
    case "failed":
      return "step-cross";
    case "running":
      return "step-running";
    case "current":
      return "step-arrow";
    default:
      return "step-dot";
  }
}

/** 状态符号 */
function statusSymbol(status: string): string {
  switch (status) {
    case "success":
      return "✓";
    case "failed":
      return "✗";
    case "running":
      return "◌";
    case "current":
      return "▶";
    default:
      return "○";
  }
}

/** 关闭面板并停止调试 */
function handleClose(): void {
  stopDebug();
}
</script>

<template>
  <Modal :open="visible" title="任务调试" size="lg" @close="handleClose">
    <div class="debug-panel-content">
      <!-- 头部信息 -->
      <div class="debug-info-bar">
        <span v-if="session.task_id" class="debug-task-id">任务: {{ session.task_id }}</span>
        <span class="debug-step-counter">{{ currentStep }} / {{ totalSteps }}</span>
      </div>

      <div class="debug-body">
        <!-- 左侧：步骤列表 -->
        <div class="debug-steps">
          <div
            v-for="(step, i) in session.steps"
            :key="i"
            class="debug-step-item"
            :class="getStepStatus(i)"
          >
            <span class="debug-step-indicator" :class="statusIcon(getStepStatus(i))">
              {{ statusSymbol(getStepStatus(i)) }}
            </span>
            <div class="debug-step-info">
              <span class="debug-step-badge">{{ step.type || "?" }}</span>
              <span class="debug-step-desc">{{ step.description || `步骤 ${i + 1}` }}</span>
              <span
                v-if="getStepResult(i)?.message"
                class="debug-step-msg"
                :class="getStepResult(i)?.running ? 'msg-running' : getStepResult(i)?.success ? 'msg-ok' : 'msg-fail'"
              >
                {{ getStepResult(i)?.message }}
              </span>
            </div>
          </div>

          <div v-if="!session.steps.length && !loading" class="empty-state">
            <span>无步骤数据</span>
          </div>
        </div>

        <!-- 右侧：截图预览 -->
        <div class="debug-screenshot-container">
          <img
            v-if="session.screenshot_url"
            :src="session.screenshot_url"
            alt="截图预览"
            class="debug-screenshot"
          />
          <span v-else class="debug-screenshot-placeholder">
            {{ loading ? "执行中..." : "暂无截图" }}
          </span>
        </div>
      </div>
    </div>

    <template #footer>
      <button class="btn btn-secondary" :disabled="loading || isDone" @click="nextStep">
        {{ loading ? "执行中..." : "单步执行" }}
      </button>
      <button class="btn btn-secondary" :disabled="loading || isDone" @click="runAll">
        {{ loading ? "执行中..." : "执行全部" }}
      </button>
      <button class="btn btn-danger" @click="handleClose">停止调试</button>
    </template>
  </Modal>
</template>
