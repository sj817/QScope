use crate::{Error, web::BackendOptions};
use clap::{Args, Subcommand};
use qscope_automation::{client::Client, devtools, runtime};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Args)]
pub struct Options {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Download, verify and unpack the official QQ package without starting it.
    Prepare {
        /// Use an already downloaded official installer instead of downloading it.
        #[arg(long)]
        installer: Option<PathBuf>,
    },
    /// Start the isolated research QQ with capture and its control bridge; the user logs in.
    Start {
        /// Separate QQ data directory; each profile keeps its own login.
        #[arg(long, default_value = "automation")]
        profile: String,
        /// Control bridge port; 19860 for the automation profile, otherwise a free port.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Report preparation progress, the running PID and its capture session.
    Status,
    /// Close the research QQ of a profile. Its capture is finished with an end marker.
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
    let data = runtime::user_data()?;
    let connection_file = data
        .join("qq/9.9.37-261103-x64/portable/profiles")
        .join(profile)
        .join(".qscope/devtools.json");
    let stopped = qscope_automation::launcher::stop(
        &data.join("automation/launches"),
        &connection_file,
        profile,
    )?;
    crate::print_json(&stopped)
}
