<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import Modal from "@/components/common/Modal.vue";
import { ref } from "vue";
import { useRouter } from "vue-router";
import { systemApi, autostartApi, uninstallApi } from "@/api";
import type { UninstallDetectItem, UninstallStepResult } from "@/api/types";
import { useConfirm } from "@/composables/useConfirm";
import { frontendLogger } from "@/utils/logger";

const { confirm } = useConfirm();
const router = useRouter();

// ---- 版本信息 ----
const version = ref("unknown");
const pythonStatus = ref("未知");
const pythonReady = ref<boolean | null>(null);
const platform = ref("");
const autostartEnabled = ref(false);

/** 并行拉取版本/自启动/环境状态；各请求独立降级，避免单点失败遮掉其它信息 */
async function loadInfo() {
  const [healthResult, autoResult, initResult] = await Promise.allSettled([
    systemApi.health(),
    autostartApi.fetchStatus(),
    systemApi.initStatus(),
  ]);
  if (healthResult.status === "fulfilled") {
    version.value = healthResult.value.version || "unknown";
  } else {
    frontendLogger.warn("about", "版本信息加载失败", healthResult.reason);
  }
  if (autoResult.status === "fulfilled") {
    platform.value = autoResult.value.platform;
    autostartEnabled.value = autoResult.value.enabled;
  } else {
    frontendLogger.warn("about", "自启动信息加载失败", autoResult.reason);
  }
  if (initResult.status === "fulfilled") {
    const env = (initResult.value as { environment?: { python_ready?: boolean } }).environment;
    pythonReady.value = env?.python_ready ?? false;
    pythonStatus.value = pythonReady.value ? "已就绪" : "未就绪";
  } else {
    frontendLogger.warn("about", "Python 环境信息加载失败", initResult.reason);
  }
}
void loadInfo();

// ---- 卸载 ----
const uninstallOpen = ref(false);
const uninstallDetecting = ref(false);
const uninstallItems = ref<UninstallDetectItem[]>([]);
const uninstallError = ref("");
const uninstallRunning = ref(false);
const uninstallDone = ref(false);
const uninstallResults = ref<UninstallStepResult[]>([]);
const uninstallMessage = ref("");

/** 打开卸载弹窗：detect→run 两段式的第一段——仅探测可清理项并重置上次结果，不执行任何删除 */
async function openUninstall() {
  uninstallOpen.value = true;
  uninstallDetecting.value = true;
  uninstallItems.value = [];
  uninstallError.value = "";
  uninstallDone.value = false;
  uninstallResults.value = [];
  try {
    uninstallItems.value = await uninstallApi.detect();
  } catch (e: unknown) {
    uninstallError.value = (e as Error).message || "检测失败";
  } finally {
    uninstallDetecting.value = false;
  }
}

/** 两段式第二段：detect→run。卸载不可恢复（删用户数据/自启动/浏览器缓存），
 *  故执行前再经确认弹窗兜底；运行中弹窗不可关闭（closeUninstall 拦截）。 */
async function runUninstall() {
  const ok = await confirm({
    title: "确认卸载清理",
    message: "将关闭开机自启动、删除用户数据目录并清理 Playwright 浏览器缓存，此操作不可恢复。是否继续？",
    confirmText: "开始清理",
  });
  if (!ok) return;
  uninstallRunning.value = true;
  uninstallError.value = "";
  try {
    const data = await uninstallApi.uninstall();
    uninstallResults.value = data.results ?? [];
    uninstallMessage.value = data.message ?? "";
    uninstallDone.value = true;
  } catch (e: unknown) {
    uninstallError.value = (e as Error).message || "卸载清理失败";
  } finally {
    uninstallRunning.value = false;
  }
}

function closeUninstall() {
  if (uninstallRunning.value) return;
  uninstallOpen.value = false;
}
</script>

<template>
  <div class="page-content">
    <div class="about-container">
      <div class="about-hero card">
        <span class="about-logo logo-mark" role="img" aria-label="Logo"></span>
        <h1>校园网自动认证</h1>
        <p class="about-subtitle">Campus Network Auth</p>
        <p class="version">Version {{ version }}</p>
        <p class="description">校园网自动认证工具</p>
      </div>

      <div class="about-grid">
        <div class="card">
          <div class="card-header"><h2>技术栈与工具链</h2></div>
          <div class="card-body">
            <div class="tech-stack">
              <div class="tech-item"><span class="tech-badge rust">Rust 2024</span></div>
              <div class="tech-item"><span class="tech-badge tokio">Tokio + Axum</span></div>
              <div class="tech-item"><span class="tech-badge vue">Vue 3 + Vite</span></div>
              <div class="tech-item"><span class="tech-badge playwright">Playwright</span></div>
              <div class="tech-item"><span class="tech-badge websockets">WebSockets</span></div>
              <div class="tech-item"><span class="tech-badge ddddocr">Ddddocr</span></div>
              <div class="tech-item"><span class="tech-badge uv">uv</span></div>
              <div class="tech-item"><span class="tech-badge python">Python 3.10+</span></div>
            </div>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h2>特性</h2></div>
          <div class="card-body">
            <ul class="feature-list">
              <li><IconApp name="check" />前后端分离架构</li>
              <li><IconApp name="check" />自动网络检测与登录</li>
              <li><IconApp name="check" />实时日志与状态检测</li>
              <li><IconApp name="check" />验证码 OCR 自动识别</li>
              <li><IconApp name="check" />开机自启动支持</li>
            </ul>
          </div>
        </div>

        <div class="card">
          <div class="card-header"><h2>系统信息</h2></div>
          <div class="card-body">
            <div class="info-list">
              <div class="info-item">
                <span class="info-label">Python</span>
                <span class="info-value" :class="{ 'info-value--success': pythonReady === true, 'info-value--warning': pythonReady === false }">
                  {{ pythonStatus }}
                  <button v-if="pythonReady === false" type="button" class="btn btn-link info-fix-link" @click="router.push({ name: 'settings-tasks' })">前往设置 · 任务与环境</button>
                </span>
              </div>
              <div class="info-item">
                <span class="info-label">平台</span>
                <span class="info-value">{{ platform }}</span>
              </div>
              <div class="info-item">
                <span class="info-label">自启动</span>
                <span class="info-value">{{ autostartEnabled ? "已启用" : "未启用" }}</span>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div class="about-footer card">
        <div class="about-links">
          <a href="https://campus-auth.misyra.com/docs/getting-started/start" target="_blank" rel="noopener noreferrer" class="docs-link">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z"/>
              <path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z"/>
            </svg>
            使用文档
          </a>
          <a href="https://github.com/Misyra/Campus-Auth-rs" target="_blank" rel="noopener noreferrer" class="github-link">
            <svg viewBox="0 0 24 24" fill="currentColor">
              <path d="M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z"/>
            </svg>
            GitHub
          </a>
        </div>
        <p>License: AGPL-3.0-only (<a href="https://github.com/Misyra/Campus-Auth-rs/blob/master/LICENSE" target="_blank" rel="noopener noreferrer">LICENSE</a>)</p>
        <p class="qq-group">QQ交流群：<strong>1105307735</strong></p>
        <p class="muted">Made with ❤️ for campus network users</p>
      </div>

      <!-- 卸载 -->
      <div class="uninstall-section card">
        <div class="uninstall-header">
          <IconApp name="trash" width="20" height="20" />
          <div>
            <h3>卸载程序</h3>
            <p class="uninstall-desc">清理开机自启动、用户数据目录与 Playwright 浏览器缓存；完成后删除程序所在文件夹即可完成卸载。</p>
          </div>
        </div>
        <button class="btn btn-danger-ghost btn-sm" @click="openUninstall">卸载</button>
      </div>

      <Modal :open="uninstallOpen" title="卸载程序" :close-on-overlay="!uninstallRunning" :close-on-esc="!uninstallRunning" @close="closeUninstall">
        <p class="uninstall-subtitle">将清理以下系统残留项</p>

        <div v-if="uninstallDetecting" class="uninstall-scanning"><span class="spinner"></span>正在检测...</div>

        <template v-else-if="!uninstallDone">
          <div v-if="uninstallError" class="empty-state empty-state--sm">{{ uninstallError }}</div>
          <template v-else>
            <div v-for="item in uninstallItems" :key="item.key" class="uninstall-item disabled">
              <div class="uninstall-item-info">
                <span class="uninstall-item-label">{{ item.label }}</span>
                <span class="uninstall-item-path">{{ item.description }}</span>
              </div>
              <span class="uninstall-item-tag" :class="item.exists ? 'tag-exists' : 'tag-missing'">{{ item.exists ? "存在" : "无" }}</span>
            </div>
            <div class="uninstall-hint-box">将关闭开机自启动、删除用户数据目录并清理 Playwright 浏览器缓存，此操作不可恢复。清理完成后，手动删除程序所在文件夹即可完成卸载。</div>
          </template>
        </template>

        <div v-else class="uninstall-results">
          <div class="uninstall-result-header"><IconApp name="check" width="16" height="16" />清理结果</div>
          <div v-for="r in uninstallResults" :key="r.key" class="uninstall-result-row">
            <span :class="r.success ? 'result-ok' : 'result-fail'">{{ r.success ? "✓" : "✗" }}</span>
            <span>{{ r.label }}</span>
            <span class="uninstall-item-path">{{ r.message }}</span>
          </div>
          <div class="uninstall-final-hint">{{ uninstallMessage }}</div>
        </div>

        <template #footer>
          <template v-if="!uninstallDone">
            <button class="btn btn-ghost btn-sm" @click="closeUninstall" :disabled="uninstallRunning">取消</button>
            <button class="btn btn-danger btn-sm" @click="runUninstall" :disabled="uninstallRunning || !!uninstallError">{{ uninstallRunning ? "清理中..." : "开始清理" }}</button>
          </template>
          <button v-else class="btn btn-primary btn-sm" @click="closeUninstall">关闭</button>
        </template>
      </Modal>
    </div>
  </div>
</template>
