use crate::{Error, client::loopback_url};
pub const BUILD_ID: &str = env!("QSCOPE_BUILD_ID");
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn no_links(path: &Path) -> Result<(), Error> {
    for ancestor in path.ancestors() {
        if let Ok(metadata) = fs::symlink_metadata(ancestor) {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(Error::new(
                        "data_directory",
                        "Data paths must not traverse a link or junction",
                    ));
                }
            }
            if metadata.file_type().is_symlink() {
                return Err(Error::new(
                    "data_directory",
                    "Data paths must not traverse a symbolic link",
                ));
            }
        }
    }
    Ok(())
}

pub fn regular_directory(path: &Path) -> Result<(), Error> {
    no_links(path)?;
    fs::create_dir_all(path)?;
    Ok(())
}

pub fn read_settings(path: &Path) -> Result<serde_json::Value, Error> {
    no_links(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::new("settings_limit", "Settings exceed 64 KiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_settings(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("settings_path", "Settings need a parent directory"))?;
    regular_directory(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file_mut().sync_all()?;
    file.persist(path)
        .map_err(|error| Error::from(error.error))?;
    Ok(())
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// Written under the data directory when an update stops the backend and the desktop should close.
pub const RESTART_MARKER: &str = "updates/restart.json";

/// Whether an update asked the desktop to close within the last few minutes.
pub fn restart_requested(data: &Path) -> bool {
    read_settings(&data.join(RESTART_MARKER))
        .ok()
        .and_then(|value| value["time_unix"].as_u64())
        .is_some_and(|time| unix_now().saturating_sub(time) < 5 * 60)
}

/// Removes executables an update moved aside, once nothing runs them any more.
pub fn cleanup_replaced(directory: &Path) {
    for name in ["QScope.exe.old", "qscope-cli.exe.old", "qs.exe.old"] {
        let path = directory.join(name);
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn data_lease(path: &Path) -> Result<File, Error> {
    regular_directory(path)?;
    let path = path.join(".desktop.lock");
    no_links(&path)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    lease.try_lock().map_err(|_| {
        Error::new(
            "service_busy",
            "This QScope data directory is already in use",
        )
    })?;
    Ok(lease)
}

#[derive(Clone)]
pub struct Layout {
    pub resources: PathBuf,
    pub storage: PathBuf,
    pub native: Option<PathBuf>,
    pub worker: Option<PathBuf>,
}

impl Layout {
    pub fn development(workspace: &Path) -> Self {
        let native_root = workspace.join("build/native/qqnt-windows-x64");
        let reference = fs::File::open(native_root.join("current.json"))
            .ok()
            .and_then(|file| serde_json::from_reader::<_, NativeBuild>(file.take(16384)).ok());
        let native = reference
            .filter(|reference| {
                reference.schema_version == 1
                    && reference.build_id.len() == 64
                    && reference
                        .build_id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            })
            .map(|reference| native_root.join(&reference.build_id[..16]));
        Self {
            resources: workspace.into(),
            storage: workspace.join("artifacts"),
            native,
            worker: Some(workspace.join(if cfg!(windows) {
                "target/debug/qscope-cli.exe"
            } else {
                "target/debug/qscope-cli"
            })),
        }
    }

    pub fn packaged(resources: PathBuf, storage: PathBuf) -> Self {
        Self {
            native: Some(resources.join("native")),
            worker: std::env::current_exe().ok(),
            resources,
            storage,
        }
    }

    pub fn package(&self) -> PathBuf {
        self.storage.join("qq/9.9.37-261103-x64")
    }

    pub fn host(&self) -> Result<PathBuf, Error> {
        let path = self
            .worker
            .clone()
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                Error::new(
                    "native_host_missing",
                    "The native QQ host is unavailable. Build the Windows collector first",
                )
            })?;
        Ok(path)
    }

    pub fn request(&self, profile: &str, port: u16) -> Result<HostRequest, Error> {
        let native = self.native.as_ref().ok_or_else(|| {
            Error::new("native_host_missing", "The native QQ host is unavailable")
        })?;
        Ok(HostRequest {
            schema_version: 1,
            kind: "qscope-native-host".into(),
            profile: profile.into(),
            port,
            data_directory: crate::display_path(&self.storage),
            package_directory: crate::display_path(&self.package()),
            resources_directory: crate::display_path(&self.resources),
            prebuilt_directory: crate::display_path(native),
            receipt_path: String::new(),
            progress_path: String::new(),
            duration_seconds: 0,
        })
    }
}

#[derive(Deserialize)]
struct NativeBuild {
    schema_version: u32,
    build_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct HostRequest {
    pub schema_version: u32,
    pub kind: String,
    pub profile: String,
    pub port: u16,
    pub data_directory: String,
    pub package_directory: String,
    pub resources_directory: String,
    pub prebuilt_directory: String,
    pub receipt_path: String,
    pub progress_path: String,
    pub duration_seconds: u32,
}

pub fn user_data() -> Result<PathBuf, Error> {
    if let Some(path) = DATA_DIRECTORY.get() {
        return Ok(path.clone());
    }
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(|path| PathBuf::from(path).join("QScope"))
            .ok_or_else(|| Error::new("data_directory", "LOCALAPPDATA is unavailable"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/QScope"))
            .ok_or_else(|| Error::new("data_directory", "HOME is unavailable"))
    } else {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .ok_or_else(|| Error::new("data_directory", "User data directory is unavailable"))?;
        Ok(base.join("QScope"))
    }
}

static DATA_DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_data_directory(path: PathBuf) -> Result<(), Error> {
    let path = std::path::absolute(path)?;
    DATA_DIRECTORY
        .set(path)
        .map_err(|_| Error::new("data_directory", "Data directory is already selected"))
}

pub fn instance_id() -> Result<String, Error> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|error| Error::new("random", error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn worker_command(path: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(path);
    command.arg("__collector");
    command
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Service {
    pub schema_version: u32,
    pub owner: String,
    pub instance_id: String,
    pub endpoint: String,
    pub pid: u32,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub build_id: String,
    #[serde(default)]
    pub resources: Option<PathBuf>,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
}

impl Service {
    pub fn read(path: &Path) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(Error::new(
                    "service_descriptor",
                    "Service descriptor must be a regular file",
                ));
            }
        }
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
            return Err(Error::new(
                "service_descriptor",
                "Invalid service descriptor",
            ));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 {
            return Err(Error::new(
                "service_descriptor",
                "Service descriptor exceeds 16 KiB",
            ));
        }
        let service: Self = serde_json::from_slice(&bytes)?;
        loopback_url(&service.endpoint)?;
        if !matches!(service.schema_version, 1 | 2)
            || !matches!(
                service.owner.as_str(),
                "qscope-desktop" | "qscope-cli" | "qscope-service"
            )
            || (service.schema_version == 2
                && (service.token.len() != 64
                    || !service.token.bytes().all(|byte| byte.is_ascii_hexdigit())))
            || service.instance_id.len() != 48
            || !service
                .instance_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || service.pid == 0
        {
            return Err(Error::new(
                "service_descriptor",
                "Unsupported service descriptor identity",
            ));
        }
        Ok(service)
    }

    pub fn publish(&self, path: &Path) -> Result<(), Error> {
        let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or_else(|| {
            Error::new("service_descriptor", "Descriptor needs a parent directory")
        })?)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file_mut().sync_all()?;
        file.persist(path)
            .map_err(|error| Error::from(error.error))?;
        Ok(())
    }

    pub fn browser_url(&self) -> String {
        if self.token.is_empty() {
            self.endpoint.clone()
        } else {
            format!("{}/#qscope-token={}", self.endpoint, self.token)
        }
    }

    pub fn remove_owned(&self, path: &Path) -> Result<(), Error> {
        if Self::read(path).is_ok_and(|current| current.instance_id == self.instance_id) {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_descriptor_preserves_other_instances_and_rejects_invalid_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("service.json");
        let mut first = Service {
            schema_version: 1,
            owner: "qscope-desktop".into(),
            instance_id: instance_id().unwrap(),
            endpoint: "http://127.0.0.1:19840".into(),
            pid: 123,
            token: String::new(),
            version: String::new(),
            build_id: String::new(),
            resources: None,
            workspace: None,
        };
        first.publish(&path).unwrap();
        assert_eq!(Service::read(&path).unwrap().instance_id, first.instance_id);
        let mut second = first.clone();
        second.instance_id = instance_id().unwrap();
        second.publish(&path).unwrap();
        first.remove_owned(&path).unwrap();
        assert!(path.exists());
        second.remove_owned(&path).unwrap();
        assert!(!path.exists());
        first.endpoint = "https://example.com".into();
        first.publish(&path).unwrap();
        assert!(Service::read(&path).is_err());
        fs::write(&path, vec![b' '; 16 * 1024 + 1]).unwrap();
        assert!(Service::read(&path).is_err());
    }
}
