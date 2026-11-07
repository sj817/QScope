use crate::{package, windows};
use qscope_automation::{Error, display_path, runtime};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
pub struct Specification {
    schema_version: u32,
    kind: String,
    profile: String,
    port: u16,
    data_directory: PathBuf,
    package_directory: PathBuf,
    resources_directory: PathBuf,
    prebuilt_directory: PathBuf,
    receipt_path: PathBuf,
    #[serde(default)]
    duration_seconds: u32,
    #[serde(default = "capture_default")]
    capture: bool,
    #[serde(default)]
    startup_diagnostics: bool,
    #[serde(default)]
    wait_for_debugger: bool,
    #[serde(default)]
    installer_path: String,
    #[serde(default)]
    http_proxy: String,
    #[serde(default)]
    http_proxy_certificate: String,
}
fn capture_default() -> bool {
    true
}
fn failure(message: impl Into<String>) -> Error {
    Error::new("native_host", message)
}
fn read(path: &Path) -> Result<Value, Error> {
    runtime::no_links(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(131073).read_to_end(&mut bytes)?;
    if bytes.len() > 131072 {
        return Err(failure("Worker JSON exceeds its byte limit"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn write(path: &Path, value: &Value) -> Result<(), Error> {
    runtime::no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure("Worker output needs a parent"))?;
    runtime::regular_directory(parent)?;
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    output.write_all(&serde_json::to_vec_pretty(value)?)?;
    output.as_file_mut().sync_all()?;
    output
        .persist(path)
        .map_err(|error| Error::from(error.error))?;
    Ok(())
}
fn inside(root: &Path, path: &Path) -> Result<(), Error> {
    let root = std::path::absolute(root)?;
    let path = std::path::absolute(path)?;
    if path == root
        || !path.starts_with(root)
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(failure("Worker output escaped its data directory"));
    }
    runtime::no_links(&path)?;
    Ok(())
}
fn lock(path: &Path) -> Result<File, Error> {
    runtime::no_links(path)?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(path)?;
    file.try_lock()
        .map_err(|_| failure("Research profile is already running"))?;
    Ok(file)
}

pub fn dispatch(arguments: Vec<std::ffi::OsString>) -> Result<Value, Error> {
    let args: Vec<_> = arguments
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    match args.as_slice() {
        [command] if command == "identify" => identify(),
        [command, pid, birth] if command == "stop" => {
            let pid = pid.parse().map_err(|_| failure("Invalid worker PID"))?;
            match windows::identity(pid) {
                Ok((actual, image)) => {
                    if actual != *birth
                        || Path::new(&image).canonicalize()?
                            != std::env::current_exe()?.canonicalize()?
                    {
                        return Err(failure("Worker identity changed"));
                    }
                    windows::stop(pid, birth)?;
                }
                Err(error) if error.code == "process_missing" => {}
                Err(error) => return Err(error),
            }
            Ok(json!({"stopped":true}))
        }
        [command, pid, birth, image, title] if command == "close-window" => {
            let pid = pid.parse().map_err(|_| failure("Invalid window PID"))?;
            let (actual, executable) = windows::identity(pid)?;
            if actual != *birth
                || Path::new(&executable).canonicalize()? != Path::new(image).canonicalize()?
                || !matches!(title.as_str(), "QScope" | "QQ DevTools · QScope")
            {
                return Err(failure("Window identity changed"));
            }
            Ok(json!({"closed":windows::close_window(pid,title)?}))
        }
        [command, input]
            if matches!(command.as_str(), "prepare" | "run" | "self-test" | "detach") =>
        {
            let value = read(Path::new(input))?;
            let spec: Specification = serde_json::from_value(value)?;
            validate(&spec)?;
            if command == "prepare" {
                return crate::prepare::run(
                    &spec.package_directory,
                    &spec.receipt_path.with_file_name("progress.json"),
                    (!spec.installer_path.is_empty()).then(|| Path::new(&spec.installer_path)),
                    &std::sync::atomic::AtomicBool::new(false),
                );
            }
            if command == "detach" {
                let mut process = Command::new(std::env::current_exe()?);
                use std::os::windows::process::CommandExt;
                process
                    .args(["__collector", "run", input])
                    .creation_flags(0x08000000)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let child = process.spawn()?;
                let (birth, _) = windows::identity(child.id())?;
                Ok(
                    json!({"schema_version":1,"kind":"qscope-research-launcher","launcher_pid":child.id(),"launcher_birth_filetime":birth,"profile":spec.profile,"endpoint":format!("http://127.0.0.1:{}",spec.port),"connection_file":spec.package_directory.join("portable/profiles").join(spec.profile).join(".qscope/devtools.json")}),
                )
            } else {
                run(&spec, command == "self-test")
            }
        }
        _ => Err(failure("Unsupported collector command")),
    }
}

fn validate(spec: &Specification) -> Result<(), Error> {
    if !spec.http_proxy.is_empty() || !spec.http_proxy_certificate.is_empty() {
        return Err(Error::new(
            "unsupported_proxy",
            "The native host does not support proxy configuration in this release",
        ));
    }
    if spec.schema_version != 1
        || spec.kind != "qscope-native-host"
        || spec.profile.is_empty()
        || spec.profile.len() > 32
        || !spec
            .profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || (spec.port != 0 && spec.port < 1024)
        || spec.duration_seconds > 86400
    {
        return Err(failure("Unsupported collector launch specification"));
    }
    if !spec.data_directory.is_absolute()
        || !spec.package_directory.is_absolute()
        || !spec.receipt_path.is_absolute()
    {
        return Err(failure("Collector paths must be absolute"));
    }
    runtime::no_links(&spec.data_directory)?;
    inside(&spec.data_directory, &spec.package_directory)?;
    inside(&spec.data_directory, &spec.receipt_path)?;
    Ok(())
}

fn identify() -> Result<Value, Error> {
    let mut bytes = Vec::new();
    std::io::stdin().take(131073).read_to_end(&mut bytes)?;
    if bytes.len() > 131072 {
        return Err(failure("Identity request exceeds its limit"));
    }
    let requests: Vec<Value> = serde_json::from_slice(&bytes)?;
    if requests.len() > 4096 {
        return Err(failure("Identity request limit exceeded"));
    }
    Ok(Value::Array(requests.into_iter().map(|request| {
        let identity = request["pid"].as_u64().and_then(|pid|u32::try_from(pid).ok()).map(windows::identity);
        match identity {
            Some(Ok((birth,image))) => json!({"id":request["id"],"birth":birth,"name":Path::new(&image).file_stem().map(|value|value.to_string_lossy()),"missing":false}),
            Some(Err(error)) if error.code == "process_missing" => json!({"id":request["id"],"birth":null,"name":null,"missing":true}),
            _ => json!({"id":request["id"],"birth":null,"name":null,"missing":false}),
        }
    }).collect()))
}

fn copy_directory(
    input: &Path,
    output: &Path,
    count: &mut usize,
    depth: usize,
) -> Result<(), Error> {
    if depth > 64 {
        return Err(failure("QQ runtime depth limit exceeded"));
    }
    runtime::no_links(input)?;
    fs::create_dir_all(output)?;
    for entry in fs::read_dir(input)? {
        let entry = entry?;
        *count += 1;
        if *count > 100000 {
            return Err(failure("QQ runtime file limit exceeded"));
        }
        let source = entry.path();
        runtime::no_links(&source)?;
        if matches!(
            source.extension().and_then(|value| value.to_str()),
            Some("i64" | "idb" | "id0" | "id1" | "id2" | "nam" | "til")
        ) || entry.file_name() == "qscope-portable.dll"
        {
            continue;
        }
        let destination = output.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source, &destination, count, depth + 1)?;
        } else {
            fs::copy(source, destination)?;
        }
    }
    Ok(())
}

fn guard(path: &Path) -> Result<Value, Error> {
    let value = read(path)?;
    if let Some(fields) = value.as_object() {
        for (key, value) in fields {
            if value.as_bool() == Some(false) {
                return Err(failure(format!(
                    "Isolation guard failed: {key}. See {}",
                    display_path(path)
                )));
            }
        }
    } else {
        return Err(failure("Invalid isolation guard result"));
    }
    Ok(value)
}

fn run(spec: &Specification, self_test: bool) -> Result<Value, Error> {
    let identity = package::identity()?;
    let native = read(&spec.prebuilt_directory.join("manifest.json"))?;
    let helper = spec.prebuilt_directory.join("qscope-portable.dll");
    let expected = native["outputs"]
        .as_array()
        .and_then(|files| {
            files
                .iter()
                .find(|file| file["path"] == "qscope-portable.dll")
        })
        .and_then(|file| file["sha256"].as_str())
        .ok_or_else(|| failure("Probe manifest has no DLL identity"))?;
    if native["schema_version"] != 1 || package::file_sha256(&helper)? != expected {
        return Err(failure("Native probe identity changed"));
    }
    let portable = spec.package_directory.join("portable");
    runtime::regular_directory(&portable)?;
    let run_id = runtime::instance_id()?;
    let run = portable.join("runs").join(&run_id);
    runtime::regular_directory(&run)?;
    let state = portable
        .join(if self_test { "tests" } else { "profiles" })
        .join(if self_test { &run_id } else { &spec.profile });
    runtime::regular_directory(&state)?;
    let _profile = lock(&state.join(".session.lock"))?;
    for suffix in [
        "user/AppData/Local",
        "user/AppData/LocalLow",
        "user/AppData/Roaming",
        "user/Documents/Tencent Files",
        "user/Desktop",
        "user/Downloads",
        "user/Pictures",
        "user/Documents",
        "public/Documents/Tencent/QQ",
        "programData",
        "temp",
        "electron",
        ".qscope",
    ] {
        runtime::regular_directory(&state.join(suffix))?;
    }
    let settings = format!(
        "[UserDataSet]\r\nUserDataSavePathType=2\r\nUserDataSavePath={}\\user\\Documents\\Tencent Files\r\n",
        display_path(&state)
    );
    let settings: Vec<u8> = [0xfeffu16]
        .into_iter()
        .chain(settings.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(
        state.join("public/Documents/Tencent/QQ/UserDataInfo.ini"),
        settings,
    )?;
    let _runtime = if self_test {
        None
    } else {
        Some(lock(
            &portable.join(format!(".runtime-{}-session.lock", spec.profile)),
        )?)
    };
    let executable = if self_test {
        std::env::current_exe()?
    } else {
        let runtime_root = portable.join("runtimes");
        runtime::regular_directory(&runtime_root)?;
        let destination = runtime_root.join(&spec.profile);
        if !destination.exists() {
            let source = spec.package_directory.join("unpacked/Files");
            package::verify_client(&source, &identity)?;
            let staging = tempfile::Builder::new()
                .prefix("qq-runtime-")
                .tempdir_in(&runtime_root)?;
            let mut files = 0;
            copy_directory(&source, staging.path(), &mut files, 0)?;
            package::verify_client(staging.path(), &identity)?;
            fs::rename(staging.path(), &destination)?;
        }
        package::verify_client(&destination, &identity)?;
        destination.join("QQ.exe")
    };
    let owner_directory = spec
        .receipt_path
        .parent()
        .ok_or_else(|| failure("Receipt needs a parent"))?;
    runtime::regular_directory(owner_directory)?;
    let capture = if !self_test && spec.capture {
        let path = state.join(".qscope/captures").join(&run_id);
        runtime::regular_directory(&path)?;
        Some(path)
    } else {
        None
    };
    let public_setting = std::env::var_os("PUBLIC")
        .map(PathBuf::from)
        .map(|path| path.join("Documents/Tencent/QQ/UserDataInfo.ini"));
    let public_before = public_setting
        .as_ref()
        .filter(|path| path.is_file())
        .map(|path| package::file_sha256(path))
        .transpose()?;
    let mut report = json!({"schema_version":1,"question":"Verify owned Windows isolation and passive capture with the Rust collector host","time_utc_unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error|failure(error.to_string()))?.as_secs().to_string(),"mode":if self_test {"SelfTest"}else{"Run"},"client_build":identity.build,"installer_sha256":identity.installer_sha256,"helper_build":native["build_id"],"state_directory":state,"module_sha256":identity.modules.iter().find(|module|module.path.ends_with("/wrapper.node")).map(|module|&module.sha256),"capture_directory":capture,"packet_capture_requested":capture.is_some(),"isolation":"WRITE_RESTRICTED plus private HKCU and folder hooks","automatic_login_performed":false,"process_ime_disabled":true});
    report["profile_sha256"] = json!(package::file_sha256(
        &spec
            .resources_directory
            .join("collectors/qqnt-windows/profile.json")
    )?);
    let mut session = None;
    let result: Result<(), Error> = (|| {
        let targets = if capture.is_some() {
            let wrapper = identity
                .modules
                .iter()
                .find(|module| module.path.ends_with("/wrapper.node"))
                .ok_or_else(|| failure("客户端清单缺少 wrapper.node"))?;
            crate::pe::verified_targets(
                &executable
                    .parent()
                    .ok_or_else(|| failure("QQ 路径无父目录"))?
                    .join(&wrapper.path),
            )?
        } else {
            [0, 0]
        };
        report["dynamic_targets"] = json!({"scanner_version":"pe-msf-v1", "send_rva":format!("0x{:x}", targets[0]), "recv_rva":format!("0x{:x}", targets[1])});
        let registry = format!("Software\\QScope\\PortableSessions\\{run_id}");
        let child = windows::Session::start(windows::ResearchOptions {
            executable: &executable,
            state: &state,
            helper: &helper,
            build: &identity.build,
            registry: &registry,
            run: &run,
            capture: capture.as_deref(),
            port: if self_test { 0 } else { spec.port },
            self_test,
            diagnostics: spec.startup_diagnostics,
            paused: !self_test && spec.wait_for_debugger,
            targets,
        })?;
        let pid = child.pid();
        let (birth, _) = windows::identity(pid)?;
        report["pid"] = json!(pid);
        report["process_birth_filetime"] = json!(birth);
        report["is_restricted"] = json!(true);
        report["is_appcontainer"] = json!(false);
        let (owner_birth, _) = windows::identity(std::process::id())?;
        write(
            &spec.receipt_path,
            &json!({"schema_version":1,"kind":"qscope-research-launcher","state":"starting","launcher_pid":std::process::id(),"launcher_birth_filetime":owner_birth,"profile":spec.profile,"endpoint":format!("http://127.0.0.1:{}",spec.port),"connection_file":state.join(".qscope/devtools.json"),"pid":pid,"process_birth_filetime":birth}),
        )?;
        session = Some(child);
        write(&run.join("manifest.json"), &report)?;
        if spec.wait_for_debugger && !self_test {
            let resume = run.join("resume");
            report["resume_file"] = json!(resume);
            write(&run.join("manifest.json"), &report)?;
            let deadline = Instant::now() + Duration::from_secs(45);
            while !resume.is_file() {
                if Instant::now() >= deadline {
                    return Err(failure("Debugger readiness timed out"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            session.as_ref().expect("Owned session").resume()?;
        }
        let begin = Instant::now();
        let limit = if self_test { 75 } else { spec.duration_seconds };
        while session.as_ref().expect("Owned session").running() {
            if owner_directory.join("stop").is_file() {
                report["result"] = json!("stop-file");
                break;
            }
            if limit > 0 && begin.elapsed() >= Duration::from_secs(u64::from(limit)) {
                if self_test {
                    return Err(failure("Isolation guard timed out"));
                }
                report["result"] = json!("duration");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let code = session.as_ref().expect("Owned session").exit_code();
        report["exit_code"] = json!(code);
        if self_test {
            if code != 0 {
                return Err(failure(format!("Isolation guard exited with 0x{code:x}")));
            }
            report["guard_parent"] = guard(&state.join("guard-result.json"))?;
            report["guard_child"] = guard(&state.join("guard-child-result.json"))?;
            report["result"] = json!("self-test-passed");
        } else if !session.as_ref().expect("Owned session").running() && code != 0 {
            return Err(failure(format!("QQ exited with 0x{code:x}")));
        } else if report["result"].is_null() {
            report["result"] = json!("process-exited");
        }
        Ok(())
    })();
    if let Some(path) = &capture {
        fs::write(path.join("stop"), b"stop")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while session.as_ref().is_some_and(|session| session.running())
            && !path.join(format!("closed.{}", report["pid"])).is_file()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let integrity = session.take().map(windows::Session::finish).transpose();
    let public_after = public_setting
        .as_ref()
        .filter(|path| path.is_file())
        .map(|path| package::file_sha256(path))
        .transpose()?;
    report["public_setting_unchanged"] = json!(public_before == public_after);
    if let Err(error) = result.as_ref().and_then(|_| integrity.as_ref().map(|_| ())) {
        report["failure"] = json!(error.message);
    }
    write(&run.join("manifest.json"), &report)?;
    let final_result = result.and(integrity.map(|_| ())).and_then(|_| {
        if public_before == public_after {
            Ok(())
        } else {
            Err(failure("The public QQ setting changed"))
        }
    });
    write(
        &owner_directory.join("exit.json"),
        &json!({"schema_version":1,"exit_code":if final_result.is_ok(){0}else{1},"message":final_result.as_ref().err().map(|error|&error.message)}),
    )?;
    final_result?;
    Ok(report)
}
