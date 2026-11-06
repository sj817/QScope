use crate::Error;
use clap::{Args, Subcommand};
use qscope_automation::{display_path, runtime, service, skills};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

#[derive(Args, Default)]
pub struct BackendOptions {
    /// Additional read-only capture files or directories.
    #[arg(long)]
    pub captures: Vec<PathBuf>,
    /// Default 19840 with automatic fallback. An explicit port is used strictly.
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long, hide = true)]
    pub resources: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub workspace: Option<PathBuf>,
}

impl BackendOptions {
    pub fn specification(&self) -> Result<service::Options, Error> {
        Ok(service::Options {
            captures: self
                .captures
                .iter()
                .map(std::fs::canonicalize)
                .collect::<Result<_, _>>()?,
            port: self.port,
            resources: self
                .resources
                .as_ref()
                .map(std::fs::canonicalize)
                .transpose()?,
            workspace: self
                .workspace
                .as_ref()
                .map(std::fs::canonicalize)
                .transpose()?,
        })
    }
    pub fn ensure(&self) -> Result<runtime::Service, Error> {
        Ok(service::ensure(
            &std::env::current_exe()?,
            &runtime::user_data()?,
            &self.specification()?,
        )?)
    }
}

#[derive(Args)]
pub struct Options {
    #[command(flatten)]
    pub backend: BackendOptions,
    /// Start without opening the default browser.
    #[arg(long)]
    pub no_open: bool,
}

#[derive(Args)]
pub struct Service {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Keep the backend running until it is explicitly stopped.
    Start(BackendOptions),
    /// Report whether the backend runs, its endpoint and its active clients.
    Status,
    /// Stop an idle backend. Active QQ and recordings must be stopped first.
    Stop,
}

pub fn manage(args: Service) -> Result<(), Error> {
    let data = runtime::user_data()?;
    let (descriptor, action) = match args.command {
        ServiceCommand::Start(options) => (options.ensure()?, "pin"),
        ServiceCommand::Status | ServiceCommand::Stop => match service::connect(&data) {
            Ok(value) => (
                value,
                if matches!(args.command, ServiceCommand::Stop) {
                    "stop"
                } else {
                    "status"
                },
            ),
            Err(error) if !data.join("service.json").exists() => {
                return crate::print_json(
                    &serde_json::json!({"schema_version":1,"running":false,"message":error.message}),
                );
            }
            Err(error) => return Err(error.into()),
        },
    };
    let connection = service::Connection::acquire(&data, descriptor)?;
    let status = connection.action(action)?;
    if action == "stop" {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while runtime::Service::read(&data.join("service.json"))
            .is_ok_and(|current| current.instance_id == connection.service.instance_id)
        {
            if std::time::Instant::now() >= deadline {
                return Err(Error::new(
                    "service_timeout",
                    "Backend is still finishing its shutdown",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    crate::print_json(
        &serde_json::json!({"schema_version":1,"running":action!="stop","pid":connection.service.pid,"endpoint":connection.service.endpoint,"status":status}),
    )
}

pub fn run(options: Options, json: bool) -> Result<(), Error> {
    let descriptor = options.backend.ensure()?;
    let connection = Arc::new(service::Connection::acquire(
        &runtime::user_data()?,
        descriptor,
    )?);
    if json {
        crate::print_json(
            &serde_json::json!({"type":"web_ready","schema_version":1,"endpoint":connection.service.endpoint,"pid":connection.service.pid,"descriptor":runtime::user_data()?.join("service.json")}),
        )?;
    } else {
        println!("QScope WebUI  {}", connection.service.endpoint);
        eprintln!(
            "Ctrl+C disconnects this terminal. Active research tasks continue in the backend."
        );
    }
    std::io::stdout().flush()?;
    if !options.no_open
        && let Err(error) = open::that_detached(connection.service.browser_url())
    {
        eprintln!("Browser launch failed: {error}");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => { result?; return Ok::<(),Error>(()); },
                _ = heartbeat.tick() => {
                    let connection = connection.clone();
                    tokio::task::spawn_blocking(move || connection.heartbeat()).await.map_err(|error| Error::new("service", error.to_string()))??;
                },
            }
        }
    })
}

pub fn backend(path: PathBuf) -> Result<(), Error> {
    runtime::no_links(&path)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::new(
            "service_start",
            "Startup request exceeds 64 KiB",
        ));
    }
    let startup: service::Startup = serde_json::from_slice(&bytes)?;
    if startup.schema_version != 1 {
        return Err(Error::new("service_version", "Unsupported startup request"));
    }
    runtime::set_data_directory(startup.data.clone())?;
    let lease = runtime::data_lease(&startup.data)?;
    let data = startup.data.canonicalize()?;
    let resource_override = startup.options.resources.clone();
    let (resources, assets) = match startup.options.resources {
        Some(path) => (path.canonicalize()?, None),
        None => {
            let assets = crate::resources::embedded()?;
            (assets.root(&data), Some(assets))
        }
    };
    let layout = runtime::Layout::packaged(resources, data.clone());
    let profiles = layout.package().join("portable/profiles");
    runtime::regular_directory(&profiles)?;
    let mut captures = startup.options.captures;
    captures.push(profiles);
    let roots = data.join("capture-roots.json");
    if roots.is_file() {
        let mut bytes = Vec::new();
        std::fs::File::open(&roots)?
            .take(16385)
            .read_to_end(&mut bytes)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if bytes.len() > 16384 || value["schema_version"] != 1 {
            return Err(Error::new(
                "capture_roots",
                "Unsupported capture root settings",
            ));
        }
        let previous: Vec<PathBuf> = serde_json::from_value(value["roots"].clone())?;
        if previous.len() > 64 {
            return Err(Error::new("capture_roots", "Capture root limit exceeded"));
        }
        captures.extend(
            previous
                .into_iter()
                .filter(|path| path.is_absolute() && path.exists()),
        );
    }
    let descriptor_path = data.join("service.json");
    let instance_id = runtime::instance_id()?;
    let token = format!(
        "{}{}",
        runtime::instance_id()?,
        &runtime::instance_id()?[..16]
    );
    let automation = skills::Environment {
        workspace: startup
            .options
            .workspace
            .unwrap_or(std::env::current_dir()?)
            .canonicalize()?,
        home: skills::user_home()?,
        cli: std::env::current_exe()?,
        server_url: format!("@{}", display_path(&descriptor_path)),
    };
    let workspace = automation.workspace.clone();
    let endpoint_settings = data.join("automation/backend.json");
    let previous_port = endpoint_settings
        .exists()
        .then(|| runtime::read_settings(&endpoint_settings))
        .transpose()?
        .and_then(|settings| {
            settings["endpoint"]
                .as_str()
                .and_then(|endpoint| qscope_automation::client::loopback_url(endpoint).ok())
        })
        .and_then(|url| url.port());
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    async_runtime.block_on(async move {
        let service = qscope_web::start(qscope_web::ServeOptions {
            captures, imports: data.join("captures/imported"), ui: layout.resources.join("ui"), assets,
            token: Some(token.clone()), port: startup.options.port.or(previous_port).unwrap_or(19840), fallback_port: startup.options.port.is_none(),
            automation, runtime: layout, instance_id: instance_id.clone(),
        }).await.map_err(|error| Error::new("web_start", error.to_string()))?;
        runtime::write_settings(&endpoint_settings, &serde_json::json!({"schema_version":1,"endpoint":service.endpoint}))?;
        let descriptor = runtime::Service { schema_version:2, owner:"qscope-service".into(), instance_id, endpoint:service.endpoint.clone(), pid:std::process::id(), token, version:env!("CARGO_PKG_VERSION").into(), build_id: runtime::BUILD_ID.into(), resources: resource_override, workspace:Some(workspace) };
        descriptor.publish(&descriptor_path)?;
        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => { result?; break; },
                _ = tokio::time::sleep(Duration::from_secs(1)) => if service.should_exit().await { break; },
            }
        }
        service.shutdown().await?;
        descriptor.remove_owned(&descriptor_path)?;
        drop(lease);
        Ok(())
    })
}
