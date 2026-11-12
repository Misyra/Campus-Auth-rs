/**
 * 命令面板的命令集：构建与关键词过滤（纯函数 + 单例入口编排）。
 *
 * 只被 CommandPalette.vue（懒加载 chunk）与本目录测试引用，不进首屏 chunk。
 * 命令全部复用既有单例入口（不新增 API）：导航走 router（任务/设置子项的
 * 名称与图标取自 navTree 单一事实源，OpenWrt 裁剪随 supportedNav 自动生效）；
 * 动作复用 useUi / useUpdateDialog / useRepoImport；主题切换写 useAppearance。
 *
 * 有意不提供的命令（现状无独立入口，做成命令只会伪装成可用）：
 * - 打开调试面板：面板随调试会话开启，DebugPanel 的关闭即 stopDebug，无"只开面板"路径；
 * - 打开日志面板：实时日志内嵌在仪表盘页，「仪表盘」导航已覆盖。
 */
import { router } from "../router";
import { useUi } from "./useUi";
import { useStatus } from "./useStatus";
import { useAppearance } from "./useAppearance";
import { useUpdateDialog } from "./useUpdateDialog";
import { useRepoImport } from "./useRepoImport";
import { useSettingsNavigation } from "./useSettingsNavigation";
import { SETTINGS_NAV_CHILDREN, TASK_NAV_CHILDREN, type NavChild } from "../utils/navTree";
import type { IconName } from "@/components/common/IconApp.vue";

/** 面板内分组标题；命令按注册顺序排列，同组自然连续 */
export type CommandGroup = "导航" | "操作" | "外观";

export interface PaletteCommand {
  id: string;
  label: string;
  group: CommandGroup;
  icon?: IconName;
  /** 额外匹配词（英文别名等小写文本），过滤时与 label/group 一并参与匹配 */
  keywords?: string;
  /** 可用性判定：本次打开返回 false 的命令不出现在列表（如「取消登录」仅在登录进行中可用） */
  when?: () => boolean;
  run: () => void;
}

/** 多关键词空格分隔需全部命中（同 filterRepoTasks 口径），大小写不敏感 */
export function filterCommands(commands: PaletteCommand[], query: string): PaletteCommand[] {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return commands;
  return commands.filter((command) => {
    const haystack = [command.label, command.group, command.keywords ?? ""].join(" ").toLowerCase();
    return terms.every((term) => haystack.includes(term));
  });
}

/** 构建当前命令集：每次打开调用一次，`when` 可用性与「启动/停止监测」动态文案即时取值 */
export function buildCommandItems(): PaletteCommand[] {
  const { manualLogin, cancelLogin, toggleMonitor } = useUi();
  const { status, busy } = useStatus();
  const { appearance } = useAppearance();
  const { openDialog } = useUpdateDialog();
  const { showRepoImport } = useRepoImport();
  const { requestScroll } = useSettingsNavigation();

  const navigate = (name: string): (() => void) => () => {
    void router.push({ name });
  };
  // 设置分区与侧栏 navigateSettings 同口径：已在目标分区时仅滚动定位，不再 push
  const gotoSettingsChild = (child: NavChild): (() => void) => () => {
    if (router.currentRoute.value.name === child.name) requestScroll(child.id);
    else void router.push({ name: child.name });
  };
  const taskCommands = TASK_NAV_CHILDREN.map((child) => ({
    id: `nav-task-${child.id}`,
    group: "导航" as const,
    label: child.label,
    icon: child.icon,
    keywords: child.title,
    run: navigate(child.name),
  }));
  const settingsCommands = SETTINGS_NAV_CHILDREN.map((child) => ({
    id: `nav-settings-${child.id}`,
    group: "导航" as const,
    label: `设置 · ${child.label}`,
    icon: child.icon,
    keywords: child.title,
    run: gotoSettingsChild(child),
  }));

  const commands: PaletteCommand[] = [
    {
      id: "nav-dashboard",
      group: "导航",
      label: "仪表盘",
      icon: "grid",
      keywords: "dashboard 首页 状态 日志",
      run: navigate("dashboard"),
    },
    {
      id: "nav-profiles",
      group: "导航",
      label: "配置方案",
      icon: "user",
      keywords: "profiles 方案 账号 认证",
      run: navigate("profiles"),
    },
    ...taskCommands,
    ...settingsCommands,
    { id: "nav-about", group: "导航", label: "关于", icon: "info", keywords: "about 版本", run: navigate("about") },
    {
      id: "act-login",
      group: "操作",
      label: "立即登录",
      icon: "log-in",
      keywords: "login 手动登录 认证",
      when: () => !busy.login,
      run: manualLogin,
    },
    {
      id: "act-cancel-login",
      group: "操作",
      label: "取消登录",
      icon: "close",
      keywords: "cancel 停止登录",
      when: () => busy.login,
      run: cancelLogin,
    },
    {
      id: "act-toggle-monitor",
      group: "操作",
      label: status.monitoring ? "停止网络监测" : "启动网络监测",
      icon: status.monitoring ? "pause" : "play",
      keywords: "monitor 检测 暂停 恢复 启动 停止",
      run: toggleMonitor,
    },
    {
      id: "act-check-update",
      group: "操作",
      label: "检查更新",
      icon: "refresh",
      keywords: "update 版本 升级",
      run: () => {
        void openDialog();
      },
    },
    {
      id: "act-import",
      group: "操作",
      label: "导入任务…",
      icon: "download",
      keywords: "import 仓库 云端",
      run: showRepoImport,
    },
    {
      id: "theme-light",
      group: "外观",
      label: "主题：浅色",
      icon: "sun",
      keywords: "theme 亮色 白天",
      when: () => appearance.theme !== "light",
      run: () => {
        appearance.theme = "light";
      },
    },
    {
      id: "theme-dark",
      group: "外观",
      label: "主题：深色",
      icon: "contrast",
      keywords: "theme 暗色 夜间",
      when: () => appearance.theme !== "dark",
      run: () => {
        appearance.theme = "dark";
      },
    },
    {
      id: "theme-auto",
      group: "外观",
      label: "主题：跟随系统",
      icon: "monitor",
      keywords: "theme 自动 prefers-color-scheme",
      when: () => appearance.theme !== "auto",
      run: () => {
        appearance.theme = "auto";
      },
    },
  ];
  return commands.filter((command) => !command.when || command.when());
}
