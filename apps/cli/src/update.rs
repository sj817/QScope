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
    /// 仅报告最新版本。
    #[arg(long)]
    check: bool,
    /// 直接安装更新。
    #[arg(long, short)]
    yes: bool,
    /// 更新前保存录制并关闭研究 QQ。
    #[arg(long)]
    close_tasks: bool,
    /// 仅本次运行使用的 HTTP 代理，例如 http://127.0.0.1:7890。
    #[arg(long, conflicts_with = "no_proxy")]
    proxy: Option<String>,
    /// 本次运行使用直连，覆盖已保存的代理设置。
    #[arg(long)]
    no_proxy: bool,
    #[command(subcommand)]
    command: Option<UpdateCommand>,
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// 显示或修改已保存的更新设置。
    Config {
        /// 保存的 HTTP 更新代理，传入空值可移除。
        #[arg(long)]
        proxy: Option<String>,
        /// 启动 QScope 时检查更新，每天最多一次。
        #[arg(long)]
        check_on_launch: Option<bool>,
    },
    /// 等待被替换的桌面端退出，再打开新版本。
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

// 将更新会关闭的研究 QQ 和录制整理为确认文本。
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
        // 多个研究 QQ 实例同时运行时，当前实例信息保持未指定。
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
    // 拉丁字母和数字与中文连接词之间保留空格。
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

// 便携版和 CLI 原地替换前先退出旧后台，确保下次启动运行新构建。确认后新启动的任务仍会阻止关闭。
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
    // 先验证签名、归档结构、可执行文件身份和资源完整性。
    let files = if install != update::Install::Installer {
        Some(update::unpack(&package, install)?)
    } else {
        None
    };
    if let Some(files) = &files {
        update::preflight(install, &directory, files, &manifest.version)?;
    }
    // 分发包校验通过后才关闭任务，下载失败时保留 QQ 运行。
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
