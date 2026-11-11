// @ts-check
import js from "@eslint/js";
import pluginVue from "eslint-plugin-vue";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist/**", "node_modules/**"] },

  js.configs.recommended,
  ...tseslint.configs.recommended,
  // 起步只取 essential（正确性类：缺 key、v-if/v-for 同级、重复属性等）；
  // strongly-recommended/recommended 里的风格规则（缩进/属性顺序/单行闭合）
  // 一次性全开会产出数百条报错，留待后续 PR 渐进（见 fixplaybook-D D3 渐进策略）
  ...pluginVue.configs["flat/essential"],

  // .vue 模板由 vue-eslint-parser 托管，<script lang="ts"> 内容交给 TS 解析器
  {
    files: ["**/*.vue"],
    languageOptions: {
      parserOptions: { parser: tseslint.parser, extraFileExtensions: [".vue"], sourceType: "module" },
    },
  },
  {
    rules: {
      // vue-tsc 才是未定义变量的守门员；no-undef 对 defineProps 等编译宏与
      // 浏览器全局只会误报（tseslint 已对 *.ts 关闭，此条覆盖 *.vue）
      "no-undef": "off",
      // logger.ts 的 5 处 console 均已带 eslint-disable 注释，本条是该口径的落地
      "no-console": "error",
      // 项目以 `_` 前缀表达"有意不用"（_isNew / _detectNetwork / (..._a)）
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", caughtErrors: "none" },
      ],
      // Modal.vue 是有意的单词命名，改文件名收益为负
      "vue/multi-word-component-names": "off",
      // 全仓既定模式：父组件传响应式草稿对象，子组件原地改成员字段
      // （HttpFlowFields/LoginChannelField/HttpResponseValues，代码注释明确写有
      // "原地写回草稿（宿主序列化比对即可感知为未保存改动）"）。shallowOnly
      // 把规则收缩到真正危险的形态——整个 prop 引用被替换（props.model = x
      // 或 v-model="model"）——该形态仍为 error；成员级改动是设计意图，不报。
      "vue/no-mutating-props": ["error", { shallowOnly: true }],
    },
  },
  // env.d.ts 的 *.vue shim 是历史样板：{} 与 any 是该形态的固定组成
  {
    files: ["src/env.d.ts"],
    rules: {
      "@typescript-eslint/no-explicit-any": "off",
      "@typescript-eslint/no-empty-object-type": "off",
    },
  },
);
