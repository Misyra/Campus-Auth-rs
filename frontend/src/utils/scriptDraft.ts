/**
 * 脚本（`type: "script"`）的草稿形态、落盘载荷与缺口校验。
 *
 * 与 `utils/httpTask` 同构：草稿是**平铺字段**（一个字段一个输入控件，脚本内容是一段
 * 文本），落盘与接口用后端的 script 载荷。这几个判定此前长在 `useScripts.saveScript`
 * 里、只能靠点按钮触发，且**只在显式保存时**校验；脚本面板改为自动保存后，同一套判定
 * 要在"每次 debounce"这条高频路径上跑，故抽到这里由 vitest 直接盯住：
 * ID 形态、内容非空、内容体积、执行程序（自定义路径必填 / 不支持 PowerShell）。
 *
 * 脚本的 ID 是**文件名**（`<base>/tasks/scripts/<id>.json` 的 stem；脚本正文在该 JSON
 * 的 `content` 字段里，`<id>.<ext>` 只出现在"导出"文件名上），也是「定时任务」
 * 指向脚本的引用值，故创建后不可修改——新建时先由用户命名，ID 合法前不落盘
 * （见 `scriptDraftGaps`，与直连任务"种子即落盘"的差别正在于此：直连任务的 ID 没有
 * 外部引用，随便取一个 `untitled_N` 就行，脚本 ID 却是用户要认的名字）。
 */

/**
 * 脚本 ID 形态：与后端 `is_valid_task_id` 同口径（1~64 位 ASCII 字母、数字、下划线或连字符）。
 *
 * 这里曾写得更严（要求字母开头、不收连字符），代价是"后端收得下、前端存不上"：
 * 一个 ID 为 `my-script` 或 `2fa` 的脚本（`POST /api/tasks`、导入、或历史上手工放进
 * `tasks/scripts/` 都能产生）打开后 `scriptDraftGaps` 恒非空 → 自动保存永远被跳过，
 * 而缺口点名的那个字段在面板上是**禁用**的（落盘后 ID 固定）——改不动、也存不下，
 * 且没有任何报错。两边同源才不会有这种死角。
 */
export const SCRIPT_ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

/** 脚本内容体积上限（后端同值；超出由后端拒绝，这里提前拦以保住自动保存的语义） */
export const SCRIPT_MAX_BYTES = 100 * 1024;

/** 执行程序下拉的"自定义"哨兵值（不是真实路径，只在本模块与面板之间流转） */
export const SCRIPT_CUSTOM_BINARY = "__custom__";

/** 脚本编辑草稿（平铺字段 + 落盘状态标记） */
export interface ScriptDraft {
  /** 脚本 ID = 文件名 stem；`_isNew` 为真时可改，落盘后不可改 */
  id: string;
  name: string;
  description: string;
  content: string;
  /** 执行程序：空串 = 项目内 Python，`__custom__` = 下方手填路径，其余为可执行文件路径 */
  binary_path: string;
  /** `binary_path === "__custom__"` 时的实际路径（界面态，不落盘） */
  _customBinary: string;
  /**
   * 尚未落盘（新建尚未命名 / 导入的新 ID）。
   *
   * 与直连任务同名的字段语义一致：只影响"是否允许改 ID"与展示，不参与落盘载荷。
   */
  _isNew: boolean;
}

/** 新建脚本的空草稿（ID 留空：由用户命名后才会落盘） */
export function emptyScriptDraft(stub: string): ScriptDraft {
  return {
    id: "",
    name: "",
    description: "",
    content: stub,
    binary_path: "",
    _customBinary: "",
    _isNew: true,
  };
}

/** 草稿里实际生效的执行程序路径（`__custom__` 取手填值，其余原样） */
export function resolveScriptBinaryPath(draft: ScriptDraft): string {
  return draft.binary_path === SCRIPT_CUSTOM_BINARY ? draft._customBinary.trim() : draft.binary_path.trim();
}

/** PowerShell 及其脚本一律拒绝：即使经"自定义可执行文件"绕进来也不放行 */
function isPowerShellBinary(path: string): boolean {
  const lower = path.toLowerCase();
  return lower.includes("powershell") || lower.includes("pwsh") || lower.endsWith(".ps1");
}

/** 草稿体积（UTF-8 字节数；后端按字节判定，不能让中文内容按字符数蒙混过闸） */
export function scriptContentBytes(content: string): number {
  return new TextEncoder().encode(content).length;
}

/**
 * 缺口判定的外部条件（不属于草稿字段的界面态）。
 *
 * 只有一项，但它挡着一类**不可逆的体验事故**：脚本 ID 是用户打的文件名、落盘后不可改，
 * 而闸口若只看"当前值恰好合法"，打字中途的一次停顿（debounce 500ms 到点）就足以把
 * `camp` 落盘并锁住 ID——`campus` 再也打不完，只能删掉重来。故新建态必须由用户显式
 * 确认一次（回车 / 离开输入框），确认前不许创建文件。
 */
export interface ScriptGapContext {
  /** 新建脚本的 ID 仍在输入中（用户还没确认） */
  idPending?: boolean;
}

/**
 * 缺口清单（空数组 = 可以落盘）。
 *
 * 自动保存模式下这份清单有两个用途：**闸口**（有缺口就不发请求，发了必被后端拒）
 * 与**状态字**（面板据此改口"有 N 处待补全，改动暂未保存"，见 `utils/autosave`）。
 * 只在显式保存时校验的老实现没有第二个用途，于是缺口态下状态字会撒谎。
 */
export function scriptDraftGaps(draft: ScriptDraft, ctx: ScriptGapContext = {}): string[] {
  const gaps: string[] = [];
  if (ctx.idPending) {
    // 名字还在打：说清要怎么结束（照 `SCRIPT_ID_PATTERN` 报形态没有意义——它现在是合法的）
    gaps.push("脚本 ID（打完名字按回车或点输入框外）");
  } else if (!SCRIPT_ID_PATTERN.test(draft.id.trim())) {
    gaps.push("脚本 ID（1~64 位字母、数字、下划线或连字符）");
  }
  if (!draft.content.trim()) {
    gaps.push("脚本内容");
  } else if (scriptContentBytes(draft.content) > SCRIPT_MAX_BYTES) {
    gaps.push(`脚本内容体积（上限 ${SCRIPT_MAX_BYTES / 1024} KB）`);
  }
  if (draft.binary_path === SCRIPT_CUSTOM_BINARY) {
    const path = resolveScriptBinaryPath(draft);
    if (!path) {
      gaps.push("自定义执行程序的路径");
    } else if (isPowerShellBinary(path)) {
      gaps.push("执行程序（不支持 PowerShell，仅支持 shell / bat / python / exe）");
    }
  } else if (draft.binary_path.trim() && isPowerShellBinary(draft.binary_path.trim())) {
    gaps.push("执行程序（不支持 PowerShell，仅支持 shell / bat / python / exe）");
  }
  return gaps;
}

/** 草稿 → 落盘载荷（`PUT /api/scripts/{id}` 的 body） */
export function scriptDraftPayload(draft: ScriptDraft): {
  type: "script";
  name: string;
  description: string;
  content: string;
  binary_path: string;
} {
  return {
    // 后端 TaskKind 反序列化在 type 缺失时默认归为 browser 任务，会把脚本载荷静默
    // 转存为空浏览器任务（脚本内容丢失），必须显式声明
    type: "script",
    name: draft.name.trim() || draft.id.trim(),
    description: draft.description.trim(),
    content: draft.content,
    binary_path: resolveScriptBinaryPath(draft),
  };
}

/**
 * 由「已落盘脚本」构造草稿。
 *
 * `binary_path` 不在已知可执行文件清单里时退回"自定义"分支并把原路径填进手填框，
 * 否则下拉会选中第一个选项、用户一保存就把执行程序改成 Python（静默改配置）。
 */
export function scriptDraftFromServer(
  data: { id: string; name?: string; description?: string; content?: string; binary_path?: string },
  knownBinaries: readonly { path: string }[],
): ScriptDraft {
  const binaryPath = (data.binary_path || "").trim();
  const isKnown = binaryPath !== "" && knownBinaries.some((b) => b.path === binaryPath);
  return {
    id: data.id,
    name: data.name || "",
    description: data.description || "",
    content: data.content || "",
    binary_path: binaryPath && !isKnown ? SCRIPT_CUSTOM_BINARY : binaryPath,
    _customBinary: binaryPath && !isKnown ? binaryPath : "",
    _isNew: false,
  };
}
