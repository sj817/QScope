//! 通过字符串引用与 LC_FUNCTION_STARTS 执行有界 ARM64 Mach-O 扫描，输出保留为分析候选。
use crate::Scan;
use qscope_automation::Error;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const ARM64: u32 = 0x0100_000c;
const MAX_MATCHES: usize = 256;
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
    pub function_rva: Option<String>,
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

#[derive(Clone, Copy)]
struct Section {
    address: u64,
    offset: usize,
    size: usize,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("macho", message)
}

fn range(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8], Error> {
    bytes
        .get(
            offset
                ..offset
                    .checked_add(size)
                    .ok_or_else(|| invalid("Range overflow"))?,
        )
        .ok_or_else(|| invalid("Truncated Mach-O range"))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        range(bytes, offset, 4)?.try_into().expect("four bytes"),
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(
        range(bytes, offset, 8)?.try_into().expect("eight bytes"),
    ))
}

fn number(value: u64) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| invalid("Offset exceeds the host address space"))
}

fn arm64_slice(bytes: &[u8]) -> Result<(&[u8], usize), Error> {
    let magic = u32_at(bytes, 0)?;
    if magic == 0xfeed_facf {
        return Ok((bytes, 0));
    }
    if !matches!(magic, 0xbeba_feca | 0xbfba_feca) {
        return Err(invalid("Expected a 64-bit Mach-O or universal Mach-O"));
    }
    let wide = magic == 0xbfba_feca;
    let count = u32_at(bytes, 4)?.swap_bytes() as usize;
    if count == 0 || count > 32 {
        return Err(invalid("Invalid universal architecture count"));
    }
    let stride = if wide { 32 } else { 20 };
    let table_end = 8 + count * stride;
    range(bytes, 0, table_end)?;
    let mut selected = None;
    let mut ranges = Vec::new();
    for index in 0..count {
        let at = 8 + index * stride;
        let cpu = u32_at(bytes, at)?.swap_bytes();
        let (offset, size) = if wide {
            (
                u64_at(bytes, at + 8)?.swap_bytes(),
                u64_at(bytes, at + 16)?.swap_bytes(),
            )
        } else {
            (
                u64::from(u32_at(bytes, at + 8)?.swap_bytes()),
                u64::from(u32_at(bytes, at + 12)?.swap_bytes()),
            )
        };
        let (offset, size) = (number(offset)?, number(size)?);
        let slice = range(bytes, offset, size)?;
        if offset < table_end
            || size < 32
            || ranges.iter().any(|&(a, b)| offset < b && a < offset + size)
        {
            return Err(invalid("Overlapping or empty universal slices"));
        }
        ranges.push((offset, offset + size));
        if cpu == ARM64 && selected.replace((slice, offset)).is_some() {
            return Err(invalid("Ambiguous ARM64 slices"));
        }
    }
    selected.ok_or_else(|| {
        Error::new(
            "architecture",
            "No ARM64 slice; Intel collector discovery is not implemented",
        )
    })
}

fn function_starts(bytes: &[u8], base: u64, text: Section) -> Result<Vec<u64>, Error> {
    let mut starts = Vec::new();
    let mut address = base;
    let mut at = 0;
    while at < bytes.len() {
        let mut delta = 0u64;
        let mut complete = false;
        for shift in (0..70).step_by(7) {
            let byte = *bytes
                .get(at)
                .ok_or_else(|| invalid("Truncated function-start ULEB128"))?;
            at += 1;
            if shift == 63 && byte > 1 {
                return Err(invalid("Function-start ULEB128 overflow"));
            }
            delta |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err(invalid("Invalid function-start ULEB128"));
        }
        if delta == 0 {
            if bytes[at..].iter().any(|byte| *byte != 0) {
                return Err(invalid("Nonzero function-start padding"));
            }
            return Ok(starts);
        }
        address = address
            .checked_add(delta)
            .ok_or_else(|| invalid("Function address overflow"))?;
        if !address.is_multiple_of(4)
            || address < text.address
            || address - text.address >= text.size as u64
        {
            return Err(invalid("Function start outside __text"));
        }
        if starts.len() == 2_000_000 {
            return Err(invalid("Too many function starts"));
        }
        starts.push(address);
    }
    Err(invalid("Missing function-start terminator"))
}

fn sign21(value: u32) -> i64 {
    ((value as i64) << 43) >> 43
}

/// 仅识别精确 ADR 或相邻 ADRP/ADD 引用。
fn referenced_address(instruction: u32, next: Option<u32>, pc: u64) -> Option<u64> {
    if instruction & 31 == 31 {
        return None;
    }
    let immediate = sign21(((instruction >> 5) & 0x7ffff) << 2 | ((instruction >> 29) & 3));
    match instruction & 0x9f00_0000 {
        0x1000_0000 => pc.checked_add_signed(immediate),
        0x9000_0000 => {
            let next = next?;
            if next & 0xff80_0000 != 0x9100_0000
                || (next >> 5) & 31 != instruction & 31
                || instruction & 31 == 31
            {
                return None;
            }
            let displacement =
                u64::from((next >> 10) & 0xfff) << if next & (1 << 22) != 0 { 12 } else { 0 };
            (pc & !0xfff)
                .checked_add_signed(immediate * 4096)?
                .checked_add(displacement)
        }
        _ => None,
    }
}

pub fn scan(bytes: &[u8]) -> Result<Scan, Error> {
    if bytes.len() as u64 > crate::MAX_MODULE_BYTES {
        return Err(invalid("Module exceeds 512 MiB"));
    }
    let (slice, slice_offset) = arm64_slice(bytes)?;
    if u32_at(slice, 0)? != 0xfeed_facf
        || u32_at(slice, 4)? != ARM64
        || u32_at(slice, 8)? & 0x00ff_ffff != 0
    {
        return Err(Error::new(
            "architecture",
            "Only ARM64 Mach-O is supported; ARM64e requires a separate adapter",
        ));
    }
    if !matches!(u32_at(slice, 12)?, 2 | 6 | 8) {
        return Err(invalid("Expected a Mach-O executable, dylib or bundle"));
    }
    let commands = u32_at(slice, 16)? as usize;
    let command_bytes = u32_at(slice, 20)? as usize;
    if commands > 16384 || command_bytes > 16 * 1024 * 1024 {
        return Err(invalid("Load command limit exceeded"));
    }
    let command_end = 32 + command_bytes;
    range(slice, 32, command_bytes)?;
    let mut at = 32;
    let mut base = None;
    let mut text = None;
    let mut strings = Vec::new();
    let mut scanned_sections: Vec<Section> = Vec::new();
    let mut starts_data = None;
    for _ in 0..commands {
        let command = u32_at(slice, at)?;
        let size = u32_at(slice, at + 4)? as usize;
        if size < 8 || !size.is_multiple_of(8) || at + size > command_end {
            return Err(invalid("Invalid load command size"));
        }
        let data = range(slice, at, size)?;
        if command == 0x19 {
            let name = range(data, 8, 16)?;
            let vmaddr = u64_at(data, 24)?;
            let vmsize = u64_at(data, 32)?;
            let fileoff = number(u64_at(data, 40)?)?;
            let filesize = number(u64_at(data, 48)?)?;
            range(slice, fileoff, filesize)?;
            vmaddr
                .checked_add(vmsize)
                .ok_or_else(|| invalid("Segment address overflow"))?;
            let sections = u32_at(data, 64)? as usize;
            if sections > 256 || size != 72 + sections * 80 {
                return Err(invalid("Invalid segment section table"));
            }
            if name == b"__TEXT\0\0\0\0\0\0\0\0\0\0" && base.replace(vmaddr).is_some() {
                return Err(invalid("Duplicate __TEXT segment"));
            }
            for index in 0..sections {
                let section = range(data, 72 + index * 80, 80)?;
                let section_name = range(section, 0, 16)?;
                let is_text = section_name == b"__text\0\0\0\0\0\0\0\0\0\0";
                let is_string = section_name == b"__cstring\0\0\0\0\0\0\0";
                if !is_text && !is_string {
                    continue;
                }
                if name != b"__TEXT\0\0\0\0\0\0\0\0\0\0" || range(section, 16, 16)? != name {
                    return Err(invalid("Discovery sections must belong to __TEXT"));
                }
                let item = Section {
                    address: u64_at(section, 32)?,
                    size: number(u64_at(section, 40)?)?,
                    offset: u32_at(section, 48)? as usize,
                };
                range(slice, item.offset, item.size)?;
                if item.address < vmaddr
                    || item
                        .address
                        .checked_add(item.size as u64)
                        .is_none_or(|end| end > vmaddr + vmsize)
                    || item.offset < fileoff
                    || item.offset + item.size > fileoff + filesize
                    || item.address - vmaddr != (item.offset - fileoff) as u64
                {
                    return Err(invalid("Section outside its segment"));
                }
                if item.offset < command_end
                    || scanned_sections.iter().any(|other| {
                        item.offset < other.offset + other.size
                            && other.offset < item.offset + item.size
                    })
                {
                    return Err(invalid("Overlapping discovery sections"));
                }
                scanned_sections.push(item);
                if is_text {
                    if u32_at(data, 60)? & 4 == 0
                        || !item.address.is_multiple_of(4)
                        || !item.size.is_multiple_of(4)
                        || text.replace(item).is_some()
                    {
                        return Err(invalid("Invalid executable __text section"));
                    }
                } else {
                    strings.push(item);
                }
            }
        } else if command == 0x26
            && (size != 16
                || starts_data
                    .replace(range(
                        slice,
                        u32_at(data, 8)? as usize,
                        u32_at(data, 12)? as usize,
                    )?)
                    .is_some())
        {
            return Err(invalid("Invalid or duplicate LC_FUNCTION_STARTS"));
        }
        at += size;
    }
    if at != command_end {
        return Err(invalid("Load command count differs from its size"));
    }
    let base = base.ok_or_else(|| invalid("Missing __TEXT segment"))?;
    let text = text.ok_or_else(|| invalid("Missing executable __text section"))?;
    let starts = function_starts(
        starts_data.ok_or_else(|| invalid("Missing LC_FUNCTION_STARTS"))?,
        base,
        text,
    )?;
    let mut anchors: Vec<Anchor> = STRINGS
        .iter()
        .map(|&(id, text)| Anchor {
            id,
            text,
            string_rvas: Vec::new(),
            references: Vec::new(),
        })
        .collect();
    let mut addresses: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for section in strings {
        let data = range(slice, section.offset, section.size)?;
        if !data.is_empty() && data.last() != Some(&0) {
            return Err(invalid("Unterminated __cstring section"));
        }
        let mut offset = 0;
        for string in data.split(|byte| *byte == 0) {
            for (index, &(_, expected)) in STRINGS.iter().enumerate() {
                if string == expected.as_bytes() {
                    if anchors[index].string_rvas.len() == MAX_MATCHES {
                        return Err(invalid("Too many anchor strings"));
                    }
                    let address = section.address + offset as u64;
                    let rva = address
                        .checked_sub(base)
                        .ok_or_else(|| invalid("String precedes image base"))?;
                    anchors[index].string_rvas.push(format!("0x{rva:x}"));
                    addresses.entry(address).or_default().push(index);
                }
            }
            offset += string.len() + 1;
        }
    }
    let code = range(slice, text.offset, text.size)?;
    let mut functions: Vec<BTreeSet<u64>> = (0..4).map(|_| BTreeSet::new()).collect();
    for offset in (0..code.len()).step_by(4) {
        let pc = text.address + offset as u64;
        let instruction = u32_at(code, offset)?;
        let next = if offset + 8 <= code.len() {
            Some(u32_at(code, offset + 4)?)
        } else {
            None
        };
        let Some(target) = referenced_address(instruction, next, pc) else {
            continue;
        };
        let Some(indices) = addresses.get(&target) else {
            continue;
        };
        let function = starts
            .partition_point(|start| *start <= pc)
            .checked_sub(1)
            .map(|index| starts[index]);
        for &index in indices {
            if anchors[index].references.len() == MAX_MATCHES {
                return Err(invalid("Too many anchor references"));
            }
            anchors[index].references.push(Reference {
                instruction_rva: format!(
                    "0x{:x}",
                    pc.checked_sub(base)
                        .ok_or_else(|| invalid("Instruction precedes image base"))?
                ),
                function_rva: function.map(|value| format!("0x{:x}", value - base)),
            });
            if let Some(function) = function {
                functions[index].insert(function);
            }
        }
    }
    let candidates = [("outbound", 0, 1), ("inbound", 2, 3)]
        .into_iter()
        .map(|(direction, name, log)| {
            let rvas: Vec<_> = functions[name]
                .intersection(&functions[log])
                .map(|address| format!("0x{:x}", address - base))
                .collect();
            Candidate {
                direction,
                status: match rvas.len() {
                    0 => "missing",
                    1 => "unique_candidate",
                    _ => "ambiguous",
                },
                function_rvas: rvas,
            }
        })
        .collect();
    Ok(Scan {
        schema_version: 1,
        scanner_version: "macho-arm64-msf-v1",
        platform: "macos",
        architecture: "arm64",
        module: String::new(),
        module_sha256: format!("{:x}", Sha256::digest(bytes)),
        slice_sha256: format!("{:x}", Sha256::digest(slice)),
        slice_offset: slice_offset.to_string(),
        evidence: "hypothesis",
        hook_ready: false,
        blockers: vec![
            "calling_convention_unverified",
            "payload_layout_unverified",
            "runtime_capture_unverified",
        ],
        anchors,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        let put32 = |bytes: &mut [u8], at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes())
        };
        let put64 = |bytes: &mut [u8], at: usize, value: u64| {
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes())
        };
        for (at, value) in [
            (0, 0xfeed_facf),
            (4, ARM64),
            (12, 6),
            (16, 2),
            (20, 248),
            (32, 0x19),
            (36, 232),
            (92, 5),
            (96, 2),
            (264, 0x26),
            (268, 16),
            (272, 0xc00),
            (276, 4),
        ] {
            put32(&mut bytes, at, value);
        }
        bytes[40..46].copy_from_slice(b"__TEXT");
        put64(&mut bytes, 56, 0x1000);
        put64(&mut bytes, 64, 4096);
        put64(&mut bytes, 80, 4096);
        let mut string_offset = 0x800;
        for (index, &(_, text)) in STRINGS.iter().enumerate() {
            bytes[string_offset..string_offset + text.len()].copy_from_slice(text.as_bytes());
            let instruction = 0x400 + (index / 2) * 0x20 + (index % 2) * 8;
            put32(&mut bytes, instruction, 0x90000000);
            put32(
                &mut bytes,
                instruction + 4,
                0x91000000 | ((string_offset as u32) << 10),
            );
            string_offset += text.len() + 1;
        }
        for (at, name, offset, size) in [
            (104, "__text", 0x400, 0x40),
            (184, "__cstring", 0x800, string_offset - 0x800),
        ] {
            bytes[at..at + name.len()].copy_from_slice(name.as_bytes());
            bytes[at + 16..at + 22].copy_from_slice(b"__TEXT");
            put64(&mut bytes, at + 32, 0x1000 + offset as u64);
            put64(&mut bytes, at + 40, size as u64);
            put32(&mut bytes, at + 48, offset as u32);
        }
        bytes[0xc00..0xc04].copy_from_slice(&[0x80, 8, 0x20, 0]);
        bytes
    }

    #[test]
    fn two_independent_anchors_find_candidates_without_claiming_hook_readiness() {
        let report = scan(&sample()).unwrap();
        assert_eq!(report.candidates[0].function_rvas, ["0x400"]);
        assert_eq!(report.candidates[1].function_rvas, ["0x420"]);
        assert!(!report.hook_ready);
        assert_eq!(report.evidence, "hypothesis");
        let mut missing = sample();
        missing[0x408..0x410].fill(0);
        assert_eq!(scan(&missing).unwrap().candidates[0].status, "missing");
        // 相同发送锚点落入多个函数时保留歧义状态。
        let original = sample();
        let mut ambiguous = original.clone();
        ambiguous[0x420..0x430].copy_from_slice(&original[0x400..0x410]);
        assert_eq!(scan(&ambiguous).unwrap().candidates[0].status, "ambiguous");
    }

    #[test]
    fn universal_slices_and_section_boundaries_are_checked() {
        let thin = sample();
        let mut fat = vec![0; 0x100 + thin.len()];
        for (at, value) in [
            (0, 0xcafebabeu32),
            (4, 1),
            (8, ARM64),
            (16, 0x100),
            (20, thin.len() as u32),
        ] {
            fat[at..at + 4].copy_from_slice(&value.to_be_bytes());
        }
        fat[0x100..].copy_from_slice(&thin);
        assert_eq!(scan(&fat).unwrap().slice_offset, "256");
        fat[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(scan(&fat).is_err());
        let mut overlap = sample();
        overlap[152..156].copy_from_slice(&4090u32.to_le_bytes());
        assert!(scan(&overlap).is_err());
        let mut wrong_arch = sample();
        wrong_arch[4..8].copy_from_slice(&0x0100_0007u32.to_le_bytes());
        assert_eq!(scan(&wrong_arch).unwrap_err().code, "architecture");
        let mut wrong_subtype = sample();
        wrong_subtype[8..12].copy_from_slice(&0x100u32.to_le_bytes());
        assert_eq!(scan(&wrong_subtype).unwrap_err().code, "architecture");
        let mut before_base = sample();
        before_base[56..64].copy_from_slice(&0x2000u64.to_le_bytes());
        assert!(scan(&before_base).is_err());
        let mut overlapping_sections = sample();
        overlapping_sections[216..224].copy_from_slice(&0x1400u64.to_le_bytes());
        overlapping_sections[232..236].copy_from_slice(&0x400u32.to_le_bytes());
        assert!(scan(&overlapping_sections).is_err());
        for end in [0, 3, 31, 279, 0x800, 0xc03] {
            assert!(scan(&thin[..end]).is_err());
        }
    }

    #[test]
    fn arm64_references_require_matching_registers_and_handle_negative_pages() {
        assert_eq!(
            referenced_address(0x90000000, Some(0x91008001), 0x1008),
            Some(0x1020)
        );
        assert_eq!(
            referenced_address(0x90000000, Some(0x91008021), 0x1008),
            None
        );
        assert_eq!(
            referenced_address(0xf0ffffe0, Some(0x91000000), 0x1000),
            Some(0)
        );
        assert_eq!(referenced_address(0x10000080, None, 0x1000), Some(0x1010));
        assert_eq!(referenced_address(0x90000000, None, 0), None);
        assert_eq!(referenced_address(0x1000009f, None, 0x1000), None);
    }

    #[test]
    fn function_starts_reject_truncation_overflow_and_out_of_range() {
        let text = Section {
            address: 4,
            offset: 0,
            size: 32,
        };
        assert_eq!(function_starts(&[4, 4, 0], 0, text).unwrap(), [4, 8]);
        for data in [
            &[0x80][..],
            &[0xff; 11][..],
            &[1, 0][..],
            &[40, 0][..],
            &[4, 0, 1][..],
            &[4][..],
        ] {
            assert!(function_starts(data, 0, text).is_err(), "{data:?}");
        }
    }

    #[test]
    fn malformed_inputs_never_panic() {
        for length in 0..128 {
            assert!(scan(&vec![0; length]).is_err());
            let mut bytes = vec![0xff; length];
            if length >= 4 {
                bytes[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
            }
            assert!(scan(&bytes).is_err());
        }
    }
}
