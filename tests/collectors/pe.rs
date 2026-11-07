use super::*;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn fixture(text_rva: u32, duplicate: bool) -> Vec<u8> {
    let mut bytes = vec![0; 0xe00];
    bytes[..2].copy_from_slice(b"MZ");
    put32(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut bytes, 0x84, 0x8664);
    put16(&mut bytes, 0x86, 3);
    put16(&mut bytes, 0x94, 0xf0);
    let optional = 0x98;
    put16(&mut bytes, optional, 0x20b);
    put32(&mut bytes, optional + 56, text_rva + 0x3000);
    put32(&mut bytes, optional + 108, 16);
    put32(&mut bytes, optional + 136, text_rva + 0x2000);
    put32(&mut bytes, optional + 140, if duplicate { 36 } else { 24 });
    for (index, (rva, raw, size, flags)) in [
        (text_rva, 0x400, 0x400, 0x6000_0020),
        (text_rva + 0x1000, 0x800, 0x400, 0x4000_0040),
        (text_rva + 0x2000, 0xc00, 0x200, 0x4000_0040),
    ]
    .into_iter()
    .enumerate()
    {
        let at = optional + 0xf0 + index * 40;
        put32(&mut bytes, at + 12, rva);
        put32(&mut bytes, at + 16, size);
        put32(&mut bytes, at + 20, raw);
        put32(&mut bytes, at + 36, flags);
    }
    let mut strings = [0; 4];
    let mut cursor = 0;
    for (index, &(_, text)) in STRINGS.iter().enumerate() {
        bytes[0x800 + cursor..0x800 + cursor + text.len()].copy_from_slice(text.as_bytes());
        strings[index] = text_rva + 0x1000 + cursor as u32;
        cursor += text.len() + 1;
    }
    for (index, &(begin, end, anchors)) in [
        (0x10, 0x80, [0, 1]),
        (0x100, 0x180, [2, 3]),
        (0x200, 0x280, [0, 1]),
    ]
    .iter()
    .take(if duplicate { 3 } else { 2 })
    .enumerate()
    {
        put32(&mut bytes, 0xc00 + index * 12, text_rva + begin);
        put32(&mut bytes, 0xc04 + index * 12, text_rva + end);
        for (reference, anchor) in anchors.into_iter().enumerate() {
            let offset = begin as usize + 0x10 + reference * 0x10;
            bytes[0x400 + offset..0x403 + offset].copy_from_slice(&[0x48, 0x8d, 0x0d]);
            let displacement = strings[anchor] as i32 - (text_rva as i32 + offset as i32 + 7);
            put32(&mut bytes, 0x403 + offset, displacement as u32);
        }
    }
    bytes
}

#[test]
fn discovers_shifted_functions_without_fixed_addresses() {
    for rva in [0x1000, 0x7000] {
        let report = scan_bytes(&fixture(rva, false)).unwrap();
        assert!(!report.hook_ready);
        assert_eq!(report.candidates[0].status, "candidate");
        assert_eq!(
            report.candidates[0].function_rvas,
            [format!("0x{:x}", rva + 0x10)]
        );
        assert_eq!(
            report.candidates[1].function_rvas,
            [format!("0x{:x}", rva + 0x100)]
        );
    }
}

#[test]
fn preserves_missing_and_ambiguous_candidates() {
    let ambiguous = scan_bytes(&fixture(0x1000, true)).unwrap();
    assert_eq!(ambiguous.candidates[0].status, "ambiguous");
    let mut bytes = fixture(0x1000, false);
    bytes[0x420] = 0;
    let missing = scan_bytes(&bytes).unwrap();
    assert_eq!(missing.candidates[0].status, "missing");
    assert_eq!(missing.candidates[1].status, "candidate");
}

#[test]
fn rejects_truncated_overlapping_and_wrong_architecture_images() {
    let bytes = fixture(0x1000, false);
    for length in 0..bytes.len() {
        assert!(scan_bytes(&bytes[..length]).is_err());
    }
    let mut bytes = bytes;
    put16(&mut bytes, 0x84, 0xaa64);
    assert!(scan_bytes(&bytes).is_err());
    put16(&mut bytes, 0x84, 0x8664);
    put32(&mut bytes, 0x98 + 0xf0 + 40 + 12, 0x1000);
    assert!(scan_bytes(&bytes).is_err());
}

#[test]
fn unknown_module_never_becomes_a_capture_profile() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wrapper.node");
    std::fs::write(&path, fixture(0x1000, false)).unwrap();
    assert_eq!(scan(&path).unwrap().candidates[0].status, "candidate");
    assert!(verified_targets(&path).is_err());
}

#[test]
#[ignore = "需要显式指定本机的受支持模块，仅读取文件"]
fn pinned_module_resolves_verified_capture_targets() {
    let path = std::env::var_os("QSCOPE_TEST_WINDOWS_MODULE").expect("需要模块路径");
    let targets = verified_targets(Path::new(&path)).unwrap();
    assert_ne!(targets[0], targets[1]);
    assert!(targets.into_iter().all(|rva| rva > 0));
}
