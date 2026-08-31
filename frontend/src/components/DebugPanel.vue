<script setup lang="ts">
/**
 * 调试面板组件。
 * 用于单步/全量调试浏览器任务，展示步骤执行结果与截图预览。
 *
 * 使用 useDebug composable 管理会话状态，通过 Modal 弹出。
 */

import { computed, ref } from "vue";
import { useDebug } from "@/composables/useDebug";
import { debugApi } from "@/api";
import { downloadBlob } from "@/utils/file";
import { extractApiError } from "@/api/client";
import { useToast } from "@/composables/useToast";
import Modal from "./common/Modal.vue";

const { session, loading, visible, nextStep, runAll, stopDebug, getStepStatus, getStepResult, clearScreenshot } =
  useDebug();
const downloading = ref(false);

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

/** 导出问题报告：日志 + 活动任务 + 当前页完整 MHTML/截图 */
async function handleFeedback(): Promise<void> {
  if (downloading.value) return;
  downloading.value = true;
  const { toastOnly } = useToast();
  try {
    const blob = await debugApi.feedbackBundle();
    const stamp = new Date().toISOString().slice(0, 19).replace(/[-:T]/g, "");
    downloadBlob(blob, `campus-auth-feedback-${stamp}.zip`, "application/zip");
    toastOnly(true, "问题报告已导出");
  } catch (e) {
    toastOnly(false, extractApiError(e as Error, "导出问题报告失败"));
  } finally {
    downloading.value = false;
  }
}
</script>

<template>
  <!-- 关闭通道收紧：点击空白/ESC 无反应；右上角 X 与右下角"停止调试"
       按钮同语义（都执行 stopDebug），保持两条显式关闭路径 -->
  <Modal
    :open="visible"
    title="任务调试"
    size="lg"
    :close-on-overlay="false"
    :close-on-esc="false"
    @close="handleClose"
  >
    <div class="debug-panel-content">
      <!-- 头部信息：任务 + 状态 + 进度 -->
      <div class="debug-info-bar">
        <span v-if="session.task_id" class="debug-task-id" :title="session.task_id">
          {{ session.task_id }}
        </span>
        <span v-else class="debug-task-id debug-task-unknown">调试会话</span>
        <div class="debug-info-right">
          <span class="debug-status-pill" :class="isDone ? 'done' : 'active'">
            <span class="debug-status-dot"></span>
            {{ isDone ? "已完成" : loading ? "执行中" : "进行中" }}
          </span>
          <span class="debug-step-counter">{{ currentStep }} / {{ totalSteps }}</span>
        </div>
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
            <span class="debug-step-index">{{ i + 1 }}</span>
            <span class="debug-step-indicator" :class="statusIcon(getStepStatus(i))">
              {{ statusSymbol(getStepStatus(i)) }}
            </span>
            <div class="debug-step-info">
              <div class="debug-step-line">
                <span class="debug-step-badge">{{ step.type || "?" }}</span>
                <span class="debug-step-desc">{{ step.description || `步骤 ${i + 1}` }}</span>
              </div>
              <span
                v-if="getStepResult(i)?.message"
                class="debug-step-msg"
                :class="getStepResult(i)?.running ? 'msg-running' : getStepResult(i)?.success ? 'msg-ok' : 'msg-fail'"
                :title="getStepResult(i)?.message"
              >
                {{ getStepResult(i)?.message }}
              </span>
            </div>
          </div>

          <div v-if="!session.steps.length" class="debug-empty">
            <span class="debug-empty-icon">◻</span>
            <span>{{ loading ? "正在获取会话数据..." : session.running ? "会话详情恢复中，当前执行结束后自动补全" : "该任务没有可执行的步骤" }}</span>
          </div>
        </div>

        <!-- 右侧：截图预览 -->
        <div class="debug-screenshot-container">
          <div class="debug-screenshot-head">
            <span>实时截图</span>
            <span class="debug-screenshot-hint">调试浏览器</span>
          </div>
          <div class="debug-screenshot-frame">
            <img
              v-if="session.screenshot_url"
              :src="session.screenshot_url"
              alt="截图预览"
              class="debug-screenshot"
              @error="clearScreenshot"
            />
            <span v-else class="debug-screenshot-placeholder">
              {{ loading ? "执行中..." : "暂无截图" }}
            </span>
          </div>
        </div>
      </div>
    </div>

    <template #footer>
      <span class="debug-footer-hint">步骤将在调试浏览器中实时执行</span>
      <div class="debug-footer-actions">
        <button class="btn btn-secondary" :disabled="loading || isDone" @click="nextStep">
          {{ loading ? "执行中..." : "单步执行" }}
        </button>
        <button class="btn btn-secondary" :disabled="loading || isDone" @click="runAll">
          {{ loading ? "执行中..." : "执行全部" }}
        </button>
        <button class="btn btn-secondary" :disabled="downloading" @click="handleFeedback">
          {{ downloading ? "导出中..." : "导出问题报告" }}
        </button>
        <button class="btn btn-danger" @click="handleClose">停止调试</button>
      </div>
    </template>
  </Modal>
</template>
