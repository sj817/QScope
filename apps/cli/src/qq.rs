use crate::{Error, web::BackendOptions};
use clap::{Args, Subcommand};
use qscope_automation::{client::Client, devtools, runtime};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Args)]
pub struct Options {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Prepare {
        #[arg(long)]
        installer: Option<PathBuf>,
    },
    Start {
        #[arg(long, default_value = "automation")]
        profile: String,
        #[arg(long)]
        port: Option<u16>,
    },
    Status,
    Stop {
        #[arg(long, default_value = "automation")]
        profile: String,
    },
}

pub fn run(options: Options) -> Result<(), Error> {
    if let Command::Stop { profile } = &options.command {
        return stop(profile);
    }
    let descriptor = BackendOptions::default().ensure()?;
    let client = Client::new(&format!(
        "@{}",
        runtime::user_data()?.join("service.json").display()
    ))?;
    let (profile, port, installer, prepare_only) = match options.command {
        Command::Prepare { installer } => (
            "automation".to_owned(),
            19860,
            installer.map(fs::canonicalize).transpose()?,
            true,
        ),
        Command::Start { profile, port } => {
            let port = match port {
                Some(port) => port,
                None if profile == "automation" => 19860,
                None => std::net::TcpListener::bind("127.0.0.1:0")?
                    .local_addr()?
                    .port(),
            };
            (profile, port, None, false)
        }
        Command::Status => return crate::print_json(&client.get("/api/v1/settings/qq", &[])?),
        Command::Stop { .. } => unreachable!("handled above"),
    };
    qscope_automation::launcher::validate_profile(&profile, port)?;
    let expected = runtime::user_data()?
        .join("qq/9.9.37-261103-x64/portable/profiles")
        .join(&profile)
        .join(".qscope/devtools.json");
    client.post(
        "/api/v1/settings/qq",
        json!({"profile":profile,"port":port,"prepare_only":prepare_only,"installer":installer}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(1000);
    loop {
        let status = client.get("/api/v1/settings/qq", &[])?;
        if status["stage"] == "error" {
            return Err(Error::new(
                "qq_prepare",
                status["error"].as_str().unwrap_or("QQ preparation failed"),
            ));
        }
        if prepare_only
            && status["installed"] == true
            && matches!(status["stage"].as_str(), Some("idle" | "ready" | "running"))
        {
            return crate::print_json(&status);
        }
        if !prepare_only && let Ok(connection) = devtools::Connection::connect_file(&expected) {
            return crate::print_json(
                &json!({"schema_version":1,"profile":profile,"pid":connection.info.pid,"birth_filetime":connection.info.birth_filetime,"stage":"running","service":descriptor.endpoint}),
            );
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "qq_timeout",
                "QQ preparation or startup exceeded its deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn stop(profile: &str) -> Result<(), Error> {
    qscope_automation::launcher::validate_profile(profile, 19860)?;
    let data = runtime::user_data()?;
    let connection_file = data
        .join("qq/9.9.37-261103-x64/portable/profiles")
        .join(profile)
        .join(".qscope/devtools.json");
    let connection = devtools::Connection::connect_file(&connection_file)?;
    let launches = data.join("automation/launches");
    runtime::no_links(&launches)?;
    let mut selected = None;
    for directory in fs::read_dir(&launches)? {
        let path = directory?.path().join("launcher.json");
        runtime::no_links(&path)?;
        if !path.is_file() || fs::metadata(&path)?.len() > 65536 {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&fs::read(&path)?) else {
            continue;
        };
        if value["pid"] == connection.info.pid
            && value["process_birth_filetime"] == connection.info.birth_filetime
            && value["profile"] == profile
        {
            if selected.is_some() {
                return Err(Error::new(
                    "qq_identity",
                    "Multiple owner receipts match this profile",
                ));
            }
            selected = path.parent().map(Path::to_path_buf);
        }
    }
    let owner = selected
        .ok_or_else(|| Error::new("qq_identity", "No matching QScope owner receipt was found"))?;
    let stop = owner.join("stop");
    runtime::no_links(&stop)?;
    fs::write(stop, b"stop")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !owner.join("exit.json").is_file() {
        if Instant::now() >= deadline {
            return Err(Error::new(
                "qq_timeout",
                "The research owner has not finished stopping",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    crate::print_json(
        &json!({"schema_version":1,"profile":profile,"stopped":true,"pid":connection.info.pid}),
    )
}
