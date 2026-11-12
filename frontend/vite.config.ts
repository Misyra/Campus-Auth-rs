/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import { resolve } from "path";

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      "@": resolve(__dirname, "src"),
    },
  },
  server: {
    proxy: {
      // changeOrigin: 后端启用 Host 头校验（DNS rebinding 防线，web::host），
      // 代理默认保留原始 Host（localhost:5173）会被拒，需改写为 target
      "/api": {
        target: "http://127.0.0.1:50721",
        changeOrigin: true,
      },
      "/ws": {
        target: "ws://127.0.0.1:50721",
        ws: true,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  base: "/",
  test: {
    // 纯函数与 composable 测试，无需 DOM
    environment: "node",
    include: ["src/**/*.test.ts"],
    // 全量并行冷启动时动态导入整棵单例图可抖过 5s；常规用例实测 <3s，
    // 10s 仅对真挂起生效
    testTimeout: 10_000,
    coverage: {
      // 覆盖率门禁（T4-4）：vitest run --coverage 时 thresholds 不达标即退出
      // 非零。provider 版本必须与 vitest 完全一致（package.json 精确 pin）。
      provider: "v8",
      // 只统计业务源码；测试文件自身不计入分母
      include: ["src/**"],
      exclude: ["src/**/*.test.ts"],
      reporter: ["text", "json-summary"],
      // thresholds 基于 2026-11-13 实测基线下调 5pp 设定（防 flaky 余量）：
      // 基线 lines 43.7 / statements 41.57 / functions 34.02 / branches 33.62
      // （vitest 5.1.0，63 测试文件 678 用例，views 组件大面积 0% 是主要拖累项）
      // 调整流程：先 `npm run test -- --coverage` 看实测值，再同步改这里的
      // 数字与基线注释
      thresholds: {
        lines: 38.7,
        statements: 36.5,
        functions: 29,
        branches: 28.6,
      },
    },
  },
});
