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
  },
});
