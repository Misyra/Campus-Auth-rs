<script setup lang="ts">
import IconApp from "@/components/common/IconApp.vue";
import Modal from "@/components/common/Modal.vue";
import { useAppearance } from "@/composables/useAppearance";
import { useBackgroundImage } from "@/composables/useBackgroundImage";
import { useCustomColors } from "@/composables/useCustomColors";

const { appearance, cardDirty, resetCard, resetThemeBackground } = useAppearance();
const {
  randomWallpaperDialog,
  bgLightbox,
  selectBackgroundImage,
  clearBackgroundImage,
  openRandomWallpaperDialog,
  closeRandomWallpaperDialog,
  confirmRandomWallpaper,
  openBgLightbox,
  closeBgLightbox,
} = useBackgroundImage();
const {
  getColorList,
  pickCustomColor,
  onCustomColorPicked,
  onColorLongPress,
  startLongPress,
} = useCustomColors();
</script>

<template>
  <div class="page-content">
    <div class="appearance-page">
      <!-- 卡片 1：背景与氛围 -->
      <div class="card appearance-card appearance-section-card">
        <div class="appearance-card-header">
          <IconApp name="image" class="appearance-card-icon" />
          <h3>背景与氛围</h3>
          <button v-if="cardDirty('background')" type="button" class="appearance-reset-btn" @click="resetCard('background')">恢复默认</button>
        </div>
        <div class="appearance-card-body appearance-grid-2col">
          <div class="appearance-bg-thumb-group">
            <div v-if="appearance.background_url" class="appearance-bg-thumb" @click="openBgLightbox">
              <img :src="appearance.background_url" alt="背景预览" />
              <div class="appearance-bg-thumb-zoom">
                <IconApp name="zoom-in" class="icon-sm" />
              </div>
              <button type="button" class="appearance-bg-thumb-remove" @click.stop="clearBackgroundImage" title="移除背景">
                <IconApp name="close" class="icon-sm" />
              </button>
            </div>
            <div v-else class="appearance-bg-thumb empty" @click="selectBackgroundImage">
              <IconApp name="image" :stroke-width="1.5" style="width:24px;height:24px" />
              <span>选择图片</span>
            </div>
            <div class="appearance-bg-thumb-actions">
              <button type="button" class="btn btn-secondary btn-sm" @click="selectBackgroundImage">选择图片</button>
              <button type="button" class="btn btn-secondary btn-sm" @click="openRandomWallpaperDialog">从链接下载</button>
            </div>
          </div>

          <div class="appearance-sliders appearance-sliders-col">
            <div class="appearance-slider-item">
              <label for="bg-blur">背景模糊</label>
              <input id="bg-blur" type="range" v-model.number="appearance.background_blur" min="0" max="30" step="1" />
              <span>{{ appearance.background_blur }}px</span>
            </div>
            <div class="appearance-slider-item">
              <label for="bg-opacity">背景可见度</label>
              <input id="bg-opacity" type="range" v-model.number="appearance.background_opacity" min="0" max="0.8" step="0.05" />
              <span>{{ Math.round(appearance.background_opacity * 100) }}%</span>
            </div>
            <div class="appearance-slider-item" :class="{ disabled: !appearance.backdrop_filter }">
              <label for="card-blur">玻璃模糊度</label>
              <input id="card-blur" type="range" v-model.number="appearance.card_blur" min="0" max="24" step="1" :disabled="!appearance.backdrop_filter" />
              <span>{{ appearance.card_blur }}px</span>
            </div>
            <label class="toggle appearance-toggle-row">
              <input type="checkbox" v-model="appearance.backdrop_filter" />
              <span class="toggle-slider"></span>
              <span class="toggle-label">毛玻璃效果</span>
            </label>
          </div>
        </div>
      </div>

      <!-- 卡片 2：主题与配色 -->
      <div class="card appearance-card appearance-section-card">
        <div class="appearance-card-header">
          <IconApp name="contrast" class="appearance-card-icon" />
          <h3>主题与配色</h3>
          <button v-if="cardDirty('theme')" type="button" class="appearance-reset-btn" @click="resetCard('theme')">恢复默认</button>
        </div>
        <div class="appearance-card-body appearance-grid-2col">
          <div class="appearance-field">
            <div class="appearance-field-label">主题</div>
            <div class="appearance-segmented">
              <button type="button" :class="{ active: appearance.theme === 'light' }" @click="appearance.theme = 'light'">浅色</button>
              <button type="button" :class="{ active: appearance.theme === 'dark' }" @click="appearance.theme = 'dark'">深色</button>
              <button type="button" :class="{ active: appearance.theme === 'auto' }" @click="appearance.theme = 'auto'">跟随系统</button>
            </div>
          </div>
          <div></div>
          <!-- 主题色 -->
          <div class="appearance-field">
            <div class="appearance-field-label">主题色</div>
            <div class="appearance-color-row">
              <div class="appearance-colors">
                <button
                  v-for="color in getColorList('accent')" :key="color.value"
                  type="button" class="appearance-color-btn"
                  :class="{ active: appearance.accent_color === color.value, custom: color.custom }"
                  :style="{ background: color.value }"
                  @click="appearance.accent_color = color.value"
                  @contextmenu.prevent="color.custom ? onColorLongPress('accent', color.value) : null"
                  @touchstart="color.custom ? startLongPress('accent', color.value, $event) : null"
                  :title="color.label"
                >
                  <IconApp name="check" v-if="appearance.accent_color === color.value" class="icon-sm" />
                </button>
                <button type="button" class="appearance-color-btn appearance-color-add" @click="pickCustomColor('accent')" title="自定义颜色">+</button>
              </div>
              <span class="appearance-color-hex">{{ appearance.accent_color }}</span>
            </div>
            <input type="color" data-color-picker="accent" class="sr-only" @change="onCustomColorPicked('accent', $event)" />
          </div>
          <!-- 背景色 -->
          <div class="appearance-field">
            <div class="appearance-field-label">
              背景颜色
              <button v-if="appearance.background_color" type="button" class="appearance-reset-btn-inline" @click="resetThemeBackground">恢复默认</button>
            </div>
            <div class="appearance-color-row">
              <div class="appearance-colors">
                <button
                  v-for="color in getColorList('bg')" :key="color.value"
                  type="button" class="appearance-color-btn"
                  :class="{ active: appearance.background_color === color.value, custom: color.custom }"
                  :style="{ background: color.value }"
                  @click="appearance.background_color = color.value"
                  @contextmenu.prevent="color.custom ? onColorLongPress('bg', color.value) : null"
                  @touchstart="color.custom ? startLongPress('bg', color.value, $event) : null"
                  :title="color.label"
                >
                  <IconApp name="check" v-if="appearance.background_color === color.value" class="icon-sm" />
                </button>
                <button type="button" class="appearance-color-btn appearance-color-add" @click="pickCustomColor('bg')" title="自定义颜色">+</button>
              </div>
              <span class="appearance-color-hex">{{ appearance.background_color }}</span>
            </div>
            <input type="color" data-color-picker="bg" class="sr-only" @change="onCustomColorPicked('bg', $event)" />
          </div>
        </div>
      </div>

      <!-- 卡片 3：卡片样式 -->
      <div class="card appearance-card appearance-section-card">
        <div class="appearance-card-header">
          <IconApp name="grid" class="appearance-card-icon" />
          <h3>卡片样式</h3>
          <button v-if="cardDirty('card')" type="button" class="appearance-reset-btn" @click="resetCard('card')">恢复默认</button>
        </div>
        <div class="appearance-card-body appearance-grid-2col">
          <div class="appearance-slider-item">
            <label for="card-opacity">不透明度</label>
            <input id="card-opacity" type="range" v-model.number="appearance.card_opacity" min="0" max="1" step="0.05" />
            <span>{{ Math.round(appearance.card_opacity * 100) }}%</span>
          </div>
          <div class="appearance-slider-item">
            <label for="border-intensity">边框</label>
            <input id="border-intensity" type="range" v-model.number="appearance.border_intensity" min="0" max="2" step="0.1" />
            <span>{{ appearance.border_intensity.toFixed(1) }}x</span>
          </div>
        </div>
      </div>

      <!-- 卡片 4：侧边栏 -->
      <div class="card appearance-card appearance-section-card">
        <div class="appearance-card-header">
          <IconApp name="sidebar" class="appearance-card-icon" />
          <h3>侧边栏</h3>
          <button v-if="cardDirty('sidebar')" type="button" class="appearance-reset-btn" @click="resetCard('sidebar')">恢复默认</button>
        </div>
        <div class="appearance-card-body appearance-grid-2col">
          <div class="appearance-slider-item">
            <label for="sidebar-opacity">不透明度</label>
            <input id="sidebar-opacity" type="range" v-model.number="appearance.sidebar_opacity" min="0.3" max="1" step="0.05" />
            <span>{{ Math.round(appearance.sidebar_opacity * 100) }}%</span>
          </div>
          <div></div>
          <div class="appearance-field">
            <div class="appearance-field-label">侧边栏色</div>
            <div class="appearance-color-row">
              <div class="appearance-colors">
                <button
                  v-for="color in getColorList('sidebar')" :key="color.value"
                  type="button" class="appearance-color-btn"
                  :class="{ active: appearance.sidebar_color === color.value, custom: color.custom }"
                  :style="{ background: color.value }"
                  @click="appearance.sidebar_color = color.value"
                  @contextmenu.prevent="color.custom ? onColorLongPress('sidebar', color.value) : null"
                  @touchstart="color.custom ? startLongPress('sidebar', color.value, $event) : null"
                  :title="color.label"
                >
                  <IconApp name="check" v-if="appearance.sidebar_color === color.value" class="icon-sm" />
                </button>
                <button type="button" class="appearance-color-btn appearance-color-add" @click="pickCustomColor('sidebar')" title="自定义颜色">+</button>
              </div>
              <span class="appearance-color-hex">{{ appearance.sidebar_color || '跟随背景色' }}</span>
            </div>
            <input type="color" data-color-picker="sidebar" class="sr-only" @change="onCustomColorPicked('sidebar', $event)" />
          </div>
          <div class="appearance-field">
            <div class="appearance-field-label">高亮色</div>
            <div class="appearance-color-row">
              <div class="appearance-colors">
                <button
                  v-for="color in getColorList('sidebar_accent')" :key="color.value"
                  type="button" class="appearance-color-btn"
                  :class="{ active: appearance.sidebar_accent === color.value, custom: color.custom }"
                  :style="{ background: color.value }"
                  @click="appearance.sidebar_accent = color.value"
                  @contextmenu.prevent="color.custom ? onColorLongPress('sidebar_accent', color.value) : null"
                  @touchstart="color.custom ? startLongPress('sidebar_accent', color.value, $event) : null"
                  :title="color.label"
                >
                  <IconApp name="check" v-if="appearance.sidebar_accent === color.value" class="icon-sm" />
                </button>
                <button type="button" class="appearance-color-btn appearance-color-add" @click="pickCustomColor('sidebar_accent')" title="自定义颜色">+</button>
              </div>
              <span class="appearance-color-hex">{{ appearance.sidebar_accent || '跟随主题色' }}</span>
            </div>
            <input type="color" data-color-picker="sidebar_accent" class="sr-only" @change="onCustomColorPicked('sidebar_accent', $event)" />
          </div>
        </div>
      </div>
    </div>

    <!-- 背景图放大预览 -->
    <div v-if="bgLightbox.visible" class="bg-lightbox-overlay" @click="closeBgLightbox">
      <div class="bg-lightbox-content">
        <img :src="appearance.background_url" alt="背景预览" />
        <button type="button" class="bg-lightbox-close" @click.stop="closeBgLightbox" title="关闭">
          <IconApp name="close" class="icon-lg" />
        </button>
      </div>
    </div>

    <!-- 从链接下载壁纸弹窗：复用公共 Modal（open prop 控制显隐，与 TasksView 用法一致） -->
    <Modal
      :open="randomWallpaperDialog.visible"
      title="从链接下载壁纸"
      @close="closeRandomWallpaperDialog"
    >
      <p class="random-wallpaper-hint">输入图片链接地址，将下载并设置为背景（如 https://picsum.photos/1920/1080）</p>
      <input type="text" class="form-input" v-model="randomWallpaperDialog.url"
        placeholder="https://t.alcy.cc/pc" @keyup.enter="confirmRandomWallpaper" />
      <div class="random-wallpaper-footer">
        <button class="btn btn-secondary btn-sm" @click="closeRandomWallpaperDialog" :disabled="randomWallpaperDialog.loading">取消</button>
        <button class="btn btn-primary btn-sm" @click="confirmRandomWallpaper" :disabled="randomWallpaperDialog.loading">
          <IconApp name="refresh" v-if="randomWallpaperDialog.loading" class="spin icon-sm" />
          {{ randomWallpaperDialog.loading ? '加载中...' : '确定' }}
        </button>
      </div>
    </Modal>
  </div>
</template>
