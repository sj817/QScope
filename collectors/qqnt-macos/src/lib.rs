//! macOS client inspection and the pinned ARM64 research-copy collector.
//! Generic discovery alone never authorizes installing a hook.
#[cfg(target_os = "macos")]
mod common;
pub mod inspect;
pub mod macho;
pub mod package;
#[cfg(target_os = "macos")]
pub mod prepare;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod process;
#[cfg(target_os = "macos")]
pub mod worker;

use qscope_automation::Error;
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::Path,
};

pub const MAX_MODULE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct Scan {
    pub schema_version: u32,
    pub scanner_version: &'static str,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub module: String,
    pub module_sha256: String,
    pub slice_sha256: String,
    pub slice_offset: String,
    pub evidence: &'static str,
    pub hook_ready: bool,
    pub blockers: Vec<&'static str>,
    pub anchors: Vec<macho::Anchor>,
    pub candidates: Vec<macho::Candidate>,
}

/// Scan a local Mach-O without loading it, launching QQ, or requesting any OS permissions.
pub fn scan(path: &Path) -> Result<Scan, Error> {
    let path = path.canonicalize()?;
    if !fs::metadata(&path)?.is_file() {
        return Err(Error::new("module_limit", "Module must be a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let mut file = options.open(&path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_MODULE_BYTES {
        return Err(Error::new(
            "module_limit",
            "Module must be a regular file of at most 512 MiB",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_MODULE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MODULE_BYTES {
        return Err(Error::new("module_limit", "Module exceeds 512 MiB"));
    }
    let mut report = macho::scan(&bytes)?;
    report.module = qscope_automation::display_path(&path);
    Ok(report)
}
