# Campus-Auth Frontend

> Vue 3 + TypeScript + Vite 单页应用，构建产物 `frontend/dist/` 经 `rust-embed` 嵌入 Rust 二进制（见 `src/app.rs`）。

## 快速开始

```bash
cd frontend
npm ci            # 安装依赖（镜像已配 npmmirror，见根 AGENTS.md）
npm run dev       # 开发：Vite 5173 + 代理 /api,/ws → 127.0.0.1:50721（需先 cargo run --no-tray）
npm run build     # 构建：vue-tsc --noEmit && vite build → dist/（Rust 侧 cargo build 需先执行此步，或用 --features no-embed 跳过）
npm test          # 单测：vitest run（node 环境，src/**/*.test.ts）
npm run test:watch # 监听
npm run preview   # 预览 dist/
```

## 与 Rust 的依赖

- `frontend/dist/` 必须在 `cargo build` 前存在，否则 `rust-embed` 编译失败；开发期可用 `cargo check --features no-embed` 跳过嵌入。
- `frontend/src/api/types.ts` 为**唯一权威**手写契约（与后端 snake_case 字段名一致），**不存在 typegen 生成链路**：`npm run typegen` 及其 `openapi-typescript` 依赖已在 C6 移除（`openapi.json` 无 `components.schemas`、各响应 schema 全为 `{}`，生成的类型无实际约束力）。根 `openapi.json` 仅作 API **路径清单**参考（`.gitignore` 仍保留 `types.generated.ts` 规则以备将来宏化）。
- `vite.config.ts: proxy` 仅开发期生效，生产由 Rust `Axum` 同源 `127.0.0.1:50721` 提供。
- 更新状态：前端经 `GET /api/update-state` 回放 `update/last_check.json`（`src/api/types.ts: UpdateState`），与 `GET /api/check-update` 按 `UpdateChannel` 拉取的清单分离。

## 目录速览

```
frontend/
├── src/
│   ├── views/        # 页面（settings/ 等）
│   ├── router/       # vue-router
│   ├── api/          # API 客户端（types.ts 手写权威）
│   ├── composables/  # 组合式逻辑
│   ├── components/   # 通用组件
│   ├── utils/        # 工具
│   └── styles/       # 样式 token
├── public/           # 静态资源（logo.png/icons/），Vite 原样拷贝，区别于根 resources/（Rust 嵌入资源）与 dist/（构建产物）
├── dist/             # 构建产物（已忽略，不提交）
└── vite.config.ts    # alias @→src，outDir=dist，vitest=node
```
