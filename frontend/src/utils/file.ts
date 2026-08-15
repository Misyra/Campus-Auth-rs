/**
 * 文件与通用工具。
 * 从 legacy js/methods/utils.js 迁移：pickFile / downloadBlob / getBinaryName。
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
