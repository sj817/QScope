use qscope_automation::{Error, runtime};
use serde::Serialize;
use serde_json::Value;
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) fn failure(message: impl Into<String>) -> Error {
    Error::new("macos_collector", message)
}

pub(crate) fn cancelled(value: &AtomicBool) -> Result<(), Error> {
    if value.load(Ordering::Acquire) || crate::process::stopping() {
        Err(Error::new("qq_cancelled", "QQ preparation cancelled"))
    } else {
        Ok(())
    }
}

pub(crate) fn regular(path: &Path) -> Result<File, Error> {
    runtime::no_links(path)?;
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(failure("Expected a regular file"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(failure("Expected a regular file"));
    }
    Ok(file)
}

pub(crate) fn directory(path: &Path) -> Result<(), Error> {
    runtime::regular_directory(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(failure("Expected an isolated directory"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub(crate) fn inside(root: &Path, path: &Path) -> Result<(), Error> {
    if !root.is_absolute()
        || !path.is_absolute()
        || path == root
        || !path.starts_with(root)
        || root
            .components()
            .chain(path.components())
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(failure("Collector path escaped its owned directory"));
    }
    runtime::no_links(root)?;
    runtime::no_links(path)
}

pub(crate) fn read_json(path: &Path) -> Result<Value, Error> {
    let mut bytes = Vec::new();
    regular(path)?.take(131073).read_to_end(&mut bytes)?;
    if bytes.len() > 131072 {
        return Err(failure("Collector JSON exceeds 128 KiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    runtime::no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure("JSON needs a parent"))?;
    directory(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file_mut().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn lock(path: &Path) -> Result<File, Error> {
    runtime::no_links(path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(failure("Collector lock must be a regular file"));
    }
    file.try_lock().map_err(|_| {
        Error::new(
            "qq_busy",
            "The research package or profile is already in use",
        )
    })?;
    Ok(file)
}

pub(crate) struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub(crate) fn command(
    executable: &str,
    arguments: &[OsString],
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<Output, Error> {
    cancelled(stop)?;
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    fn reader(
        pipe: impl Read + Send + 'static,
    ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(131073).read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    }
    let stdout = reader(child.stdout.take().expect("stdout pipe"));
    let stderr = reader(child.stderr.take().expect("stderr pipe"));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if cancelled(stop).is_ok() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(error) => Error::from(error),
                    _ if cancelled(stop).is_err() => {
                        Error::new("qq_cancelled", "QQ preparation cancelled")
                    }
                    _ => Error::new(
                        "qq_tool_timeout",
                        format!("{executable} exceeded its deadline"),
                    ),
                });
            }
        }
    };
    let stdout = stdout
        .join()
        .map_err(|_| failure("Tool output reader failed"))??;
    let stderr = stderr
        .join()
        .map_err(|_| failure("Tool error reader failed"))??;
    let status = status?;
    if stdout.len() > 131072 || stderr.len() > 131072 {
        return Err(failure("macOS tool output exceeds 128 KiB"));
    }
    if !status.success() {
        return Err(Error::new(
            "qq_tool",
            format!(
                "{executable} failed ({}): {}",
                status,
                String::from_utf8_lossy(&stderr).trim()
            ),
        ));
    }
    Ok(Output { stdout, stderr })
}

pub(crate) fn args(values: &[&OsStr]) -> Vec<OsString> {
    values.iter().map(|value| value.to_os_string()).collect()
}
