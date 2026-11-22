// openapi-typescript 生成类型的 re-export 入口（T5-2a 管线就绪）。
//
// 消费方式约定：
// - 本目录下的 api.d.ts 由 `npm run typegen` 从根目录 openapi.json 生成，
//   带工具自带 banner（自动生成勿手改），改动会被 `npm run typegen:check` 拦下。
// - T5-2a 只建管线不铺开：当前 src/api/index.ts 等消费方仍用手写 types.ts，
//   T5-5 批次才把手写类型逐步切换到本入口 re-export 的生成类型。
//
// webhooks / $defs 在当前 openapi.json 中恒为 Record<string, never>（无对应条目），
// 仍一并 re-export，让消费方在规范演进后无需改 import。

export type { paths, operations, components, webhooks, $defs } from "./api";
