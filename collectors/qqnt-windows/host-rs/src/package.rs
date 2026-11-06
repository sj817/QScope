use qscope_automation::{Error, runtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
pub struct Identity {
    pub build: String,
    pub architecture: String,
    pub installer_bytes: u64,
    pub installer_sha256: String,
    pub installer_url: String,
    pub installer_sign_url: String,
    pub modules: Vec<Module>,
}

#[derive(Deserialize)]
pub struct Module {
    pub path: String,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct Extraction {
    pub schema_version: u32,
    pub build: String,
    pub input_sha256: String,
    pub resource_offset: u64,
    pub resource_bytes: u64,
    pub files: Vec<Entry>,
}

pub fn identity() -> Result<Identity, Error> {
    Ok(serde_json::from_str(include_str!(
        "../../../../tools/qq-portable/client.json"
    ))?)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("qq_package", message)
}

pub fn file_sha256(path: &Path) -> Result<String, Error> {
    runtime::no_links(path)?;
    let mut input = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn verify_client(root: &Path, identity: &Identity) -> Result<(), Error> {
    for module in &identity.modules {
        if file_sha256(&root.join(&module.path))? != module.sha256 {
            return Err(invalid(format!(
                "QQ module identity changed: {}",
                module.path
            )));
        }
    }
    Ok(())
}

struct Resource<'a> {
    file: File,
    base: u64,
    length: u64,
    position: u64,
    cancelled: &'a std::sync::atomic::AtomicBool,
}

impl Read for Resource<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other("QQ extraction cancelled"));
        }
        let count = output.len().min((self.length - self.position) as usize);
        self.file.seek(SeekFrom::Start(self.base + self.position))?;
        let size = self.file.read(&mut output[..count])?;
        self.position += size as u64;
        Ok(size)
    }
}

impl Seek for Resource<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.length) + i128::from(value),
        };
        if position < 0 || position > i128::from(self.length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek outside QQ resource",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

fn read_at<const N: usize>(file: &mut File, offset: u64) -> Result<[u8; N], Error> {
    let mut bytes = [0; N];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn u16_at(file: &mut File, offset: u64) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(read_at(file, offset)?))
}
fn u32_at(file: &mut File, offset: u64) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(read_at(file, offset)?))
}

fn resource(file: &mut File) -> Result<(u64, u64), Error> {
    let length = file.metadata()?.len();
    if read_at::<2>(file, 0)? != *b"MZ" {
        return Err(invalid("Missing PE DOS header"));
    }
    let pe = u64::from(u32_at(file, 60)?);
    if pe > length.saturating_sub(256) || read_at::<4>(file, pe)? != *b"PE\0\0" {
        return Err(invalid("Invalid PE header"));
    }
    let sections = u16_at(file, pe + 6)?;
    let optional_size = u64::from(u16_at(file, pe + 20)?);
    let optional = pe + 24;
    let directories = match u16_at(file, optional)? {
        0x10b => optional + 96,
        0x20b => optional + 112,
        _ => return Err(invalid("Unsupported PE optional header")),
    };
    if sections == 0 || sections > 96 || optional_size < directories - optional + 24 {
        return Err(invalid("Invalid PE section count or optional header"));
    }
    let resource_rva = u64::from(u32_at(file, directories + 16)?);
    let resource_size = u64::from(u32_at(file, directories + 20)?);
    let mut section_map = Vec::new();
    for index in 0..u64::from(sections) {
        let entry = optional + optional_size + index * 40;
        let rva = u64::from(u32_at(file, entry + 12)?);
        let size = u64::from(u32_at(file, entry + 16)?);
        let offset = u64::from(u32_at(file, entry + 20)?);
        if offset.checked_add(size).is_none_or(|end| end > length) {
            return Err(invalid("PE section extends past file"));
        }
        section_map.push((rva, size, offset));
    }
    let map = |rva: u64, size: u64| -> Result<u64, Error> {
        section_map
            .iter()
            .find_map(|&(start, length, offset)| {
                let relative = rva.checked_sub(start)?;
                (relative.checked_add(size)? <= length).then_some(offset + relative)
            })
            .ok_or_else(|| invalid("PE resource extends outside a physical section"))
    };
    let base = map(resource_rva, resource_size)?;
    let mut directory = 0u64;
    for (level, desired) in [None, Some(101u32), Some(2052u32)].into_iter().enumerate() {
        if directory + 16 > resource_size {
            return Err(invalid("Truncated PE resource directory"));
        }
        let names = u64::from(u16_at(file, base + directory + 12)?);
        let ids = u64::from(u16_at(file, base + directory + 14)?);
        if names + ids > 4096 || directory + 16 + (names + ids) * 8 > resource_size {
            return Err(invalid("Invalid PE resource entries"));
        }
        let mut found = None;
        for index in 0..names + ids {
            let entry = base + directory + 16 + index * 8;
            let name = u32_at(file, entry)?;
            let matches = if let Some(id) = desired {
                name == id
            } else if name & 0x80000000 != 0 {
                let text = u64::from(name & 0x7fffffff);
                text + 8 <= resource_size
                    && u16_at(file, base + text)? == 3
                    && read_at::<6>(file, base + text + 2)? == *b"M\0S\0I\0"
            } else {
                false
            };
            if matches {
                if found.is_some() {
                    return Err(invalid("Duplicate QQ resource identity"));
                }
                found = Some(u32_at(file, entry + 4)?);
            }
        }
        let next = found.ok_or_else(|| invalid("QQ MSI/101/2052 resource is missing"))?;
        if (next & 0x80000000 != 0) != (level < 2) {
            return Err(invalid("Unexpected QQ resource tree"));
        }
        directory = u64::from(next & 0x7fffffff);
    }
    if directory + 16 > resource_size {
        return Err(invalid("Truncated PE resource data"));
    }
    let size = u64::from(u32_at(file, base + directory + 4)?);
    let offset = map(u64::from(u32_at(file, base + directory)?), size)?;
    if size < 32 || read_at::<6>(file, offset)? != [b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c] {
        return Err(invalid("QQ resource is not a 7z archive"));
    }
    Ok((offset, size))
}

fn entry_path(name: &str) -> Result<PathBuf, Error> {
    let normalized = name.replace('\\', "/");
    if normalized.len() > 2048
        || normalized.split('/').count() > 64
        || normalized.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.ends_with(['.', ' '])
                || part.contains([':', '\0', '<', '>', '"', '|', '?', '*'])
                || matches!(
                    part.split('.')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
    {
        return Err(invalid("Unsafe QQ archive entry path"));
    }
    Ok(PathBuf::from(normalized))
}

pub fn extract(installer: &Path, destination: &Path) -> Result<Extraction, Error> {
    extract_cancellable(
        installer,
        destination,
        &std::sync::atomic::AtomicBool::new(false),
    )
}

pub fn extract_cancellable(
    installer: &Path,
    destination: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Extraction, Error> {
    let identity = identity()?;
    runtime::no_links(installer)?;
    if fs::metadata(installer)?.len() != identity.installer_bytes
        || file_sha256(installer)? != identity.installer_sha256
    {
        return Err(invalid(
            "QQ installer length or SHA256 does not match the pinned identity",
        ));
    }
    runtime::no_links(destination)?;
    if destination.exists() {
        return Err(invalid("Extraction destination already exists"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("Extraction destination needs a parent"))?;
    runtime::regular_directory(parent)?;
    let mut file = File::open(installer)?;
    let (offset, length) = resource(&mut file)?;
    let source = Resource {
        file,
        base: offset,
        length,
        position: 0,
        cancelled,
    };
    let mut reader = sevenz_rust2::ArchiveReader::new(source, sevenz_rust2::Password::empty())
        .map_err(|error| invalid(error.to_string()))?;
    if reader.archive().files.len() > 100_000 {
        return Err(invalid("QQ archive exceeds its entry limit"));
    }
    let mut paths = BTreeSet::new();
    let mut total = 0u64;
    for entry in &reader.archive().files {
        entry_path(&entry.name)?;
        if entry.is_anti_item
            || entry.windows_attributes & 0x400 != 0
            || !paths.insert(entry.name.replace('\\', "/").to_ascii_lowercase())
        {
            return Err(invalid("Unsupported or duplicate QQ archive entry"));
        }
        total = total
            .checked_add(entry.size)
            .ok_or_else(|| invalid("QQ archive size overflow"))?;
        if total > 4 * 1024 * 1024 * 1024 || entry.size > 1024 * 1024 * 1024 {
            return Err(invalid("QQ archive exceeds its expanded size limit"));
        }
    }
    let staging = tempfile::Builder::new()
        .prefix("qq-extract-")
        .tempdir_in(parent)?;
    let mut files = Vec::new();
    reader
        .for_each_entries(|entry, input| {
            let path = staging.path().join(
                entry_path(&entry.name).map_err(|error| io::Error::other(error.to_string()))?,
            );
            if entry.is_directory {
                fs::create_dir_all(path)?;
                return Ok(true);
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::options().write(true).create_new(true).open(&path)?;
            let mut hash = Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                let size = input.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                bytes += size as u64;
                if bytes > entry.size {
                    return Err(io::Error::other("QQ entry exceeds its declared size").into());
                }
                output.write_all(&buffer[..size])?;
                hash.update(&buffer[..size]);
            }
            if bytes != entry.size {
                return Err(io::Error::other("Truncated QQ archive entry").into());
            }
            files.push(Entry {
                path: entry.name.replace('\\', "/"),
                bytes,
                sha256: format!("{:x}", hash.finalize()),
            });
            Ok(true)
        })
        .map_err(|error| invalid(error.to_string()))?;
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.as_str()))
        .collect();
    for module in &identity.modules {
        if hashes
            .get(format!("Files/{}", module.path).as_str())
            .copied()
            != Some(module.sha256.as_str())
        {
            return Err(invalid(format!(
                "Extracted module hash mismatch: {}",
                module.path
            )));
        }
    }
    fs::rename(staging.path(), destination)?;
    Ok(Extraction {
        schema_version: 1,
        build: identity.build,
        input_sha256: identity.installer_sha256,
        resource_offset: offset,
        resource_bytes: length,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_paths_reject_escapes_and_windows_aliases() {
        for name in [
            "../a", "/a", "C:/a", "a//b", "a\\..\\b", "AUX.txt", "a/LPT1", "a. /b", "a:",
        ] {
            assert!(entry_path(name).is_err(), "{name}");
        }
        assert_eq!(
            entry_path("Files\\versions\\app.node").unwrap(),
            PathBuf::from("Files/versions/app.node")
        );
    }
    #[test]
    fn invalid_installer_does_not_publish_or_delete_existing_data() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("installer.exe");
        fs::write(&input, b"MZ").unwrap();
        let destination = directory.path().join("unpacked");
        assert!(extract(&input, &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(fs::read(input).unwrap(), b"MZ");
    }
}
