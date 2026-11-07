//! Identity of the one Mac build whose capture layout has been observed live.
use qscope_automation::{Error, runtime};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub const BUILD: &str = "7.0.3-54395";
pub const MODULE: &str = "Contents/Resources/app/wrapper.node";
pub const MODULE_SHA256: &str = "0e4394e79de45ae6a9fef3c1c5cf99a22d91b1ae4c95e435f2f2f118b1c47738";
pub const TEAM: &str = "FN2V63AD2J";
pub const SEND_PROLOGUE: &str = "ffc303d1fa670aa9f85f0ba9f6570ca9";
pub const RECEIVE_PROLOGUE: &str = "ff8304d1fc6f0ca9fa670da9f85f0ea9";
pub const INSTALLER_NAME: &str = "QQ_7.0.3_261103_01.dmg";

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub build: String,
    pub architecture: String,
    pub installer_bytes: u64,
    pub installer_sha256: String,
    pub installer_url: String,
    pub installer_sign_url: String,
    pub modules: Vec<Module>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Module {
    pub path: String,
    pub sha256: String,
}

pub fn identity() -> Result<Identity, Error> {
    Ok(Identity {
        build: BUILD.into(),
        architecture: "arm64".into(),
        installer_bytes: 487_596_067,
        installer_sha256: "2f1d9ae1d1f02bf7e1b0a62339817d4f016f4a59dc2bd3c0aa28a0cf4ca36bb1".into(),
        installer_url:
            "https://qqdl.gtimg.cn/qqfile/QQNTV2/9.9.37/release/a8ffd03b/QQ_7.0.3_261103_01.dmg"
                .into(),
        installer_sign_url:
            "https://im.qq.com/http2rpc/gotrpc/noauth/trpc.qqntv2.urlsign.UrlSign/GetSign".into(),
        modules: vec![Module {
            path: MODULE.into(),
            sha256: MODULE_SHA256.into(),
        }],
    })
}

pub fn file_sha256(path: &Path) -> Result<String, Error> {
    runtime::no_links(path)?;
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(Error::new(
            "qq_package",
            "Package input must be a regular file",
        ));
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut input = options.open(path)?;
    let length = input.metadata()?.len();
    if !input.metadata()?.is_file() || length > 1024 * 1024 * 1024 {
        return Err(Error::new("qq_package", "Package file exceeds 1 GiB"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut received = 0u64;
    loop {
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        received += size as u64;
        if received > length {
            return Err(Error::new(
                "qq_package",
                "Package file changed while hashing",
            ));
        }
        hash.update(&buffer[..size]);
    }
    if received != length || input.metadata()?.len() != length {
        return Err(Error::new(
            "qq_package",
            "Package file changed while hashing",
        ));
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn verify_installer(path: &Path, identity: &Identity) -> Result<(), Error> {
    runtime::no_links(path)?;
    if std::fs::symlink_metadata(path)?.len() != identity.installer_bytes
        || file_sha256(path)? != identity.installer_sha256
    {
        return Err(Error::new(
            "qq_installer",
            "QQ DMG size or SHA256 differs from the pinned build",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct HookProfile {
    pub send: String,
    pub receive: String,
    pub module_sha256: String,
}

/// Discover both entries afresh, then constrain them to the one observed ABI.
/// Neither a successful generic scan nor a plausible RVA is sufficient alone.
pub fn capture_profile(app: &Path) -> Result<HookProfile, Error> {
    let module = app.join(MODULE);
    runtime::no_links(&module)?;
    if !std::fs::symlink_metadata(&module)?.is_file() {
        return Err(Error::new(
            "qq_profile",
            "Capture module must be a regular file",
        ));
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&module)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > crate::MAX_MODULE_BYTES {
        return Err(Error::new("qq_package", "Unsupported capture module size"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(crate::MAX_MODULE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > crate::MAX_MODULE_BYTES {
        return Err(Error::new(
            "qq_package",
            "Capture module grew while scanning",
        ));
    }
    let scan = crate::macho::scan(&bytes)?;
    if scan.module_sha256 != MODULE_SHA256 {
        return Err(Error::new(
            "qq_profile",
            "No verified payload layout for this module hash",
        ));
    }
    let slice: usize = scan
        .slice_offset
        .parse()
        .map_err(|_| Error::new("qq_profile", "Invalid scanner slice offset"))?;
    let slice = bytes
        .get(slice..)
        .ok_or_else(|| Error::new("qq_profile", "Scanner slice escaped the module"))?;
    let read32 = |offset: usize| -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            slice
                .get(offset..offset + 4)
                .ok_or_else(|| Error::new("qq_profile", "Truncated Mach-O header"))?
                .try_into()
                .expect("four bytes"),
        ))
    };
    let read64 = |offset: usize| -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            slice
                .get(offset..offset + 8)
                .ok_or_else(|| Error::new("qq_profile", "Truncated Mach-O segment"))?
                .try_into()
                .expect("eight bytes"),
        ))
    };
    let entry = |direction: &str, expected: &str| -> Result<String, Error> {
        let candidates: Vec<_> = scan
            .candidates
            .iter()
            .filter(|candidate| candidate.direction == direction)
            .collect();
        if candidates.len() != 1
            || candidates[0].status != "unique_candidate"
            || candidates[0].function_rvas.len() != 1
        {
            return Err(Error::new(
                "qq_profile",
                "Capture boundary is not a unique scanner candidate",
            ));
        }
        let rva = u64::from_str_radix(candidates[0].function_rvas[0].trim_start_matches("0x"), 16)
            .map_err(|_| Error::new("qq_profile", "Invalid scanner candidate"))?;
        let count = read32(16)?;
        if count > 4096 {
            return Err(Error::new(
                "qq_profile",
                "Mach-O load command limit exceeded",
            ));
        }
        let mut offset = 32usize;
        for _ in 0..count {
            let size = read32(offset + 4)? as usize;
            if size < 8 {
                return Err(Error::new("qq_profile", "Invalid Mach-O load command"));
            }
            if read32(offset)? == 0x19 {
                let vm = read64(offset + 24)?;
                let file = read64(offset + 40)?;
                let length = read64(offset + 48)?;
                if let Some(relative) = rva.checked_sub(vm)
                    && relative.checked_add(16).is_some_and(|end| end <= length)
                {
                    let start = file
                        .checked_add(relative)
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| Error::new("qq_profile", "Candidate file range overflow"))?;
                    let prefix = slice
                        .get(
                            start..start.checked_add(16).ok_or_else(|| {
                                Error::new("qq_profile", "Candidate prefix overflow")
                            })?,
                        )
                        .ok_or_else(|| {
                            Error::new("qq_profile", "Candidate prefix escaped the module")
                        })?;
                    let hex = prefix
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    if hex != expected {
                        return Err(Error::new(
                            "qq_profile",
                            "Discovered entry prologue differs from the observed ABI",
                        ));
                    }
                    return Ok(format!("{rva:x}:{hex}"));
                }
            }
            offset = offset
                .checked_add(size)
                .ok_or_else(|| Error::new("qq_profile", "Mach-O load command overflow"))?;
        }
        Err(Error::new(
            "qq_profile",
            "Candidate is not backed by module file bytes",
        ))
    };
    Ok(HookProfile {
        send: entry("outbound", SEND_PROLOGUE)?,
        receive: entry("inbound", RECEIVE_PROLOGUE)?,
        module_sha256: scan.module_sha256,
    })
}

/// Verify the original extracted QQ.app, not the modified research copy.
pub fn verify_client(app: &Path, identity: &Identity) -> Result<(), Error> {
    for module in &identity.modules {
        if file_sha256(&app.join(&module.path))? != module.sha256 {
            return Err(Error::new(
                "qq_package",
                format!("QQ module identity changed: {}", module.path),
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        use crate::common::{args, command, failure};
        use std::{sync::atomic::AtomicBool, time::Duration};
        let stop = AtomicBool::new(false);
        let info = command(
            "/usr/bin/plutil",
            &args(&[
                "-convert".as_ref(),
                "json".as_ref(),
                "-o".as_ref(),
                "-".as_ref(),
                app.join("Contents/Info.plist").as_os_str(),
            ]),
            Duration::from_secs(10),
            &stop,
        )?;
        let info: serde_json::Value = serde_json::from_slice(&info.stdout)?;
        if info["CFBundleIdentifier"] != "com.tencent.qq"
            || info["CFBundleShortVersionString"] != "7.0.3"
            || info["CFBundleVersion"] != "54395"
            || info["CFBundleExecutable"] != "QQ"
        {
            return Err(failure("QQ app metadata differs from the pinned source"));
        }
        command(
            "/usr/bin/codesign",
            &args(&[
                "--verify".as_ref(),
                "--deep".as_ref(),
                "--strict".as_ref(),
                app.as_os_str(),
            ]),
            Duration::from_secs(60),
            &stop,
        )?;
        let signature = command(
            "/usr/bin/codesign",
            &args(&["-dv".as_ref(), "--verbose=4".as_ref(), app.as_os_str()]),
            Duration::from_secs(10),
            &stop,
        )?;
        if !String::from_utf8_lossy(&signature.stderr)
            .lines()
            .any(|line| line == format!("TeamIdentifier={TEAM}"))
        {
            return Err(failure("QQ developer team differs from the pinned source"));
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    Err(Error::new(
        "unsupported_platform",
        "QQ app signatures require macOS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_changed_installer_and_non_regular_inputs_without_loading_them() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let file = root.path().join("synthetic.dmg");
        std::fs::write(&file, b"synthetic").unwrap();
        let mut identity = identity().unwrap();
        assert!(verify_installer(&file, &identity).is_err());
        identity.installer_bytes = 9;
        identity.installer_sha256 = file_sha256(&file).unwrap();
        verify_installer(&file, &identity).unwrap();
        std::fs::write(&file, b"different").unwrap();
        assert!(verify_installer(&file, &identity).is_err());
        assert!(file_sha256(root.path()).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("linked.dmg");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(file_sha256(&link).is_err());
        }
    }
}
