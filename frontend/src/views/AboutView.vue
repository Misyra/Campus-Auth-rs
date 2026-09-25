<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import Modal from "@/components/common/Modal.vue";
import { ref } from "vue";
import { useRouter } from "vue-router";
import { systemApi, autostartApi } from "@/api";
import { useUninstall } from "@/composables/useUninstall";
import { frontendLogger } from "@/utils/logger";
import { BILIBILI_SPACE_URL, DOCS } from "@/utils/constants";

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
// 流程（探测 → 确认 → 清系统残留 → 删程序并退出）在 composable 里，含各步的失败语义；
// 模板只负责渲染清单与勾选。解构出的都是 ref，模板里自动解包。
const {
  open: uninstallOpen,
  detecting: uninstallDetecting,
  detectError: uninstallDetectError,
  items: uninstallItems,
  program: uninstallProgram,
  data: uninstallData,
  // 只用合成后的 blockReason（守卫拒绝 或 卸载助手缺失）：单看 blocked 会漏掉后者
  blockReason: uninstallBlockReason,
  keepUserData: uninstallKeepUserData,
  phase: uninstallPhase,
  running: uninstallRunning,
  cleanupResults: uninstallCleanupResults,
  cleanupMessage: uninstallCleanupMessage,
  error: uninstallError,
  pendingUpdateLeft: uninstallPendingUpdateLeft,
  deletionLabels: uninstallDeletionLabels,
  deleteCount: uninstallDeleteCount,
  openDialog: openUninstall,
  run: runUninstall,
  closeDialog: closeUninstall,
} = useUninstall();
</script>

<template>
  <div class="page-content">
    <div class="about-container">
      <div class="about-hero card">
        <span class="about-logo logo-mark" role="img" aria-label="Logo"></span>
        <h1>认证喵</h1>
        <p class="about-subtitle">Campus Network Auth</p>
        <p class="version">Version {{ version }}</p>
        <p class="description">校园网自动认证工具</p>
      </div>

      <div class="about-grid">
        <div class="card">
          <div class="card-header"><h2>技术栈与工具链</h2></div>
          <div class="card-body">
            <!-- 徽标直接作为 .tech-stack（flex + wrap + gap）的子项：
                 此前每项还套了一层 `<div class="tech-item">`，而全仓没有任何
                 `.tech-item` 规则——是一层不产生任何效果的空壳。 -->
            <div class="tech-stack">
              <span class="tech-badge rust">Rust 2024</span>
              <span class="tech-badge tokio">Tokio + Axum</span>
              <span class="tech-badge vue">Vue 3 + Vite</span>
              <span class="tech-badge playwright">Playwright</span>
              <span class="tech-badge websockets">WebSockets</span>
              <span class="tech-badge ddddocr">Ddddocr</span>
              <span class="tech-badge uv">uv</span>
              <span class="tech-badge python">Python 3.10+</span>
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
          <a :href="DOCS.gettingStarted" target="_blank" rel="noopener noreferrer" class="docs-link">
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
          <a href="https://blog.misyra.com/sponsor/" target="_blank" rel="noopener noreferrer" class="sponsor-link">
            <IconApp name="heart" />
            赞助
          </a>
          <a :href="BILIBILI_SPACE_URL" target="_blank" rel="noopener noreferrer" class="bilibili-link">
            <IconApp name="bilibili" />
            B 站主页
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
            <p class="uninstall-desc">删除程序文件（可勾选保留配置与任务），并关闭开机自启动、清理加密密钥目录与 Playwright 浏览器缓存。</p>
          </div>
        </div>
        <button class="btn btn-danger-ghost btn-sm" @click="openUninstall">卸载</button>
      </div>

      <Modal :open="uninstallOpen" title="卸载程序" :close-on-overlay="!uninstallRunning" :close-on-esc="!uninstallRunning" @close="closeUninstall">
        <div v-if="uninstallDetecting" class="uninstall-scanning"><span class="spinner"></span>正在检测...</div>

        <!-- 卸载已启动：后端随时消失，只回执不提供操作 -->
        <div v-else-if="uninstallPhase === 'done'" class="uninstall-results">
          <div class="uninstall-result-header"><IconApp name="check" width="16" height="16" />程序即将退出</div>
          <p class="uninstall-subtitle">卸载助手将在退出后立即删除：</p>
          <ul class="uninstall-plan-list">
            <li v-for="label in uninstallDeletionLabels" :key="label">{{ label }}</li>
          </ul>
          <template v-if="uninstallCleanupResults.length">
            <p class="uninstall-subtitle">系统残留清理结果</p>
            <div v-for="r in uninstallCleanupResults" :key="r.key" class="uninstall-result-row">
              <span :class="r.success ? 'result-ok' : 'result-fail'">{{ r.success ? "✓" : "✗" }}</span>
              <span>{{ r.label }}</span>
              <span class="uninstall-item-path">{{ r.message }}</span>
            </div>
          </template>
          <div class="uninstall-hint-box">
            完成删除后会弹出系统提示框，告诉你删了什么、以及有没有删不掉的项。
          </div>
          <p v-if="uninstallCleanupMessage" class="uninstall-note">{{ uninstallCleanupMessage }}</p>
          <!-- 取消失败必须出声：否则用户以为卸干净了，下次开机程序还在 -->
          <p v-if="uninstallPendingUpdateLeft" class="uninstall-blocked">
            注意：待应用的更新未能取消，程序可能在退出后被更新助手重新安装。
          </p>
        </div>

        <template v-else>
          <div v-if="uninstallDetectError" class="empty-state empty-state--sm">{{ uninstallDetectError }}</div>
          <!-- 拦下卸载的原因：守卫拒绝 或 卸载助手缺失（后者一定导致"清完残留却删不掉程序"） -->
          <div v-else-if="uninstallBlockReason" class="uninstall-blocked">{{ uninstallBlockReason }}</div>
          <template v-else>
            <p class="uninstall-subtitle">将删除以下内容（不可恢复）</p>

            <div v-if="uninstallProgram" class="uninstall-item disabled">
              <div class="uninstall-item-info">
                <span class="uninstall-item-label">{{ uninstallProgram.label }}</span>
                <span class="uninstall-item-path">{{ uninstallProgram.path }}</span>
              </div>
              <span class="uninstall-item-tag" :class="uninstallProgram.exists ? 'tag-exists' : 'tag-missing'">
                {{ uninstallProgram.exists ? "将删除" : "无" }}
              </span>
            </div>

            <!-- 保留勾选：默认不勾（默认真卸载）。放在数据目录上方，勾选后下方行的标签
                 立刻从「将删除」变成「保留」，后果在按下按钮前就看得见 -->
            <label class="toggle toggle-help-inline uninstall-keep">
              <input type="checkbox" v-model="uninstallKeepUserData" :disabled="uninstallRunning" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">保留配置与任务</span>
            </label>
            <p class="uninstall-keep-hint">
              勾选后下列用户数据整棵保留，重装到别处后仍可用；程序文件照常删除。
            </p>

            <div v-for="d in uninstallData" :key="d.key" class="uninstall-item" :class="{ disabled: !d.exists || uninstallKeepUserData }">
              <div class="uninstall-item-info">
                <span class="uninstall-item-label">{{ d.label }}</span>
                <span class="uninstall-item-path">{{ d.path }}</span>
              </div>
              <!-- 类名字面量留在模板里（不藏进函数返回值）：死类审计按模板字面量判定，
                   藏起来会让一个在用的类被报成死类 -->
              <span
                class="uninstall-item-tag"
                :class="uninstallKeepUserData ? 'tag-kept' : d.exists ? 'tag-exists' : 'tag-missing'"
              >
                {{ uninstallKeepUserData ? "保留" : d.exists ? "将删除" : "无" }}
              </span>
            </div>

            <p class="uninstall-subtitle">并清理以下系统残留</p>
            <div v-for="item in uninstallItems" :key="item.key" class="uninstall-item disabled">
              <div class="uninstall-item-info">
                <span class="uninstall-item-label">{{ item.label }}</span>
                <span class="uninstall-item-path">{{ item.description }}</span>
              </div>
              <span class="uninstall-item-tag" :class="item.exists ? 'tag-exists' : 'tag-missing'">{{ item.exists ? "存在" : "无" }}</span>
            </div>

            <div class="uninstall-hint-box">
              程序会立即退出，删除由更新助手在退出后完成（运行中的程序文件无法自己删除），完成后会弹出系统提示框。
            </div>
          </template>
          <div v-if="uninstallError" class="uninstall-blocked">{{ uninstallError }}</div>
        </template>

        <template #footer>
          <span v-if="uninstallPhase === 'done'" class="uninstall-footer-note">程序正在退出，请勿关闭窗口…</span>
          <template v-else>
            <button class="btn btn-ghost btn-sm" @click="closeUninstall" :disabled="uninstallRunning">取消</button>
            <button
              class="btn btn-danger btn-sm"
              @click="runUninstall"
              :disabled="uninstallRunning || !!uninstallBlockReason || !!uninstallDetectError || uninstallDeleteCount === 0"
            >
              {{ uninstallRunning ? (uninstallPhase === "cleanup" ? "清理中..." : "正在启动卸载...") : "卸载并退出" }}
            </button>
          </template>
        </template>
      </Modal>
    </div>
  </div>
</template>
