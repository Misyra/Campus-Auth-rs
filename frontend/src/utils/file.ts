/**
 * 文件与通用工具。
 * 从 legacy js/methods/utils.js 迁移：pickFile / downloadBlob / getBinaryName。
 */

/** 打开文件选择对话框（支持取消回退，避免 Promise 永久挂起） */
export function pickFile(accept = ""): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    let done = false;
    const cleanup = () => {
      input.onchange = null;
      (input as unknown as { oncancel: unknown }).oncancel = null;
      window.removeEventListener("focus", onFocus);
    };
    const finish = (file: File | null) => {
      if (done) return;
      done = true;
      cleanup();
      resolve(file);
      // 延迟移除，避免同步移除影响部分浏览器的 change 事件派发
      setTimeout(() => input.remove(), 0);
    };
    input.onchange = (e) => {
      finish((e.target as HTMLInputElement).files?.[0] || null);
    };
    // Chrome 119+ 支持 oncancel（用户取消对话框时触发）
    (input as unknown as { oncancel: ((() => void) | null) }).oncancel = () => finish(null);
    const onFocus = () => {
      // 对话框关闭后窗口重新获得焦点：若无选中文件则视为取消
      setTimeout(() => {
        if (!done && !input.files?.length) finish(null);
      }, 300);
    };
    window.addEventListener("focus", onFocus, { once: true });
    input.click();
    // 兜底：若浏览器不支持 oncancel 且 focus 未触发（如非用户手势），超时后清理
    setTimeout(() => {
      if (!done && !input.files?.length && document.hasFocus()) {
        // 已有焦点但无文件，说明对话框已关闭且未选择
        // 由 focus 监听兜底，此处不再额外处理，避免重复 resolve
      }
    }, 2000);
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
