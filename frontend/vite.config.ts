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
      "/api": "http://127.0.0.1:50721",
      "/ws": {
        target: "ws://127.0.0.1:50721",
        ws: true,
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
  },
});
