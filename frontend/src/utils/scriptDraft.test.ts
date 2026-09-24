/**
 * 脚本草稿的缺口校验与载荷互转（自动保存的闸口）。
 *
 * 这些判定此前写在 `useScripts.saveScript` 里、只有点「保存脚本」时才会跑；
 * 脚本面板改为自动保存后它们跑在每次 debounce 上，坏掉的后果从"保存失败弹一句"
 * 升级为"静默不落盘"，故把边界逐条钉住。
 */
import { describe, expect, it } from "vitest";
import {
  SCRIPT_CUSTOM_BINARY,
  SCRIPT_MAX_BYTES,
  emptyScriptDraft,
  resolveScriptBinaryPath,
  scriptContentBytes,
  scriptDraftFromServer,
  scriptDraftGaps,
  scriptDraftPayload,
} from "./scriptDraft";

const STUB = "#!/usr/bin/env python3\nprint('hi')\n";

function draft(overrides: Partial<ReturnType<typeof emptyScriptDraft>> = {}) {
  return { ...emptyScriptDraft(STUB), id: "checkin", ...overrides };
}

describe("scriptDraftGaps", () => {
  it("合法草稿没有缺口", () => {
    expect(scriptDraftGaps(draft())).toEqual([]);
  });

  it("ID 形态：与后端 is_valid_task_id 同口径（长度 1~64，字母/数字/下划线/连字符）", () => {
    for (const id of ["", "   ", "打卡", "my.script", "a b", "a/b", "x".repeat(65)]) {
      expect(scriptDraftGaps(draft({ id }))).toContain(
        "脚本 ID（1~64 位字母、数字、下划线或连字符）",
      );
    }
    // 数字开头与连字符是**后端接受**的形态：前端曾更严，导致这类已存在的脚本
    // 打开后自动保存被永久跳过（而 ID 字段落盘后是禁用的）——回归护栏。
    for (const id of ["check_in_2", "my-script", "2fa", "a".repeat(64)]) {
      expect(scriptDraftGaps(draft({ id }))).not.toContain(
        "脚本 ID（1~64 位字母、数字、下划线或连字符）",
      );
    }
  });

  it("内容为空是缺口（后端拒空内容脚本：存得下也跑不出结果）", () => {
    expect(scriptDraftGaps(draft({ content: "   \n" }))).toContain("脚本内容");
  });

  it("内容按 UTF-8 字节数判体积，中文不按字符数蒙混", () => {
    const justOver = "a".repeat(SCRIPT_MAX_BYTES + 1);
    expect(scriptDraftGaps(draft({ content: justOver }))).toContain(
      `脚本内容体积（上限 ${SCRIPT_MAX_BYTES / 1024} KB）`,
    );
    // 35KB 个三字节汉字 = 105KB > 100KB
    const cjk = "测".repeat(35 * 1024);
    expect(scriptContentBytes(cjk)).toBeGreaterThan(SCRIPT_MAX_BYTES);
    expect(scriptDraftGaps(draft({ content: cjk }))).toHaveLength(1);
  });

  it("选了自定义执行程序却没填路径是缺口（否则会被静默存成 Python）", () => {
    expect(
      scriptDraftGaps(draft({ binary_path: SCRIPT_CUSTOM_BINARY, _customBinary: "  " })),
    ).toContain("自定义执行程序的路径");
  });

  it("PowerShell 一律拒绝：直接选、经自定义路径、.ps1 文件三种都拦", () => {
    const expected = "执行程序（不支持 PowerShell，仅支持 shell / bat / python / exe）";
    expect(scriptDraftGaps(draft({ binary_path: "C:\\WINDOWS\\pwsh.exe" }))).toContain(expected);
    expect(
      scriptDraftGaps(
        draft({ binary_path: SCRIPT_CUSTOM_BINARY, _customBinary: "C:\\x\\PowerShell.exe" }),
      ),
    ).toContain(expected);
    expect(scriptDraftGaps(draft({ binary_path: "D:\\task.ps1" }))).toContain(expected);
  });

  it("缺口可以同时有多条（面板按数量改口状态字）", () => {
    const gaps = scriptDraftGaps({
      ...emptyScriptDraft(""),
      id: "",
      content: "",
      binary_path: SCRIPT_CUSTOM_BINARY,
    });
    expect(gaps).toHaveLength(3);
  });
});

describe("resolveScriptBinaryPath", () => {
  it("空串 = 项目内 Python（原样透传）", () => {
    expect(resolveScriptBinaryPath(draft({ binary_path: "" }))).toBe("");
  });

  it("自定义分支取手填值并 trim", () => {
    expect(
      resolveScriptBinaryPath(draft({ binary_path: SCRIPT_CUSTOM_BINARY, _customBinary: " D:\\n.exe " })),
    ).toBe("D:\\n.exe");
  });
});

describe("scriptDraftPayload", () => {
  it("名称留空时回退用 ID（列表里不该出现无名脚本）", () => {
    expect(scriptDraftPayload(draft({ name: "  " })).name).toBe("checkin");
  });

  it("type 显式为 script（缺了后端会按 browser 反序列化，脚本内容直接丢）", () => {
    expect(scriptDraftPayload(draft()).type).toBe("script");
  });

  it("描述 trim、内容原样（脚本首行是 shebang，不能被改动）", () => {
    const payload = scriptDraftPayload(draft({ description: " 打卡 ", content: STUB }));
    expect(payload.description).toBe("打卡");
    expect(payload.content).toBe(STUB);
  });
});

describe("scriptDraftFromServer", () => {
  it("已知执行程序原样选中", () => {
    const d = scriptDraftFromServer(
      { id: "a", binary_path: "C:\\Python312\\python.exe" },
      [{ path: "C:\\Python312\\python.exe" }],
    );
    expect(d.binary_path).toBe("C:\\Python312\\python.exe");
    expect(d._customBinary).toBe("");
    expect(d._isNew).toBe(false);
  });

  it("未知执行程序落回自定义分支（否则下拉默选第一项，一保存就把执行程序改成 Python）", () => {
    const d = scriptDraftFromServer({ id: "a", binary_path: "D:\\tool.exe" }, []);
    expect(d.binary_path).toBe(SCRIPT_CUSTOM_BINARY);
    expect(d._customBinary).toBe("D:\\tool.exe");
  });

  it("Python（空路径）不落自定义分支", () => {
    const d = scriptDraftFromServer({ id: "a" }, []);
    expect(d.binary_path).toBe("");
    expect(d._customBinary).toBe("");
  });
});
