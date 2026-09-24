/**
 * 全局设置里数值字段的合法区间 —— **单一出处**。
 *
 * 为什么需要它：设置页的数字输入框各自声明 `min` / `max`，但浏览器**不会**因此拦住
 * 手工输入或程序化赋值（页面用的是 `<form @submit.prevent>`，没有表单提交校验这条路径）。
 * 于是「区间由界面声明、由后端兜底、前端保存前不校验」三者之间没有任何一致性约束：
 * 空串（`v-model.number` 得到 NaN）、越界值都会被原样 PATCH 出去。
 * 端口那一项甚至已经出现了漂移——输入框写 `min="1024"`，而 `validateConfig` 放行
 * 1–65535（见下方 `app_settings.port` 的注释）。
 *
 * 收录规则：**只收录界面明确声明了区间的字段**。没有对应输入框的字段（如
 * `monitor.script_timeout`、`updater.check_interval_hours`）不在此处校验，
 * 免得凭空收紧后端本来允许的值——那份判断留给后端。
 *
 * `configRanges.test.ts` 会读设置页模板，断言「界面声明的区间 == 本表」，
 * 两侧任何一处改动导致不一致都会在 CI 失败。
 */

export interface NumericRange {
  min: number;
  max: number;
  /** 出错提示里用的字段中文名 */
  label: string;
}

export const CONFIG_RANGES: Record<string, NumericRange> = {
  "browser.timeout": { min: 1, max: 120, label: "浏览器操作超时（秒）" },
  "browser.navigation_timeout": { min: 3, max: 120, label: "页面导航超时（秒）" },
  "browser.login_timeout": { min: 10, max: 600, label: "登录超时（秒）" },
  "browser.viewport_width": { min: 320, max: 3840, label: "视口宽度" },
  "browser.viewport_height": { min: 240, max: 2160, label: "视口高度" },
  "worker.idle_timeout_seconds": { min: 60, max: 3600, label: "Worker 空闲超时（秒）" },
  "monitor.check_interval_seconds": { min: 20, max: 1200, label: "检测间隔（秒）" },
  "monitor.network_check_timeout": { min: 1, max: 30, label: "网络检测超时（秒）" },
  "monitor.post_login_delay": { min: 0, max: 60, label: "登录后等待（秒）" },
  "retry.max_retries": { min: 1, max: 5, label: "最大重试次数" },
  "retry.retry_interval": { min: 1, max: 300, label: "重试间隔（秒）" },
  "pause.start_hour": { min: 0, max: 23, label: "暂停开始（时）" },
  "pause.end_hour": { min: 0, max: 23, label: "暂停结束（时）" },
  "logging.retention_days": { min: 1, max: 365, label: "日志保留天数" },
  // 端口上限与后端一致，下限取 1 而不是输入框原先写的 1024：
  // 1024 以下并非"非法"（有权限时监听 80 是合理用法），把它写成硬错误会拦住既有配置。
  // 输入框已同步改成 min=1，低于 1024 时改为给一条"需要特权"的**警告**（见 validateRangeValues）。
  "app_settings.port": { min: 1, max: 65535, label: "控制台端口" },
};

/** 低于此值的端口在多数系统上需要管理员/root 权限才能监听 */
const PRIVILEGED_PORT_BELOW = 1024;

function isPlainNumber(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

/**
 * 按 `CONFIG_RANGES` 校验一组扁平化的取值。
 *
 * **分级原则**（重要，别把它改成"越界即拦"）：
 *
 * - `errors`（阻断保存）只放**定义上不可能正确**的值：
 *   NaN / 非整数（清空输入框就会得到，且参与比较恒为 false，是最容易漏的一类），
 *   以及端口越出 1–65535（超出必然起不来）。
 * - `warnings`（提示但不拦）放"越出界面建议区间"的值：
 *   后端这些字段是**裸 u32 直收、不做任何钳制**（`src/config/schema.rs`），
 *   前端若把建议区间当成硬闸门，就会出现"昨天还能保存的配置今天保存不了"——
 *   `retry.max_retries: 0`（不重试）、`worker.idle_timeout_seconds: 0` 都是
 *   合法且有人用的取值，而界面只是按常用范围写了 min=1。
 *
 * 前端这一层的价值是"当场告诉用户哪里可疑"，不是替后端收紧它本来允许的输入。
 *
 * `values` 的键与 `CONFIG_RANGES` 相同（形如 `browser.timeout`），
 * 值为 `undefined` 时视为"该字段不在本次表单里"，跳过——不把缺失当非法。
 */
export function validateRangeValues(values: Readonly<Record<string, unknown>>): {
  errors: string[];
  warnings: string[];
} {
  const errors: string[] = [];
  const warnings: string[] = [];

  for (const [key, range] of Object.entries(CONFIG_RANGES)) {
    if (!(key in values)) continue;
    const value = values[key];
    if (value === undefined) continue;

    if (!isPlainNumber(value) || !Number.isInteger(value)) {
      errors.push(`${range.label}必须是整数`);
      continue;
    }

    // 端口是唯一"越界必然起不来"的字段，故它越界算硬错误
    if (key === "app_settings.port") {
      if (value < range.min || value > range.max) {
        errors.push(`${range.label}必须在 ${range.min}-${range.max} 之间`);
      } else if (value < PRIVILEGED_PORT_BELOW) {
        warnings.push(
          `${range.label}低于 ${PRIVILEGED_PORT_BELOW}，多数系统需要管理员权限才能监听`,
        );
      }
      continue;
    }

    if (value < range.min || value > range.max) {
      warnings.push(`${range.label}建议在 ${range.min}-${range.max} 之间，当前为 ${value}`);
    }
  }

  return { errors, warnings };
}
