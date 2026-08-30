<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import { ref } from "vue";
import { systemApi, autostartApi } from "@/api";
import type { UpdateInfo } from "@/api/types";
import { useConfirm } from "@/composables/useConfirm";

const { confirm } = useConfirm();

// ---- 版本信息 ----
const version = ref("unknown");
const pythonStatus = ref("未知");
const platform = ref("");
const autostartEnabled = ref(false);

async function loadInfo() {
  try {
    // /api/health 仅返回 status/version，不返回 python_version；
    // Python 就绪状态改由 /api/init-status 的 environment.python_ready 推导
    const [health, auto, init] = await Promise.all([
      systemApi.health(),
      autostartApi.fetchStatus(),
      systemApi.initStatus(),
    ]);
    version.value = health.version || "unknown";
    const env = (init as { environment?: { python_ready?: boolean } }).environment;
    pythonStatus.value = env?.python_ready ? "已就绪" : "未就绪";
    platform.value = auto.platform;
    autostartEnabled.value = auto.enabled;
  } catch { /* 静默 */ }
}
void loadInfo();

// ---- 更新检查 ----
const updateLoading = ref(false);
const updating = ref(false);
const updateInfo = ref<UpdateInfo | null>(null);

async function checkUpdate() {
  updateLoading.value = true;
  updateInfo.value = null;
  try {
    updateInfo.value = await systemApi.checkUpdate();
  } catch (e: unknown) {
    updateInfo.value = { has_update: false, error: (e as Error).message || "检查失败" };
  } finally {
    updateLoading.value = false;
  }
}

// 通过后台执行更新（下载并暂存，重启后生效）
async function applyUpdate() {
  updating.value = true;
  try {
    const data = await systemApi.update();
    updateInfo.value = {
      has_update: false,
      message: (data.message as string) || "更新已就绪，重启后生效",
    };
    // 更新已就绪，询问是否立即优雅关闭并重启（联动 Rust 侧优雅关闭接口）
    const ok = await confirm({
      title: "更新已就绪",
      message: "更新已下载完成，是否立即重启应用以生效？",
      confirmText: "立即重启",
    });
    if (ok) {
      try {
        await systemApi.shutdown();
      } catch (e) {
        updateInfo.value.message = "更新已就绪，但自动重启失败，请手动重启应用";
      }
    }
  } catch (e: unknown) {
    updateInfo.value = { has_update: false, error: (e as Error).message || "更新失败" };
  } finally {
    updating.value = false;
  }
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
        <div class="update-section">
          <button class="btn btn-secondary btn-sm" @click="checkUpdate" :disabled="updateLoading">
            {{ updateLoading ? "检查中..." : "检查更新" }}
          </button>
          <div v-if="updateInfo && !updateInfo.error && !updateInfo.message" class="update-result">
            <div v-if="updateInfo.has_update" class="update-available">
              <IconApp name="upload" width="16" height="16" />
              <span>发现新版本 <strong>v{{ updateInfo.latest }}</strong></span>
              <button class="btn btn-primary btn-sm" @click="applyUpdate" :disabled="updating">
                {{ updating ? "更新中..." : "立即更新" }}
              </button>
              <a :href="updateInfo.url" target="_blank" rel="noopener noreferrer" class="btn btn-ghost btn-sm">前往下载</a>
            </div>
            <div v-else class="update-latest">
              <IconApp name="check" width="16" height="16" />
              <span>当前已是最新版本</span>
            </div>
          </div>
          <div v-else-if="updateInfo && updateInfo.message" class="update-success">
            <IconApp name="check" width="16" height="16" />
            <span>{{ updateInfo.message }}，请重启程序生效</span>
          </div>
          <div v-else-if="updateInfo && updateInfo.error" class="update-error">
            {{ updateInfo.error }}
          </div>
        </div>
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
              <li><IconApp name="check" />实时日志与状态监控</li>
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
                <span class="info-value">{{ pythonStatus }}</span>
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
          <a href="https://github.com/Misyra/Campus-Auth-rs" target="_blank" rel="noopener noreferrer" class="github-link">
            <svg viewBox="0 0 24 24" fill="currentColor">
              <path d="M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z"/>
            </svg>
            GitHub
          </a>
        </div>
        <p>License: MIT</p>
        <p class="qq-group">QQ交流群：<strong>1105307735</strong></p>
        <p class="muted">Made with ❤️ for campus network users</p>
      </div>

      <!-- 卸载提示 -->
      <div class="uninstall-section card">
        <div class="uninstall-header">
          <IconApp name="trash" width="20" height="20" />
          <div>
            <h3>卸载程序</h3>
            <p class="uninstall-desc">
              如需完整卸载，请先关闭本程序，然后手动删除整个项目文件夹即可。
              若需清理开机自启项、浏览器缓存或 Playwright 浏览器等附加组件，可在项目目录运行
              <code>campus-auth uninstall</code> 命令。
            </p>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
