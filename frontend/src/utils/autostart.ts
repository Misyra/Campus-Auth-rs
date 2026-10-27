/** 将系统观测转换为自启动说明，避免把配置缓存或读取失败当作有效状态。 */
import type { AutostartStatus } from "../api/types";

/** 设置页和关于页共用的自启动状态文字。 */
export function autostartDescription(status: AutostartStatus): string {
  if (status.error) return status.error;
  switch (status.state) {
    case "unknown": return "无法确认 Windows 自启动审批状态，可重新启用后复核";
    case "blocked": return "Windows 已禁用此启动项，重新启用可恢复；也可取消注册";
    case "path_mismatch": return "注册路径与当前程序不符或文件已不存在，重新启用可修复";
    case "command_mismatch": return "启动参数或数据目录与当前实例不符，重新启用可修复";
    case "not_registered": return "系统中未注册自启动";
    case "enabled": return "系统注册与 Windows 审批检查通过";
    default: return status.enabled ? "已启用" : "未启用";
  }
}
