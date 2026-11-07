use crate::{Error, print_json};
use clap::{Args, Subcommand};
use qscope_automation::{display_path, runtime, service, update};
use serde_json::json;
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

#[derive(Args)]
pub struct Options {
    /// Only report the latest release without downloading it.
    #[arg(long)]
    check: bool,
    /// Install without asking for confirmation.
    #[arg(long, short)]
    yes: bool,
    /// Let the update save running recordings and close research QQ first.
    #[arg(long)]
    close_tasks: bool,
    /// HTTP proxy for this run only, such as http://127.0.0.1:7890.
    #[arg(long, conflicts_with = "no_proxy")]
    proxy: Option<String>,
    /// Connect directly for this run even when a proxy is saved.
    #[arg(long)]
    no_proxy: bool,
    #[command(subcommand)]
    command: Option<UpdateCommand>,
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Show or change the saved update settings.
    Config {
        /// Saved HTTP proxy for updates; an empty value removes it.
        #[arg(long)]
        proxy: Option<String>,
        /// Check for a new release when QScope opens, at most once a day.
        #[arg(long)]
        check_on_launch: Option<bool>,
    },
    /// Opens the new desktop once the replaced one has exited.
    #[command(hide = true)]
    Relaunch {
        #[arg(long)]
        directory: PathBuf,
    },
}

fn confirm(version: &str, tasks: Option<&str>) -> Result<bool, Error> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::new(
            "update_confirm",
            "非交互运行时请加 --yes 确认安装",
        ));
    }
    match tasks {
        Some(tasks) => eprint!("安装 QScope {version}？会先关闭{tasks}。[y/N] "),
        None => eprint!("安装 QScope {version}？[y/N] "),
    }
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

// Research QQ and recordings the update would close, as text for the confirmation.
fn running_tasks(data: &Path) -> Result<Option<String>, Error> {
    if service::connect(data).is_err() {
        return Ok(None);
    }
    let client = qscope_automation::client::Client::new(&format!(
        "@{}",
        display_path(&data.join("service.json"))
    ))?;
    let mut tasks = Vec::new();
    match client.get("/api/v1/settings/qq", &[]) {
        Ok(qq) if qq["stage"] == "running" => tasks.push(match qq["pid"].as_u64() {
            Some(pid) => format!("研究 QQ（PID {pid}）"),
            None => "研究 QQ".into(),
        }),
        Ok(_) => {}
        // More than one research QQ cannot be shown as the current one.
        Err(_) => tasks.push("研究 QQ".into()),
    }
    let recordings = client.get("/api/v1/recordings", &[])?;
    let recording = recordings.as_array().map_or(0, |list| {
        list.iter()
            .filter(|item| item["state"] == "recording")
            .count()
    });
    if recording > 0 {
        tasks.push(format!("{recording} 段录制"));
    }
    // Latin letters and digits keep a space from the Chinese conjunction.
    let mut text = String::new();
    for task in tasks {
        if !text.is_empty() {
            if text.ends_with(|c: char| c.is_ascii_alphanumeric()) {
                text.push(' ');
            }
            text.push('和');
            if task.starts_with(|c: char| c.is_ascii_alphanumeric()) {
                text.push(' ');
            }
        }
        text.push_str(&task);
    }
    Ok((!text.is_empty()).then_some(text))
}

// A portable or CLI copy is replaced in place, which needs the old backend gone first so the
// next start runs the new build. Tasks started after the confirmation still refuse the stop.
fn stop_backend(data: &Path) -> Result<bool, Error> {
    let Ok(service) = service::connect(data) else {
        return Ok(false);
    };
    let client = qscope_automation::client::Client::new(&format!(
        "@{}",
        display_path(&data.join("service.json"))
    ))?;
    client
        .post(
            "/api/v1/service/control",
            json!({"action":"stop","client_id":runtime::instance_id()?}),
        )
        .map_err(|error| {
            Error::new(
                "active_tasks",
                format!(
                    "后台拒绝停止（{}），请先结束研究 QQ 和录制再更新",
                    error.message
                ),
            )
        })?;
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while service::connect(data).is_ok_and(|current| current.pid == service.pid) {
        if Instant::now() >= deadline {
            return Err(Error::new("service_timeout", "后台没有在 20 秒内停止"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(true)
}

pub fn run(options: Options, json_output: bool) -> Result<(), Error> {
    if !update::supported() && !matches!(options.command, Some(UpdateCommand::Config { .. })) {
        return Err(Error::new(
            "unsupported_platform",
            "Online updates are not enabled for this platform. Install a matching platform package manually.",
        ));
    }
    let data = runtime::user_data()?;
    let mut settings = update::load(&data)?;
    match options.command {
        Some(UpdateCommand::Config {
            proxy,
            check_on_launch,
        }) => {
            if let Some(proxy) = proxy {
                settings.proxy = update::proxy(&proxy)?;
            }
            if let Some(check) = check_on_launch {
                settings.check_on_launch = check;
            }
            update::save(&data, &settings)?;
            return print_json(&json!({"schema_version":1,"settings":settings}));
        }
        Some(UpdateCommand::Relaunch { directory }) => {
            return update::relaunch(&directory, &data).map_err(Into::into);
        }
        None => {}
    }
    let (install, directory) = update::locate()?;
    if install == update::Install::Development {
        return Err(Error::new(
            "update_unsupported",
            "开发构建不支持在线更新，请用源码重新构建",
        ));
    }
    let proxy = if options.no_proxy {
        None
    } else if let Some(proxy) = &options.proxy {
        update::proxy(proxy)?
    } else {
        settings.proxy.clone()
    };
    let manifest = update::check(proxy.as_deref())?;
    settings.last_check_unix = Some(update::now());
    update::save(&data, &settings)?;
    let current = env!("CARGO_PKG_VERSION");
    let newer = update::newer(&manifest.version, current)?;
    let report = json!({"schema_version":1,"current":current,"latest":manifest.version,"newer":newer,"install":install.name(),"directory":directory,"notes_url":manifest.notes_url,"proxy":proxy});
    if options.check || !newer {
        return print_json(&report);
    }
    update::asset(&manifest, install)?;
    let tasks = running_tasks(&data)?;
    if let Some(tasks) = &tasks
        && options.yes
        && !options.close_tasks
    {
        return Err(Error::new(
            "active_tasks",
            format!("{tasks}仍在运行；加 --close-tasks 让更新先关闭它们"),
        ));
    }
    if !options.yes && !confirm(&manifest.version, tasks.as_deref())? {
        return print_json(&json!({"schema_version":1,"installed":false,"cancelled":true}));
    }
    let mut last = Instant::now();
    let total = update::asset(&manifest, install)?.bytes;
    let package = update::download(
        &manifest,
        install,
        &data,
        proxy.as_deref(),
        &mut |received| {
            if !json_output && last.elapsed().as_millis() >= 1000 {
                eprintln!("已下载 {} / {} MiB", received >> 20, total >> 20);
                last = Instant::now();
            }
        },
        &AtomicBool::new(false),
    )?;
    // Signature, archive structure, executable identity and bundle integrity all pass first.
    let files = if install != update::Install::Installer {
        Some(update::unpack(&package, install)?)
    } else {
        None
    };
    if let Some(files) = &files {
        update::preflight(install, &directory, files, &manifest.version)?;
    }
    // Only a verified package closes them, so a failed download leaves QQ running.
    if tasks.is_some() {
        let descriptor = service::connect(&data)?;
        service::Connection::acquire(&data, descriptor)?.close_tasks()?;
    }
    let result = match install {
        update::Install::Installer => {
            update::start_installer(&package)?;
            json!({"schema_version":1,"installed":false,"installer_started":true,"version":manifest.version,"package":package})
        }
        update::Install::Portable | update::Install::Cli | update::Install::App => {
            let files = files.expect("verified update files");
            if install.needs_restart() {
                update::mark_restart(&data, &manifest.version)?;
            }
            let stopped = stop_backend(&data)?;
            update::replace(&directory, &files)?;
            let global = update::refresh_global_command(&directory);
            if install == update::Install::App {
                update::start_relaunch(&directory, &data)?;
            }
            json!({"schema_version":1,"installed":true,"version":manifest.version,"directory":directory,"backend_stopped":stopped,"global_command":global,"restart":install.needs_restart()})
        }
        update::Install::Development => unreachable!("development builds have no update asset"),
    };
    print_json(&result)
}
