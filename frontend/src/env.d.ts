/// <reference types="vite/client" />

// 历史上这里还有一段 `declare module "*.vue"` 的通配 shim，那是引入 vue-tsc 之前的
// 遗留：typecheck（vue-tsc）已原生解析 SFC 的真实类型，通配 shim 反而把所有组件
// 声明成 `DefineComponent<{}, {}, any>`，掩盖 props 与泛型的真实签名，故删除。
// 若工具链里出现不识别 .vue 的新入口，再按需补类型而非恢复该 shim。
