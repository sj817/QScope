use crate::{Error, client::Client, display_path, runtime};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Options {
    pub captures: Vec<PathBuf>,
    pub resources: Option<PathBuf>,
    pub port: Option<u16>,
    pub workspace: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
pub struct Startup {
    pub schema_version: u32,
    pub data: PathBuf,
    pub options: Options,
}

pub fn connect(data: &Path) -> Result<runtime::Service, Error> {
    let path = data.join("service.json");
    let descriptor = runtime::Service::read(&path)?;
    if descriptor.schema_version != 2
        || descriptor.version != env!("CARGO_PKG_VERSION")
        || descriptor.build_id != runtime::BUILD_ID
    {
        return Err(Error::new(
            "service_version",
            "The running backend requires a compatible product version",
        ));
    }
    Client::new(&format!("@{}", display_path(&path)))?;
    Ok(descriptor)
}

pub fn ensure(
    executable: &Path,
    data: &Path,
    options: &Options,
) -> Result<runtime::Service, Error> {
    runtime::regular_directory(data)?;
    let start_lock = data.join(".service-start.lock");
    runtime::no_links(&start_lock)?;
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(start_lock)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match lease.try_lock() {
            Ok(()) => break,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(Error::new("service_busy", error.to_string())),
        }
    }
    if let Ok(service) = connect(data) {
        if options
            .resources
            .as_ref()
            .is_some_and(|path| service.resources.as_ref() != Some(path))
            || options
                .workspace
                .as_ref()
                .is_some_and(|path| service.workspace.as_ref() != Some(path))
        {
            return Err(Error::new(
                "service_options",
                "The running backend uses another resource or workspace directory. Use a separate --data-dir",
            ));
        }
        if options.port.is_some_and(|port| {
            port != 0
                && crate::client::Url::parse(&service.endpoint)
                    .ok()
                    .and_then(|url| url.port())
                    != Some(port)
        }) {
            return Err(Error::new(
                "service_options",
                "An existing backend uses another port. Select its port or use a separate --data-dir",
            ));
        }
        if !options.captures.is_empty() {
            Client::new(&format!("@{}", display_path(&data.join("service.json"))))?.post(
                "/api/v1/service/roots",
                serde_json::json!({"roots":options.captures}),
            )?;
        }
        return Ok(service);
    }
    if data.join("service.json").is_file()
        && Client::new(&format!("@{}", display_path(&data.join("service.json")))).is_ok()
    {
        return Err(Error::new(
            "service_version",
            "Another QScope build is running in this data directory. Finish its tasks and stop it before upgrading",
        ));
    }
    let logs = data.join("logs");
    runtime::regular_directory(&logs)?;
    let mut input = tempfile::Builder::new()
        .prefix(".service-start-")
        .suffix(".json")
        .tempfile_in(data)?;
    input.write_all(&serde_json::to_vec(&Startup {
        schema_version: 1,
        data: data.into(),
        options: options.clone(),
    })?)?;
    input.as_file_mut().sync_all()?;
    let mut process = Command::new(executable);
    process
        .arg("__service")
        .arg(input.path())
        .stdin(Stdio::null())
        .stdout(
            File::options()
                .create(true)
                .append(true)
                .open(logs.join("service.log"))?,
        )
        .stderr(
            File::options()
                .create(true)
                .append(true)
                .open(logs.join("service.err.log"))?,
        );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x0800_0000);
    }
    let mut child = process.spawn()?;
    loop {
        if let Ok(service) = connect(data)
            && service.pid == child.id()
        {
            if !options.captures.is_empty() {
                Client::new(&format!("@{}", display_path(&data.join("service.json"))))?.post(
                    "/api/v1/service/roots",
                    serde_json::json!({"roots":options.captures}),
                )?;
            }
            return Ok(service);
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::new(
                "service_start",
                format!(
                    "Backend exited with {status}. See {}",
                    display_path(&logs.join("service.err.log"))
                ),
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(
                "service_timeout",
                "The backend did not become ready within 30 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub struct Connection {
    client: Client,
    id: String,
    pub service: runtime::Service,
}

impl Connection {
    pub fn acquire(data: &Path, service: runtime::Service) -> Result<Self, Error> {
        let client = Client::new(&format!("@{}", display_path(&data.join("service.json"))))?;
        let connection = Self {
            client,
            service,
            id: runtime::instance_id()?,
        };
        connection.action("acquire")?;
        Ok(connection)
    }
    pub fn action(&self, action: &str) -> Result<serde_json::Value, Error> {
        self.client.post(
            "/api/v1/service/control",
            serde_json::json!({"action":action,"client_id":self.id}),
        )
    }
    pub fn heartbeat(&self) -> Result<(), Error> {
        self.action("heartbeat").map(|_| ())
    }
    /// 更新前保存录制并关闭全部研究 QQ。
    pub fn close_tasks(&self) -> Result<serde_json::Value, Error> {
        self.client.post_with_timeout(
            "/api/v1/service/control",
            serde_json::json!({"action":"close_tasks","client_id":self.id}),
            Duration::from_secs(90),
        )
    }
    /// 关闭 QScope 页面启动的研究 QQ，CLI、AI 及其他生产者录制分别管理。
    pub fn close_page_qq(&self) -> Result<serde_json::Value, Error> {
        self.client.post_with_timeout(
            "/api/v1/settings/qq/stop",
            serde_json::json!({"page_only":true}),
            Duration::from_secs(20),
        )
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.action("release");
    }
}
