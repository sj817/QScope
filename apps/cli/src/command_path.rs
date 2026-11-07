use crate::Error;
use clap::{Args, Subcommand};
use qscope_automation::{display_path, runtime};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Args)]
pub struct Options {
    /// 覆盖全局命令目录。
    #[arg(long, global = true)]
    directory: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 为当前安装注册全局 qs 命令。
    Install,
    /// 显示 qs 指向及当前安装是否拥有该命令。
    Status,
    /// 移除全局 qs 命令。
    Uninstall {
        /// 仅在 qs 由当前安装注册时移除。
        #[arg(long)]
        if_owned: bool,
    },
}

fn sha(path: &Path) -> Result<String, Error> {
    Ok(qscope_qqnt::package::file_sha256(path)?)
}

fn owner(path: &Path) -> Result<Value, Error> {
    runtime::no_links(path)?;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(Error::new(
            "cli_conflict",
            "Command ownership exceeds 16 KiB",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if value["schema_version"] != 1 || value["owner"] != "qscope-global-command" {
        return Err(Error::new(
            "cli_conflict",
            "Global command belongs to another application",
        ));
    }
    Ok(value)
}

fn save(path: &Path, value: &Value) -> Result<(), Error> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().expect("Owned command directory"))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file_mut().sync_all()?;
    file.persist(path)
        .map_err(|error| Error::from(error.error))?;
    Ok(())
}

#[cfg(windows)]
fn user_path() -> Result<String, Error> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Environment")?;
    match key.get_value("Path") {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(windows))]
fn user_path() -> Result<String, Error> {
    Ok(std::env::var("PATH").unwrap_or_default())
}

fn path_matches(value: &str, directory: &Path) -> bool {
    let normalized = value.trim().trim_matches('"').trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        normalized.eq_ignore_ascii_case(&directory.to_string_lossy())
    } else {
        normalized == directory.to_string_lossy()
    }
}

fn registered(directory: &Path) -> Result<bool, Error> {
    Ok(std::env::split_paths(&user_path()?)
        .any(|entry| path_matches(&entry.to_string_lossy(), directory)))
}

#[cfg(windows)]
fn update_path(directory: &Path, install: bool) -> Result<(), Error> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER, types::ToRegValue};
    let current = user_path()?;
    let present = current
        .split(';')
        .any(|value| path_matches(value, directory));
    let next = if install && !present {
        if current.is_empty() {
            directory.to_string_lossy().into_owned()
        } else {
            format!("{current};{}", directory.display())
        }
    } else if !install && present {
        current
            .split(';')
            .filter(|value| !path_matches(value, directory))
            .collect::<Vec<_>>()
            .join(";")
    } else {
        return Ok(());
    };
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Environment")?;
    let mut value = next.to_reg_value();
    if let Ok(previous) = key.get_raw_value("Path") {
        value.vtype = previous.vtype;
    }
    key.set_raw_value("Path", &value)?;
    qscope_qqnt::windows::notify_environment_changed();
    Ok(())
}

#[cfg(not(windows))]
fn update_path(_directory: &Path, _install: bool) -> Result<(), Error> {
    // 需要调整 PATH 时报告具体操作，shell 启动文件由用户维护。
    Ok(())
}

pub fn run(options: Options) -> Result<(), Error> {
    let directory = std::path::absolute(match options.directory {
        Some(path) => path,
        None => qscope_automation::skills::user_home()?
            .join(".local")
            .join("bin"),
    })?;
    let executable = directory.join(if cfg!(windows) { "qs.exe" } else { "qs" });
    let ownership = directory.join(".qs-command.json");
    runtime::no_links(&executable)?;
    if matches!(options.command, Command::Status) && !directory.exists() {
        return crate::print_json(
            &json!({"schema_version":1,"command":"qs","path":executable,"installed":false}),
        );
    }
    runtime::regular_directory(&directory)?;
    let lock_path = directory.join(".qs-command.lock");
    runtime::no_links(&lock_path)?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.try_lock()
        .map_err(|error| Error::new("cli_busy", error.to_string()))?;
    let previous = if ownership.exists() {
        Some(owner(&ownership)?)
    } else {
        None
    };
    let hash = if executable.exists() {
        Some(sha(&executable)?)
    } else {
        None
    };
    let source = std::env::current_exe()?.canonicalize()?;
    let desired = sha(&source)?;
    let same_source = previous
        .as_ref()
        .and_then(|value| value["target"].as_str())
        .is_some_and(|target| {
            if cfg!(windows) {
                display_path(Path::new(target)).eq_ignore_ascii_case(&display_path(&source))
            } else {
                Path::new(target) == source
            }
        });
    // 原地替换已注册源文件会同时更新硬链接。
    let owned = hash.as_ref().is_some_and(|hash| {
        previous
            .as_ref()
            .is_some_and(|value| value["sha256"] == *hash || value["pending_sha256"] == *hash)
            || (same_source && *hash == desired)
    });
    if matches!(options.command, Command::Uninstall { if_owned: true }) && !same_source {
        return crate::print_json(
            &json!({"schema_version":1,"installed":owned,"skipped":true,"command":"qs"}),
        );
    }
    if matches!(options.command, Command::Status) {
        let registered = registered(&directory)?;
        return crate::print_json(
            &json!({"schema_version":1,"command":"qs","path":executable,"installed":owned && registered,"owned":owned,"in_user_path":registered}),
        );
    }
    if hash.is_some() && !owned {
        return Err(Error::new(
            "cli_conflict",
            "The existing qs command has changed or belongs to another application",
        ));
    }
    if matches!(options.command, Command::Uninstall { .. }) {
        if let Some(previous) = previous {
            if executable.exists() {
                fs::remove_file(&executable).map_err(|error| {
                    Error::new(
                        "cli_in_use",
                        format!(
                            "Close running qs commands and uninstall with qscope-cli.exe. {error}"
                        ),
                    )
                })?;
            }
            if previous["path_added"] == true {
                update_path(&directory, false)?;
            }
            fs::remove_file(ownership)?;
        }
        return crate::print_json(&json!({"schema_version":1,"installed":false,"command":"qs"}));
    }
    let path_added = cfg!(windows)
        && (previous
            .as_ref()
            .is_some_and(|value| value["path_added"] == true)
            || !registered(&directory)?);
    // 先持久化操作意图，重试时可识别中断更新的前后状态。
    let mut value = json!({"schema_version":1,"owner":"qscope-global-command","target":source,"sha256":hash.as_ref().unwrap_or(&desired),"pending_sha256":desired,"path_added":path_added,"version":env!("CARGO_PKG_VERSION")});
    save(&ownership, &value)?;
    if hash.as_ref() != Some(&desired) {
        let staging = tempfile::Builder::new()
            .prefix("qs-install-")
            .tempdir_in(&directory)?;
        let staged = staging.path().join("qs");
        if fs::hard_link(&source, &staged).is_err() {
            fs::copy(&source, &staged)?;
        }
        if sha(&staged)? != desired {
            return Err(Error::new(
                "cli_changed",
                "CLI changed during command registration",
            ));
        }
        fs::rename(staged, &executable).map_err(|error| {
            Error::new(
                "cli_in_use",
                format!("Close running qs commands before updating. {error}"),
            )
        })?;
    }
    update_path(&directory, true)?;
    value["sha256"] = desired.into();
    value
        .as_object_mut()
        .expect("Command ownership object")
        .remove("pending_sha256");
    save(&ownership, &value)?;
    crate::print_json(
        &json!({"schema_version":1,"installed":true,"command":"qs","path":executable,"open_new_terminal":cfg!(windows),"in_user_path":registered(&directory)?,"path_directory":directory,"path_setup_required":!cfg!(windows) && !registered(&directory)?}),
    )
}
