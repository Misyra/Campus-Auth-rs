/**
 * 直连测试的单飞状态与结果（任务页编辑器与方案编辑器共用）。
 *
 * 两个宿主的差别只在**参数来源**：任务页用编辑器里的草稿 + 手填的测试账号，
 * 方案页用已保存的任务 id + 方案的凭据。发送、错误提示与单飞闸门在此收敛，
 * 免得两处各写一遍 toast 口径而漂移。
 *
 * 后端对直连测试有全局互斥（`WebOperations::http_login_test`），并发第二发会
 * 拿到 409「已有直连测试正在进行」，故前端只做「本次不发第二枪」的轻量防抖。
 */

import { ref } from "vue";
import { httpTasksApi } from "../api";
import { extractApiError } from "../api/client";
import { frontendLogger } from "../utils/logger";
import { useToast } from "./useToast";
import type { HttpLoginTestResult, HttpTaskTestPayload } from "../api/types";

const running = ref(false);
const result = ref<HttpLoginTestResult | null>(null);

/** 清空上一次结果（切换任务 / 改字段后调用，避免旧结论误导） */
function clearTestResult(): void {
  result.value = null;
}

/**
 * 发送一次直连测试。
 *
 * 不保存任何东西、不触发登录状态机；返回 null 表示没发出去（已在途 / 校验失败 / 异常），
 * 调用方据此只更新提示即可。
 */
async function runHttpTaskTest(payload: HttpTaskTestPayload): Promise<HttpLoginTestResult | null> {
  if (running.value) return null;
  const { toastOnly } = useToast();
  running.value = true;
  try {
    const report = await httpTasksApi.test(payload);
    result.value = report;
    toastOnly(report.outcome === "success", report.message);
    return report;
  } catch (error) {
    const message = extractApiError(error, "测试请求失败");
    frontendLogger.error("http-task", "直连测试异常: " + message, error);
    toastOnly(false, message);
    return null;
  } finally {
    running.value = false;
  }
}

export function useHttpTaskTest() {
  return {
    /** 是否有测试在途（按钮禁用 / 显示「正在发送…」） */
    running,
    /** 最近一次测试结果（null = 尚未测试） */
    result,
    runHttpTaskTest,
    clearTestResult,
  };
}
