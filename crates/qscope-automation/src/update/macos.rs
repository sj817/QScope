//! macOS 更新整体替换应用包，保留签名字节及符号链接。
use super::{Install, MAX_PACKAGE, semver};
use crate::{Error, runtime};
use serde_json::Value;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn invalid(message: impl Into<String>) -> Error {
    Error::new("update_package", message)
}

pub(super) fn bundle_root(directory: &Path) -> Option<PathBuf> {
    if directory.file_name()? != "MacOS" {
        return None;
    }
    let contents = directory.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    app.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        .then(|| app.to_path_buf())
}

fn tool(program: &str, arguments: &[&OsStr]) -> Result<Vec<u8>, Error> {
    let mut out = tempfile::tempfile()?;
    let mut err = tempfile::tempfile()?;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(err.try_clone()?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if out.metadata()?.len() > 65536 || err.metadata()?.len() > 65536 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("macOS validation output exceeds 64 KiB"));
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(
                "update_timeout",
                format!("{program} exceeded 60 seconds"),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    out.seek(SeekFrom::Start(0))?;
    err.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    out.take(65537).read_to_end(&mut output)?;
    err.take(65537).read_to_end(&mut errors)?;
    if output.len() > 65536 || errors.len() > 65536 {
        return Err(invalid("macOS validation output exceeds 64 KiB"));
    }
    if !status.success() {
        return Err(invalid(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&errors).trim()
        )));
    }
    Ok(output)
}

fn executable(path: &Path) -> Result<(), Error> {
    runtime::no_links(path)?;
    if !fs::symlink_metadata(path)?.is_file()
        || fs::metadata(path)?.permissions().mode() & 0o111 == 0
    {
        return Err(invalid("更新包缺少可执行文件或执行权限"));
    }
    let mut header = [0u8; 32];
    File::open(path)?.read_exact(&mut header)?;
    if u32::from_le_bytes(header[..4].try_into().expect("magic")) != 0xfeedfacf
        || u32::from_le_bytes(header[4..8].try_into().expect("cpu")) != 0x0100000c
        || u32::from_le_bytes(header[12..16].try_into().expect("type")) != 2
    {
        return Err(invalid("更新包必须包含 Apple Silicon Mach-O 可执行文件"));
    }
    tool(
        "/usr/bin/codesign",
        &["--verify".as_ref(), "--strict".as_ref(), path.as_os_str()],
    )?;
    Ok(())
}

fn verify_tree(path: &Path, root: &Path, count: &mut usize, depth: usize) -> Result<(), Error> {
    *count += 1;
    if *count > 20000 || depth > 64 {
        return Err(invalid("应用包目录数量或深度超过限制"));
    }
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        if !path.canonicalize()?.starts_with(root) {
            return Err(invalid("应用包符号链接越过包边界"));
        }
    } else if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            verify_tree(&entry?.path(), root, count, depth + 1)?;
        }
    } else if !meta.is_file() {
        return Err(invalid("应用包含非常规文件"));
    }
    Ok(())
}

fn app_version(app: &Path, expected: Option<&str>) -> Result<String, Error> {
    runtime::no_links(app)?;
    let root = app.canonicalize()?;
    verify_tree(&root, &root, &mut 0, 0)?;
    let info = app.join("Contents/Info.plist");
    runtime::no_links(&info)?;
    let bytes = tool(
        "/usr/bin/plutil",
        &[
            "-convert".as_ref(),
            "json".as_ref(),
            "-o".as_ref(),
            "-".as_ref(),
            info.as_os_str(),
        ],
    )?;
    let info: Value = serde_json::from_slice(&bytes)?;
    let configuration: Value =
        serde_json::from_str(include_str!("../../../../apps/desktop/tauri.conf.json"))?;
    let version = info["CFBundleShortVersionString"]
        .as_str()
        .ok_or_else(|| invalid("应用缺少版本号"))?;
    semver(version)?;
    if info["CFBundleIdentifier"] != configuration["identifier"]
        || info["CFBundleExecutable"] != "QScope"
        || expected.is_some_and(|expected| expected != version)
    {
        return Err(invalid("更新应用的身份或版本与签名清单不符"));
    }
    for name in ["QScope", "qscope-cli"] {
        executable(&app.join("Contents/MacOS").join(name))?;
    }
    tool(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app.as_os_str(),
        ],
    )?;
    Ok(version.into())
}

fn cli_version(cli: &Path, expected: Option<&str>) -> Result<(), Error> {
    executable(cli)?;
    let actual = tool(
        cli.to_str()
            .ok_or_else(|| invalid("CLI path is not UTF-8"))?,
        &["--version".as_ref()],
    )?;
    let version = std::str::from_utf8(&actual)
        .ok()
        .and_then(|text| text.split_whitespace().nth(1))
        .ok_or_else(|| invalid("CLI 缺少版本号"))?;
    semver(version)?;
    if expected.is_some_and(|expected| expected != version) {
        return Err(invalid("CLI 版本与更新清单不符"));
    }
    Ok(())
}

struct Entry {
    index: usize,
    name: String,
    directory: bool,
    link: Option<String>,
    mode: u32,
    metadata: bool,
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 2048
        && !name.starts_with('/')
        && !name.contains(['\\', ':'])
        && !name.chars().any(char::is_control)
        && name
            .trim_end_matches('/')
            .split('/')
            .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
}

fn link_target(name: &str, target: &str) -> Result<(), Error> {
    if target.is_empty()
        || target.len() > 4096
        || target.contains(['\\', ':'])
        || target.chars().any(char::is_control)
    {
        return Err(invalid("更新包符号链接目标无效"));
    }
    let mut parts: Vec<_> = name.split('/').collect();
    parts.pop();
    for part in Path::new(target).components() {
        match part {
            Component::Normal(part) => {
                parts.push(part.to_str().ok_or_else(|| invalid("符号链接不是 UTF-8"))?)
            }
            Component::CurDir => {}
            Component::ParentDir if parts.len() > 1 => {
                parts.pop();
            }
            _ => return Err(invalid("更新包符号链接越过应用包边界")),
        }
    }
    if parts.first() != Some(&"QScope.app") {
        return Err(invalid("更新包符号链接越过应用包边界"));
    }
    Ok(())
}

pub(super) fn unpack(package: &Path, install: Install) -> Result<Vec<(String, PathBuf)>, Error> {
    runtime::no_links(package)?;
    let filename = package
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("更新包文件名无效"))?;
    let suffix = if install == Install::App {
        "-macos-arm64.zip"
    } else {
        "-macos-arm64-cli-webui.zip"
    };
    let version = if filename == format!("QScope{suffix}") {
        None
    } else {
        // 兼容已有发布归档，版本一致性由签名清单校验。
        let version = filename
            .strip_prefix("QScope-")
            .and_then(|name| name.strip_suffix(suffix))
            .ok_or_else(|| invalid("更新包的平台或架构不符"))?;
        semver(version)?;
        Some(version)
    };
    let mut archive =
        zip::ZipArchive::new(File::open(package)?).map_err(|error| invalid(error.to_string()))?;
    if archive.len() > 20000 {
        return Err(invalid("更新包文件数量超过限制"));
    }
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut links = HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| invalid(error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| invalid(error.to_string()))?
            .trim_end_matches('/')
            .to_string();
        if !safe_name(&name) || !names.insert(name.to_lowercase()) {
            return Err(invalid("更新包路径不安全或重复"));
        }
        let metadata = name == "__MACOSX"
            || name.starts_with("__MACOSX/")
            || (install == Install::Cli && name == "._qscope-cli");
        let allowed = if install == Install::App {
            name == "QScope.app"
                || name.starts_with("QScope.app/")
                || name == "__MACOSX"
                || name == "__MACOSX/._QScope.app"
                || name == "__MACOSX/QScope.app"
                || name.starts_with("__MACOSX/QScope.app/")
        } else {
            name == "qscope-cli"
                || name == "__MACOSX"
                || name == "__MACOSX/._qscope-cli"
                || name == "._qscope-cli"
        };
        let mode = entry.unix_mode().unwrap_or(0o644);
        let kind = mode & 0o170000;
        if !allowed
            || !matches!(kind, 0 | 0o100000 | 0o040000 | 0o120000)
            || (metadata && kind == 0o120000)
        {
            return Err(invalid("更新包包含意外的文件"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid("更新包长度溢出"))?;
        if total > MAX_PACKAGE {
            return Err(invalid("更新包解压超过 1 GiB"));
        }
        let link = if kind == 0o120000 {
            if entry.size() > 4096 || install != Install::App {
                return Err(invalid("更新包符号链接无效"));
            }
            let mut bytes = Vec::new();
            entry.by_ref().take(4097).read_to_end(&mut bytes)?;
            let target = String::from_utf8(bytes).map_err(|_| invalid("符号链接不是 UTF-8"))?;
            link_target(&name, &target)?;
            links.insert(name.clone());
            Some(target)
        } else {
            None
        };
        entries.push(Entry {
            index,
            name,
            directory: entry.is_dir(),
            link,
            mode,
            metadata,
        });
    }
    for entry in &entries {
        for parent in Path::new(&entry.name).ancestors().skip(1) {
            if links.contains(parent.to_str().unwrap_or_default()) {
                return Err(invalid("更新包含符号链接目录下的写入"));
            }
        }
    }
    let parent = package
        .parent()
        .ok_or_else(|| invalid("更新包缺少父目录"))?;
    let output = tempfile::Builder::new()
        .prefix(".qscope-update-")
        .tempdir_in(parent)?;
    for entry in entries
        .iter()
        .filter(|entry| !entry.metadata && entry.link.is_none())
    {
        let destination = output.path().join(&entry.name);
        if entry.directory {
            fs::create_dir_all(&destination)?;
            continue;
        }
        fs::create_dir_all(destination.parent().expect("archive entry parent"))?;
        let mut input = archive
            .by_index(entry.index)
            .map_err(|error| invalid(error.to_string()))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)?;
        let expected = input.size();
        let copied = std::io::copy(&mut input.by_ref().take(MAX_PACKAGE + 1), &mut file)?;
        if copied != expected {
            return Err(invalid("更新包文件长度不符"));
        }
        file.sync_all()?;
        fs::set_permissions(
            &destination,
            fs::Permissions::from_mode(0o644 | (entry.mode & 0o111)),
        )?;
    }
    for entry in entries.iter().filter(|entry| entry.link.is_some()) {
        let destination = output.path().join(&entry.name);
        fs::create_dir_all(destination.parent().expect("symlink parent"))?;
        runtime::no_links(destination.parent().expect("symlink parent"))?;
        symlink(entry.link.as_ref().expect("symlink target"), &destination)?;
    }
    let (name, file) = if install == Install::App {
        let app = output.path().join("QScope.app");
        app_version(&app, version)?;
        ("QScope.app", app)
    } else {
        let cli = output.path().join("qscope-cli");
        cli_version(&cli, version)?;
        ("qscope-cli", cli)
    };
    let _ = output.keep();
    Ok(vec![(name.into(), file)])
}

pub(super) fn preflight(
    install: Install,
    directory: &Path,
    files: &[(String, PathBuf)],
    version: &str,
) -> Result<(), Error> {
    if let [(name, source)] = files {
        if install == Install::App && name == "QScope.app" {
            app_version(directory, None)?;
            app_version(source, Some(version))?;
        } else if install == Install::Cli && name == "qscope-cli" {
            cli_version(source, Some(version))?;
        } else {
            return Err(invalid("更新包与安装方式不符"));
        }
    } else {
        return Err(invalid("更新包与安装方式不符"));
    }
    let parent = if install == Install::App {
        directory
            .parent()
            .ok_or_else(|| invalid("应用缺少父目录"))?
    } else {
        directory
    };
    runtime::no_links(parent)?;
    let _probe = tempfile::Builder::new()
        .prefix(".qscope-update-permission-")
        .tempdir_in(parent)
        .map_err(|error| {
            Error::new(
                "update_permission",
                format!("无法写入安装目录，请手动下载安装包：{error}"),
            )
        })?;
    Ok(())
}

fn publish(target: &Path, fresh: &Path, backup: &Path, version: &str) -> Result<(), Error> {
    fs::rename(target, backup)?;
    let result = (|| {
        fs::rename(fresh, target)?;
        app_version(target, Some(version))?;
        Ok::<(), Error>(())
    })();
    if let Err(error) = result {
        let rollback = (|| {
            if target.exists() {
                fs::rename(target, fresh)?;
            }
            fs::rename(backup, target)
        })();
        if let Err(rollback) = rollback {
            return Err(Error::new(
                "update_rollback",
                format!(
                    "更新失败：{error}；恢复失败：{rollback}。原版保留于 {}",
                    backup.display()
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn replace_app(target: &Path, source: &Path) -> Result<(), Error> {
    runtime::no_links(target)?;
    runtime::no_links(source)?;
    if target == source || target.starts_with(source) || source.starts_with(target) {
        return Err(invalid("更新目录与原应用重叠"));
    }
    app_version(target, None)?;
    let version = app_version(source, None)?;
    let parent = target.parent().ok_or_else(|| invalid("应用缺少父目录"))?;
    runtime::no_links(parent)?;
    // 先复制到已安装应用旁，保证最终重命名位于同一卷。
    let stage = tempfile::Builder::new()
        .prefix(".qscope-update-")
        .tempdir_in(parent)
        .map_err(|error| {
            Error::new(
                "update_permission",
                format!("无法写入应用目录，请手动安装 DMG：{error}"),
            )
        })?;
    let fresh = stage.path().join("QScope.app");
    tool("/usr/bin/ditto", &[source.as_os_str(), fresh.as_os_str()])?;
    app_version(&fresh, Some(&version))?;
    let backup = parent.join(format!(
        ".{}.qscope-backup-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("QScope.app"),
        runtime::instance_id()?
    ));
    publish(target, &fresh, &backup, &version)?;
    // 旧 CLI 或界面仍可能映射文件，因此保留完整备份。
    Ok(())
}

fn wait_for_exit(data: &Path, wait: Duration) -> Result<(File, File), Error> {
    let deadline = Instant::now() + wait;
    loop {
        if let Ok(backend) = runtime::data_lease(data)
            && let Ok(desktop) = runtime::desktop_exclusive_lease(data)
        {
            return Ok((backend, desktop));
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "update_restart",
                "旧界面或后台尚未退出，保留更新后的应用，请手动重新打开 QScope",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn relaunch(directory: &Path, data: &Path) -> Result<(), Error> {
    app_version(directory, None)?;
    let leases = wait_for_exit(data, Duration::from_secs(60))?;
    let _ = fs::remove_file(data.join(runtime::RESTART_MARKER));
    drop(leases);
    tool(
        "/usr/bin/open",
        &[
            "-n".as_ref(),
            directory.as_os_str(),
            "--args".as_ref(),
            "--data-dir".as_ref(),
            data.as_os_str(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }

    #[test]
    fn archives_reject_traversal_external_links_and_writes_below_links() {
        for (name, link, child) in [
            ("QScope.app/../escape", None, None),
            ("/QScope.app/escape", None, None),
            ("QScope.app/link", Some("../../escape"), None),
            (
                "QScope.app/Contents",
                Some("Resources"),
                Some("QScope.app/Contents/Info.plist"),
            ),
            ("__MACOSX/QScope.app/link", Some("../QScope.app"), None),
        ] {
            let root = root();
            let package = root.path().join("QScope-macos-arm64.zip");
            let mut writer = zip::ZipWriter::new(File::create(&package).unwrap());
            let options = SimpleFileOptions::default().unix_permissions(0o755);
            if let Some(target) = link {
                writer.add_symlink(name, target, options).unwrap();
            } else {
                writer.start_file(name, options).unwrap();
                writer.write_all(b"synthetic").unwrap();
            }
            if let Some(child) = child {
                writer.start_file(child, options).unwrap();
                writer.write_all(b"synthetic").unwrap();
            }
            writer.finish().unwrap();
            assert!(unpack(&package, Install::App).is_err(), "{name}");
            assert!(!root.path().join("escape").exists());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn missing_staged_app_restores_the_complete_original_directory() {
        let root = root();
        let target = root.path().join("QScope.app");
        let backup = root.path().join("backup");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("original"), b"original bytes").unwrap();
        assert!(publish(&target, &root.path().join("missing.app"), &backup, "0.2.0").is_err());
        assert_eq!(
            fs::read(target.join("original")).unwrap(),
            b"original bytes"
        );
        assert!(!backup.exists());
    }

    #[test]
    fn wrong_architecture_is_rejected_before_executing_a_downloaded_cli() {
        let root = root();
        let package = root.path().join("QScope-macos-arm64-cli-webui.zip");
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
        let mut writer = zip::ZipWriter::new(File::create(&package).unwrap());
        writer
            .start_file(
                "qscope-cli",
                SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        writer.write_all(&bytes).unwrap();
        writer.finish().unwrap();
        assert!(
            unpack(&package, Install::Cli)
                .unwrap_err()
                .message
                .contains("Apple Silicon")
        );
        let legacy = root.path().join("QScope-0.2.0-macos-arm64-cli-webui.zip");
        fs::copy(&package, &legacy).unwrap();
        assert!(
            unpack(&legacy, Install::Cli)
                .unwrap_err()
                .message
                .contains("Apple Silicon")
        );
    }

    #[test]
    fn relaunch_waits_for_both_backend_and_desktop_leases() {
        let root = root();
        let backend = runtime::data_lease(root.path()).unwrap();
        assert!(wait_for_exit(root.path(), Duration::from_millis(10)).is_err());
        drop(backend);
        let desktop = runtime::desktop_lease(root.path()).unwrap();
        assert!(wait_for_exit(root.path(), Duration::from_millis(10)).is_err());
        drop(desktop);
        drop(wait_for_exit(root.path(), Duration::from_millis(10)).unwrap());
    }

    #[test]
    #[ignore = "requires signed Mac build artifacts; run node tests/update/macos.ts"]
    fn packaged_app_and_cli_preserve_signatures_modes_and_rollback_backup() {
        let root = root();
        let source =
            PathBuf::from(std::env::var_os("QSCOPE_MAC_UPDATE_ARCHIVE").expect("Mac app ZIP"));
        let package = root.path().join("QScope-macos-arm64.zip");
        fs::copy(source, &package).unwrap();
        let files = unpack(&package, Install::App).unwrap();
        let app = &files[0].1;
        let version = app_version(app, None).unwrap();
        let resources = app.join("Contents/Resources");
        fs::write(
            resources.join("qscope-synthetic-target.txt"),
            b"synthetic link target",
        )
        .unwrap();
        symlink(
            "qscope-synthetic-target.txt",
            resources.join("qscope-synthetic-link"),
        )
        .unwrap();
        tool(
            "/usr/bin/codesign",
            &[
                "--force".as_ref(),
                "--sign".as_ref(),
                "-".as_ref(),
                app.as_os_str(),
            ],
        )
        .unwrap();
        let repacked = root.path().join("resigned");
        fs::create_dir(&repacked).unwrap();
        let linked_package = repacked.join(package.file_name().unwrap());
        tool(
            "/usr/bin/ditto",
            &[
                "-c".as_ref(),
                "-k".as_ref(),
                "--sequesterRsrc".as_ref(),
                "--keepParent".as_ref(),
                app.as_os_str(),
                linked_package.as_os_str(),
            ],
        )
        .unwrap();
        let files = unpack(&linked_package, Install::App).unwrap();
        let app = &files[0].1;
        let directory = root.path().join("中文 安装目录");
        fs::create_dir(&directory).unwrap();
        let target = directory.join("QScope.app");
        tool("/usr/bin/ditto", &[app.as_os_str(), target.as_os_str()]).unwrap();
        let old_file = target.join("Contents/Resources/old-build-test.txt");
        fs::write(&old_file, b"previous build").unwrap();
        tool(
            "/usr/bin/codesign",
            &[
                "--force".as_ref(),
                "--sign".as_ref(),
                "-".as_ref(),
                target.as_os_str(),
            ],
        )
        .unwrap();
        preflight(Install::App, &target, &files, &version).unwrap();
        replace_app(&target, app).unwrap();
        assert!(!old_file.exists());
        app_version(&target, Some(&version)).unwrap();
        assert_eq!(
            fs::read_link(target.join("Contents/Resources/qscope-synthetic-link")).unwrap(),
            Path::new("qscope-synthetic-target.txt")
        );
        let backup = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".QScope.app.qscope-backup-")
            })
            .unwrap();
        assert_eq!(
            fs::read(backup.join("Contents/Resources/old-build-test.txt")).unwrap(),
            b"previous build"
        );
        assert_eq!(
            bundle_root(&target.join("Contents/MacOS")),
            Some(target.clone())
        );
        assert_eq!(super::super::classify(&target), Install::App);
        let original =
            super::super::file_sha256(&target.join("Contents/MacOS/qscope-cli")).unwrap();
        fs::write(app.join("Contents/MacOS/qscope-cli"), b"damaged").unwrap();
        assert!(replace_app(&target, app).is_err());
        assert_eq!(
            super::super::file_sha256(&target.join("Contents/MacOS/qscope-cli")).unwrap(),
            original
        );

        let source =
            PathBuf::from(std::env::var_os("QSCOPE_MAC_CLI_UPDATE_ARCHIVE").expect("Mac CLI ZIP"));
        let package = root.path().join("QScope-macos-arm64-cli-webui.zip");
        fs::copy(source, &package).unwrap();
        let files = unpack(&package, Install::Cli).unwrap();
        let directory = root.path().join("独立 CLI");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("qscope-cli"), b"old").unwrap();
        assert!(preflight(Install::Cli, &directory, &files, "0.0.0").is_err());
        assert_eq!(fs::read(directory.join("qscope-cli")).unwrap(), b"old");
        preflight(Install::Cli, &directory, &files, &version).unwrap();
        super::super::replace(&directory, &files).unwrap();
        executable(&directory.join("qscope-cli")).unwrap();
        assert_eq!(fs::read(directory.join("qscope-cli.old")).unwrap(), b"old");
        let legacy = root
            .path()
            .join(format!("QScope-{version}-macos-arm64-cli-webui.zip"));
        fs::copy(&package, &legacy).unwrap();
        assert!(unpack(&legacy, Install::Cli).is_ok());
        let mismatch = root.path().join("QScope-0.0.0-macos-arm64-cli-webui.zip");
        fs::copy(&package, &mismatch).unwrap();
        assert!(unpack(&mismatch, Install::Cli).is_err());
    }
}
