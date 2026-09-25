//! 安装包版本号提取：解析 Windows PE 的 VERSIONINFO 资源
//!
//! 手动「选择安装包」路径的版本闸门需要**包内真实版本**：远程清单在离线 /
//! 未发版时不可用（且不应成为上传路径的依赖），压缩包文件名中的版本号可被任意
//! 改写，都不构成可信来源。本模块手写最小 PE 解析器（DOS header → PE header →
//! optional header → 资源目录 → `RT_VERSION` → `VS_VERSIONINFO`），不引入新依赖。
//!
//! 平台差异：仅 Windows 产物（PE）携带 VERSIONINFO 资源；非 Windows 平台恒返回
//! `None`，上传路径据此以 `VersionUnrecognized` 明确拒绝（fail-closed），不降级。

use std::path::Path;

/// 从可执行文件中提取版本号字符串
///
/// Windows 上解析 PE 的 VERSIONINFO 资源（优先 `StringFileInfo` 中的
/// `ProductVersion` / `FileVersion` 字符串，缺失时回退 `VS_FIXEDFILEINFO` 的
/// 数字 FileVersion）；非 Windows、非 PE、缺资源或解析失败一律返回 `None`。
pub(crate) fn extract_exe_version(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        let data = std::fs::read(path).ok()?;
        let resource = find_version_resource(&data)?;
        parse_product_version(resource)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

// ═══════════ PE 结构解析（仅 Windows 产物会走到；解析函数本身跨平台编译便于单测） ═══════════

/// 小端读取 `u16`（越界返回 `None`，解析器全程不 panic）
#[cfg_attr(not(windows), allow(dead_code))]
fn u16le(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// 小端读取 `u32`（越界返回 `None`，解析器全程不 panic）
#[cfg_attr(not(windows), allow(dead_code))]
fn u32le(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// 4 字节对齐（VERSIONINFO / 资源结构的对齐单位）
#[cfg_attr(not(windows), allow(dead_code))]
fn align4(off: usize) -> usize {
    off.div_ceil(4) * 4
}

/// 从 `off` 起读取一个 UTF-16LE 以 NUL 结尾的字符串
#[cfg_attr(not(windows), allow(dead_code))]
fn read_wide_str(data: &[u8], off: usize) -> Option<String> {
    let mut units = Vec::new();
    let mut i = off;
    loop {
        // 上限保护：畸形数据不给死循环机会（单个字符串最长 4K 字节）
        if units.len() > 2048 || i + 2 > data.len() {
            return None;
        }
        let unit = u16le(data, i)?;
        i += 2;
        if unit == 0 {
            return Some(String::from_utf16_lossy(&units));
        }
        units.push(unit);
    }
}

/// 从 `off` 起读取固定 WORD 数的 UTF-16LE 字符串（不含 NUL）
#[cfg_attr(not(windows), allow(dead_code))]
fn read_wide_n(data: &[u8], off: usize, words: usize) -> Option<String> {
    let mut units = Vec::with_capacity(words);
    for i in 0..words {
        units.push(u16le(data, off + i * 2)?);
    }
    Some(String::from_utf16_lossy(&units))
}

#[cfg(windows)]
/// 在 PE 镜像中定位 `RT_VERSION` 资源的数据内容
fn find_version_resource(data: &[u8]) -> Option<&[u8]> {
    // DOS header：e_magic "MZ"，e_lfanew 指向 PE 签名
    if data.len() < 0x40 || &data[0..2] != b"MZ" {
        return None;
    }
    let pe_off = u32le(data, 0x3C)? as usize;
    // PE 签名 "PE\0\0" + COFF header（20 字节）
    if data.len() < pe_off + 24 || &data[pe_off..pe_off + 4] != b"PE\0\0" {
        return None;
    }
    let coff = pe_off + 4;
    let num_sections = u16le(data, coff + 2)? as usize;
    let opt_size = u16le(data, coff + 16)? as usize;
    let opt = coff + 20;
    let magic = u16le(data, opt)?;
    // 数据目录的数量与起始偏移随 PE32 / PE32+ 不同
    let (num_dirs_off, dirs_off) = match magic {
        0x10B => (opt + 92, opt + 96),   // PE32
        0x20B => (opt + 108, opt + 112), // PE32+
        _ => return None,
    };
    let num_dirs = u32le(data, num_dirs_off)? as usize;
    // 资源目录是第 3 个数据目录（索引 2）
    if num_dirs < 3 {
        return None;
    }
    let res_dir_entry = dirs_off + 2 * 8;
    let res_rva = u32le(data, res_dir_entry)? as usize;
    if res_rva == 0 {
        return None;
    }
    let sections = opt + opt_size;
    let res_off = rva_to_offset(data, sections, num_sections, res_rva)?;

    // 资源目录三层：类型（RT_VERSION = 16）→ 资源 ID → 语言 ID。
    // l1 指向层 2 目录，层 2 条目指向层 3 目录，层 3 条目直接指向数据条目
    let l1 = find_resource_entry_by_id(data, res_off, 16)?;
    let (sub1, l3_rel) = first_resource_entry(data, res_off + l1)?;
    if !sub1 {
        return None;
    }
    let (is_subdir, data_rel) = first_resource_entry(data, res_off + l3_rel)?;
    if is_subdir {
        return None;
    }
    // 数据条目：OffsetToData 是**镜像 RVA**（非资源节内偏移），需再换算一次
    let entry = res_off + data_rel;
    let data_rva = u32le(data, entry)? as usize;
    let size = u32le(data, entry + 4)? as usize;
    let start = rva_to_offset(data, sections, num_sections, data_rva)?;
    data.get(start..start.checked_add(size)?)
}

#[cfg(windows)]
/// RVA → 文件偏移（遍历节表，命中节内区间后按 raw 指针平移）
fn rva_to_offset(
    data: &[u8],
    sections_off: usize,
    num_sections: usize,
    rva: usize,
) -> Option<usize> {
    for i in 0..num_sections {
        let s = sections_off.checked_add(i.checked_mul(40)?)?;
        let vsize = u32le(data, s + 8)? as usize;
        let va = u32le(data, s + 12)? as usize;
        let rsize = u32le(data, s + 16)? as usize;
        let raw = u32le(data, s + 20)? as usize;
        if va == 0 {
            continue;
        }
        let span = vsize.max(rsize);
        if rva >= va && rva < va.checked_add(span)? {
            return raw.checked_add(rva - va).filter(|&o| o < data.len());
        }
    }
    None
}

#[cfg(windows)]
/// 在资源目录中查找 ID 等于 `want_id` 的条目，返回其偏移（相对资源节起始）
fn find_resource_entry_by_id(data: &[u8], dir_off: usize, want_id: u32) -> Option<usize> {
    let named = u16le(data, dir_off + 12)? as usize;
    let ids = u16le(data, dir_off + 14)? as usize;
    for i in 0..named.saturating_add(ids) {
        let e = dir_off + 16 + i * 8;
        let name = u32le(data, e)?;
        // 高位 0 表示整数 ID；ID 取低 16 位
        if name & 0x8000_0000 == 0 && (name & 0xFFFF) == want_id {
            return u32le(data, e + 4).map(|o| (o & 0x7FFF_FFFF) as usize);
        }
    }
    None
}

#[cfg(windows)]
/// 取资源目录的**首个整数 ID 条目**，返回 `(是否子目录, 相对资源节的偏移)`
fn first_resource_entry(data: &[u8], dir_off: usize) -> Option<(bool, usize)> {
    let named = u16le(data, dir_off + 12)? as usize;
    let ids = u16le(data, dir_off + 14)? as usize;
    if ids == 0 {
        return None;
    }
    let e = dir_off + 16 + named * 8;
    let off = u32le(data, e + 4)?;
    Some((off & 0x8000_0000 != 0, (off & 0x7FFF_FFFF) as usize))
}

#[cfg(windows)]
/// 解析 `VS_VERSIONINFO` 块：优先 `StringFileInfo` 中的 `ProductVersion` /
/// `FileVersion` 字符串，缺失时回退 `VS_FIXEDFILEINFO` 的数字 FileVersion
/// （`major.minor.patch`，四段的 build 号丢弃）
fn parse_product_version(vs: &[u8]) -> Option<String> {
    if vs.len() < 40 {
        return None;
    }
    // 头部：wLength / wValueLength / wType + 宽字符 szKey "VS_VERSIONINFO"
    if read_wide_str(vs, 6)? != "VS_VERSIONINFO" {
        return None;
    }
    let value_len = u16le(vs, 2)? as usize;
    // szKey 固定 15 字符 + NUL = 32 字节，6 + 32 = 38 → 按 4 对齐到 40
    let fixed_off = 40;
    // VS_FIXEDFILEINFO：dwSignature 后依次是 FileVersionMS/LS（偏移 +8/+12）
    let numeric =
        if value_len >= 52 && vs.len() >= fixed_off + 52 && u32le(vs, fixed_off)? == 0xFEEF_04BD {
            let ms = u32le(vs, fixed_off + 8)?;
            let ls = u32le(vs, fixed_off + 12)?;
            Some(format!("{}.{}.{}", ms >> 16, ms & 0xFFFF, ls >> 16))
        } else {
            None
        };
    // 子块（StringFileInfo / VarFileInfo）从 value 之后按 4 字节对齐处开始
    let children = align4(fixed_off + value_len);
    scan_string_file_info(vs, children).or(numeric)
}

#[cfg(windows)]
/// 遍历 `VS_VERSIONINFO` 的子块，查找 `StringFileInfo` 中的版本字符串
///
/// `ProductVersion` 优先于 `FileVersion`；两者都缺失时返回 `None`。
fn scan_string_file_info(vs: &[u8], mut off: usize) -> Option<String> {
    let mut file_version = None;
    while off + 6 <= vs.len() {
        let wlen = u16le(vs, off)? as usize;
        if wlen == 0 {
            break;
        }
        let key = read_wide_str(vs, off + 6)?;
        if key == "StringFileInfo" {
            // StringTable 从 szKey 之后按 4 字节对齐处开始
            let table_off = align4(off + 6 + (key.chars().count() + 1) * 2);
            let (product, file) = scan_string_table(vs, table_off, off + wlen);
            if let Some(p) = product {
                return Some(p);
            }
            file_version = file_version.or(file);
        }
        off = align4(off + wlen);
    }
    file_version
}

#[cfg(windows)]
/// 遍历 StringTable 内的 String 块，返回 `(ProductVersion, FileVersion)`
///
/// `table_off` 指向 StringTable 头部：先跳过其 wLength/wValueLength/wType +
/// szKey（8 字符语言/代码页）头，再逐块读取 String 条目。
fn scan_string_table(vs: &[u8], table_off: usize, end: usize) -> (Option<String>, Option<String>) {
    let mut product = None;
    let mut file = None;
    // StringTable 头：6 字节固定字段 + szKey（如 "040904b0"），按 4 字节对齐
    let table_key = read_wide_str(vs, table_off + 6).unwrap_or_default();
    let mut off = align4(table_off + 6 + (table_key.chars().count() + 1) * 2);
    while off + 6 <= end && off + 6 <= vs.len() {
        let wlen = u16le(vs, off).unwrap_or(0) as usize;
        if wlen == 0 {
            break;
        }
        let wtype = u16le(vs, off + 4).unwrap_or(0);
        let key = read_wide_str(vs, off + 6).unwrap_or_default();
        let value_off = align4(off + 6 + (key.chars().count() + 1) * 2);
        // wType == 1 表示值为文本；wValueLength 为含结尾 NUL 的 WORD 数
        let wvalue = u16le(vs, off + 2).unwrap_or(0) as usize;
        if wtype == 1 && !key.is_empty() && value_off < vs.len() && wvalue > 0 {
            let val = read_wide_n(vs, value_off, wvalue - 1);
            match (key.as_str(), val) {
                ("ProductVersion", Some(v)) if product.is_none() => product = Some(v),
                ("FileVersion", Some(v)) if file.is_none() => file = Some(v),
                _ => {}
            }
        }
        off = align4(off + wlen);
    }
    (product, file)
}

// ═══════════ 测试夹具：构造带 VERSIONINFO 资源的最小 PE ═══════════

/// 构造一个带 VERSIONINFO 资源的最小 PE 镜像（`doc(hidden)` 测试夹具）
///
/// 仅服务于版本解析的单元/集成测试：产物不是可运行的程序，只需满足本模块的
/// 解析路径（DOS → PE → 资源目录 → `RT_VERSION` → `VS_VERSIONINFO`）。资源内
/// 同时写入 `VS_FIXEDFILEINFO` 数字 FileVersion 与 `StringFileInfo` 的
/// `ProductVersion` 字符串（`include_string_table = false` 时省略后者，用于
/// 覆盖数字回退分支）。
#[doc(hidden)]
pub fn build_minimal_pe_with_version(version: &str, include_string_table: bool) -> Vec<u8> {
    let vs = build_version_info_blob(version, include_string_table);
    const RES_RVA: usize = 0x2000;
    const SEC_RAW: usize = 0x400;
    let res_size = 0x80 + vs.len();

    let mut pe = Vec::new();
    // DOS header（0x40 字节）：e_magic "MZ"，e_lfanew = 0x40
    pe.extend_from_slice(b"MZ");
    pe.resize(0x3C, 0);
    pe.extend_from_slice(&0x40u32.to_le_bytes());
    pe.resize(0x40, 0);
    // PE 签名 + COFF header：1 个节（.rsrc），optional header 240 字节（PE32+）
    pe.extend_from_slice(b"PE\0\0");
    pe.extend_from_slice(&0x8664u16.to_le_bytes()); // Machine
    pe.extend_from_slice(&1u16.to_le_bytes()); // NumberOfSections
    pe.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    pe.extend_from_slice(&0u32.to_le_bytes()); // PointerToSymbolTable
    pe.extend_from_slice(&0u32.to_le_bytes()); // NumberOfSymbols
    pe.extend_from_slice(&240u16.to_le_bytes()); // SizeOfOptionalHeader
    pe.extend_from_slice(&0x0022u16.to_le_bytes()); // Characteristics
    // Optional header（PE32+，240 字节，仅填充解析器读取的字段）
    let opt_start = pe.len();
    pe.extend_from_slice(&0x20Bu16.to_le_bytes()); // Magic: PE32+
    pe.resize(opt_start + 108, 0);
    pe.extend_from_slice(&16u32.to_le_bytes()); // NumberOfRvaAndSizes
    // 数据目录：索引 2 = 资源目录
    pe.resize(opt_start + 112 + 2 * 8, 0);
    pe.extend_from_slice(&(RES_RVA as u32).to_le_bytes());
    pe.extend_from_slice(&(res_size as u32).to_le_bytes());
    pe.resize(opt_start + 240, 0);
    // 节表：.rsrc
    let mut section = [0u8; 40];
    section[..5].copy_from_slice(b".rsrc");
    section[8..12].copy_from_slice(&(res_size as u32).to_le_bytes()); // VirtualSize
    section[12..16].copy_from_slice(&(RES_RVA as u32).to_le_bytes()); // VirtualAddress
    section[16..20].copy_from_slice(&(res_size as u32).to_le_bytes()); // SizeOfRawData
    section[20..24].copy_from_slice(&(SEC_RAW as u32).to_le_bytes()); // PointerToRawData
    pe.extend_from_slice(&section);

    // 资源节：三层目录 + 数据条目 + VS_VERSIONINFO
    // （目录头 16 字节 + 单条目 8 字节 = 24 字节/层）
    let mut res = Vec::new();
    // 层 1：类型目录，1 个 ID 条目（RT_VERSION = 16）→ 子目录 @0x18
    push_dir(&mut res, &[(16, 0x8000_0000 | 0x18)]);
    debug_assert_eq!(res.len(), 0x18);
    // 层 2：资源 ID 目录，1 个 ID 条目 → 子目录 @0x30
    push_dir(&mut res, &[(1, 0x8000_0000 | 0x30)]);
    debug_assert_eq!(res.len(), 0x30);
    // 层 3：语言目录，1 个 ID 条目 → 数据条目 @0x48（无子目录位）
    push_dir(&mut res, &[(0x409, 0x48)]);
    debug_assert_eq!(res.len(), 0x48);
    // 数据条目：OffsetToData 是镜像 RVA（资源节基址 + 数据偏移 0x58）
    res.extend_from_slice(&((RES_RVA + 0x58) as u32).to_le_bytes());
    res.extend_from_slice(&(vs.len() as u32).to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // CodePage
    res.extend_from_slice(&0u32.to_le_bytes()); // Reserved
    debug_assert_eq!(res.len(), 0x58);
    res.extend_from_slice(&vs);

    pe.resize(SEC_RAW, 0);
    pe.extend_from_slice(&res);
    pe
}

/// 向资源目录写入目录头（无命名条目）与整数 ID 条目
fn push_dir(res: &mut Vec<u8>, entries: &[(u32, u32)]) {
    res.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    res.extend_from_slice(&0u16.to_le_bytes()); // MajorVersion
    res.extend_from_slice(&0u16.to_le_bytes()); // MinorVersion
    res.extend_from_slice(&0u16.to_le_bytes()); // NumberOfNamedEntries
    res.extend_from_slice(&(entries.len() as u16).to_le_bytes()); // NumberOfIdEntries
    for (id, offset) in entries {
        res.extend_from_slice(&id.to_le_bytes());
        res.extend_from_slice(&offset.to_le_bytes());
    }
}

/// 构造 `VS_VERSIONINFO` 块：固定信息（数字 FileVersion）+ 可选 StringFileInfo
///
/// 版本串取前三段数字填入 `VS_FIXEDFILEINFO`；`include_string_table` 为真时
/// 另写一个含 `ProductVersion` 字符串（原样保留 `version`）的 StringFileInfo 子块。
fn build_version_info_blob(version: &str, include_string_table: bool) -> Vec<u8> {
    let nums: Vec<u64> = version
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect();
    let get = |i: usize| nums.get(i).copied().unwrap_or(0);
    let (major, minor, patch) = (get(0) as u16, get(1) as u16, get(2) as u16);

    let mut vs = Vec::new();
    vs.extend_from_slice(&0u16.to_le_bytes()); // wLength（最后回填）
    vs.extend_from_slice(&52u16.to_le_bytes()); // wValueLength
    vs.extend_from_slice(&0u16.to_le_bytes()); // wType（二进制）
    for unit in "VS_VERSIONINFO\0".encode_utf16() {
        vs.extend_from_slice(&unit.to_le_bytes());
    }
    vs.resize(40, 0); // szKey 后按 4 字节对齐
    // VS_FIXEDFILEINFO（52 字节）
    vs.extend_from_slice(&0xFEEF_04BDu32.to_le_bytes()); // dwSignature
    vs.extend_from_slice(&0u32.to_le_bytes()); // dwStrucVersion
    vs.extend_from_slice(&((major as u32) << 16 | minor as u32).to_le_bytes()); // dwFileVersionMS
    vs.extend_from_slice(&((patch as u32) << 16).to_le_bytes()); // dwFileVersionLS
    vs.extend_from_slice(&((major as u32) << 16 | minor as u32).to_le_bytes()); // dwProductVersionMS
    vs.extend_from_slice(&((patch as u32) << 16).to_le_bytes()); // dwProductVersionLS
    vs.resize(92, 0); // 固定信息共 52 字节（40 + 52 = 92）

    if include_string_table {
        // StringFileInfo → StringTable → String(ProductVersion)
        let mut string_block = Vec::new();
        string_block.extend_from_slice(&0u16.to_le_bytes()); // wLength（回填）
        string_block.extend_from_slice(&((version.chars().count() + 1) as u16).to_le_bytes()); // wValueLength（WORD 数，含 NUL）
        string_block.extend_from_slice(&1u16.to_le_bytes()); // wType（文本）
        for unit in "ProductVersion\0".encode_utf16() {
            string_block.extend_from_slice(&unit.to_le_bytes());
        }
        string_block.resize(align4_pub(string_block.len()), 0);
        for unit in format!("{version}\0").encode_utf16() {
            string_block.extend_from_slice(&unit.to_le_bytes());
        }
        let str_len = string_block.len();
        string_block[0..2].copy_from_slice(&(str_len as u16).to_le_bytes());

        let mut table = Vec::new();
        table.extend_from_slice(&0u16.to_le_bytes()); // wLength（回填）
        table.extend_from_slice(&0u16.to_le_bytes()); // wValueLength
        table.extend_from_slice(&1u16.to_le_bytes()); // wType
        for unit in "040904b0\0".encode_utf16() {
            table.extend_from_slice(&unit.to_le_bytes());
        }
        table.resize(align4_pub(table.len()), 0);
        table.extend_from_slice(&string_block);
        let table_len = table.len();
        table[0..2].copy_from_slice(&(table_len as u16).to_le_bytes());

        let mut sfi = Vec::new();
        sfi.extend_from_slice(&0u16.to_le_bytes()); // wLength（回填）
        sfi.extend_from_slice(&0u16.to_le_bytes()); // wValueLength
        sfi.extend_from_slice(&1u16.to_le_bytes()); // wType
        for unit in "StringFileInfo\0".encode_utf16() {
            sfi.extend_from_slice(&unit.to_le_bytes());
        }
        sfi.resize(align4_pub(sfi.len()), 0);
        sfi.extend_from_slice(&table);
        let sfi_len = sfi.len();
        sfi[0..2].copy_from_slice(&(sfi_len as u16).to_le_bytes());

        vs.extend_from_slice(&sfi);
    }
    let total = vs.len();
    vs[0..2].copy_from_slice(&(total as u16).to_le_bytes());
    vs
}

/// 夹具内部的 4 字节对齐（与解析器 `align4` 同语义；独立命名避免私有项互引）
fn align4_pub(off: usize) -> usize {
    off.div_ceil(4) * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 完全不是 PE 的字节流返回 None（MZ 前缀缺失 / 文件过短均不 panic）
    #[cfg(windows)]
    #[test]
    fn find_version_resource_rejects_non_pe() {
        assert!(find_version_resource(b"").is_none());
        assert!(find_version_resource(b"short").is_none());
        assert!(find_version_resource(b"definitely not a pe image").is_none());
        // MZ 头在但 PE 签名缺失
        let mut dos = vec![0u8; 0x40];
        dos[0..2].copy_from_slice(b"MZ");
        dos[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        assert!(find_version_resource(&dos).is_none());
    }

    /// PE 头合法但无资源目录（数据目录数 < 3）→ None
    #[cfg(windows)]
    #[test]
    fn find_version_resource_rejects_missing_resource_dir() {
        let pe = build_minimal_pe_with_version("9.9.9", false);
        // 数据目录数（NumberOfRvaAndSizes）改为 2 → 资源目录不存在
        let mut stripped = pe.clone();
        let num_dirs_off = 0x58 + 108;
        stripped[num_dirs_off..num_dirs_off + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(find_version_resource(&stripped).is_none());
        // 资源目录 RVA 置 0（无资源段）→ None
        let mut no_res = pe;
        let rva_off = 0x58 + 112 + 2 * 8;
        no_res[rva_off..rva_off + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(find_version_resource(&no_res).is_none());
    }

    /// VS_VERSIONINFO 解析：StringFileInfo 的 ProductVersion 优先
    #[cfg(windows)]
    #[test]
    fn parse_product_version_prefers_string_table() {
        let blob = build_version_info_blob("8.8.8-beta.1", true);
        assert_eq!(
            parse_product_version(&blob).as_deref(),
            Some("8.8.8-beta.1")
        );
    }

    /// VS_VERSIONINFO 解析：无 StringFileInfo 时回退数字 FileVersion（丢 build 段）
    #[cfg(windows)]
    #[test]
    fn parse_product_version_falls_back_to_fixed_info() {
        let blob = build_version_info_blob("7.2.9", false);
        assert_eq!(parse_product_version(&blob).as_deref(), Some("7.2.9"));
    }

    /// VS_VERSIONINFO 解析：畸形块返回 None 不 panic
    #[cfg(windows)]
    #[test]
    fn parse_product_version_rejects_malformed() {
        assert!(parse_product_version(b"").is_none());
        assert!(parse_product_version(&[0u8; 39]).is_none());
        // szKey 不是 VS_VERSIONINFO
        assert!(parse_product_version(&[0u8; 64]).is_none());
    }

    /// 端到端：夹具 PE 经完整解析路径提取出版本（仅 Windows 有真实提取实现）
    #[cfg(windows)]
    #[test]
    fn extract_exe_version_reads_fixture_pe() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("campus-auth.exe");
        std::fs::write(&exe, build_minimal_pe_with_version("9.9.9", true)).unwrap();
        assert_eq!(extract_exe_version(&exe).as_deref(), Some("9.9.9"));

        // 无 StringFileInfo 的夹具走数字回退
        std::fs::write(&exe, build_minimal_pe_with_version("6.0.1", false)).unwrap();
        assert_eq!(extract_exe_version(&exe).as_deref(), Some("6.0.1"));
    }

    /// 非 PE 文件 / 损坏文件提取返回 None，不 panic（Windows 与非 Windows 均跑）
    #[test]
    fn extract_exe_version_returns_none_for_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("not-an-exe");
        std::fs::write(&exe, b"plain text file").unwrap();
        assert_eq!(extract_exe_version(&exe), None);
        assert_eq!(extract_exe_version(&dir.path().join("missing")), None);
    }
}
