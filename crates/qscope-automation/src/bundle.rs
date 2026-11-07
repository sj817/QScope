use crate::{Error, runtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// 将写入数据目录的原生采集器文件与内嵌资源比较。
#[derive(Debug, Default)]
pub struct NativeCheck {
    /// 文件是否至少写入过一次。
    pub written: bool,
    pub missing: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    owner: String,
    files: Vec<Entry>,
}

#[derive(Clone)]
pub struct Resources {
    bytes: &'static [u8],
    id: String,
    files: Arc<BTreeMap<String, Entry>>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("resource_bundle", message)
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 2048
        && !path.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
        && path.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && !part.ends_with(['.', ' '])
                && !matches!(
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
}

impl Resources {
    pub fn embedded(bytes: &'static [u8]) -> Result<Self, Error> {
        if bytes.is_empty() {
            return Err(invalid(
                "This development CLI has no embedded resources. Build the product or supply --resources",
            ));
        }
        if bytes.len() > 256 * 1024 * 1024 {
            return Err(invalid("Resource archive exceeds 256 MiB"));
        }
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| invalid(error.to_string()))?;
        if archive.len() > 20_001 {
            return Err(invalid("Resource entry limit exceeded"));
        }
        let mut manifest = Vec::new();
        archive
            .by_name("manifest.json")
            .map_err(|error| invalid(error.to_string()))?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut manifest)?;
        if manifest.len() > 4 * 1024 * 1024 {
            return Err(invalid("Resource manifest exceeds its limit"));
        }
        let manifest: Manifest = serde_json::from_slice(&manifest)?;
        if manifest.schema_version != 1
            || manifest.owner != "qscope-resources"
            || manifest.files.len() + 1 != archive.len()
        {
            return Err(invalid("Unsupported or incomplete resource manifest"));
        }
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        for entry in manifest.files {
            if !safe_path(&entry.path)
                || entry.bytes > 64 * 1024 * 1024
                || entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(invalid("Invalid resource manifest entry"));
            }
            total = total
                .checked_add(entry.bytes)
                .ok_or_else(|| invalid("Resource size overflow"))?;
            if total > 512 * 1024 * 1024
                || files
                    .insert(entry.path.to_ascii_lowercase(), entry)
                    .is_some()
            {
                return Err(invalid(
                    "Duplicate resource or expanded size limit exceeded",
                ));
            }
        }
        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .map_err(|error| invalid(error.to_string()))?;
            let name = file.name().map_err(|error| invalid(error.to_string()))?;
            if name == "manifest.json" {
                continue;
            }
            if !files
                .get(&name.to_ascii_lowercase())
                .is_some_and(|entry| entry.path == name && entry.bytes == file.size())
            {
                return Err(invalid("Resource archive differs from its manifest"));
            }
        }
        Ok(Self {
            bytes,
            id: format!("{:x}", Sha256::digest(bytes)),
            files: Arc::new(files),
        })
    }

    pub fn contains(&self, path: &str) -> bool {
        self.files
            .get(&path.to_ascii_lowercase())
            .is_some_and(|entry| entry.path == path)
    }

    pub fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>, Error> {
        let entry = self
            .files
            .get(&path.to_ascii_lowercase())
            .filter(|entry| entry.path == path)
            .ok_or_else(|| invalid("Embedded resource was not found"))?;
        if entry.bytes > limit {
            return Err(invalid("Requested resource exceeds its byte limit"));
        }
        let mut archive = zip::ZipArchive::new(Cursor::new(self.bytes))
            .map_err(|error| invalid(error.to_string()))?;
        let mut bytes = Vec::with_capacity(entry.bytes as usize);
        archive
            .by_name(path)
            .map_err(|error| invalid(error.to_string()))?
            .take(entry.bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != entry.bytes
            || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
        {
            return Err(invalid("Embedded resource hash mismatch"));
        }
        Ok(bytes)
    }

    pub fn root(&self, data: &Path) -> PathBuf {
        data.join("cache/resources").join(&self.id)
    }

    pub fn materialize(&self, data: &Path, path: &str) -> Result<PathBuf, Error> {
        let bytes = self.read(path, 64 * 1024 * 1024)?;
        let destination = self.root(data).join(path);
        runtime::no_links(&destination)?;
        if destination.exists() {
            if fs::metadata(&destination)?.len() != bytes.len() as u64
                || fs::read(&destination)? != bytes
            {
                return Err(invalid("Cached resource has changed"));
            }
            return Ok(destination);
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("Resource needs a parent directory"))?;
        runtime::regular_directory(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&bytes)?;
        file.as_file_mut().sync_all()?;
        match file.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error)
                if error.error.kind() == std::io::ErrorKind::AlreadyExists
                    && fs::metadata(&destination)?.len() == bytes.len() as u64
                    && fs::read(&destination)? == bytes => {}
            Err(error) => return Err(error.error.into()),
        }
        Ok(destination)
    }

    fn native(&self) -> impl Iterator<Item = &Entry> {
        self.files.values().filter(|entry| {
            entry.path.starts_with("native/")
                || entry.path.starts_with("collectors/qqnt-windows/")
                || entry.path.starts_with("collectors/qqnt-macos/")
        })
    }

    pub fn prepare_native(&self, data: &Path) -> Result<PathBuf, Error> {
        for entry in self.native() {
            self.materialize(data, &entry.path)?;
        }
        Ok(self.root(data))
    }

    pub fn check_native(&self, data: &Path) -> Result<NativeCheck, Error> {
        let root = self.root(data);
        let mut check = NativeCheck {
            written: root.exists(),
            ..Default::default()
        };
        for entry in self.native() {
            let path = root.join(&entry.path);
            runtime::no_links(&path)?;
            match fs::metadata(&path) {
                Err(_) => check.missing.push(entry.path.clone()),
                Ok(metadata) => {
                    if metadata.len() != entry.bytes
                        || format!("{:x}", Sha256::digest(fs::read(&path)?)) != entry.sha256
                    {
                        check.changed.push(entry.path.clone());
                    }
                }
            }
        }
        Ok(check)
    }

    /// 从内嵌资源重新写入原生文件，替换已发生变化的副本。
    pub fn repair_native(&self, data: &Path) -> Result<(), Error> {
        let check = self.check_native(data)?;
        let root = self.root(data);
        for path in &check.changed {
            fs::remove_file(root.join(path))?;
        }
        self.prepare_native(data).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;
    fn fixture(path: &str, actual: &[u8], expected: &[u8]) -> &'static [u8] {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(serde_json::json!({"schema_version":1,"owner":"qscope-resources","files":[{"path":path,"bytes":expected.len(),"sha256":format!("{:x}",Sha256::digest(expected))}]}).to_string().as_bytes()).unwrap();
        zip.start_file(path, options).unwrap();
        zip.write_all(actual).unwrap();
        Box::leak(zip.finish().unwrap().into_inner().into_boxed_slice())
    }
    #[test]
    fn resources_are_read_on_demand_and_cache_changes_are_rejected() {
        let resources =
            Resources::embedded(fixture("native/test.dll", b"sample", b"sample")).unwrap();
        let data = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        assert!(!resources.root(data.path()).exists());
        assert_eq!(resources.read("native/test.dll", 32).unwrap(), b"sample");
        assert!(!resources.root(data.path()).exists());
        let path = resources
            .materialize(data.path(), "native/test.dll")
            .unwrap();
        fs::write(&path, b"changed").unwrap();
        assert!(
            resources
                .materialize(data.path(), "native/test.dll")
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"changed");
    }
    #[test]
    fn native_files_report_changes_and_repair_writes_them_again() {
        let resources =
            Resources::embedded(fixture("native/test.dll", b"sample", b"sample")).unwrap();
        let data = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let check = resources.check_native(data.path()).unwrap();
        assert!(!check.written);
        assert_eq!(check.missing, ["native/test.dll"]);
        let path = resources
            .materialize(data.path(), "native/test.dll")
            .unwrap();
        assert!(
            resources
                .check_native(data.path())
                .unwrap()
                .changed
                .is_empty()
        );
        fs::write(&path, b"changed").unwrap();
        assert_eq!(
            resources.check_native(data.path()).unwrap().changed,
            ["native/test.dll"]
        );
        resources.repair_native(data.path()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"sample");
        fs::remove_file(&path).unwrap();
        let check = resources.check_native(data.path()).unwrap();
        assert!(check.written && check.missing.len() == 1);
        resources.repair_native(data.path()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"sample");
    }
    #[test]
    fn resource_paths_and_damaged_entries_are_rejected() {
        assert!(Resources::embedded(fixture("../escape", b"sample", b"sample")).is_err());
        let resources =
            Resources::embedded(fixture("ui/index.html", b"broken", b"sample")).unwrap();
        assert!(resources.read("ui/index.html", 32).is_err());
        assert!(resources.read("ui/index.html", 1).is_err());
    }
}
