<script setup lang="ts">
/**
 * 调试面板组件。
 * 用于单步/全量调试浏览器任务，展示步骤执行结果与截图预览。
 *
 * 使用 useDebug composable 管理会话状态，通过 Modal 弹出。
 */

import { computed, ref, watch } from "vue";
import { useDebug } from "@/composables/useDebug";
import { debugApi } from "@/api";
import { downloadBlob } from "@/utils/file";
import { extractApiError } from "@/api/client";
import { useToast } from "@/composables/useToast";
import Modal from "./common/Modal.vue";

const { session, loading, visible, screenshotStep, nextStep, runAll, stopDebug, stopping, getStepStatus, getStepResult, clearScreenshot } =
  useDebug();
const downloading = ref(false);

/** 放大预览的截图地址与标题（空即关闭）。
 *
 * 开图时冻结 URL 与归属步骤：预览区只有 390~520px 宽，看验证码/表单细节必须放大，
 * 而放大后再来新一帧会把画面换掉——冻住才能安心看这一帧。数据 URL 快照驻留内存，
 * 无额外请求。 */
const zoomUrl = ref("");
const zoomStep = ref<number | null>(null);

/** 放大预览标题：区分会话启动帧与某一步之后的画面 */
const zoomTitle = computed(() =>
  zoomStep.value === null ? "调试截图 · 会话启动" : `调试截图 · 步骤 ${zoomStep.value + 1} 执行后`,
);

/** 打开放大预览 */
function openZoom(): void {
  if (!session.screenshot_url) return;
  zoomUrl.value = session.screenshot_url;
  zoomStep.value = screenshotStep.value;
}

/** 关闭放大预览 */
function closeZoom(): void {
  zoomUrl.value = "";
  zoomStep.value = null;
}

// 调试面板关闭（停止调试/会话结束）时放大预览一起关，避免预览窗单独悬空
watch(visible, (open) => {
  if (!open) closeZoom();
});

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
    // WEB-1：导出后本机调试产物（截图/页面快照）已被清理，显式告知避免误以为仍在
    toastOnly(true, "问题报告已导出；本机调试产物已一并清理");
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
    size="xxl"
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
          <span class="badge debug-status-pill" :class="isDone ? 'badge--success' : 'badge--info'">
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
                <span class="debug-step-desc" :title="step.description">{{ step.description || `步骤 ${i + 1}` }}</span>
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

          <div v-if="!session.steps.length" class="empty-state empty-state--dashed">
            <span class="debug-empty-icon">◻</span>
            <span>{{ loading ? "正在获取会话数据..." : session.running ? "会话详情恢复中，当前执行结束后自动补全" : "该任务没有可执行的步骤" }}</span>
          </div>
        </div>

        <!-- 右侧：截图预览 -->
        <div class="debug-screenshot-container">
          <div class="debug-screenshot-head">
            <span>实时截图</span>
            <!-- 每步执行后补拍一帧，标注归属步骤可直观确认"预览确实前进了" -->
            <span class="debug-screenshot-hint">
              {{ screenshotStep === null ? "调试浏览器" : `步骤 ${screenshotStep + 1} 后` }}
            </span>
          </div>
          <div class="debug-screenshot-frame">
            <!-- 缩略区只有几百像素宽，看不清验证码/表单细节：整图可点，弹大图 -->
            <button
              v-if="session.screenshot_url"
              type="button"
              class="debug-screenshot-btn"
              title="点击放大查看"
              aria-label="放大查看调试截图"
              @click="openZoom"
            >
              <img
                :src="session.screenshot_url"
                alt="截图预览"
                class="debug-screenshot"
                @error="clearScreenshot"
              />
              <span class="debug-screenshot-zoom-hint">点击放大</span>
            </button>
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
        <button class="btn btn-danger" @click="handleClose" :disabled="stopping">停止调试</button>
      </div>
    </template>
  </Modal>

  <!-- 截图放大预览：复用 Modal（沉浸遮罩），大图按原分辨率铺满并允许滚动查看 -->
  <Modal :open="!!zoomUrl" :title="zoomTitle" size="xxl" preview @close="closeZoom">
    <div class="debug-zoom-body">
      <img v-if="zoomUrl" :src="zoomUrl" alt="调试截图大图" />
    </div>
  </Modal>
</template>
