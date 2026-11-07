//! 根据独立字符串引用与 PE 异常表定位 x64 MSF 函数，扫描结果保留缺失和歧义。
use qscope_automation::{Error, display_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const STRINGS: [(&str, &str); 4] = [
    ("send_name", "InternalPushTask"),
    (
        "send_log",
        "MSF task pushed seq:{} uin:{} uin_type:{} cmd:{} data_size:{} custom_sig:{}",
    ),
    ("receive_name", "DidRecvResponseData"),
    (
        "receive_log",
        "MSF recv seq:{} cmd:{} data_size:{}, error_code:{}",
    ),
];

#[derive(Debug, Serialize)]
pub struct Reference {
    pub instruction_rva: String,
    pub function_rva: String,
}

#[derive(Debug, Serialize)]
pub struct Anchor {
    pub id: &'static str,
    pub text: &'static str,
    pub string_rvas: Vec<String>,
    pub references: Vec<Reference>,
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub direction: &'static str,
    pub status: &'static str,
    pub function_rvas: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Scan {
    pub schema_version: u32,
    pub scanner_version: &'static str,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub module: String,
    pub module_sha256: String,
    pub hook_ready: bool,
    pub blockers: Vec<&'static str>,
    pub anchors: Vec<Anchor>,
    pub candidates: Vec<Candidate>,
}

struct Section {
    rva: u32,
    offset: usize,
    size: usize,
    executable: bool,
}

fn invalid(message: &str) -> Error {
    Error::new("pe_scan", message)
}

fn range(bytes: &[u8], at: usize, size: usize) -> Result<&[u8], Error> {
    bytes
        .get(at..at.checked_add(size).ok_or_else(|| invalid("PE 范围溢出"))?)
        .ok_or_else(|| invalid("PE 数据被截断"))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(
        range(bytes, at, 2)?.try_into().expect("两个字节"),
    ))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        range(bytes, at, 4)?.try_into().expect("四个字节"),
    ))
}

fn map(sections: &[Section], rva: u32, size: usize) -> Result<usize, Error> {
    sections
        .iter()
        .find_map(|section| {
            let relative = rva.checked_sub(section.rva)? as usize;
            (relative.checked_add(size)? <= section.size).then_some(section.offset + relative)
        })
        .ok_or_else(|| invalid("RVA 超出 PE 文件节区"))
}

fn sections(bytes: &[u8]) -> Result<(Vec<Section>, usize), Error> {
    if range(bytes, 0, 2)? != b"MZ" {
        return Err(invalid("需要 PE x64 模块"));
    }
    let pe = u32_at(bytes, 0x3c)? as usize;
    if range(bytes, pe, 4)? != b"PE\0\0" || u16_at(bytes, pe + 4)? != 0x8664 {
        return Err(invalid("需要 PE x64 模块"));
    }
    let count = u16_at(bytes, pe + 6)? as usize;
    let optional_size = u16_at(bytes, pe + 20)? as usize;
    let optional = pe + 24;
    range(bytes, optional, optional_size)?;
    if count == 0
        || count > 96
        || optional_size < 144
        || u16_at(bytes, optional)? != 0x20b
        || u32_at(bytes, optional + 108)? < 4
    {
        return Err(invalid("PE 节区或可选头无效"));
    }
    let image_size = u32_at(bytes, optional + 56)?;
    let table = optional + optional_size;
    range(bytes, table, count * 40)?;
    let mut sections: Vec<Section> = Vec::new();
    for index in 0..count {
        let at = table + index * 40;
        let rva = u32_at(bytes, at + 12)?;
        let size = u32_at(bytes, at + 16)? as usize;
        let offset = u32_at(bytes, at + 20)? as usize;
        range(bytes, offset, size)?;
        if u64::from(rva) + size as u64 > u64::from(image_size)
            || (size > 0 && offset < table + count * 40)
            || sections.iter().any(|previous| {
                size > 0
                    && previous.size > 0
                    && ((offset < previous.offset + previous.size
                        && previous.offset < offset + size)
                        || (u64::from(rva) < u64::from(previous.rva) + previous.size as u64
                            && u64::from(previous.rva) < u64::from(rva) + size as u64))
            })
        {
            return Err(invalid("PE 节区越界或重叠"));
        }
        sections.push(Section {
            rva,
            offset,
            size,
            executable: u32_at(bytes, at + 36)? & 0x2000_0000 != 0,
        });
    }
    Ok((sections, optional))
}

fn scan_bytes(bytes: &[u8]) -> Result<Scan, Error> {
    let (sections, optional) = sections(bytes)?;
    let exception_rva = u32_at(bytes, optional + 112 + 24)?;
    let exception_size = u32_at(bytes, optional + 112 + 28)? as usize;
    if exception_size == 0 || exception_size > 24_000_000 || !exception_size.is_multiple_of(12) {
        return Err(invalid("PE 异常表缺失或无效"));
    }
    let table = map(&sections, exception_rva, exception_size)?;
    let mut functions: Vec<(u32, u32)> = Vec::new();
    for at in (table..table + exception_size).step_by(12) {
        let start = u32_at(bytes, at)?;
        let end = u32_at(bytes, at + 4)?;
        if start >= end
            || functions
                .last()
                .is_some_and(|&(_, previous_end)| start < previous_end)
            || !sections.iter().any(|section| {
                section.executable
                    && start >= section.rva
                    && u64::from(end) <= u64::from(section.rva) + section.size as u64
            })
        {
            return Err(invalid("PE 函数边界无效"));
        }
        functions.push((start, end));
    }
    let mut anchors: Vec<Anchor> = STRINGS
        .iter()
        .map(|&(id, text)| Anchor {
            id,
            text,
            string_rvas: Vec::new(),
            references: Vec::new(),
        })
        .collect();
    let mut strings = BTreeMap::new();
    for section in sections.iter().filter(|section| !section.executable) {
        let data = range(bytes, section.offset, section.size)?;
        for (index, anchor) in anchors.iter_mut().enumerate() {
            for (offset, window) in data.windows(anchor.text.len() + 1).enumerate() {
                if window.last() == Some(&0)
                    && &window[..anchor.text.len()] == anchor.text.as_bytes()
                {
                    if anchor.string_rvas.len() >= 256 {
                        return Err(invalid("字符串候选超过上限"));
                    }
                    let rva = section.rva + offset as u32;
                    anchor.string_rvas.push(format!("0x{rva:x}"));
                    strings.insert(rva, index);
                }
            }
        }
    }
    let mut targets = [const { BTreeSet::new() }; 4];
    for section in sections.iter().filter(|section| section.executable) {
        let data = range(bytes, section.offset, section.size)?;
        for (offset, instruction) in data.windows(7).enumerate() {
            // 只记录 RIP 相对 LEA，函数名与日志格式必须独立指向同一函数。
            if !matches!(instruction[0], 0x48 | 0x4c)
                || instruction[1] != 0x8d
                || instruction[2] & 0xc7 != 5
            {
                continue;
            }
            let rva = section.rva + offset as u32;
            let displacement =
                i32::from_le_bytes(instruction[3..7].try_into().expect("四字节位移"));
            let target = i64::from(rva) + 7 + i64::from(displacement);
            let Some(index) = u32::try_from(target)
                .ok()
                .and_then(|target| strings.get(&target))
                .copied()
            else {
                continue;
            };
            let function = functions.partition_point(|&(start, _)| start <= rva);
            let Some(&(start, end)) = function
                .checked_sub(1)
                .and_then(|index| functions.get(index))
            else {
                continue;
            };
            if rva >= end {
                continue;
            }
            if anchors[index].references.len() >= 256 {
                return Err(invalid("代码引用超过上限"));
            }
            targets[index].insert(start);
            anchors[index].references.push(Reference {
                instruction_rva: format!("0x{rva:x}"),
                function_rva: format!("0x{start:x}"),
            });
        }
    }
    let candidates = [("send", 0), ("receive", 2)]
        .into_iter()
        .map(|(direction, index)| {
            let rvas: Vec<_> = targets[index]
                .intersection(&targets[index + 1])
                .map(|rva| format!("0x{rva:x}"))
                .collect();
            Candidate {
                direction,
                status: match rvas.len() {
                    0 => "missing",
                    1 => "candidate",
                    _ => "ambiguous",
                },
                function_rvas: rvas,
            }
        })
        .collect();
    Ok(Scan {
        schema_version: 1,
        scanner_version: "pe-msf-v1",
        platform: "windows",
        architecture: "x64",
        module: String::new(),
        module_sha256: format!("{:x}", Sha256::digest(bytes)),
        hook_ready: false,
        blockers: vec!["module_identity_and_layout_require_independent_validation"],
        anchors,
        candidates,
    })
}

fn read_module(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_BYTES {
        return Err(invalid("模块必须是 512 MiB 以内的普通文件"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("模块超过大小上限"));
    }
    Ok(bytes)
}

pub fn scan(path: &Path) -> Result<Scan, Error> {
    let path = path.canonicalize()?;
    let mut report = scan_bytes(&read_module(&path)?)?;
    report.module = display_path(&path);
    Ok(report)
}

/// 采集前复核完整模块身份、双锚点唯一性和前缀，返回本次扫描得到的 RVA。
pub fn verified_targets(path: &Path) -> Result<[u32; 2], Error> {
    #[derive(Deserialize)]
    struct Profile {
        module_sha256: String,
        send_prefix: String,
        recv_prefix: String,
    }
    let profile: Profile = serde_json::from_str(include_str!("../../profile.json"))?;
    let bytes = read_module(path)?;
    let report = scan_bytes(&bytes)?;
    if report.module_sha256 != profile.module_sha256 {
        return Err(invalid("模块身份未验证，仅支持只读扫描"));
    }
    let (sections, _) = sections(&bytes)?;
    let mut result = [0; 2];
    for (index, prefix) in [profile.send_prefix, profile.recv_prefix]
        .iter()
        .enumerate()
    {
        let candidate = &report.candidates[index];
        if candidate.status != "candidate" {
            return Err(invalid("MSF 函数缺失或存在歧义"));
        }
        let rva = u32::from_str_radix(&candidate.function_rvas[0][2..], 16)
            .map_err(|_| invalid("函数 RVA 无效"))?;
        let offset = map(&sections, rva, 32)?;
        let actual: String = range(&bytes, offset, 32)?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if actual != *prefix {
            return Err(invalid("动态定位的函数前缀未通过验证"));
        }
        result[index] = rva;
    }
    if result[0] == result[1] {
        return Err(invalid("收发函数指向同一地址"));
    }
    Ok(result)
}

#[cfg(test)]
#[path = "../../../../tests/collectors/pe.rs"]
mod tests;
