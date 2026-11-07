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
    /// 只读扫描 Windows PE x64 或 macOS ARM64 模块的 MSF 候选。
    Scan {
        /// 显式指定 PE x64 或 Mach-O ARM64 模块。
        #[arg(long, conflicts_with = "app")]
        module: Option<PathBuf>,
        /// 只读源应用包，默认为 /Applications/QQ.app。
        #[arg(long)]
        app: Option<PathBuf>,
    },
    /// 只读检查 Mac QQ 签名、权限声明和 ARM64 抓包候选。
    Inspect {
        #[arg(long, default_value = "/Applications/QQ.app")]
        app: PathBuf,
    },
    /// 下载、校验并解包官方 QQ 安装包。
    Prepare {
        /// 使用已经下载的官方安装包。
        #[arg(long)]
        installer: Option<PathBuf>,
    },
    /// 启动隔离研究 QQ、抓包和控制桥，由用户手动登录。
    Start {
        /// 独立 QQ 数据目录，各配置分别保存登录状态。
        #[arg(long, default_value = "automation")]
        profile: String,
        /// 控制桥端口，automation 配置默认使用 19860，其他配置使用空闲端口。
        #[arg(long)]
        port: Option<u16>,
    },
    /// 报告准备进度、运行 PID 和抓包会话。
    Status,
    /// 关闭指定配置的研究 QQ，并写入抓包结束标记。
    Stop {
        #[arg(long, default_value = "automation")]
        profile: String,
    },
}

pub fn run(options: Options) -> Result<(), Error> {
    if let Command::Inspect { app } = &options.command {
        return crate::print_json(&serde_json::to_value(qscope_qqnt_macos::inspect::inspect(
            app,
        )?)?);
    }
    if let Command::Scan { module, app } = &options.command {
        let path = module.clone().unwrap_or_else(|| {
            app.clone()
                .unwrap_or_else(|| PathBuf::from("/Applications/QQ.app"))
                .join("Contents/Resources/app/wrapper.node")
        });
        let mut file = std::fs::File::open(&path)?;
        let mut magic = [0; 2];
        std::io::Read::read_exact(&mut file, &mut magic)?;
        let report = if magic == *b"MZ" {
            serde_json::to_value(qscope_qqnt::pe::scan(&path)?)?
        } else {
            serde_json::to_value(qscope_qqnt_macos::scan(&path)?)?
        };
        return crate::print_json(&report);
    }
    if !runtime::capture_supported() && !matches!(options.command, Command::Status) {
        return Err(Error::new(
            "unsupported_platform",
            "Live QQ capture requires Windows x64 or macOS Apple Silicon. Offline analysis remains available.",
        ));
    }
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
        Command::Scan { .. } => unreachable!("handled above"),
        Command::Inspect { .. } => unreachable!("handled above"),
    };
    qscope_automation::launcher::validate_profile(&profile, port)?;
    let expected = runtime::qq_package(&runtime::user_data()?)
        .join("portable/profiles")
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
    let connection_file = runtime::qq_package(&data)
        .join("portable/profiles")
        .join(profile)
        .join(".qscope/devtools.json");
    let stopped = qscope_automation::launcher::stop(
        &data.join("automation/launches"),
        &connection_file,
        profile,
    )?;
    crate::print_json(&stopped)
}
