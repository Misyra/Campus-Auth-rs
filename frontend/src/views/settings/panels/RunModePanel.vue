<script setup lang="ts">
/**
 * 设置 · 配置预设面板（chromeless 整面板槽位）：一键应用预设运行设置组合。
 *
 * 卡头带「当前模式」徽标且按钮组为分段卡形态，整面板自绘 chrome。
 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import IconApp from "@/components/common/IconApp.vue";
import { computed } from "vue";
import { useConfig } from "@/composables/useConfig";
import { useConfirm } from "@/composables/useConfirm";
import { useRunMode } from "@/composables/useRunMode";
import { RUN_MODE_PRESETS } from "@/utils/runMode";
import type { RunModeId } from "@/utils/runMode";

const { currentMode, applying: modeApplying, applyRunMode, diffFor } = useRunMode();
const { confirm } = useConfirm();
useConfig();

const modeOptions = computed(() =>
  RUN_MODE_PRESETS.map((p) => ({ id: p.id, label: p.label, description: p.description })),
);

/**
 * 切换模式：先弹确认并列出**将要改动的项**。
 *
 * 不做"静默应用"：这个动作会写多处设置、还会真实注册/取消开机自启，用户有权在
 * 动手前看到究竟改了什么。改动清单以结构化 `changes` 传入（而非拼成一段文本）——
 * 确认框据此分列着色，多项时仍能逐行扫读。
 */
async function switchMode(id: Exclude<RunModeId, "custom">): Promise<void> {
  if (id === currentMode.value) return;
  const preset = RUN_MODE_PRESETS.find((p) => p.id === id);
  if (!preset) return;
  const changes = diffFor(id);
  const ok = await confirm({
    title: `切换到${preset.label}`,
    message: changes.length
      ? "将应用以下改动："
      : `当前设置已与${preset.label}一致，无需改动。`,
    changes: changes.map((c) => ({ label: c.label, from: c.from, to: c.to })),
    confirmText: "应用",
  });
  if (ok !== true) return;
  await applyRunMode(id);
}
</script>

<template>
  <!-- 配置预设会同时调整多项设置，区别于下方只控制服务形态的单项设置。 -->
  <section class="settings-panel">
    <div class="settings-card-header">
      <IconApp name="sliders" class="settings-card-icon" />
      <h2>配置预设</h2>
      <span class="badge badge--sm" :class="currentMode === 'custom' ? 'badge--warn' : 'badge--success'">
        当前：
        {{ currentMode === "custom" ? "自定义" : (RUN_MODE_PRESETS.find((p) => p.id === currentMode)?.label ?? "") }}
      </span>
    </div>
    <div class="card-body">
      <SettingsRow label="配置预设" description="一次调整多项运行设置，应用前会列出具体改动">
        <template #description>{{ modeOptions.find(opt => opt.id === currentMode)?.description }}</template>
        <div class="run-mode-options" role="group" aria-label="配置预设">
          <button
            v-for="opt in modeOptions"
            :key="opt.id"
            type="button"
            class="run-mode-option"
            :class="{ active: currentMode === opt.id }"
            :disabled="modeApplying"
            :aria-pressed="currentMode === opt.id"
            :title="opt.description"
            @click="switchMode(opt.id)"
          >
            <span class="run-mode-option-head">
              <span class="run-mode-option-label">{{ opt.label }}</span>
              <IconApp v-if="currentMode === opt.id" name="check" class="icon-sm" />
            </span>
            <span class="run-mode-option-desc">{{ opt.description }}</span>
          </button>
        </div>
      </SettingsRow>
    </div>
  </section>
</template>
