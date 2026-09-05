<script setup lang="ts">
/**
 * AI 生成任务页：捕获登录页面（截图 + HTML/JS）→ 交由用户配置的视觉 LLM
 * 生成任务 JSON → 校验回显 → 预览编辑后保存为任务。
 * 三段向导按序依赖：保存配置 → 捕获 → 生成 → 保存任务。
 */
import IconApp from "@/components/common/IconApp.vue";
import { aiApi } from "@/api";
import { extractApiError } from "@/api/client";
import type { AiCaptureResult, AiGenerateResult, AiLlmConfig } from "@/api/types";
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { useToast } from "@/composables/useToast";
import { downloadBlob } from "@/utils/file";

const router = useRouter();
const { toastOnly } = useToast();

// ---- LLM 配置 ----
/** 服务商预设：填好后可手动改（模型名随服务商版本更新） */
const PRESETS = [
  { label: "智谱 GLM", base: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4v-flash" },
  { label: "DeepSeek", base: "https://api.deepseek.com", model: "deepseek-chat" },
  { label: "硅基流动", base: "https://api.siliconflow.cn/v1", model: "Qwen/Qwen2.5-VL-32B-Instruct" },
  { label: "自定义 / 本地模型", base: "", model: "" },
];

const preset = ref(0);
const baseUrl = ref("");
const model = ref("");
const apiKey = ref("");
const hasApiKey = ref(false);
const savingConfig = ref(false);

function applyPreset(): void {
  const p = PRESETS[preset.value];
  if (p.base) {
    baseUrl.value = p.base;
    model.value = p.model;
  }
}

async function loadConfig(): Promise<void> {
  try {
    const cfg = await aiApi.fetchLlmConfig();
    baseUrl.value = cfg.base_url || "";
    model.value = cfg.model || "";
    hasApiKey.value = cfg.has_api_key;
  } catch (error) {
    toastOnly(false, extractApiError(error, "读取 LLM 配置失败"));
  }
}

async function saveConfig(): Promise<void> {
  savingConfig.value = true;
  try {
    const payload: { base_url: string; model: string; api_key?: string } = {
      base_url: baseUrl.value,
      model: model.value,
    };
    // 留空 = 保持已有 key；输入了新值才更新（后端支持空串清除，界面不暴露）
    if (apiKey.value.trim()) payload.api_key = apiKey.value.trim();
    const saved = await aiApi.saveLlmConfig(payload);
    hasApiKey.value = saved.has_api_key;
    apiKey.value = "";
    toastOnly(true, "LLM 配置已保存");
  } catch (error) {
    toastOnly(false, extractApiError(error, "保存 LLM 配置失败"));
  } finally {
    savingConfig.value = false;
  }
}

// ---- 捕获 ----
const captureUrl = ref("");
const capturing = ref(false);
const captureResult = ref<AiCaptureResult | null>(null);
const screenshotUrl = ref("");
const savingBundle = ref(false);

async function capture(): Promise<void> {
  if (!captureUrl.value.trim()) {
    toastOnly(false, "请输入登录页地址");
    return;
  }
  capturing.value = true;
  captureResult.value = null;
  try {
    captureResult.value = await aiApi.capture(captureUrl.value.trim());
    // 时间戳防缓存：每次捕获后强制刷新截图
    screenshotUrl.value = aiApi.captureScreenshotUrl();
    toastOnly(true, "页面捕获完成");
  } catch (error) {
    toastOnly(false, extractApiError(error, "页面捕获失败"));
  } finally {
    capturing.value = false;
  }
}

/** 保存页面文件：MHTML 完整布局 + HTML + CSS/JS 资源 + 截图（zip） */
async function saveBundle(): Promise<void> {
  if (savingBundle.value) return;
  savingBundle.value = true;
  try {
    const blob = await aiApi.captureBundle();
    const stamp = new Date().toISOString().slice(0, 19).replace(/[-:T]/g, "");
    downloadBlob(blob, `campus-auth-capture-${stamp}.zip`, "application/zip");
    toastOnly(true, "页面文件已保存");
  } catch (error) {
    toastOnly(false, extractApiError(error as Error, "保存页面文件失败"));
  } finally {
    savingBundle.value = false;
  }
}

// ---- 生成 ----
const extraPrompt = ref("");
const generating = ref(false);
const generateResult = ref<AiGenerateResult | null>(null);
const taskJson = ref("");
const jsonError = ref("");
const savingTask = ref(false);

async function generate(): Promise<void> {
  if (!hasApiKey.value && !apiKey.value.trim()) {
    toastOnly(false, "部分服务商要求 API Key，请先在上方保存配置");
  }
  generating.value = true;
  generateResult.value = null;
  jsonError.value = "";
  try {
    generateResult.value = await aiApi.generate({ extra_prompt: extraPrompt.value });
    taskJson.value = JSON.stringify(generateResult.value.task, null, 2);
    toastOnly(true, `任务生成成功（第 ${generateResult.value.attempts} 轮通过校验）`);
  } catch (error) {
    toastOnly(false, extractApiError(error, "任务生成失败"));
  } finally {
    generating.value = false;
  }
}

async function saveTask(): Promise<void> {
  if (!taskJson.value.trim()) return;
  try {
    const task = JSON.parse(taskJson.value) as Record<string, unknown>;
    // 后端导入以 task_id 为文件名（[a-zA-Z0-9_-]{1,64}），生成的 JSON 不带 id，这里补齐
    if (!task.task_id) task.task_id = `ai-${Date.now()}`;
    if (!task.type) task.type = "browser";
    savingTask.value = true;
    const r = await tasksImport(task);
    if (r.failed && (r.failed as unknown[]).length > 0) {
      toastOnly(false, `任务校验未通过：${JSON.stringify(r.failed[0])}`);
      return;
    }
    toastOnly(true, "任务已保存，可在任务管理中查看");
    void router.push({ name: "tasks" });
  } catch (error) {
    if (error instanceof SyntaxError) {
      toastOnly(false, "任务 JSON 格式有误，请检查后重试");
      return;
    }
    toastOnly(false, extractApiError(error, "保存任务失败"));
  } finally {
    savingTask.value = false;
  }
}

async function tasksImport(task: Record<string, unknown>): Promise<{ failed?: unknown[] }> {
  const { tasksApi } = await import("@/api");
  return (await tasksApi.import({ tasks: [task] })) as { failed?: unknown[] };
}

function formatJson(): void {
  try {
    const obj = JSON.parse(taskJson.value) as Record<string, unknown>;
    taskJson.value = JSON.stringify(obj, null, 2);
    jsonError.value = "";
  } catch (e) {
    jsonError.value = (e as Error).message;
  }
}

onMounted(() => {
  void loadConfig();
});
</script>

<template>
  <div class="page-content">
    <div class="ai-task-grid">
      <div class="card">
        <div class="card-header">
          <h2><IconApp name="sparkles" class="icon-sm" /> 第 1 步 · 配置 LLM 服务</h2>
        </div>
        <div class="card-body">
          <div class="hint ai-privacy-hint">
            API Key 使用 AES-256-GCM 加密存储在本机（与校园网密码同一密钥体系），不会明文落盘。
          </div>
          <div class="ai-config-row-wide">
            <div class="form-group">
              <label for="ai-preset">服务商预设</label>
              <select id="ai-preset" v-model.number="preset" @change="applyPreset">
                <option v-for="(p, i) in PRESETS" :key="i" :value="i">{{ p.label }}</option>
              </select>
            </div>
            <div class="form-group">
              <label for="ai-base-url" class="required">Base URL</label>
              <input
                id="ai-base-url"
                v-model="baseUrl"
                type="text"
                placeholder="https://open.bigmodel.cn/api/paas/v4"
                autocomplete="off"
                spellcheck="false"
              />
            </div>
          </div>
          <div class="ai-config-row-eq">
            <div class="form-group">
              <label for="ai-model" class="required">模型名（需支持视觉输入）</label>
              <input
                id="ai-model"
                v-model="model"
                type="text"
                placeholder="例如 glm-4v-flash"
                autocomplete="off"
                spellcheck="false"
              />
            </div>
            <div class="form-group">
              <label for="ai-api-key">API Key</label>
              <input
                id="ai-api-key"
                v-model="apiKey"
                type="password"
                :placeholder="hasApiKey ? '已保存（留空保持不变）' : 'sk-...'"
                autocomplete="new-password"
              />
            </div>
          </div>
          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="savingConfig" @click="saveConfig">
              <IconApp name="save" class="icon-sm" />
              {{ savingConfig ? "保存中…" : "保存配置" }}
            </button>
          </div>
        </div>
      </div>

      <div class="card">
        <div class="card-header">
          <h2><IconApp name="image" class="icon-sm" /> 第 2 步 · 捕获登录页面</h2>
        </div>
        <div class="card-body">
          <div class="hint">
            请在<b>未登录校园网</b>状态下捕获（已认证时访问不会跳转到登录页）。页面内容与截图将发送给你配置的 LLM 服务商。
          </div>
          <div class="form-group">
            <label for="ai-capture-url" class="required">登录页地址</label>
            <input
              id="ai-capture-url"
              v-model="captureUrl"
              type="text"
              placeholder="例如 http://10.x.x.x 或任意网址（未登录时自动跳转到认证页）"
              autocomplete="off"
              spellcheck="false"
              @keyup.enter="capture"
            />
          </div>
          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="capturing" @click="capture">
              <IconApp name="zoom-in" class="icon-sm" />
              {{ capturing ? "捕获中…" : "开始捕获" }}
            </button>
          </div>

          <template v-if="captureResult">
            <div class="ai-capture-meta">
              <div>落地地址：<code>{{ captureResult.final_url }}</code></div>
              <div v-if="captureResult.title">页面标题：{{ captureResult.title }}</div>
              <div>资源快照：{{ captureResult.resources_count ?? 0 }} 个文件<span v-if="captureResult.note">（{{ captureResult.note }}）</span></div>
            </div>
            <div class="ai-actions">
              <button class="btn btn-secondary btn-sm" :disabled="savingBundle" @click="saveBundle" title="下载 MHTML 完整布局 + HTML + CSS/JS 资源 + 截图">
                <IconApp name="download" class="icon-sm" />
                {{ savingBundle ? "打包中…" : "保存页面文件" }}
              </button>
            </div>
            <img
              v-if="screenshotUrl"
              class="ai-screenshot"
              :src="screenshotUrl"
              alt="登录页截图"
            />
          </template>
        </div>
      </div>

      <div class="card ai-generate-card">
        <div class="card-header">
          <h2><IconApp name="code" class="icon-sm" /> 第 3 步 · 生成并保存任务</h2>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="ai-extra">补充说明（可选）</label>
            <textarea
              id="ai-extra"
              v-model="extraPrompt"
              rows="2"
              placeholder="例如：运营商选择「中国电信」；验证码每 60 秒刷新一次"
            ></textarea>
          </div>
          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="generating" @click="generate">
              <IconApp name="sparkles" class="icon-sm" />
              {{ generating ? "生成中（可能需要 1~2 分钟）…" : "生成任务" }}
            </button>
            <span v-if="generateResult" class="hint">
              第 {{ generateResult.attempts }} 轮通过校验 · 模型 {{ generateResult.model }}
            </span>
          </div>
          <ul v-if="generateResult?.warnings?.length" class="ai-warnings">
            <li v-for="(w, i) in generateResult.warnings" :key="i">{{ w }}</li>
          </ul>

          <div v-if="taskJson" class="form-group">
            <label for="ai-task-json">任务 JSON（可编辑）</label>
            <textarea id="ai-task-json" v-model="taskJson" rows="14" spellcheck="false" class="ai-json"></textarea>
            <div v-if="jsonError" class="ai-json-error">JSON 格式错误：{{ jsonError }}</div>
          </div>
          <div v-if="taskJson" class="ai-actions">
            <button class="btn btn-secondary btn-sm" @click="formatJson">格式化</button>
            <button class="btn btn-primary" :disabled="savingTask" @click="saveTask">
              <IconApp name="file-check" class="icon-sm" />
              {{ savingTask ? "保存中…" : "保存为任务" }}
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
