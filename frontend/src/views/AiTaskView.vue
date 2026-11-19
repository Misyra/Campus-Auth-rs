<script setup lang="ts">
/**
 * AI 生成任务页：捕获登录页面（截图 + HTML/JS）→ 交由用户配置的视觉 LLM 生成任务 JSON
 * → 校验回显 → 预览编辑后保存为任务。三段向导按序依赖：保存配置 → 捕获 → 生成 → 保存任务。
 * 逻辑按职责拆分（设计系统收口 T8-2）：配置草稿 useAiLlmConfig / 捕获 useAiCapture /
 * 生成编排 useAiGenerate，本组件只保留模板装配与生命周期接线。
 */
import IconApp from "@/components/common/IconApp.vue";
import CustomSelect from "@/components/common/CustomSelect.vue";
import { onMounted, onUnmounted, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { TUTORIAL_VIDEO_AI_URL } from "@/utils/constants";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { useAiLlmConfig } from "@/composables/useAiLlmConfig";
import { useAiCapture } from "@/composables/useAiCapture";
import { useAiGenerate } from "@/composables/useAiGenerate";

const router = useRouter();
const { toastOnly } = useToast();
const { confirm } = useConfirm();

// 装配顺序即依赖顺序：生成编排消费前两步的就绪状态与当前模型配置
const aiConfig = useAiLlmConfig({ toastOnly });
const aiCapture = useAiCapture({ toastOnly });
const aiGenerate = useAiGenerate({
  isCaptureDone: aiCapture.isCaptureDone,
  isConfigDone: aiConfig.isConfigDone,
  model: aiConfig.model, baseUrl: aiConfig.baseUrl,
  toastOnly, confirm,
  // 保存成功后的跳转保持在视图层（composable 不感知 router）
  goToBrowserTasks: () => void router.push({ name: "tasks-browser" }),
});
onMounted(() => { void aiConfig.loadConfig(); void aiCapture.restoreCapture(); void aiCapture.loadBrowserChannel(); });
// 离开页面即中止流式请求：后端经断连检测取消 LLM 调用，toast 也不再跨页弹出
onUnmounted(aiGenerate.dispose);

const {
  PRESETS, provider, configuredProviders, baseUrl, model, modelList, customModel, loadingModels,
  apiKey, hasApiKey, maxTokens, maxTokenOptions, configExpanded, configSummary, actualRequestUrl,
  modelListUrl, modelOptions, modelChoice, savingConfig, testingConfig, isConfigDone, configTestHint,
  selectProvider, fetchModelList, saveConfig, clearApiKey, testConnection,
} = aiConfig;
const {
  captureUrl, capturing, captureResult, screenshotUrl, savingBundle, isCaptureDone,
  captureChannelDegraded, browserChannel, capture, saveBundle,
} = aiCapture;
// 截图文件可能已被清理（restoreCapture 恢复的会话跨了清理周期）：加载失败时隐藏
// <img>，避免留下空框 + alt 文本的破图观感（RepoImportModals.markImageBroken 同款守卫）
const screenshotBroken = ref(false);
watch(screenshotUrl, () => { screenshotBroken.value = false; });
const {
  extraPrompt, generating, generateResult, taskJson, jsonError, savingTask, streamText, streamEvents,
  streamPhase, streamIdleMs, streamIdleValue, streamIdleOptions, isGenerateDone,
  generate, cancelGenerate, copyStream, clearStream, formatJson, saveTask,
} = aiGenerate;
</script>

<template>
  <div class="ai-task-page">
    <div class="ai-dev-notice">
      <IconApp name="alert-triangle" class="icon-sm" />
      <span>
        当前功能仍在开发，可能不稳定。如果无法生成正确任务，请到
        <router-link to="/settings/tasks">设置 · 任务与环境</router-link>
        下载<router-link to="/settings/tasks">任务录制器</router-link>，点选元素后把提示词交给任意大模型生成；
        也可参考
        <a :href="TUTORIAL_VIDEO_AI_URL" target="_blank" rel="noopener noreferrer">使用教程视频</a>。
      </span>
    </div>

    <div class="ai-stepper" aria-label="步骤进度">
      <div class="ai-step" :class="{ done: isConfigDone, active: !isConfigDone }">
        <span class="ai-step-dot">{{ isConfigDone ? "✓" : "1" }}</span>
        <span class="ai-step-label">配置 LLM</span>
      </div>
      <span class="ai-step-sep" :class="{ done: isConfigDone }"></span>
      <div class="ai-step" :class="{ done: isCaptureDone, active: isConfigDone && !isCaptureDone }">
        <span class="ai-step-dot">{{ isCaptureDone ? "✓" : "2" }}</span>
        <span class="ai-step-label">捕获页面</span>
      </div>
      <span class="ai-step-sep" :class="{ done: isCaptureDone }"></span>
      <div class="ai-step" :class="{ done: isGenerateDone, active: isConfigDone && isCaptureDone && !isGenerateDone }">
        <span class="ai-step-dot">{{ isGenerateDone ? "✓" : "3" }}</span>
        <span class="ai-step-label">生成任务</span>
      </div>
    </div>

    <div class="hint ai-steps-hint">
      <b>使用步骤</b>
      <ol>
        <li>先选择模型服务商并保存配置：需要该服务商的 API Key（各自独立加密保存），模型名可在保存前点「获取模型列表」从服务商拉取。</li>
        <li>退出校园网登录后捕获认证页，再连回网络生成任务（总时长最长 10 分钟）。</li>
        <li>在下方预览 JSON，确认后保存为任务。</li>
      </ol>
    </div>

    <div class="ai-task-grid">
      <div class="card" :class="{ 'ai-card-done': isConfigDone }">
        <div class="card-header">
          <h2><IconApp name="sparkles" class="icon-sm" /> 第 1 步 · 配置 LLM 服务</h2>
          <span class="badge badge--sm" :class="isConfigDone ? 'badge--success' : 'badge--warn'">{{ isConfigDone ? "已配置" : "待配置" }}</span>
        </div>
        <div class="card-body">
          <button type="button" class="ai-config-toggle" :aria-expanded="configExpanded" @click="configExpanded = !configExpanded">
            <span class="ai-config-toggle-label">模型配置</span>
            <span class="ai-config-summary">{{ configSummary }}</span>
            <IconApp name="chevron-down" class="icon-sm ai-config-chevron" :class="{ open: configExpanded }" />
          </button>
          <div v-show="configExpanded">
            <div class="hint ai-privacy-hint">
              API Key 按服务商分别加密保存在本机。切换服务商时会自动切换到对应 Key，不会把 DeepSeek Key 发给 GLM。
            </div>
            <div class="ai-provider-grid" role="radiogroup" aria-label="模型服务商">
              <button v-for="item in PRESETS" :key="item.id" type="button" class="ai-provider-card"
                :class="{ selected: provider === item.id }" role="radio" :aria-checked="provider === item.id"
                @click="selectProvider(item.id)">
                <span class="ai-provider-signal"></span>
                <span class="ai-provider-copy"><b>{{ item.label }}</b><small>{{ item.hint }}</small></span>
                <span v-if="configuredProviders.includes(item.id) || item.defaultKey" class="ai-key-state">Key 已就绪</span>
              </button>
            </div>
            <div class="form-row form-row--wide">
              <div class="form-group">
                <label for="ai-base-url" class="required">Base URL</label>
                <input id="ai-base-url" v-model="baseUrl" type="text" placeholder="https://open.bigmodel.cn/api/paas/v4" autocomplete="off" spellcheck="false" />
                <span class="hint">实际请求：<code>{{ actualRequestUrl }}</code></span>
              </div>
            </div>
            <!-- 两行两列：地址与模型一行、凭据与输出上限一行。此前这一行放 3 个字段，
                 而 .form-row 只有两列，「最长输出」被挤到第二行首列，右侧空着且与
                 API Key 的说明文字错位。 -->
            <div class="form-row">
              <div class="form-group">
                <label for="ai-model" class="required">模型名（需支持视觉输入）</label>
                <div class="ai-model-row">
                  <CustomSelect v-if="modelList.length" id="ai-model-select" v-model="modelChoice" :options="modelOptions"
                    :disabled="loadingModels" placeholder="请选择模型" aria-label="模型名" />
                  <input v-if="!modelList.length || customModel" id="ai-model" v-model="model" type="text"
                    placeholder="例如 glm-5.3-flash" autocomplete="off" spellcheck="false" />
                  <button type="button" class="btn btn-secondary btn-sm" :disabled="loadingModels || !baseUrl.trim()"
                    :title="`从 ${modelListUrl} 拉取模型列表`" @click="fetchModelList">
                    <IconApp v-if="loadingModels" name="refresh" class="spin" /> {{ loadingModels ? "获取中…" : "获取模型列表" }}
                  </button>
                </div>
                <span v-if="!baseUrl.trim()" class="hint">请先填写 Base URL，再拉取模型列表</span>
                <span v-else-if="!modelList.length" class="hint">
                  可手填；点「获取模型列表」从 <code>{{ modelListUrl }}</code> 拉取后改为下拉选择
                </span>
                <span v-else-if="customModel" class="hint">当前为手动填写，也可从左侧列表中选择</span>
              </div>
              <div class="form-group">
                <label for="ai-max-tokens">最长输出</label>
                <CustomSelect id="ai-max-tokens" v-model="maxTokens" :options="maxTokenOptions" />
              </div>
            </div>
            <div class="form-row">
              <div class="form-group">
                <label for="ai-api-key">API Key</label>
                <input id="ai-api-key" v-model="apiKey" type="password" :placeholder="hasApiKey ? '已保存（留空保持不变）' : 'sk-...'" autocomplete="new-password" />
                <span class="hint">{{ hasApiKey ? `已使用 ${PRESETS.find(p => p.id === provider)?.label || "当前服务"} 的独立 Key` : "当前服务商尚未保存 Key" }}</span>
              </div>
            </div>
            <div class="ai-actions">
              <button class="btn btn-primary" :disabled="savingConfig" @click="saveConfig">
                <IconApp name="save" class="icon-sm" /> {{ savingConfig ? "保存中…" : "保存配置" }}
              </button>
              <button v-if="hasApiKey && !PRESETS.find(p => p.id === provider)?.defaultKey" class="btn btn-secondary" :disabled="savingConfig" @click="clearApiKey">
                清除当前 Key
              </button>
              <button class="btn btn-secondary" :disabled="testingConfig || !isConfigDone" :title="!isConfigDone ? configTestHint : '测试当前已保存的模型配置'" @click="testConnection">
                {{ testingConfig ? "测试中…" : "测试连接" }}
              </button>
              <span v-if="!isConfigDone" class="hint">{{ configTestHint }}</span>
            </div>
          </div>
        </div>
      </div>

      <div class="card" :class="{ 'ai-card-done': isCaptureDone }">
        <div class="card-header">
          <h2><IconApp name="image" class="icon-sm" /> 第 2 步 · 捕获登录页面</h2>
          <span class="badge badge--sm" :class="isCaptureDone ? 'badge--success' : 'badge--warn'">{{ isCaptureDone ? "已捕获" : "待捕获" }}</span>
        </div>
        <div class="card-body">
          <div class="hint">
            请在<b>未登录校园网</b>状态下捕获（已认证时不会跳转到登录页）。页面内容与截图将发送给你配置的 LLM 服务商。
          </div>
          <div v-if="captureChannelDegraded" class="ai-capture-warn">
            <span>
              当前浏览器渠道 <b>{{ browserChannel }}</b> 不支持 CDP：捕获仍可进行，但 <b>MHTML 完整布局快照不可用</b>，CSS/JS 改为联网回补抓取。
              需要完整快照请到「浏览器设置」把浏览器渠道切换为 Chromium / Chrome / Edge 后重新捕获。
            </span>
          </div>
          <div class="form-group">
            <label for="ai-capture-url" class="required">登录页地址</label>
            <input id="ai-capture-url" v-model="captureUrl" type="text" placeholder="例如 http://10.x.x.x 或任意网址（未登录时自动跳转到认证页）" autocomplete="off" spellcheck="false" @keyup.enter="capture" />
            <span class="hint">请填写登录页或可触发校园网跳转的地址。</span>
          </div>
          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="capturing" @click="capture">
              <IconApp name="zoom-in" class="icon-sm" /> {{ capturing ? "捕获中…" : "开始捕获" }}
            </button>
            <span v-if="capturing" class="hint">正在导航并抓取页面资源…</span>
          </div>

          <template v-if="captureResult">
            <div class="ai-capture-meta">
              <div>落地地址：<code>{{ captureResult.final_url }}</code></div>
              <div v-if="captureResult.title">页面标题：{{ captureResult.title }}</div>
              <div>资源快照：{{ captureResult.resources_count ?? 0 }} 个文件<span v-if="captureResult.note">（{{ captureResult.note }}）</span></div>
            </div>
            <div v-if="captureResult.structure_summary" class="ai-structure-summary" aria-label="结构化页面扫描结果">
              <span><b>{{ captureResult.structure_summary.frames }}</b> 个页面层级</span>
              <span><b>{{ captureResult.structure_summary.forms }}</b> 个表单</span>
              <span><b>{{ captureResult.structure_summary.controls }}</b> 个控件</span>
              <span><b>{{ captureResult.structure_summary.captcha_candidates }}</b> 个验证码候选</span>
              <small>生成时优先读取结构信息，并始终附带脱敏局部 HTML。</small>
            </div>
            <div class="ai-actions">
              <button class="btn btn-secondary btn-sm" :disabled="savingBundle" @click="saveBundle" title="下载离线页面文件：MHTML（Chromium 渠道）+ 原始 HTML + 离线副本 + CSS/JS 资源 + 截图">
                <IconApp name="download" class="icon-sm" /> {{ savingBundle ? "打包中…" : "保存页面文件" }}
              </button>
            </div>
            <img v-if="screenshotUrl && !screenshotBroken" class="ai-screenshot" :src="screenshotUrl" alt="登录页截图" @error="screenshotBroken = true" />
          </template>
        </div>
      </div>

      <div class="card" :class="{ 'ai-card-done': isGenerateDone }">
        <div class="card-header">
          <h2><IconApp name="code" class="icon-sm" /> 第 3 步 · 生成并保存任务</h2>
          <span class="badge badge--sm" :class="isGenerateDone ? 'badge--success' : generating ? 'badge--primary' : 'badge--warn'">{{ isGenerateDone ? "已生成" : generating ? "生成中" : "待生成" }}</span>
        </div>
        <div class="card-body">
          <div class="form-group">
            <label for="ai-extra">补充说明（可选）</label>
            <textarea id="ai-extra" v-model="extraPrompt" rows="2" placeholder="例如：运营商选择「中国电信」；验证码每 60 秒刷新一次" :disabled="generating"></textarea>
          </div>

          <div class="ai-generate-bar">
            <div class="ai-idle-group">
              <label for="ai-idle" class="ai-idle-label">前端空闲超时</label>
              <CustomSelect v-model="streamIdleValue" :options="streamIdleOptions" class="ai-idle-select" compact />
            </div>
            <span class="hint ai-idle-hint">有输出会刷新空闲计时；服务端单次生成总时长仍不超过 10 分钟</span>
            <span class="ai-generate-hint hint" v-if="streamPhase && !generateResult">{{ streamPhase }}</span>
            <span class="ai-generate-hint hint" v-else-if="generateResult">第 {{ generateResult.attempts }} 轮通过校验 · {{ generateResult.model }}</span>
          </div>

          <div class="ai-actions">
            <button class="btn btn-primary" :disabled="generating || !isCaptureDone" title="需先完成第 2 步捕获" @click="generate">
              <IconApp name="sparkles" class="icon-sm" /> {{ generating ? "生成中…" : "生成任务" }}
            </button>
            <button v-if="generating" class="btn btn-secondary" @click="cancelGenerate">取消</button>
            <span v-if="!isCaptureDone && !generating" class="hint">请先完成页面捕获</span>
          </div>

          <div v-if="generating || streamText || streamEvents.length" class="ai-stream-card">
            <div class="ai-stream-header">
              <span class="ai-stream-title"><IconApp name="activity" class="icon-sm" /> 流式输出</span>
              <span class="hint">{{ streamText.length ? `${streamText.length} 字符` : "等待输出…" }} · {{ streamPhase || "准备中" }}</span>
              <span class="ai-stream-actions">
                <button class="btn btn-secondary btn-sm" :disabled="!streamText" @click="copyStream">复制</button>
                <button class="btn btn-secondary btn-sm" :disabled="!streamText && !streamEvents.length" @click="clearStream">清空</button>
              </span>
            </div>
            <pre id="ai-stream-preview" class="ai-stream-preview">{{ streamText || "等待模型输出…" }}</pre>
            <div v-if="streamEvents.length" class="ai-stream-events">
              <span v-for="(ev, i) in streamEvents.slice(-6)" :key="i" class="ai-stream-chip" :class="'chip-' + String(ev.type)">{{ String(ev.type) }}</span>
            </div>
          </div>

          <ul v-if="generateResult?.warnings?.length" class="ai-warnings">
            <li v-for="(w, i) in generateResult.warnings" :key="i">{{ w }}</li>
          </ul>

          <div v-if="taskJson" class="form-group">
            <label for="ai-task-json">任务 JSON（可编辑）</label>
            <!-- aria-invalid/aria-describedby：语法错误结论直连控件，屏读器聚焦即报 -->
            <textarea
              id="ai-task-json"
              v-model="taskJson"
              rows="14"
              spellcheck="false"
              class="ai-json"
              :aria-invalid="!!jsonError || undefined"
              :aria-describedby="jsonError ? 'ai-task-json-error' : undefined"
            ></textarea>
            <div v-if="jsonError" id="ai-task-json-error" class="ai-json-error">JSON 格式错误：{{ jsonError }}</div>
          </div>
          <div v-if="taskJson" class="ai-actions">
            <button class="btn btn-secondary btn-sm" @click="formatJson">格式化</button>
            <button class="btn btn-primary" :disabled="savingTask" @click="saveTask">
              <IconApp name="file-check" class="icon-sm" /> {{ savingTask ? "保存中…" : "保存为任务" }}
            </button>
          </div>
        </div>
      </div>
    </div>

    <div v-if="generating || streamPhase" id="ai-bottom-progress" class="ai-bottom-progress" role="status" aria-live="polite">
      <div class="ai-bottom-progress-inner">
        <span class="ai-bottom-dot" :class="{ active: generating }"></span>
        <span class="ai-bottom-text">{{ streamPhase || "准备中…" }}</span>
        <span class="hint ai-bottom-meta">{{ streamText.length ? `${streamText.length} 字符` : "" }} · 空闲 {{ Math.round(streamIdleMs/60000) }} 分钟（有输出续命）</span>
        <button v-if="generating" class="btn btn-secondary btn-sm ai-bottom-cancel" @click="cancelGenerate">取消</button>
      </div>
      <div class="ai-bottom-bar"><div class="ai-bottom-bar-fill" :class="{ running: generating }"></div></div>
    </div>
  </div>
</template>
