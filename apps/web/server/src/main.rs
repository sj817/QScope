use clap::{Parser, Subcommand};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "QScope local capture workbench")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 提供已构建 WebUI，并读取显式配置的本机抓包根目录。
    Serve {
        #[arg(long, required = true)]
        captures: Vec<PathBuf>,
        #[arg(long, default_value = "artifacts/captures/imported")]
        imports: PathBuf,
        #[arg(long, default_value = "apps/web/dist")]
        ui_dir: PathBuf,
        #[arg(long, default_value_t = 19840)]
        port: u16,
        /// 集成设置页使用的项目目录。
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// QQ 安装包与设置的数据目录，默认使用 <workspace>/artifacts。
        #[arg(long)]
        storage: Option<PathBuf>,
    },
    /// 从 Rust 响应类型生成前端契约。
    Types {
        #[arg(long, default_value = "apps/web/src/api.generated.ts")]
        out: PathBuf,
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match Cli::parse().command {
        Command::Types { out, check } => {
            let expected = qscope_web::typescript();
            if check {
                if fs::read_to_string(&out)?.replace("\r\n", "\n") != expected {
                    return Err("前端类型已过期，请运行 qscope-web types".into());
                }
                println!("Frontend contract matches Rust response types.");
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(out, expected)?;
            }
        }
        Command::Serve {
            captures,
            imports,
            ui_dir,
            port,
            workspace,
            storage,
        } => {
            let workspace = workspace
                .unwrap_or(std::env::current_dir()?)
                .canonicalize()?;
            let mut runtime = qscope_automation::runtime::Layout::development(&workspace);
            if let Some(storage) = storage {
                fs::create_dir_all(&storage)?;
                runtime.storage = storage.canonicalize()?;
            }
            let service = qscope_web::start(qscope_web::ServeOptions {
                captures,
                imports,
                ui: ui_dir,
                assets: None,
                token: None,
                port,
                fallback_port: false,
                runtime,
                instance_id: qscope_automation::runtime::instance_id()?,
                automation: qscope_automation::skills::Environment {
                    workspace,
                    cli: std::env::current_exe()?.with_file_name(if cfg!(windows) {
                        "qscope-cli.exe"
                    } else {
                        "qscope-cli"
                    }),
                    home: qscope_automation::skills::user_home()?,
                    server_url: String::new(),
                },
            })
            .await?;
            println!("QScope WebUI: {}", service.endpoint);
            println!("跟随配置的采集目录，AI 接入与研究副本控制位于设置页。Ctrl+C 关闭本地服务。");
            tokio::signal::ctrl_c().await?;
            service.shutdown().await?;
        }
    }
    Ok(())
}
