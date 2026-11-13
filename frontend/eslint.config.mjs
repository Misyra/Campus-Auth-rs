// @ts-check
import js from "@eslint/js";
import pluginVue from "eslint-plugin-vue";
import tseslint from "typescript-eslint";

export default tseslint.config(
  // src/api/generated 为 openapi-typescript 生成物（npm run typegen 产物，
  // 见目录内 README），整体豁免 lint——生成代码风格不归人工规则管
  { ignores: ["dist/**", "node_modules/**", "src/api/generated/**"] },

  js.configs.recommended,
  ...tseslint.configs.recommended,
  // strongly-recommended：在 essential 的正确性类之上加可读性约定（v-for 带
  // key、v-if/v-for 优先级、组件名风格、props 定义形态等）。
  // 紧随其后的 no-layout-rules 是 v10 官方规则补丁，整体关掉排版类规则
  // （html-indent / max-attributes-per-line / html-self-closing 等 9+ 条）：
  // 本仓无 Prettier 权威、模板排版以人工为准，一次性按排版规则重排全仓
  // 模板属于纯噪音 diff，故显式剥离（早年 essential 起步决策的正式收敛）
  ...pluginVue.configs["flat/strongly-recommended"],
  pluginVue.configs["no-layout-rules"],

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
  // 两个挂载测试文件的"多组件"是有意的测试基建：AppTopbar.notif.test.ts 的
  // 路由桩只为注入 router、外层壳只为挂载被测组件；Modal.focus.test.ts 的
  // mountHost 是文件头注释明确文档化的宿主组件（触发按钮 + Modal 模拟真实
  // 使用方）。匿名桩不承载业务语义，拆成独立文件收益为负
  {
    files: [
      "src/components/common/AppTopbar.notif.test.ts",
      "src/components/common/Modal.focus.test.ts",
    ],
    rules: {
      "vue/one-component-per-file": "off",
    },
  },
);
