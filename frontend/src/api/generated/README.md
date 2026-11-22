# api/generated — OpenAPI 生成类型（T5-2a 管线）

本目录是 **openapi-typescript 自动生成链**的落点，全部内容围绕根目录 `openapi.json`（手写 baseline）运转：

| 文件 | 说明 |
|------|------|
| `api.d.ts` | `npm run typegen`（openapi-typescript 7.x）从 `../openapi.json` 生成的类型；头部 banner 为工具自带「自动生成勿手改」 |
| `index.ts` | re-export 入口：`paths` / `operations` / `components` / `webhooks` / `$defs`，手写代码从这里消费生成类型 |

## 管线命令

- `npm run typegen` — 重新生成 `api.d.ts`（参数：`openapi-typescript ../openapi.json -o src/api/generated/api.d.ts`；不加额外 flag，保持工具默认输出风格，便于 typegen:check 做逐字节比对）
- `npm run typegen:check` — 重新生成到系统临时目录并与入库的 `api.d.ts` 比对，不一致即失败（openapi.json 变更后未重新生成的兜底；CI 接线留给后续批次）

## 当前状态

**只建管线，不铺开替换（T5-2a）**：`src/api/index.ts` 等现有消费方仍使用手写的
`src/api/types.ts`（字段级契约的权威来源）；**T5-5 批次**才会把手写类型逐步切换到
本目录的生成类型，切换完成前两套类型并存，以手写为准。

## 约定

- 本目录整体加入 eslint 忽略（见 `eslint.config.mjs` 顶部的 ignores 注释）
- `api.d.ts` 纳入 tsconfig 编译范围（`tsconfig.app.json` 的 `src/**/*.ts` 已覆盖），
  配合 `skipLibCheck` 不参与深度类型检查，`index.ts` 的 re-export 保证可被消费
- 生成输出确定性（同一 openapi.json 输入恒产出同一文件），typegen:check 的逐字节比对因此成立
