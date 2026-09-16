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

/**
 * 生成方案分享文件名：`campus-auth-profile-<name>.json`。
 *
 * 方案名是用户可输入的自由文本（含中文、空格、路径分隔符等），既不能直接进
 * 文件名（Windows 非法字符会导致下载失败），也不能全丢（用户靠名字区分文件）。
 * 保留中文与字母数字，其余折叠为 `-`。
 */
export function shareFileName(profileName: string, profileId: string): string {
  const source = profileName.trim() || profileId.trim() || "profile";
  const safe = source
    .replace(/[/\\:*?"<>|\u0000-\u001f]/g, "-")
    .replace(/\s+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^[-.]+|[-.]+$/g, "")
    .slice(0, 60);
  return `campus-auth-profile-${safe || "profile"}.json`;
}

/**
 * 展开分享载荷的信封包裹，返回内层根对象。
 *
 * 后端导出为顶层形态，但导入时可能拿到 `{ data: {...} }`（API 信封）包裹的
 * 文件（例如人为保存了接口响应）。与后端 `parse_share_payload` 同一口径。
 */
export function unwrapSharePayload(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const obj = value as Record<string, unknown>;
  const inner = obj.data;
  if (inner && typeof inner === "object" && !Array.isArray(inner)) {
    return inner as Record<string, unknown>;
  }
  return obj;
}

/**
 * 判定是否为本应用导出的方案分享载荷。
 *
 * 只看格式标记 `campus_auth_profile`（数字）与 `profile`（对象）是否存在，
 * 不猜字段结构——后端同样以该标记为准，前端提前判定只为给出更快的提示。
 *
 * 断言为 `Record<string, unknown>` 而非具体的 `ProfileSharePayload`：本函数在
 * `utils/` 层，不应依赖 `api/types`（那会让工具层反向依赖接口层）；调用方拿到
 * 宽类型后自行按需取值。全字段的类型校验交给后端。
 */
export function isProfileSharePayload(
  value: unknown,
): value is Record<string, unknown> & { campus_auth_profile: number; profile: Record<string, unknown> } {
  const root = unwrapSharePayload(value);
  if (!root) return false;
  if (typeof root.campus_auth_profile !== "number") return false;
  const profile = root.profile;
  return !!profile && typeof profile === "object" && !Array.isArray(profile);
}
