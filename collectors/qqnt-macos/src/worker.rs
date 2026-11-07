//! Owned research-copy lifecycle, hosted by the same qscope-cli executable.
use crate::{common, package, process};
use qscope_automation::{Error, display_path, runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    progress_path: PathBuf,
    #[serde(default)]
    duration_seconds: u32,
    #[serde(default)]
    installer_path: String,
    #[serde(default)]
    http_proxy: String,
    #[serde(default)]
    http_proxy_certificate: String,
    #[serde(default)]
    wait_for_debugger: bool,
}

fn validate(spec: &Specification) -> Result<(), Error> {
    if !cfg!(target_arch = "aarch64") {
        return Err(Error::new(
            "unsupported_platform",
            "The Mac capture worker requires Apple Silicon",
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
        return Err(common::failure("Unsupported Mac host specification"));
    }
    if !spec.http_proxy.is_empty()
        || !spec.http_proxy_certificate.is_empty()
        || spec.wait_for_debugger
    {
        return Err(Error::new(
            "unsupported_mode",
            "Mac capture does not support proxy configuration or a debugger startup pause",
        ));
    }
    common::inside(&spec.data_directory, &spec.package_directory)?;
    common::inside(&spec.data_directory, &spec.receipt_path)?;
    for path in [&spec.resources_directory, &spec.prebuilt_directory] {
        if !path.is_absolute() {
            return Err(common::failure("Collector resource paths must be absolute"));
        }
        runtime::no_links(path)?;
    }
    if !spec.progress_path.as_os_str().is_empty() {
        common::inside(&spec.data_directory, &spec.progress_path)?;
    }
    Ok(())
}

pub fn dispatch(arguments: Vec<OsString>) -> Result<Value, Error> {
    let values: Vec<_> = arguments
        .iter()
        .map(|value| {
            value
                .to_str()
                .ok_or_else(|| common::failure("Collector arguments must be UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    match values.as_slice() {
        ["identify"] => identify(),
        ["stop", pid, birth] => {
            let pid = pid
                .parse::<u32>()
                .map_err(|_| common::failure("Invalid worker PID"))?;
            match process::identity(pid) {
                Ok((actual, image)) => {
                    if actual != *birth
                        || image.canonicalize()? != std::env::current_exe()?.canonicalize()?
                    {
                        return Err(common::failure("Worker identity changed"));
                    }
                    process::signal(pid, birth, &image, libc::SIGTERM, false)?;
                }
                Err(error) if error.code == "process_missing" => {}
                Err(error) => return Err(error),
            }
            Ok(json!({"stopped":true}))
        }
        ["exec", input] => execute(Path::new(input)),
        ["isolation-guard", input] => guard(Path::new(input), false),
        ["isolation-guard", input, "child"] => guard(Path::new(input), true),
        [command, input]
            if matches!(
                *command,
                "prepare" | "repair" | "run" | "self-test" | "detach"
            ) =>
        {
            let spec: Specification = serde_json::from_value(common::read_json(Path::new(input))?)?;
            validate(&spec)?;
            if matches!(*command, "prepare" | "repair") {
                let status = if spec.progress_path.as_os_str().is_empty() {
                    spec.receipt_path.with_file_name("progress.json")
                } else {
                    spec.progress_path.clone()
                };
                let provided =
                    (!spec.installer_path.is_empty()).then(|| Path::new(&spec.installer_path));
                return if *command == "repair" {
                    crate::prepare::repair(
                        &spec.package_directory,
                        &status,
                        provided,
                        &AtomicBool::new(false),
                    )
                } else {
                    crate::prepare::run(
                        &spec.package_directory,
                        &status,
                        provided,
                        &AtomicBool::new(false),
                    )
                };
            }
            if *command == "detach" {
                let mut command = Command::new(std::env::current_exe()?);
                let owner = spec
                    .receipt_path
                    .parent()
                    .ok_or_else(|| common::failure("Receipt needs a parent"))?;
                common::directory(owner)?;
                command
                    .args(["__collector", "run", input])
                    .stdin(Stdio::null())
                    .stdout(private_log(&owner.join("host.log"))?)
                    .stderr(private_log(&owner.join("host.err.log"))?);
                let child = process::spawn_detached(&mut command)?;
                let (birth, _) = process::identity(child.id())?;
                return Ok(
                    json!({"schema_version":1,"kind":"qscope-research-launcher","launcher_pid":child.id(),"launcher_birth_filetime":birth,"profile":spec.profile,"endpoint":format!("http://127.0.0.1:{}",spec.port),"connection_file":profile(&spec).join(".qscope/devtools.json")}),
                );
            }
            let result = run(&spec, *command == "self-test");
            let exit = spec.receipt_path.with_file_name("exit.json");
            let _ = common::write_json(
                &exit,
                &json!({"schema_version":1,"exit_code":if result.is_ok(){0}else{1},"message":result.as_ref().err().map(|error|&error.message)}),
            );
            result
        }
        _ => Err(common::failure("Unsupported Mac collector command")),
    }
}

fn identify() -> Result<Value, Error> {
    let mut bytes = Vec::new();
    std::io::stdin().take(131073).read_to_end(&mut bytes)?;
    if bytes.len() > 131072 {
        return Err(common::failure("Identity request exceeds 128 KiB"));
    }
    let requests: Vec<Value> = serde_json::from_slice(&bytes)?;
    if requests.len() > 512 {
        return Err(common::failure("Identity request exceeds 512 processes"));
    }
    let mut result = Vec::new();
    for request in requests {
        let id = request["id"]
            .as_str()
            .filter(|id| id.len() <= 512)
            .ok_or_else(|| common::failure("Invalid identity request ID"))?;
        let pid = request["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| common::failure("Invalid identity request PID"))?;
        result.push(match process::identity(pid) {
            Ok((birth,image)) => json!({"id":id,"birth":birth,"name":image.file_stem().map(|value|value.to_string_lossy()),"missing":false}),
            Err(error) if error.code=="process_missing" => json!({"id":id,"birth":null,"name":null,"missing":true}),
            Err(_) => json!({"id":id,"birth":null,"name":null,"missing":false}),
        });
    }
    Ok(Value::Array(result))
}

fn profile(spec: &Specification) -> PathBuf {
    spec.package_directory
        .join("portable/profiles")
        .join(&spec.profile)
}
fn research_app(spec: &Specification) -> PathBuf {
    spec.package_directory
        .join("portable/runtimes")
        .join(&spec.profile)
        .join("QScope Research QQ.app")
}
fn bundle_namespace(profile: &str) -> String {
    let hash = format!("{:x}", Sha256::digest(profile.as_bytes()));
    format!("dev.qscope.research.qq.p{}", &hash[..16])
}
fn private_log(path: &Path) -> Result<File, Error> {
    runtime::no_links(path)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(common::failure("Log must be a regular file"));
    }
    Ok(file)
}

struct Assets {
    probe: PathBuf,
    probe_sha256: String,
    build_id: String,
}
fn assets(spec: &Specification) -> Result<Assets, Error> {
    let deployed = spec.resources_directory.join("collectors/qqnt-macos");
    let (probe, manifest) = if deployed.join("libqscope_probe.dylib").is_file() {
        (
            deployed.join("libqscope_probe.dylib"),
            deployed.join("native-manifest.json"),
        )
    } else {
        (
            spec.prebuilt_directory.join("qscope-probe.dylib"),
            spec.prebuilt_directory.join("manifest.json"),
        )
    };
    let manifest = common::read_json(&manifest)?;
    let expected = manifest["outputs"]
        .as_array()
        .and_then(|files| {
            files.iter().find(|file| {
                matches!(
                    file["path"].as_str(),
                    Some("qscope-probe.dylib" | "libqscope_probe.dylib")
                )
            })
        })
        .and_then(|file| file["sha256"].as_str())
        .ok_or_else(|| common::failure("Native manifest has no probe identity"))?;
    let build = manifest["build_id"]
        .as_str()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| common::failure("Native build identity is invalid"))?;
    if manifest["schema_version"] != 1
        || fs::metadata(&probe)?.len() > 16 * 1024 * 1024
        || package::file_sha256(&probe)? != expected
    {
        return Err(common::failure("Native Mac probe identity changed"));
    }
    Ok(Assets {
        probe: probe.canonicalize()?,
        probe_sha256: expected.into(),
        build_id: build.into(),
    })
}

fn initialize_profile(state: &Path) -> Result<(), Error> {
    for suffix in [
        "home",
        "home/Library/Application Support",
        "home/Library/Caches",
        "home/Library/Preferences",
        "home/Documents",
        "home/Desktop",
        "home/Pictures",
        "home/Movies",
        "home/Music",
        "tmp",
        "userData",
        "sessionData",
        "logs",
        "crashDumps",
        "files",
        ".qscope",
    ] {
        common::directory(&state.join(suffix))?;
    }
    Ok(())
}

fn bundled_source(spec: &Specification, name: &str, embedded: &[u8]) -> Result<Vec<u8>, Error> {
    let file = spec
        .resources_directory
        .join("collectors/qqnt-macos")
        .join(name);
    if file.exists() {
        let mut bytes = Vec::new();
        common::regular(&file)?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != embedded {
            return Err(common::failure(
                "Mac bootstrap resources differ from this CLI build",
            ));
        }
        Ok(bytes)
    } else {
        Ok(embedded.to_vec())
    }
}

fn app_bundles(
    root: &Path,
    depth: usize,
    count: &mut usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    if depth > 64 {
        return Err(common::failure("App directory depth exceeds 64"));
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        *count += 1;
        if *count > 100000 {
            return Err(common::failure("App file count exceeds 100000"));
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            if !path
                .canonicalize()?
                .starts_with(output.first().expect("root app"))
            {
                return Err(common::failure("App symlink escapes the research bundle"));
            }
        } else if kind.is_dir() {
            // Framework identifiers and signatures remain unchanged. Electron's
            // helper apps live beside the framework in Contents/Frameworks.
            if path.extension().is_some_and(|value| value == "framework") {
                continue;
            }
            if path.extension().is_some_and(|value| value == "app") {
                output.push(path.clone());
            }
            app_bundles(&path, depth + 1, count, output)?;
        } else if !kind.is_file() {
            return Err(common::failure(
                "App contains a non-regular filesystem entry",
            ));
        }
    }
    Ok(())
}

fn verify_research(app: &Path, marker: &Path, expected: &Value) -> Result<(), Error> {
    if common::read_json(marker)? != *expected
        || package::file_sha256(&app.join(package::MODULE))? != package::MODULE_SHA256
    {
        return Err(common::failure("Prepared research app identity changed"));
    }
    let files = app.join("Contents/Resources/app");
    for (name, expected) in [
        (
            "qscope-bootstrap.cjs",
            include_bytes!("../bootstrap.cjs").as_slice(),
        ),
        (
            "qscope-automation.cjs",
            include_bytes!("../../qqnt-windows/automation.js").as_slice(),
        ),
    ] {
        let mut bytes = Vec::new();
        common::regular(&files.join(name))?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != expected {
            return Err(common::failure("Prepared research bootstrap changed"));
        }
    }
    common::command(
        "/usr/bin/codesign",
        &common::args(&[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app.as_os_str(),
        ]),
        Duration::from_secs(60),
        &AtomicBool::new(false),
    )?;
    Ok(())
}

fn prepare_research(spec: &Specification, native: &Assets) -> Result<PathBuf, Error> {
    let destination = research_app(spec);
    let root = destination.parent().expect("runtime parent");
    common::directory(root)?;
    let marker = root.join("prepared.json");
    let expected = json!({"schema_version":1,"build":package::BUILD,"module_sha256":package::MODULE_SHA256,"probe_sha256":native.probe_sha256,"native_build":native.build_id,"cli_build":runtime::BUILD_ID,"profile":spec.profile});
    if destination.exists() && verify_research(&destination, &marker, &expected).is_ok() {
        return Ok(destination);
    }
    let _package = common::lock(&spec.package_directory.join(".prepare.lock"))?;
    let source = spec.package_directory.join("unpacked/QQ.app");
    package::verify_client(&source, &package::identity()?)?;
    let stage = tempfile::Builder::new()
        .prefix("qq-research-")
        .tempdir_in(root)?;
    let app = stage.path().join("QScope Research QQ.app");
    common::command(
        "/usr/bin/ditto",
        &common::args(&[source.as_os_str(), app.as_os_str()]),
        Duration::from_secs(180),
        &AtomicBool::new(false),
    )?;
    let files = app.join("Contents/Resources/app");
    let original = common::read_json(&files.join("package.json"))?;
    let mut package = original.clone();
    package["main"] = json!("./qscope-bootstrap.cjs");
    common::write_json(&files.join("qscope-original-main.json"), &original)?;
    common::write_json(&files.join("package.json"), &package)?;
    fs::write(
        files.join("qscope-bootstrap.cjs"),
        bundled_source(spec, "bootstrap.cjs", include_bytes!("../bootstrap.cjs"))?,
    )?;
    fs::write(
        files.join("qscope-automation.cjs"),
        bundled_source(
            spec,
            "automation.cjs",
            include_bytes!("../../qqnt-windows/automation.js"),
        )?,
    )?;
    let mut apps = vec![app.canonicalize()?];
    app_bundles(&app, 0, &mut 0, &mut apps)?;
    for bundle in &apps {
        let info = bundle.join("Contents/Info.plist");
        let value = common::command(
            "/usr/bin/plutil",
            &common::args(&[
                "-convert".as_ref(),
                "json".as_ref(),
                "-o".as_ref(),
                "-".as_ref(),
                info.as_os_str(),
            ]),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )?;
        let mut value: Value = serde_json::from_slice(&value.stdout)?;
        if let Some(identifier) = value["CFBundleIdentifier"]
            .as_str()
            .and_then(|identifier| identifier.strip_prefix("com.tencent.qq"))
        {
            value["CFBundleIdentifier"] =
                json!(format!("{}{identifier}", bundle_namespace(&spec.profile)));
        }
        if bundle == &apps[0] {
            value["CFBundleName"] = json!("QQ");
            value["CFBundleDisplayName"] = json!("QScope Research QQ");
            if let Some(value) = value.as_object_mut() {
                value.remove("CFBundleURLTypes");
                value.remove("NSServices");
            }
        }
        common::write_json(&info, &value)?;
        common::command(
            "/usr/bin/plutil",
            &common::args(&["-convert".as_ref(), "binary1".as_ref(), info.as_os_str()]),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )?;
    }
    let entitlements = stage.path().join("entitlements.plist");
    fs::write(&entitlements,b"<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>com.apple.security.cs.allow-jit</key><true/><key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/><key>com.apple.security.cs.disable-library-validation</key><true/><key>com.apple.security.cs.allow-dyld-environment-variables</key><true/></dict></plist>")?;
    apps.sort_by_key(|app| std::cmp::Reverse(app.components().count()));
    for bundle in apps {
        common::command(
            "/usr/bin/codesign",
            &common::args(&[
                "--force".as_ref(),
                "--sign".as_ref(),
                "-".as_ref(),
                "--options".as_ref(),
                "runtime".as_ref(),
                "--entitlements".as_ref(),
                entitlements.as_os_str(),
                bundle.as_os_str(),
            ]),
            Duration::from_secs(60),
            &AtomicBool::new(false),
        )?;
    }
    common::command(
        "/usr/bin/codesign",
        &common::args(&[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app.as_os_str(),
        ]),
        Duration::from_secs(60),
        &AtomicBool::new(false),
    )?;
    if package::file_sha256(&app.join(package::MODULE))? != package::MODULE_SHA256 {
        return Err(common::failure(
            "Research signing changed the capture module",
        ));
    }
    let previous = if destination.exists() {
        let rejected = root.join(format!("rejected-app-{}", runtime::instance_id()?));
        fs::rename(&destination, &rejected)?;
        Some(rejected)
    } else {
        None
    };
    if let Err(error) = fs::rename(&app, &destination) {
        if let Some(previous) = previous {
            let _ = fs::rename(previous, &destination);
        }
        return Err(error.into());
    }
    common::write_json(&marker, &expected)?;
    verify_research(&destination, &marker, &expected)?;
    Ok(destination)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Exec {
    schema_version: u32,
    specification: Specification,
    run: PathBuf,
    state: PathBuf,
    app: PathBuf,
    probe: PathBuf,
    probe_sha256: String,
    capture: PathBuf,
    stop: PathBuf,
    closed: PathBuf,
    send: String,
    receive: String,
    guard: bool,
}

fn validate_exec(value: &Exec) -> Result<(), Error> {
    validate(&value.specification)?;
    common::inside(&value.specification.package_directory, &value.run)?;
    common::inside(&value.specification.package_directory, &value.state)?;
    for path in [&value.capture, &value.stop, &value.closed] {
        common::inside(&value.state, path)?;
    }
    if value.schema_version != 1
        || value.run.parent()
            != Some(
                value
                    .specification
                    .package_directory
                    .join("portable/runs")
                    .as_path(),
            )
        || value.state
            != if value.guard {
                value
                    .specification
                    .package_directory
                    .join("portable/tests")
                    .join(
                        value
                            .run
                            .file_name()
                            .ok_or_else(|| common::failure("Run has no ID"))?,
                    )
            } else {
                profile(&value.specification)
            }
        || (!value.guard && value.app != research_app(&value.specification))
    {
        return Err(common::failure(
            "Internal Mac launch paths do not match their owned profile",
        ));
    }
    if !process::sandbox_denies_write(&value.run.join("sandbox-denied"))? {
        return Err(common::failure(
            "Internal launch is not protected by its filesystem sandbox",
        ));
    }
    Ok(())
}

fn environment(value: &Exec) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("HOME".into(), value.state.join("home").into_os_string()),
        (
            "CFFIXED_USER_HOME".into(),
            value.state.join("home").into_os_string(),
        ),
        ("TMPDIR".into(), value.state.join("tmp").into_os_string()),
        (
            "LANG".into(),
            std::env::var_os("LANG").unwrap_or_else(|| "zh_CN.UTF-8".into()),
        ),
        (
            "QSCOPE_RESEARCH_PROFILE".into(),
            value.state.as_os_str().into(),
        ),
        ("QSCOPE_STATE_ROOT".into(), value.state.as_os_str().into()),
        (
            "QSCOPE_DEVTOOLS_PORT".into(),
            value.specification.port.to_string().into(),
        ),
        ("QSCOPE_CLIENT_BUILD".into(), package::BUILD.into()),
    ]);
    for name in ["USER", "LOGNAME"] {
        if let Some(value) = std::env::var_os(name) {
            env.insert(name.into(), value);
        }
    }
    env
}

fn execute(path: &Path) -> Result<Value, Error> {
    let value: Exec = serde_json::from_value(common::read_json(path)?)?;
    validate_exec(&value)?;
    if value.guard {
        return Err(common::failure("Guard requests cannot execute a QQ app"));
    }
    let native = assets(&value.specification)?;
    if native.probe != value.probe || native.probe_sha256 != value.probe_sha256 {
        return Err(common::failure("Internal probe identity changed"));
    }
    let hooks = package::capture_profile(&value.app)?;
    if hooks.send != value.send || hooks.receive != value.receive {
        return Err(common::failure("Internal capture discovery changed"));
    }
    let executable = value.app.join("Contents/MacOS/QQ").canonicalize()?;
    let mut command = Command::new(&executable);
    command
        .args([
            OsString::from(format!(
                "--user-data-dir={}",
                display_path(&value.state.join("userData"))
            )),
            "--disable-breakpad".into(),
            "--disable-crash-reporter".into(),
        ])
        .current_dir(executable.parent().expect("QQ executable parent"))
        .env_clear()
        .envs(environment(&value))
        .env("DYLD_INSERT_LIBRARIES", &value.probe)
        .env("QSCOPE_PROBE_EXECUTABLE", &executable)
        .env(
            "QSCOPE_PROBE_MODULE",
            value.app.join(package::MODULE).canonicalize()?,
        )
        .env("QSCOPE_PROBE_SEND", &value.send)
        .env("QSCOPE_PROBE_RECEIVE", &value.receive)
        .env("QSCOPE_PROBE_OUTPUT", &value.capture)
        .env("QSCOPE_PROBE_STOP", &value.stop)
        .env("QSCOPE_PROBE_CLOSED", &value.closed);
    Err(command.exec().into())
}

fn sandbox(value: &Exec) -> Result<String, Error> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| common::failure("Real user HOME is unavailable"))?
        .canonicalize()?;
    let quote = |path: &Path| -> Result<String, Error> {
        Ok(serde_json::to_string(
            path.to_str()
                .filter(|path| !path.chars().any(char::is_control))
                .ok_or_else(|| common::failure("Sandbox paths must be UTF-8 without controls"))?,
        )?)
    };
    let cli = std::env::current_exe()?.canonicalize()?;
    Ok(format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (subpath {}) (literal \"/dev/null\") (literal \"/dev/random\") (literal \"/dev/urandom\"))\n(deny file-read* (subpath {}))\n(allow file-read* (subpath {}) (subpath {}) (subpath {}) (subpath {}) (literal {}) (literal {}))\n(allow file-read-metadata)\n(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\"))\n",
        quote(&value.state)?,
        quote(&home)?,
        quote(&value.state)?,
        quote(&value.app)?,
        quote(&value.run)?,
        quote(&value.specification.resources_directory)?,
        quote(&cli)?,
        quote(&value.probe)?
    ))
}

fn guard(path: &Path, child: bool) -> Result<Value, Error> {
    let value: Exec = serde_json::from_value(common::read_json(path)?)?;
    validate_exec(&value)?;
    if !value.guard {
        return Err(common::failure(
            "Isolation guard requires a synthetic request",
        ));
    }
    for name in ["HOME", "CFFIXED_USER_HOME"] {
        if std::env::var_os(name).map(PathBuf::from) != Some(value.state.join("home")) {
            return Err(common::failure("Guard home environment is not isolated"));
        }
    }
    let marker = value.state.join(if child {
        "guard-child-write"
    } else {
        "guard-parent-write"
    });
    fs::write(&marker, b"synthetic")?;
    if fs::read(&marker)? != b"synthetic" {
        return Err(common::failure("Guard profile write failed"));
    }
    fs::remove_file(marker)?;
    let result = json!({"schema_version":1,"isolated_home":true,"profile_write":true,"outside_write_denied":true,"pid":std::process::id(),"birth_filetime":process::identity(std::process::id())?.0});
    common::write_json(
        &value.state.join(if child {
            "guard-child.json"
        } else {
            "guard-parent.json"
        }),
        &result,
    )?;
    if !child {
        let status = Command::new(std::env::current_exe()?)
            .args([
                OsString::from("__collector"),
                "isolation-guard".into(),
                path.as_os_str().into(),
                "child".into(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(common::failure("Child isolation guard failed"));
        }
        common::write_json(&value.state.join("guard-ready.json"), &result)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(ack) = common::read_json(&value.state.join("guard-continue.json")) {
                if ack["pid"] != result["pid"] || ack["birth_filetime"] != result["birth_filetime"]
                {
                    return Err(common::failure("Guard acknowledgment identity changed"));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(common::failure("Guard acknowledgment timed out"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(result)
}

struct Session {
    child: Child,
    birth: String,
    image: PathBuf,
    stop: PathBuf,
    closed: PathBuf,
    capture: bool,
    finished: bool,
}
struct PendingChild(Option<Child>);
impl Drop for PendingChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
impl Session {
    fn finish(&mut self) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        let mut failure = None;
        if self.capture {
            if let Err(error) = runtime::no_links(&self.stop)
                .and_then(|_| fs::write(&self.stop, b"stop").map_err(Error::from))
            {
                failure = Some(error);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait()?.is_none()
                && !self.closed.is_file()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        if self.child.try_wait()?.is_none() {
            if let Err(error) = process::signal(
                self.child.id(),
                &self.birth,
                &self.image,
                libc::SIGTERM,
                true,
            ) {
                failure.get_or_insert(error);
                let _ = self.child.kill();
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait()?.is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            if self.child.try_wait()?.is_none()
                && let Err(error) = process::signal(
                    self.child.id(),
                    &self.birth,
                    &self.image,
                    libc::SIGKILL,
                    true,
                )
            {
                failure.get_or_insert(error);
                self.child.kill()?;
            }
        }
        self.child.wait()?;
        self.finished = true;
        failure.map_or(Ok(()), Err)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn capture_state(path: &Path, pid: u32, birth: &str) -> Result<Value, Error> {
    let file = common::regular(path)?;
    if file.metadata()?.len() > 256 * 1024 * 1024 {
        return Err(common::failure("Capture exceeds its 256 MiB limit"));
    }
    let mut reader = qscope_core::native::Reader::new(file)
        .map_err(|error| Error::new(error.code, error.to_string()))?;
    if reader.header.pid != pid || reader.header.process_birth_filetime.0.to_string() != birth {
        return Err(common::failure("Capture process identity changed"));
    }
    let mut records = 0u64;
    let mut ready = false;
    let mut dropped = 0u64;
    let mut partial = false;
    loop {
        match reader.next_event() {
            Ok(Some(event)) => {
                records += 1;
                ready |= event.command.as_deref() == Some("hooks_ready");
                dropped = event.dropped_total.0;
            }
            Ok(None) => break,
            Err(error) if error.code == "native_partial_tail" => {
                partial = true;
                break;
            }
            Err(error) => return Err(Error::new(error.code, error.to_string())),
        }
    }
    Ok(
        json!({"clean_end":reader.ended,"partial_tail":partial,"hooks_ready":ready,"records":records.to_string(),"dropped_total":dropped.to_string()}),
    )
}

fn run(spec: &Specification, self_test: bool) -> Result<Value, Error> {
    process::install_stop_handler()?;
    process::stop_file(spec.receipt_path.with_file_name("stop"));
    common::cancelled(&AtomicBool::new(false))?;
    let native = assets(spec)?;
    let run_id = runtime::instance_id()?;
    let run = spec.package_directory.join("portable/runs").join(&run_id);
    common::directory(&run)?;
    let state = if self_test {
        spec.package_directory.join("portable/tests").join(&run_id)
    } else {
        profile(spec)
    };
    common::directory(&state)?;
    let _profile = common::lock(&state.join(".session.lock"))?;
    initialize_profile(&state)?;
    let app = if self_test {
        run.join("synthetic.app")
    } else {
        prepare_research(spec, &native)?
    };
    let hooks = if self_test {
        None
    } else {
        Some(package::capture_profile(&app)?)
    };
    let capture = state.join(".qscope/captures").join(&run_id);
    common::directory(&capture)?;
    let value = Exec {
        schema_version: 1,
        specification: spec.clone(),
        run: run.clone(),
        state: state.clone(),
        app,
        probe: native.probe,
        probe_sha256: native.probe_sha256,
        capture: capture.join("capture.qscap"),
        stop: capture.join("stop"),
        closed: capture.join("closed"),
        send: hooks
            .as_ref()
            .map(|hooks| hooks.send.clone())
            .unwrap_or_default(),
        receive: hooks
            .as_ref()
            .map(|hooks| hooks.receive.clone())
            .unwrap_or_default(),
        guard: self_test,
    };
    let policy = run.join("isolation.sb");
    fs::write(&policy, sandbox(&value)?)?;
    let input = run.join("exec.json");
    common::write_json(&input, &value)?;
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .args([
            OsString::from("-f"),
            policy.into_os_string(),
            std::env::current_exe()?.into_os_string(),
            "__collector".into(),
            if self_test {
                "isolation-guard".into()
            } else {
                "exec".into()
            },
            input.into_os_string(),
        ])
        .env_clear()
        .envs(environment(&value))
        .current_dir(&run)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(private_log(&run.join("process.log"))?)
        .stderr(private_log(&run.join("process.err.log"))?);
    let mut pending = PendingChild(Some(command.spawn()?));
    let child = pending.0.as_mut().expect("owned child");
    let pid = child.id();
    let expected = if self_test {
        std::env::current_exe()?.canonicalize()?
    } else {
        value.app.join("Contents/MacOS/QQ").canonicalize()?
    };
    let begin = Instant::now();
    let (birth, image) = loop {
        if process::stopping() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(common::failure("Research startup cancelled before exec"));
        }
        if let Ok((birth, image)) = process::identity(pid)
            && image.canonicalize().ok().as_ref() == Some(&expected)
        {
            break (birth, image);
        }
        if let Some(status) = child.try_wait()? {
            return Err(common::failure(format!(
                "Research process exited before exec: {status}. See {}",
                run.display()
            )));
        }
        if begin.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(common::failure(
                "Research process did not exec the expected image",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut session = Session {
        child: pending.0.take().expect("owned child"),
        birth: birth.clone(),
        image,
        stop: value.stop.clone(),
        closed: value.closed.clone(),
        capture: !self_test,
        finished: false,
    };
    let (owner_birth, _) = process::identity(std::process::id())?;
    let mut report = json!({"schema_version":1,"kind":"qscope-research-launcher","state":"starting","launcher_pid":std::process::id(),"launcher_birth_filetime":owner_birth,"profile":spec.profile,"endpoint":format!("http://127.0.0.1:{}",spec.port),"connection_file":state.join(".qscope/devtools.json"),"pid":pid,"process_birth_filetime":birth,"client_build":package::BUILD,"capture_directory":if self_test {None}else{Some(&capture)},"state_directory":state,"isolation":"inherited filesystem sandbox plus private HOME/CFFIXED_USER_HOME and Electron paths","automatic_login_performed":false,"module_sha256":package::MODULE_SHA256,"native_build":native.build_id});
    let outcome: Result<(), Error> = (|| {
        if self_test {
            loop {
                if let Ok(ready) = common::read_json(&state.join("guard-ready.json")) {
                    if ready["pid"] != pid || ready["birth_filetime"] != birth {
                        return Err(common::failure("Isolation guard identity changed"));
                    }
                    common::write_json(&state.join("guard-continue.json"), &ready)?;
                    break;
                }
                if process::stopping()
                    || session.child.try_wait()?.is_some()
                    || begin.elapsed() > Duration::from_secs(10)
                {
                    return Err(common::failure("Isolation guard readiness failed"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        } else {
            loop {
                if process::stopping() {
                    return Err(common::failure("Research startup cancelled"));
                }
                if let Ok(status) = capture_state(&value.capture, pid, &birth)
                    && status["hooks_ready"] == true
                {
                    break;
                }
                if session.child.try_wait()?.is_some() || begin.elapsed() > Duration::from_secs(30)
                {
                    return Err(common::failure(format!(
                        "Capture hooks did not become ready; see {}",
                        run.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        common::write_json(&spec.receipt_path, &report)?;
        common::write_json(&run.join("manifest.json"), &report)?;
        let owner = spec
            .receipt_path
            .parent()
            .ok_or_else(|| common::failure("Receipt needs a parent"))?;
        loop {
            if let Some(status) = session.child.try_wait()? {
                report["exit_code"] = json!(status.code());
                if !status.success() {
                    return Err(common::failure(format!(
                        "Research process exited with {status}"
                    )));
                }
                report["result"] = json!(if self_test {
                    "self-test-passed"
                } else {
                    "process-exited"
                });
                break;
            }
            if process::stopping() || owner.join("stop").is_file() {
                report["result"] = json!("stop-file");
                break;
            }
            if !self_test && value.closed.is_file() {
                report["result"] = json!("capture-ended");
                break;
            }
            let seconds = if self_test { 75 } else { spec.duration_seconds };
            if seconds > 0 && begin.elapsed() >= Duration::from_secs(u64::from(seconds)) {
                if self_test {
                    return Err(common::failure("Isolation guard timed out"));
                }
                report["result"] = json!("duration");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if self_test {
            report["guard_parent"] = common::read_json(&state.join("guard-parent.json"))?;
            report["guard_child"] = common::read_json(&state.join("guard-child.json"))?;
        }
        Ok(())
    })();
    let finished = session.finish();
    let integrity = if !self_test {
        capture_state(&value.capture, pid, &birth).and_then(|summary| {
            let valid = summary["clean_end"] == true && summary["partial_tail"] == false;
            report["capture_summary"] = summary;
            let mut marker=Vec::new();common::regular(&value.closed)?.take(64).read_to_end(&mut marker)?;
            if valid && marker==b"closed" {
                Ok(())
            } else {
                Err(common::failure(
                    "Research capture has no complete durable end marker; original file is preserved",
                ))
            }
        })
    } else {
        Ok(())
    };
    let result = outcome.and(finished).and(integrity);
    report["state"] = json!(if result.is_ok() { "stopped" } else { "error" });
    if let Err(error) = &result {
        report["failure"] = json!(error.message);
    }
    common::write_json(&run.join("manifest.json"), &report)?;
    result?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_capture(footer: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[..8].copy_from_slice(b"QSCAP01\0");
        for (offset, value) in [(8, 1u32), (12, 64), (16, 7319), (56, 1024 * 1024)] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[24..32].copy_from_slice(&123u64.to_le_bytes());
        bytes[48..56].copy_from_slice(&1_000_000_000u64.to_le_bytes());
        for (id, kind, command) in [
            (1u64, 2u16, "hooks_ready"),
            (2u64, 4u16, "controller_stopped"),
        ] {
            if kind == 4 && !footer {
                continue;
            }
            let mut record = vec![0u8; 80];
            record[..4].copy_from_slice(b"QSR1");
            record[4..8].copy_from_slice(&(80u32 + command.len() as u32).to_le_bytes());
            record[8..10].copy_from_slice(&kind.to_le_bytes());
            record[12..16].copy_from_slice(&7319u32.to_le_bytes());
            record[24..32].copy_from_slice(&id.to_le_bytes());
            record[68..72].copy_from_slice(&(command.len() as u32).to_le_bytes());
            bytes.extend(record);
            bytes.extend(command.as_bytes());
        }
        bytes
    }

    #[test]
    fn capture_integrity_preserves_partial_files_and_requires_exact_process_identity() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let file = root.path().join("synthetic.qscap");
        fs::write(&file, synthetic_capture(true)).unwrap();
        let report = capture_state(&file, 7319, "123").unwrap();
        assert_eq!(report["clean_end"], true);
        assert_eq!(report["hooks_ready"], true);
        assert!(capture_state(&file, 7320, "123").is_err());
        assert!(capture_state(&file, 7319, "124").is_err());
        fs::write(&file, synthetic_capture(false)).unwrap();
        let report = capture_state(&file, 7319, "123").unwrap();
        assert_eq!(report["clean_end"], false);
        assert_eq!(report["partial_tail"], false);
        let mut partial = synthetic_capture(false);
        partial.extend(b"QSR1\0");
        fs::write(&file, &partial).unwrap();
        let report = capture_state(&file, 7319, "123").unwrap();
        assert_eq!(report["clean_end"], false);
        assert_eq!(report["partial_tail"], true);
        assert_eq!(fs::read(&file).unwrap(), partial);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(256 * 1024 * 1024 + 1)
            .unwrap();
        assert!(capture_state(&file, 7319, "123").is_err());
    }

    #[test]
    fn bundle_selection_keeps_framework_metadata_and_rejects_external_links() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let app = root.path().join("Research.app");
        let framework = app.join("Contents/Frameworks/Electron.framework");
        common::directory(&framework).unwrap();
        let info = framework.join("Info.plist");
        fs::write(&info, b"original framework metadata").unwrap();
        let helper = app.join("Contents/Frameworks/QQ Helper.app");
        common::directory(&helper).unwrap();
        let mut bundles = vec![app.canonicalize().unwrap()];
        app_bundles(&app, 0, &mut 0, &mut bundles).unwrap();
        assert_eq!(bundles, vec![app.clone(), helper]);
        assert_eq!(fs::read(&info).unwrap(), b"original framework metadata");
        std::os::unix::fs::symlink(root.path(), app.join("escape")).unwrap();
        let mut bundles = vec![app];
        assert!(app_bundles(&bundles[0].clone(), 0, &mut 0, &mut bundles).is_err());
    }
    #[test]
    fn specifications_reject_path_escape_and_unsupported_modes_before_launch() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let mut spec = Specification {
            schema_version: 1,
            kind: "qscope-native-host".into(),
            profile: "synthetic".into(),
            port: 19860,
            data_directory: root.path().into(),
            package_directory: root.path().join("qq/macos/7.0.3-54395"),
            resources_directory: root.path().join("resources"),
            prebuilt_directory: root.path().join("native"),
            receipt_path: root.path().join("logs/manifest.json"),
            progress_path: PathBuf::new(),
            duration_seconds: 0,
            installer_path: String::new(),
            http_proxy: String::new(),
            http_proxy_certificate: String::new(),
            wait_for_debugger: false,
        };
        if !cfg!(target_arch = "aarch64") {
            assert!(validate(&spec).is_err());
            return;
        }
        validate(&spec).unwrap();
        spec.profile = "../daily".into();
        assert!(validate(&spec).is_err());
        spec.profile = "synthetic".into();
        spec.package_directory = root.path().join("../outside");
        assert!(validate(&spec).is_err());
        spec.package_directory = root.path().join("qq");
        spec.http_proxy = "http://127.0.0.1:1".into();
        assert!(validate(&spec).is_err());
    }
    #[test]
    fn profile_paths_are_separate_from_signed_apps_and_other_profiles() {
        let bytes = json!({"schema_version":1,"kind":"qscope-native-host","profile":"alpha","port":19860,"data_directory":"/owned","package_directory":"/owned/qq/macos/7.0.3-54395","resources_directory":"/resources","prebuilt_directory":"/native","receipt_path":"/owned/logs/a.json"});
        let a: Specification = serde_json::from_value(bytes.clone()).unwrap();
        let mut b: Specification = serde_json::from_value(bytes).unwrap();
        b.profile = "beta".into();
        assert_ne!(profile(&a), profile(&b));
        assert_ne!(research_app(&a), research_app(&b));
        assert!(!profile(&a).starts_with(research_app(&a)));
        assert_ne!(bundle_namespace("Foo"), bundle_namespace("foo"));
        assert!(!bundle_namespace("with_under_score").contains('_'));
    }
}
