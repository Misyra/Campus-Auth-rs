<script setup lang="ts">
/** 本浏览器外观设置：主题与材质分区、快捷风格、阅读偏好和即时反馈。 */
import { computed } from "vue";
import SettingsRow from "@/components/common/SettingsRow.vue";
import IconApp from "@/components/common/IconApp.vue";
import Modal from "@/components/common/Modal.vue";
import AppearanceColors from "@/components/common/AppearanceColors.vue";
import { useAppearance } from "@/composables/useAppearance";
import { useBackgroundImage } from "@/composables/useBackgroundImage";
import { useConfirm } from "@/composables/useConfirm";
import { APPEARANCE_PRESETS } from "@/utils/appearance";

const { appearance, storageAvailable, cardDirty, resetCard, resetAll } = useAppearance();
const { confirm } = useConfirm();
const {
  randomWallpaperDialog, bgLightbox, uploading, removing, selectBackgroundImage, clearBackgroundImage,
  openRandomWallpaperDialog, closeRandomWallpaperDialog, confirmRandomWallpaper, openBgLightbox, closeBgLightbox,
} = useBackgroundImage();
const activePreset = computed(() => APPEARANCE_PRESETS.find((preset) => Object.entries(preset.values).every(([key, value]) => appearance[key as keyof typeof appearance] === value))?.id);
async function restoreAppearance(): Promise<void> {
  if (await confirm({ title: "恢复默认外观", message: "恢复本浏览器的主题、背景和显示偏好？自定义色板及已上传的图片文件会保留。" })) resetAll();
}
</script>

<template>
  <div class="appearance-page settings-list-page">
    <div class="appearance-intro">
      <p role="status">{{ storageAvailable ? '修改即时生效，自动保存在本浏览器。' : '浏览器存储不可用，当前修改关闭后可能无法保留。' }}</p>
      <button
        type="button"
        class="btn btn-secondary btn-sm"
        :disabled="uploading || removing || randomWallpaperDialog.loading"
        @click="restoreAppearance"
      >
        恢复全部默认
      </button>
    </div>
    <section class="appearance-section-card">
      <div class="appearance-card-header">
        <h3>主题与配色</h3>
        <button
          type="button"
          class="appearance-reset-btn"
          :disabled="!cardDirty('theme')"
          @click="resetCard('theme')"
        >
          恢复默认
        </button>
      </div>
      <div class="appearance-card-body">
        <SettingsRow label="显示模式" description="选择浅色、深色，或跟随系统自动切换">
          <div class="segmented" role="group" aria-label="显示模式">
            <button
              type="button"
              :class="{ active: appearance.theme === 'light' }"
              :aria-pressed="appearance.theme === 'light'"
              @click="appearance.theme = 'light'"
            >
              浅色
            </button>
            <button
              type="button"
              :class="{ active: appearance.theme === 'dark' }"
              :aria-pressed="appearance.theme === 'dark'"
              @click="appearance.theme = 'dark'"
            >
              深色
            </button>
            <button
              type="button"
              :class="{ active: appearance.theme === 'auto' }"
              :aria-pressed="appearance.theme === 'auto'"
              @click="appearance.theme = 'auto'"
            >
              跟随系统
            </button>
          </div>
        </SettingsRow>
        <AppearanceColors type="accent" label="主题色" />
        <AppearanceColors type="bg" label="页面背景色" default-label="自动" />
      </div>
    </section>
    <section class="appearance-section-card">
      <div class="appearance-card-header">
        <h3>卡片与层次</h3>
        <button
          type="button"
          class="appearance-reset-btn"
          :disabled="!cardDirty('card')"
          @click="resetCard('card')"
        >
          恢复默认
        </button>
      </div>
      <div class="appearance-card-body">
        <SettingsRow label="快捷风格" description="调整卡片与导航材质，保留配色和背景图片">
          <div class="appearance-presets" role="group" aria-label="快捷风格">
            <button
              v-for="preset in APPEARANCE_PRESETS"
              :key="preset.id"
              type="button"
              class="appearance-preset"
              :class="[{ active: activePreset === preset.id }, 'appearance-preset--' + preset.id]"
              :aria-pressed="activePreset === preset.id"
              :title="preset.hint"
              @click="Object.assign(appearance, preset.values)"
            >
              <span class="appearance-preset-sample" aria-hidden="true">
                <i></i>
                <i></i>
                <i></i>
              </span>
              <strong>{{ preset.label }}</strong>
            </button>
          </div>
        </SettingsRow>
        <SettingsRow description="控制卡片底色的可见度">
          <template #label>
            <label for="card-opacity">卡片不透明度</label>
          </template>
          <div class="appearance-slider-item">
            <input
              id="card-opacity"
              type="range"
              v-model.number="appearance.card_opacity"
              min="0"
              max="1"
              step="0.05"
            />
            <output for="card-opacity">{{ Math.round(appearance.card_opacity * 100) }}%</output>
          </div>
        </SettingsRow>
        <SettingsRow>
          <template #label>
            <label for="border-intensity">边框强度</label>
          </template>
          <div class="appearance-slider-item">
            <input
              id="border-intensity"
              type="range"
              v-model.number="appearance.border_intensity"
              min="0"
              max="2"
              step="0.1"
            />
            <output for="border-intensity">{{ appearance.border_intensity.toFixed(1) }}×</output>
          </div>
        </SettingsRow>
        <SettingsRow label="毛玻璃效果" description="模糊卡片背后的内容；设备性能有限时可关闭">
          <label class="toggle setting-row-switch">
            <input type="checkbox" v-model="appearance.backdrop_filter" />
            <span class="toggle-slider"></span>
            <span class="sr-only">毛玻璃效果</span>
          </label>
        </SettingsRow>
        <SettingsRow :class="{ disabled: !appearance.backdrop_filter }" description="开启毛玻璃后可调节，搭配背景图片更明显">
          <template #label>
            <label for="card-blur">玻璃模糊度</label>
          </template>
          <div class="appearance-slider-item">
            <input
              id="card-blur"
              type="range"
              v-model.number="appearance.card_blur"
              min="0"
              max="24"
              step="1"
              :disabled="!appearance.backdrop_filter"
            />
            <output for="card-blur">{{ appearance.card_blur }}px</output>
          </div>
        </SettingsRow>
      </div>
    </section>
    <section class="appearance-section-card">
      <div class="appearance-card-header">
        <h3>背景图片</h3>
        <button
          type="button"
          class="appearance-reset-btn"
          :disabled="uploading || removing || randomWallpaperDialog.loading || !cardDirty('background')"
          @click="resetCard('background')"
        >
          恢复默认
        </button>
      </div>
      <div class="appearance-card-body">
        <SettingsRow label="背景图片" description="支持常见图片格式，文件最大 5MB">
          <div class="appearance-bg-thumb-group">
            <button
              v-if="appearance.background_url"
              type="button"
              class="appearance-bg-thumb"
              @click="openBgLightbox"
              aria-label="放大背景图片预览"
            >
              <img :src="appearance.background_url" alt="背景图片" />
              <span class="appearance-bg-thumb-zoom">
                <IconApp name="zoom-in" class="icon-sm" />
              </span>
            </button>
            <button
              v-else
              type="button"
              class="appearance-bg-thumb empty"
              @click="selectBackgroundImage"
              :disabled="uploading || removing || randomWallpaperDialog.loading"
            >
              <IconApp name="image" class="appearance-bg-thumb-icon" />
              <span>选择图片</span>
            </button>
            <div class="appearance-bg-thumb-actions">
              <button
                type="button"
                class="btn btn-secondary btn-sm"
                @click="selectBackgroundImage"
                :disabled="uploading || removing || randomWallpaperDialog.loading"
              >
                {{ uploading ? '上传中…' : appearance.background_url ? '更换图片' : '选择图片' }}
              </button>
              <button
                type="button"
                class="btn btn-secondary btn-sm"
                @click="openRandomWallpaperDialog"
                :disabled="uploading || removing || randomWallpaperDialog.loading"
              >
                从链接下载
              </button>
              <button
                v-if="appearance.background_url"
                type="button"
                class="btn btn-text btn-sm"
                @click="clearBackgroundImage"
                :disabled="uploading || removing || randomWallpaperDialog.loading"
              >
                移除图片
              </button>
            </div>
          </div>
        </SettingsRow>
        <SettingsRow :class="{ disabled: !appearance.background_url }" description="添加背景图片后可调节">
          <template #label>
            <label for="bg-blur">图片模糊</label>
          </template>
          <div class="appearance-slider-item">
            <input
              id="bg-blur"
              type="range"
              v-model.number="appearance.background_blur"
              min="0"
              max="30"
              step="1"
              :disabled="!appearance.background_url"
            />
            <output for="bg-blur">{{ appearance.background_blur }}px</output>
          </div>
        </SettingsRow>
        <SettingsRow :class="{ disabled: !appearance.background_url }">
          <template #label>
            <label for="bg-opacity">图片可见度</label>
          </template>
          <div class="appearance-slider-item">
            <input
              id="bg-opacity"
              type="range"
              v-model.number="appearance.background_opacity"
              min="0"
              max="0.8"
              step="0.05"
              :disabled="!appearance.background_url"
            />
            <output for="bg-opacity">{{ Math.round(appearance.background_opacity * 100) }}%</output>
          </div>
        </SettingsRow>
      </div>
    </section>
    <section class="appearance-section-card">
      <div class="appearance-card-header">
        <h3>导航栏</h3>
        <button
          type="button"
          class="appearance-reset-btn"
          :disabled="!cardDirty('sidebar')"
          @click="resetCard('sidebar')"
        >
          恢复默认
        </button>
      </div>
      <div class="appearance-card-body">
        <AppearanceColors type="sidebar" label="导航背景色" default-label="跟随背景" />
        <AppearanceColors type="sidebar_accent" label="选中标记色" default-label="跟随主题" />
        <SettingsRow description="同时作用于侧栏与顶栏，手机端使用底部导航">
          <template #label>
            <label for="sidebar-opacity">导航不透明度</label>
          </template>
          <div class="appearance-slider-item">
            <input
              id="sidebar-opacity"
              type="range"
              v-model.number="appearance.sidebar_opacity"
              min="0.3"
              max="1"
              step="0.05"
            />
            <output for="sidebar-opacity">{{ Math.round(appearance.sidebar_opacity * 100) }}%</output>
          </div>
        </SettingsRow>
      </div>
    </section>
    <section class="appearance-section-card appearance-reading">
      <div class="appearance-card-header">
        <h3>阅读与动效</h3>
        <button
          type="button"
          class="appearance-reset-btn"
          :disabled="!cardDirty('reading')"
          @click="resetCard('reading')"
        >
          恢复默认
        </button>
      </div>
      <div class="appearance-card-body">
        <SettingsRow label="文字大小" description="调整整个界面的文字与控件大小">
          <div class="segmented" role="group" aria-label="文字大小">
            <button
              v-for="size in [{ value: 1, label: '标准' }, { value: 1.1, label: '较大' }, { value: 1.2, label: '大' }]"
              :key="size.value"
              type="button"
              :class="{ active: appearance.font_scale === size.value }"
              :aria-pressed="appearance.font_scale === size.value"
              @click="appearance.font_scale = size.value"
            >
              {{ size.label }}
            </button>
          </div>
        </SettingsRow>
        <SettingsRow label="减少界面动效" description="减少切换和悬停动画，同时遵循系统的减少动效偏好">
          <label class="toggle setting-row-switch">
            <input type="checkbox" v-model="appearance.reduce_motion" />
            <span class="toggle-slider"></span>
            <span class="sr-only">减少界面动效</span>
          </label>
        </SettingsRow>
      </div>
    </section>
    <Modal :open="bgLightbox.visible" title="背景预览" size="lg" preview @close="closeBgLightbox">
      <div class="bg-preview-body">
        <img :src="appearance.background_url" alt="背景预览" />
      </div>
    </Modal>
    <Modal
      :open="randomWallpaperDialog.visible"
      title="从链接下载背景图片"
      :close-disabled="randomWallpaperDialog.loading"
      @close="closeRandomWallpaperDialog"
    >
      <p class="random-wallpaper-hint">输入 HTTP 或 HTTPS 图片链接，下载后会立即设为背景。</p>
      <div class="form-group">
        <label for="wallpaper-url">图片链接</label>
        <input
          id="wallpaper-url"
          type="url"
          v-model="randomWallpaperDialog.url"
          placeholder="https://example.com/wallpaper.jpg"
          :disabled="randomWallpaperDialog.loading"
          @keyup.enter="confirmRandomWallpaper"
        />
      </div>
      <template #footer>
        <button
          type="button"
          class="btn btn-secondary btn-sm"
          @click="closeRandomWallpaperDialog"
          :disabled="randomWallpaperDialog.loading"
        >
          取消
        </button>
        <button
          type="button"
          class="btn btn-primary btn-sm"
          @click="confirmRandomWallpaper"
          :disabled="randomWallpaperDialog.loading"
        >
          <IconApp v-if="randomWallpaperDialog.loading" name="refresh" class="spin icon-sm" />
          {{ randomWallpaperDialog.loading ? '下载中…' : '下载并设为背景' }}
        </button>
      </template>
    </Modal>
  </div>
</template>
