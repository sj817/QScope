use crate::{Error, client::loopback_url, devtools, display_path, runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use ts_rs::TS;

#[derive(Debug, Deserialize, Serialize, TS)]
#[ts(rename = "DevtoolsLaunch")]
pub struct Specification {
    pub launch_version: u32,
    pub profile: String,
    pub endpoint: String,
    pub host_path: String,
    pub command: String,
    pub connection_file: String,
    pub log_directory: String,
    pub resources_directory: String,
    pub data_directory: String,
    pub package_directory: String,
    pub prebuilt_directory: String,
}

pub fn validate_profile(profile: &str, port: u16) -> Result<(), Error> {
    if profile.is_empty()
        || profile.len() > 32
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        || port < 1024
    {
        return Err(Error::new(
            "query",
            "调试 profile 只能使用 1–32 位字母、数字、下划线或短横线，端口为 1024–65535",
        ));
    }
    Ok(())
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::new("launch_cancelled", "QQ startup was cancelled"));
    }
    Ok(())
}

// Every temporary host process is reaped on errors, cancellation and timeouts.
struct HostProcess(Child);

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host_command(host: &Path) -> Command {
    let mut process = runtime::worker_command(host);
    process.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x0800_0000);
    }
    process
}

fn run_host(
    mut command: Command,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ExitStatus, Error> {
    check_cancelled(cancelled)?;
    let mut process = HostProcess(command.spawn()?);
    let deadline = Instant::now() + timeout;
    loop {
        check_cancelled(cancelled)?;
        if let Some(status) = process.0.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "launch_timeout",
                "Native host exceeded its deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read_json(path: &Path) -> Result<Value, Error> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::new("launch", "Native host JSON exceeds 64 KiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

// Only a ready bridge transfers ownership to the independent native host.
struct PendingLaunch<'a> {
    host: &'a Path,
    directory: PathBuf,
    owner: Option<(u32, String)>,
    ready: bool,
}

impl Drop for PendingLaunch<'_> {
    fn drop(&mut self) {
        if self.ready {
            return;
        }
        let _ = fs::write(self.directory.join("stop"), b"stop");
        let Some((pid, birth)) = &self.owner else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.directory.join("exit.json").is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if !self.directory.join("exit.json").is_file() {
            let mut process = host_command(self.host);
            process
                .arg("stop")
                .arg(pid.to_string())
                .arg(birth)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = run_host(process, Duration::from_secs(3), &AtomicBool::new(false));
        }
    }
}

/// Start an isolated QQ copy using the same native interface from every entry point.
pub fn launch(
    specification: &Specification,
    wait: u64,
    cancelled: &AtomicBool,
) -> Result<Value, Error> {
    if specification.launch_version != 3 {
        return Err(Error::new(
            "launch_version",
            "This launcher requires native interface v3. Update the desktop app and CLI together",
        ));
    }
    let endpoint = loopback_url(&specification.endpoint)?;
    let port = endpoint.port_or_known_default().expect("HTTP port");
    validate_profile(&specification.profile, port)?;
    if !(1..=120).contains(&wait) {
        return Err(Error::new(
            "query",
            "Startup wait must be between 1 and 120 seconds",
        ));
    }
    check_cancelled(cancelled)?;
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err(Error::new(
            "platform",
            "The QQ collector currently requires Windows x64",
        ));
    }
    let host = Path::new(&specification.host_path).canonicalize()?;
    if !matches!(
        host.file_name().and_then(|name| name.to_str()),
        Some("qscope-cli.exe" | "qs.exe" | "qscope-cli" | "qs")
    ) {
        return Err(Error::new("launch", "Invalid native QQ host"));
    }
    fs::create_dir_all(&specification.log_directory)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(Path::new(&specification.log_directory).join(".launcher.lock"))?;
    lease
        .try_lock()
        .map_err(|_| Error::new("launch_busy", "Another QQ startup is in progress"))?;
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    drop(probe);
    let run = tempfile::Builder::new()
        .prefix("launch-")
        .tempdir_in(&specification.log_directory)?
        .keep();
    let receipt = run.join("manifest.json");
    let mut values = serde_json::to_value(specification)?;
    values["schema_version"] = json!(1);
    values["kind"] = json!("qscope-native-host");
    values["port"] = json!(port);
    values["receipt_path"] = json!(display_path(&receipt));
    values["progress_path"] = json!("");
    values["duration_seconds"] = json!(0);
    let mut request: runtime::HostRequest = serde_json::from_value(values)?;
    let preflight = run.join("preflight");
    fs::create_dir(&preflight)?;
    request.receipt_path = display_path(&preflight.join("manifest.json"));
    let spec_path = run.join("request.json");
    fs::write(&spec_path, serde_json::to_vec_pretty(&request)?)?;
    let mut process = host_command(&host);
    process
        .arg("self-test")
        .arg(&spec_path)
        .stdout(fs::File::create(preflight.join("host.log"))?)
        .stderr(fs::File::create(preflight.join("host.err.log"))?);
    if !run_host(process, Duration::from_secs(90), cancelled)?.success() {
        return Err(Error::new(
            "native_selftest",
            format!(
                "Native isolation check failed. Inspect {}",
                preflight.display()
            ),
        ));
    }
    request.receipt_path = display_path(&receipt);
    fs::write(&spec_path, serde_json::to_vec_pretty(&request)?)?;
    let mut pending = PendingLaunch {
        host: &host,
        directory: run.clone(),
        owner: None,
        ready: false,
    };
    let mut process = host_command(&host);
    process
        .arg("detach")
        .arg(&spec_path)
        .stdout(fs::File::create(run.join("detach.log"))?)
        .stderr(fs::File::create(run.join("detach.err.log"))?);
    if !run_host(process, Duration::from_secs(10), cancelled)?.success() {
        return Err(Error::new(
            "launch",
            format!("Native host failed. Inspect {}", run.display()),
        ));
    }
    let owner = read_json(&run.join("detach.log"))?;
    let owner_pid = owner["launcher_pid"]
        .as_u64()
        .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64)
        .ok_or_else(|| Error::new("launch", "Native owner PID is unavailable"))?;
    let owner_birth = owner["launcher_birth_filetime"]
        .as_str()
        .filter(|birth| birth.parse::<u64>().is_ok())
        .ok_or_else(|| Error::new("launch", "Native owner birth time is unavailable"))?;
    pending.owner = Some((owner_pid as u32, owner_birth.into()));
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        check_cancelled(cancelled)?;
        if run.join("exit.json").exists() {
            return Err(Error::new(
                "launch",
                format!("Native QQ owner exited. Inspect {}", run.display()),
            ));
        }
        if receipt.exists() {
            let mut launched = read_json(&receipt)?;
            if launched["launcher_pid"] != owner_pid
                || launched["launcher_birth_filetime"] != owner_birth
            {
                return Err(Error::new(
                    "process_identity",
                    "Native QQ owner identity changed",
                ));
            }
            if let Ok(connection) =
                devtools::Connection::connect_file(Path::new(&specification.connection_file))
                && connection.info.endpoint == specification.endpoint
                && launched["pid"] == connection.info.pid
                && launched["process_birth_filetime"] == connection.info.birth_filetime
            {
                check_cancelled(cancelled)?;
                launched["state"] = json!("ready");
                launched["info"] = serde_json::to_value(connection.info)?;
                launched["manifest"] = json!(display_path(&receipt));
                fs::write(&receipt, serde_json::to_vec_pretty(&launched)?)?;
                pending.ready = true;
                return Ok(launched);
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "launch_timeout",
                format!("Owned QQ startup timed out. Inspect {}", run.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Close a running research copy through its owner. The owner ends the capture with its footer,
/// stops QQ and writes `exit.json`; only the owner whose receipt names the live process is asked.
pub fn stop(log_directory: &Path, connection_file: &Path, profile: &str) -> Result<Value, Error> {
    validate_profile(profile, 1024)?;
    let connection = devtools::Connection::connect_file(connection_file)?;
    runtime::no_links(log_directory)?;
    let mut selected = None;
    for entry in fs::read_dir(log_directory)? {
        let directory = entry?.path();
        let receipt = directory.join("manifest.json");
        runtime::no_links(&receipt)?;
        if !receipt.is_file() {
            continue;
        }
        let Ok(value) = read_json(&receipt) else {
            continue;
        };
        if value["kind"] == "qscope-research-launcher"
            && value["profile"] == profile
            && value["pid"] == connection.info.pid
            && value["process_birth_filetime"] == connection.info.birth_filetime
        {
            if selected.is_some() {
                return Err(Error::new(
                    "qq_identity",
                    "Multiple owner receipts match this profile",
                ));
            }
            selected = Some(directory);
        }
    }
    let owner = selected.ok_or_else(|| {
        Error::new(
            "qq_identity",
            "No QScope owner receipt matches the running research QQ",
        )
    })?;
    fs::write(owner.join("stop"), b"stop")?;
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
    Ok(json!({
        "schema_version": 1,
        "profile": profile,
        "stopped": true,
        "pid": connection.info.pid,
        "birth_filetime": connection.info.birth_filetime,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_profiles_and_ports_are_bounded() {
        for (profile, port) in [("", 19860), ("../other", 19860), ("test", 80), ("test", 0)] {
            assert_eq!(validate_profile(profile, port).unwrap_err().code, "query");
        }
        assert!(validate_profile(&"a".repeat(33), 19860).is_err());
        assert!(validate_profile("test_01-a", 65535).is_ok());
    }

    #[test]
    fn stop_asks_only_the_owner_of_the_live_research_copy() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let info = json!({"version":1,"pid":4242,"birth_filetime":"134000000000000001","build":"synthetic","endpoint":endpoint}).to_string();
        let bridge = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{info}",
                info.len()
            )
            .unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let connection = root.path().join("devtools.json");
        fs::write(&connection, json!({"schema_version":1,"pid":4242,"birth_filetime":"134000000000000001","endpoint":endpoint,"ready":true,"token":"a".repeat(64)}).to_string()).unwrap();
        let launches = root.path().join("launches");
        let receipt = |name: &str, pid: u32| {
            let directory = launches.join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("manifest.json"), json!({"kind":"qscope-research-launcher","profile":"automation","pid":pid,"process_birth_filetime":"134000000000000001"}).to_string()).unwrap();
            directory
        };
        let (owner, earlier) = (receipt("launch-live", 4242), receipt("launch-old", 4141));
        fs::write(launches.join(".launcher.lock"), b"").unwrap();
        let exit = owner.clone();
        let simulated = std::thread::spawn(move || {
            while !exit.join("stop").is_file() {
                std::thread::sleep(Duration::from_millis(20));
            }
            fs::write(exit.join("exit.json"), b"{}").unwrap();
        });
        let stopped = stop(&launches, &connection, "automation").unwrap();
        assert_eq!(stopped["pid"], 4242);
        assert_eq!(stopped["birth_filetime"], "134000000000000001");
        assert!(!earlier.join("stop").exists());
        simulated.join().unwrap();
        bridge.join().unwrap();
    }

    #[test]
    fn pending_launch_stops_only_unfinished_runs() {
        let directory = tempfile::tempdir().unwrap();
        let mut pending = PendingLaunch {
            host: Path::new("unused"),
            directory: directory.path().into(),
            owner: None,
            ready: true,
        };
        drop(pending);
        assert!(!directory.path().join("stop").exists());
        pending = PendingLaunch {
            host: Path::new("unused"),
            directory: directory.path().into(),
            owner: None,
            ready: false,
        };
        drop(pending);
        assert_eq!(fs::read(directory.path().join("stop")).unwrap(), b"stop");
    }
}
