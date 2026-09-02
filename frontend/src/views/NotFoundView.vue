<script setup lang="ts">
import { useRouter } from "vue-router";

const router = useRouter();
</script>

<template>
  <div class="not-found-page">
    <div class="not-found-card">
      <div class="not-found-icon">404</div>
      <h1>页面未找到</h1>
      <p>你访问的页面不存在或已被移动。</p>
      <button class="btn btn-primary" @click="router.push({ name: 'dashboard' })">
        返回仪表盘
      </button>
    </div>
  </div>
</template>

<style scoped>
/* 404 页为装饰性大字号，px 直接对齐全局字号档位 */
.not-found-page {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: calc(100vh - 56px);
  padding: var(--space-xl);
}

.not-found-card {
  text-align: center;
  max-width: 400px;
}

.not-found-icon {
  font-size: 80px;
  font-weight: 700;
  color: var(--text-muted);
  line-height: 1.2;
}

.not-found-card h1 {
  margin: var(--space-sm) 0;
  font-size: var(--text-xl);
}

.not-found-card p {
  color: var(--text-secondary);
  margin-bottom: var(--space-lg);
}
</style>
