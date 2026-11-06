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

fn confirm(version: &str) -> Result<bool, Error> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::new(
            "update_confirm",
            "非交互运行时请加 --yes 确认安装",
        ));
    }
    eprint!("安装 QScope {version}？[y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

// A portable or CLI copy is replaced in place, which needs the old backend gone first so the
// next start runs the new build. Active QQ or recordings refuse the stop.
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
    let proxy = if options.no_proxy {
        None
    } else if let Some(proxy) = &options.proxy {
        update::proxy(proxy)?
    } else {
        settings.proxy.clone()
    };
    let (install, directory) = update::locate()?;
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
    if !options.yes && !confirm(&manifest.version)? {
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
    let result = match install {
        update::Install::Installer => {
            update::start_installer(&package)?;
            json!({"schema_version":1,"installed":false,"installer_started":true,"version":manifest.version,"package":package})
        }
        update::Install::Portable | update::Install::Cli => {
            let files = update::unpack(&package, install)?;
            let stopped = stop_backend(&data)?;
            if stopped {
                // An open desktop closes once its backend is gone for an update.
                update::mark_restart(&data, &manifest.version)?;
            }
            update::replace(&directory, &files)?;
            let global = update::refresh_global_command(&directory);
            json!({"schema_version":1,"installed":true,"version":manifest.version,"directory":directory,"backend_stopped":stopped,"global_command":global,"restart":install == update::Install::Portable})
        }
        update::Install::Development => unreachable!("development builds have no update asset"),
    };
    print_json(&result)
}
