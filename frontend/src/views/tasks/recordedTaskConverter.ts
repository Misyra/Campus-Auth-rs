/**
 * 录制动作 → 浏览器任务步骤转换器（纯函数，无副作用、无 IO）。
 *
 * 输入是配置会话 stop 取回的 `RecordedAction[]`（Worker 侧按 ts 升序），输出是与
 * 任务编辑器「新建任务」种子步骤同一**最小字段集**的 `TaskConfig.steps` 形态——
 * 后端 `StepConfig` 反序列化时会为缺省字段补默认值（clear=true / duration=1000 /
 * required=true，见 src/tasks/models.rs 的 StepHelper），前端只写转换真正需要的键，
 * 不产出后端会原样存盘的 null 噪音。
 *
 * 映射口径（录制词表 → Worker 步骤类型）：
 * - `click`   → `click`（点击）
 * - `fill`    → `input`（键盘输入）
 * - `change`  → `select`（下拉选择）
 * - `navigate`→ `navigate`（页面跳转）
 *
 * `value` 原样透传：`{{USERNAME}}` / `{{PASSWORD}}` 占位符留给运行时的变量解析
 * 管线（浏览器任务已支持系统变量直填步骤值），前端不展开、不改写。唯一例外：
 * **首个带 URL 的 navigate 参数化为 `{{LOGIN_URL}}`**——配置会话自动打开的
 * 门户页即首个导航标记，保留字面量会让复用任务的浏览器被拉回录制时方案的
 * 旧门户（方案改址 / 换方案挂此任务即必败）；后续导航是用户真实浏览轨迹，
 * 字面量保留。
 *
 * iframe 链同理透传：非顶层 frame 内录制的动作携带 `frame`（CSS 选择器链，与
 * Worker 回放侧 `' >> '` 逐层下钻契约一致），丢弃会让步骤回放到主文档、命中
 * 错误元素或超时；顶层动作不产出该键（后端按缺省顶层处理）。
 */

import type { RecordedAction } from "@/api/types";

/** 转换产物步骤：id/type/description 恒在，其余按键类型按需出现 */
export interface RecordedTaskStep {
  /** 步骤标识：`recorded_N`（N 为保留步骤的 1 起序号） */
  id: string;
  /** Worker 步骤类型（click / input / select / navigate） */
  type: string;
  /** 人读描述（预览与编辑器共用） */
  description: string;
  /** 目标元素选择器（navigate 可缺省） */
  selector?: string;
  /** 填写值 / 选中值 / 跳转 URL（原样透传，占位符留给运行时） */
  value?: string;
  /** iframe CSS 选择器链（仅非顶层 frame 录制的动作携带；后端 StepConfig.frame 同名直传） */
  frame?: string;
}

/** 转换结果：`ok=false` 仅在无可转换动作（空序列 / 词表外动作）时出现 */
export type RecordedTaskConversion =
  | { ok: true; steps: RecordedTaskStep[]; /** 词表外被丢弃的动作数 */ skippedUnknown: number }
  | { ok: false; reason: "empty" };

/** iframe 动作的帧定位透传：仅非顶层 frame 录制的动作携带，顶层动作不产出
 * 该键（保持产物最小字段集，后端 StepConfig.frame 缺省按顶层处理） */
function frameField(action: RecordedAction): { frame?: string } {
  return action.frame === undefined ? {} : { frame: action.frame };
}

/** 单个动作 → 单个步骤；词表外类型返回 null（丢弃并计数，不让一个未知值中断整段转换） */
function convertOne(
  action: RecordedAction,
  index: number,
  parameterizePortal: boolean,
): RecordedTaskStep | null {
  const id = `recorded_${index}`;
  switch (action.type) {
    case "click":
      return {
        id,
        type: "click",
        description: `点击 ${action.selector}`,
        selector: action.selector,
        ...frameField(action),
      };
    case "fill":
      return {
        id,
        type: "input",
        description: `在 ${action.selector} 中输入`,
        selector: action.selector,
        value: action.value,
        ...frameField(action),
      };
    case "change":
      return {
        id,
        type: "select",
        description: `选择 ${action.selector}：${action.value}`,
        selector: action.selector,
        value: action.value,
        ...frameField(action),
      };
    case "navigate":
      // 首个带 URL 的导航 = 配置会话自动打开的门户页：参数化为 {{LOGIN_URL}}
      // （任务本体已先打开同值，重复导航幂等且随方案走）；后续导航是真实
      // 浏览轨迹，value 原样透传。纯标记动作（无 URL）保留，运行时缺目标
      // 地址会以明确的步骤失败暴露，不在此静默吞掉
      if (parameterizePortal) {
        return { id, type: "navigate", description: "跳转登录页 {{LOGIN_URL}}", value: "{{LOGIN_URL}}" };
      }
      return action.value === undefined
        ? { id, type: "navigate", description: "页面跳转" }
        : { id, type: "navigate", description: `跳转 ${action.value}`, value: action.value };
    default:
      return null;
  }
}

/**
 * 动作序列 → 任务步骤序列。
 *
 * 空序列（或全是词表外动作）返回 `{ ok: false, reason: "empty" }`——后端
 * validate_task 拒绝空 steps，转出来的任务存得下但必然失败，必须在转换层拦下。
 */
export function convertRecordedActions(actions: readonly RecordedAction[]): RecordedTaskConversion {
  const steps: RecordedTaskStep[] = [];
  let skippedUnknown = 0;
  let portalParameterized = false;
  for (const action of actions) {
    const isPortalCandidate =
      action.type === "navigate" && action.value !== undefined && !portalParameterized;
    const step = convertOne(action, steps.length + 1, isPortalCandidate);
    if (step) {
      if (isPortalCandidate) portalParameterized = true;
      steps.push(step);
    } else {
      skippedUnknown += 1;
    }
  }
  if (!steps.length) return { ok: false, reason: "empty" };
  return { ok: true, steps, skippedUnknown };
}
