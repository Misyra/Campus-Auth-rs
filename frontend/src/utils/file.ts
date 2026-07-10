/**
 * 文件与通用工具。
 * 从 legacy js/methods/utils.js 迁移：pickFile / downloadBlob / getBinaryName / safeApiCall。
 */

/** 打开文件选择对话框 */
export function pickFile(accept = ""): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.onchange = (e) => {
      resolve((e.target as HTMLInputElement).files?.[0] || null);
      input.value = "";
      input.onchange = null;
    };
    input.click();
  });
}

/** 触发浏览器下载 Blob 数据 */
export function downloadBlob(data: BlobPart, filename: string, mimeType = "application/octet-stream"): void {
  const blob = new Blob([data], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** 从路径中提取程序名（去目录与扩展名） */
export function getBinaryName(path: string): string {
  if (!path) return "Python";
  const name = path.split(/[/\\]/).pop() || path;
  return name.replace(/\.(exe|cmd|bat|sh)$/i, "") || name;
}

/** 包装 API 调用，统一处理错误 toast */
export async function safeApiCall<T>(
  fn: () => Promise<T>,
  fallbackMsg = "操作失败",
  onError?: (msg: string) => void,
): Promise<T | null> {
  try {
    return await fn();
  } catch (error) {
    const msg = extractError(error, fallbackMsg);
    onError?.(msg);
    return null;
  }
}

/** 从异常提取用户友好消息（与 api/client.extractApiError 一致，避免循环依赖在此复制） */
function extractError(error: unknown, fallback = "操作失败"): string {
  const detail = (error as { response?: { data?: { detail?: unknown; message?: string } } })?.response?.data;
  if (detail) {
    const d = detail.detail;
    if (Array.isArray(d)) {
      return (
        d
          .map((x) => {
            if (typeof x === "string") return x;
            const loc = x?.loc ? `[${x.loc[x.loc.length - 1]}] ` : "";
            return loc + (x?.msg || x?.detail || String(x));
          })
          .join("; ") || fallback
      );
    }
    if (typeof d === "string") return d;
    if (detail.message) return detail.message;
  }
  return (error as { message?: string })?.message || fallback;
}
