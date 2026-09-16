/**
 * 运行模式预设（默认 / 调试 / 自定义）。
 *
 * 「运行模式」是一组设置的命名组合，让用户在「稳定跑」与「看得见、好排查」之间
 * 一键切换，而不必逐项去猜该改哪几个开关。
 *
 * 设计约束（调研后确定，勿轻易放宽）：
 *
 * 1. **只有 `PATCH /api/config` 白名单内的项才进预设主体**。跨多个 API 的"一键切换"
 *    不是原子的：中途失败会留下半套配置（例如浏览器已改成前台、自启动却没注册上），
 *    而这种不一致用户很难察觉。故预设主体只包含能一次提交的项。
 *
 * 2. **`logging.level` 是唯一例外，走专用 `PUT /api/config/log-level`**。单靠 PATCH
 *    写 `logging.level` 只落盘、**不热更新运行时 filter**（见 `set_log_level` 里的
 *    `reload_log_level` 调用），会出现「界面显示 DEBUG、实际仍按 INFO 过滤」的静默
 *    假象。专用接口是写入该字段的唯一正确方式，故必须用它。
 *
 * 3. **开机自启不在 PATCH 白名单内**（`autostart_enabled` 由 `POST /api/autostart/*`
 *    驱动，且会真实写系统注册表）。用户明确要求纳入预设，故单列一个 `autostart`
 *    字段由调用方单独调用；它幂等且与其他项互不依赖，失败不会污染其余设置。
 *
 * 4. **本模块是纯函数**，不碰 API、不碰响应式状态，便于直接单测。
 */

/** 运行模式：两种预设 + 一个"已手动改过"的兜底态 */
export type RunModeId = "default" | "debug" | "custom";

/**
 * 参与模式判定的设置集合。
 *
 * 只列进预设的项：判定「当前属于哪个模式」时，用**同一组字段**与预设逐一比对，
 * 多列一个字段会让"自定义"被无关改动误触发，少列一个会让模式名与实际不符。
 *
 * **刻意不含 `strict_login_mode`**：它决定"何时触发登录"，属功能行为而非可观测性。
 * 把它放进调试模式会让登录在证据不足时也尝试，可能在用户没预期的时机弹出浏览器
 * ——那是修 bug 的手段，不是"方便排查"的开关，故不纳入预设。
 */
export interface RunModeSettings {
  /** 浏览器后台运行（headless） */
  headless: boolean;
  /** 低资源模式（不加载图片） */
  low_resource_mode: boolean;
  /** 登录后保持浏览器进程（便于反复查看现场） */
  keep_alive: boolean;
  /** 启动后执行的动作 */
  startup_action: string;
  /** 日志级别 */
  log_level: string;
  /** 开机自启（不在 PATCH 白名单，单独调用 API） */
  autostart: boolean;
  /**
   * 是否启用暂停时段（默认模式 true：23:00–06:00 夜间不自动登录；
   * 调试模式 false：随时手动复现不被暂停窗口拦住）。
   * 只预设启用与否，不预设时段起止——起止是用户可调的具体值，覆盖它们会
   * 抹掉用户自定义的时段；「启用暂停时段」在「设置 · 检测」页可改。
   */
  pause_enabled: boolean;
}

/** 一个预设的完整定义 */
export interface RunModeDefinition {
  id: Exclude<RunModeId, "custom">;
  label: string;
  /** 段落式说明：这个模式适合谁 */
  description: string;
  /** 该模式会写入的设置值 */
  settings: RunModeSettings;
}

/**
 * 预设定义。
 *
 * 「启动后执行」的取舍：调试模式设 `none`，因为调试时希望「手动跑一次、看现场」，
 * 若启动就自动登录，会在你还没打开浏览器时就已经跑完一轮，反而看不到过程。
 * 默认模式设 `monitor`，即"开机就连"——这正是自动认证的用途。
 *
 * `low_resource_mode` 两组都关：调试时关掉它才能看见验证码图；默认模式也没有
 * 理由为省资源而牺牲兼容性（图片验证码任务会失败）。故它虽是预设字段，但两组
 * 取值相同——`diffRunMode` 会正确地不把它列为差异。
 */
export const RUN_MODE_PRESETS: readonly RunModeDefinition[] = [
  {
    id: "default",
    label: "默认模式",
    description: "日常使用：浏览器后台静默运行，启动后自动检测并在断线时重连。",
    settings: {
      headless: true,
      low_resource_mode: false,
      keep_alive: false,
      startup_action: "monitor",
      log_level: "INFO",
      autostart: true,
      // 夜间（默认 23:00–06:00）不自动登录：宿舍定时断网时段反复重连无意义
      pause_enabled: true,
    },
  },
  {
    id: "debug",
    label: "调试模式",
    description:
      "排查问题时使用：显示浏览器窗口、保留浏览器进程、记录调试日志、启动后不自动登录，便于手动复现一次并观察全过程。",
    settings: {
      headless: false,
      // 不加载图片会让验证码图看不见，调试时反而更难判断，故明确关闭
      low_resource_mode: false,
      keep_alive: true,
      startup_action: "none",
      log_level: "DEBUG",
      autostart: false,
      // 调试要随时手动复现：若落在暂停窗口里，登录会被引擎拦住，看似"没反应"
      pause_enabled: false,
    },
  },
] as const;

/** 该项的用户可见名称（差异提示里用，避免暴露字段名） */
export const RUN_MODE_FIELD_LABELS: Record<keyof RunModeSettings, string> = {
  headless: "浏览器后台运行",
  low_resource_mode: "低资源模式",
  keep_alive: "登录后保持浏览器进程",
  startup_action: "启动后执行",
  log_level: "日志级别",
  autostart: "开机自启动",
  pause_enabled: "启用暂停时段",
};

/** 可读值格式化（差异提示里用，布尔转「开启/关闭」而非 true/false） */
export function formatRunModeValue(
  key: keyof RunModeSettings,
  value: boolean | string,
): string {
  if (key === "startup_action") {
    return (
      { monitor: "开始检测", login_once: "登录一次后退出", none: "无操作" }[
        String(value)
      ] ?? String(value)
    );
  }
  if (typeof value === "boolean") return value ? "开启" : "关闭";
  return String(value);
}

/**
 * 判定当前设置属于哪个模式。
 *
 * 与每个预设逐字段比对，**任一字段不符即不匹配**；两个预设都不匹配则为 `custom`。
 * 不做"部分匹配度最高者胜"的模糊判定——那会让界面显示一个用户并没选过的模式名，
 * 比老老实实显示"自定义"更误导。
 */
export function detectRunMode(current: RunModeSettings): RunModeId {
  for (const preset of RUN_MODE_PRESETS) {
    if (matchesPreset(current, preset.settings)) return preset.id;
  }
  return "custom";
}

/** 当前设置是否与某个预设完全一致（导出供测试与差异计算复用） */
export function matchesPreset(
  current: RunModeSettings,
  target: RunModeSettings,
): boolean {
  return (Object.keys(target) as Array<keyof RunModeSettings>).every(
    (key) => normalize(current[key]) === normalize(target[key]),
  );
}

/**
 * 归一化后比较：日志级别大小写与别名（WARNING/WARN）不应造成"其实是同一模式却显示自定义"。
 * 后端会把 WARNING 归一为 WARN（见 `set_log_level`），前端比较必须同口径。
 */
function normalize(value: boolean | string): string {
  if (typeof value === "boolean") return value ? "1" : "0";
  const s = String(value).trim().toUpperCase();
  return s === "WARNING" ? "WARN" : s;
}

/** 一处待变更的设置 */
export interface RunModeChange {
  key: keyof RunModeSettings;
  label: string;
  from: string;
  to: string;
}

/**
 * 计算从当前设置切到目标预设需要改动哪些项——只列出**实际有变化**的，
 * 供确认弹窗如实展示"将要改什么"，而不是笼统说"将应用默认模式"。
 */
export function diffRunMode(
  current: RunModeSettings,
  target: RunModeSettings,
): RunModeChange[] {
  const changes: RunModeChange[] = [];
  for (const key of Object.keys(target) as Array<keyof RunModeSettings>) {
    const from = current[key];
    const to = target[key];
    if (normalize(from) === normalize(to)) continue;
    changes.push({
      key,
      label: RUN_MODE_FIELD_LABELS[key],
      from: formatRunModeValue(key, from),
      to: formatRunModeValue(key, to),
    });
  }
  return changes;
}

/** 按 id 取预设定义；`custom` 无定义，返回 undefined */
export function getRunModePreset(
  id: RunModeId,
): RunModeDefinition | undefined {
  return RUN_MODE_PRESETS.find((p) => p.id === id);
}
