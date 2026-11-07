use qscope_core::data::{from_hex, sha256};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const HEX: &str = include_str!("../../../tests/fixtures/wire-basics/input.hex");

#[test]
fn web_command_serves_the_local_api_and_uses_the_same_data_lease_as_desktop() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    struct Server(std::process::Child, std::path::PathBuf);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Ok(client) =
                qscope_automation::client::Client::new(&format!("@{}", self.1.display()))
            {
                let _ = client.post(
                    "/api/v1/service/control",
                    serde_json::json!({"action":"stop","client_id":"a".repeat(48)}),
                );
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
            for _ in 0..100 {
                if !self.1.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    let root = temporary_directory();
    let resources = root.path().join("resources");
    fs::create_dir_all(resources.join("ui")).unwrap();
    fs::write(
        resources.join("ui/index.html"),
        b"<title>Synthetic WebUI</title>",
    )
    .unwrap();
    let data = root.path().join("user data");
    let descriptor = data.join("service.json");
    let arguments = [
        "--json",
        "web",
        "--no-open",
        "--resources",
        path(&resources),
        "--data-dir",
        path(&data),
        "--port",
        "0",
    ];
    let mut command = Command::new(env!("CARGO_BIN_EXE_qscope-cli"));
    command
        .args(arguments)
        .env("PATH", "")
        .stdout(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut server = Server(command.spawn().unwrap(), descriptor.clone());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !descriptor.is_file() {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "WebUI exited before publishing its service: {}",
            fs::read_to_string(data.join("logs/service.err.log")).unwrap_or_default()
        );
        assert!(Instant::now() < deadline, "WebUI startup deadline");
        thread::sleep(Duration::from_millis(50));
    }
    let service = qscope_automation::runtime::Service::read(&descriptor).unwrap();
    assert_eq!(service.owner, "qscope-service");
    assert_ne!(service.pid, server.0.id());
    let client =
        qscope_automation::client::Client::new(&format!("@{}", path(&descriptor))).unwrap();
    assert!(
        client.get("/api/v1/sessions", &[]).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        qscope_automation::client::Client::new(&service.endpoint)
            .unwrap()
            .get("/api/v1/sessions", &[])
            .is_err()
    );
    let second = json(&cli(&[
        "--json",
        "--data-dir",
        path(&data),
        "service",
        "start",
    ]));
    assert_eq!(second["pid"], service.pid);
    assert_eq!(
        qscope_automation::runtime::Service::read(&descriptor)
            .unwrap()
            .pid,
        service.pid
    );
}

#[test]
fn agent_cli_installs_each_skill_and_exposes_a_usable_generic_prompt() {
    let root = temporary_directory();
    let project = root.path().join("project with spaces");
    fs::create_dir(&project).unwrap();
    for (agent, directory) in [
        ("claude-code", ".claude"),
        ("codex", ".agents"),
        ("cursor", ".cursor"),
    ] {
        let installed = json(&cli(&[
            "--json",
            "agent",
            "install",
            "--agent",
            agent,
            "--project",
            path(&project),
        ]));
        assert_eq!(installed["state"], "installed");
        let content =
            fs::read_to_string(project.join(directory).join("skills/qscope/SKILL.md")).unwrap();
        assert!(content.starts_with("---\nname: qscope\n"));
        assert!(
            content.contains(
                env!("CARGO_BIN_EXE_qscope-cli")
                    .rsplit(std::path::MAIN_SEPARATOR)
                    .next()
                    .unwrap()
            )
        );
        assert!(!content.contains("{{CLI_PATH}}"));
    }
    let status = json(&cli(&[
        "--json",
        "agent",
        "status",
        "--project",
        path(&project),
    ]));
    assert!(
        status["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|target| target["state"] == "installed")
    );
    let prompt = json(&cli(&[
        "--json",
        "agent",
        "prompt",
        "--project",
        path(&project),
    ]));
    assert!(
        prompt["prompt"]
            .as_str()
            .unwrap()
            .contains("agent install --agent")
    );
}

#[test]
fn explicit_tlv_jce_and_hex_views_keep_the_same_input_hash() {
    let tlv = "010600020801";
    let decoded = json(&cli(&[
        "--json", "decode", "--hex", tlv, "--format", "tlv16be",
    ]));
    let raw = json(&cli(&["--json", "decode", "--hex", tlv, "--format", "hex"]));
    assert_eq!(decoded["input_sha256"], raw["input_sha256"]);
    assert_eq!(decoded["message"]["fields"][0]["tag"], 262);
    assert_eq!(raw["hex"], tlv);
    let jce = json(&cli(&[
        "--json",
        "decode",
        "--hex",
        "00011603616263",
        "--format",
        "jce",
    ]));
    assert_eq!(jce["message"]["fields"][0]["value"]["signed_decimal"], "1");
    let bad = cli(&[
        "--json",
        "decode",
        "--hex",
        "0106000501",
        "--format",
        "tlv16be",
    ]);
    assert!(!bad.status.success());
    let partial: Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert_eq!(
        partial["message"]["undecoded"],
        serde_json::json!({"start":0,"end":5})
    );
}

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qscope-cli"))
        .args(arguments)
        .output()
        .unwrap()
}
fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn temporary_directory() -> tempfile::TempDir {
    // macOS exposes its temporary directory through /var, which aliases /private/var.
    // Product data paths reject symlink traversal, so use the physical parent in tests too.
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

#[test]
fn invalid_macho_discovery_fails_without_starting_a_backend() {
    let root = temporary_directory();
    let module = root.path().join("bad module.node");
    let data = root.path().join("unused data");
    let source = b"synthetic invalid Mach-O";
    fs::write(&module, source).unwrap();
    let output = cli(&[
        "--json",
        "--data-dir",
        path(&data),
        "qq",
        "scan",
        "--module",
        path(&module),
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "macho");
    assert_eq!(fs::read(&module).unwrap(), source);
    assert!(!data.exists(), "Offline discovery created runtime state");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(not(windows))]
#[test]
fn unsupported_capture_and_updates_refuse_before_backend_or_download_preparation() {
    let root = temporary_directory();
    for (index, arguments) in [
        vec!["qq", "prepare", "--installer", "missing-installer.exe"],
        vec!["qq", "start"],
        vec!["update", "--check"],
        vec![
            "update",
            "--yes",
            "--close-tasks",
            "--proxy",
            "invalid-proxy",
        ],
    ]
    .iter()
    .enumerate()
    {
        if arguments[0] == "qq" && qscope_automation::runtime::capture_supported() {
            continue;
        }
        let data = root.path().join(format!("unused-{index}"));
        let mut all = vec!["--json", "--data-dir", path(&data)];
        all.extend_from_slice(arguments);
        let output = cli(&all);
        assert!(!output.status.success(), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        let code = if arguments[0] == "update" && qscope_automation::update::supported() {
            "update_unsupported"
        } else {
            "unsupported_platform"
        };
        assert_eq!(error["code"], code, "{arguments:?}");
        assert!(
            !data.exists(),
            "Unsupported command created backend or download state: {arguments:?}"
        );
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn unix_qs_install_status_and_uninstall_use_only_the_selected_directory_and_child_path() {
    use std::os::unix::fs::PermissionsExt;

    let root = temporary_directory();
    let directory = root.path().join("command bin with spaces");
    let executable = directory.join("qs");
    let ownership = directory.join(".qs-command.json");
    let data = root.path().join("unused data");
    let scoped_path = std::env::join_paths([&directory]).unwrap();
    let command = |arguments: &[&str], in_path: bool| {
        Command::new(env!("CARGO_BIN_EXE_qscope-cli"))
            .args([
                "--json",
                "--data-dir",
                path(&data),
                "cli",
                "--directory",
                path(&directory),
            ])
            .args(arguments)
            .env(
                "PATH",
                if in_path {
                    scoped_path.as_os_str()
                } else {
                    std::ffi::OsStr::new("")
                },
            )
            .output()
            .unwrap()
    };
    assert_eq!(json(&command(&["status"], false))["installed"], false);
    assert!(!directory.exists());
    let installed = json(&command(&["install"], false));
    assert_eq!(installed["installed"], true);
    assert_eq!(installed["in_user_path"], false);
    assert_eq!(installed["path_setup_required"], true);
    assert_eq!(installed["open_new_terminal"], false);
    assert_eq!(installed["path"], path(&executable));
    assert!(fs::symlink_metadata(&executable).unwrap().is_file());
    assert_ne!(
        fs::metadata(&executable).unwrap().permissions().mode() & 0o111,
        0
    );
    assert!(ownership.is_file());

    let outside = json(&command(&["status"], false));
    assert_eq!(outside["owned"], true);
    assert_eq!(outside["installed"], false);
    assert_eq!(outside["in_user_path"], false);
    let registered = json(&command(&["status"], true));
    assert_eq!(registered["owned"], true);
    assert_eq!(registered["installed"], true);
    assert_eq!(registered["in_user_path"], true);

    let decoded = Command::new("qs")
        .args(["--json", "decode", "--hex", "0801"])
        .env("PATH", &scoped_path)
        .output()
        .unwrap();
    assert_eq!(
        json(&decoded)["message"]["fields"][0]["value"]["unsigned"],
        "1"
    );
    assert_eq!(
        json(&command(&["uninstall", "--if-owned"], true))["installed"],
        false
    );
    assert!(!executable.exists());
    assert!(!ownership.exists());
    let removed = json(&command(&["status"], true));
    assert_eq!(removed["installed"], false);
    assert_eq!(removed["owned"], false);
    assert!(!data.exists(), "Command registration started a backend");
}

#[test]
fn import_inspect_record_and_decode_preserve_the_same_raw_payload() {
    let root = temporary_directory();
    let capture = root.path().join("session.jsonl");
    let recorded = root.path().join("recorded.jsonl");
    let saved = json(&cli(&[
        "import",
        "--hex",
        HEX,
        "--out",
        path(&capture),
        "--direction",
        "inbound",
        "--layer",
        "plaintext",
        "--boundary",
        "frame",
        "--protocol-sequence",
        "18446744073709551615",
    ]));
    assert_eq!(saved["summary"]["packets"], "1");
    let inspected = json(&cli(&["inspect", path(&capture)]));
    assert_eq!(inspected["header"]["source"], "manual");
    assert!(inspected["header"]["client"].is_null());
    assert_eq!(inspected["summary"]["stream_closed"], true);
    let decoded = json(&cli(&["--json", "decode", "--capture", path(&capture)]));
    assert_eq!(
        decoded["input_sha256"],
        sha256(&from_hex(HEX, 1024).unwrap())
    );
    let fields = decoded["message"]["fields"].as_array().unwrap();
    assert_eq!(
        fields
            .iter()
            .map(|field| field["number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 1]
    );
    assert_eq!(fields[0]["value"]["unsigned"], "150");
    assert_eq!(
        fields[3]["value"]["little_endian_bits"],
        "72623859790382856"
    );
    assert_eq!(fields[4]["value"]["message"]["fields"][0]["number"], 1);
    assert_eq!(
        json(&cli(&[
            "record",
            "--input",
            path(&capture),
            "--out",
            path(&recorded)
        ]))["summary"]["captured_bytes"],
        "27"
    );
    assert_eq!(fs::read(&capture).unwrap(), fs::read(&recorded).unwrap());
}

#[test]
fn existing_outputs_and_failed_streams_are_never_published_over_evidence() {
    let root = temporary_directory();
    let output = root.path().join("existing.jsonl");
    fs::write(&output, b"keep original").unwrap();
    let failed = cli(&["--json", "import", "--hex", "0801", "--out", path(&output)]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert_eq!(fs::read(&output).unwrap(), b"keep original");
    let broken = root.path().join("broken.jsonl");
    let rejected = root.path().join("rejected.jsonl");
    fs::write(&broken, b"{not json}\n").unwrap();
    assert!(
        !cli(&[
            "--json",
            "record",
            "--input",
            path(&broken),
            "--out",
            path(&rejected)
        ])
        .status
        .success()
    );
    assert!(!rejected.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn partial_wire_errors_are_usable_json_and_nonzero_exit() {
    let output = cli(&["--json", "decode", "--hex", "0801120300"]);
    assert!(!output.status.success());
    let decoded: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decoded["message"]["fields"].as_array().unwrap().len(), 1);
    assert_eq!(decoded["message"]["error"]["code"], "length_out_of_bounds");
    assert_eq!(decoded["message"]["undecoded"]["start"], 2);
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "wire_decode");
    assert!(!cli(&["decode", "--hex", "123"]).status.success());
}

#[test]
fn an_interrupted_capture_or_bad_hash_cannot_be_imported_as_complete() {
    let root = temporary_directory();
    let capture = root.path().join("session.jsonl");
    json(&cli(&["import", "--hex", "0801", "--out", path(&capture)]));
    let source = fs::read_to_string(&capture).unwrap();
    let mut lines = source
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    lines[1]["payload_sha256"] = Value::String("0".repeat(64));
    let tampered = root.path().join("tampered.jsonl");
    fs::write(
        &tampered,
        lines
            .iter()
            .map(|line| line.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let output = cli(&["--json", "inspect", path(&tampered)]);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "payload_hash"
    );
    let interrupted = root.path().join("interrupted.jsonl");
    fs::write(
        &interrupted,
        source
            .lines()
            .take(2)
            .map(|line| line.to_owned() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let output = cli(&["--json", "inspect", path(&interrupted)]);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "incomplete_stream"
    );
}

// A native capture whose packets carry explicit times, in seconds after the session start.
fn timed_capture(packets: &[(u64, &str)]) -> Vec<u8> {
    const START: u64 = 134383000000000000;
    let mut bytes = vec![0; 64];
    bytes[..8].copy_from_slice(b"QSCAP01\0");
    for (at, value) in [(8, 1u32), (12, 64), (16, 123), (56, 1024 * 1024)] {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (at, value) in [(24, START), (32, START), (48, 10_000_000)] {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    for (index, (second, command)) in packets.iter().enumerate() {
        let id = index as u64 + 1;
        let payload = [8u8, 1];
        let mut record = vec![0; 80];
        record[..4].copy_from_slice(b"QSR1");
        record[4..8].copy_from_slice(&(80 + command.len() as u32 + 2).to_le_bytes());
        record[8..10].copy_from_slice(&1u16.to_le_bytes());
        record[10] = 1 + (id % 2) as u8;
        record[12..16].copy_from_slice(&123u32.to_le_bytes());
        record[24..32].copy_from_slice(&id.to_le_bytes());
        record[32..40].copy_from_slice(&(START + second * 10_000_000).to_le_bytes());
        record[48..56].copy_from_slice(&id.to_le_bytes());
        record[56..64].copy_from_slice(&2u64.to_le_bytes());
        record[64..68].copy_from_slice(&2u32.to_le_bytes());
        record[68..72].copy_from_slice(&(command.len() as u32).to_le_bytes());
        record.extend_from_slice(command.as_bytes());
        record.extend_from_slice(&payload);
        bytes.extend(record);
    }
    bytes
}

#[test]
fn live_queries_leave_out_heartbeats_and_the_background_seen_before_an_action() {
    use std::time::{Duration, Instant};
    struct Server(std::process::Child, std::path::PathBuf);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Ok(client) =
                qscope_automation::client::Client::new(&format!("@{}", self.1.display()))
            {
                let _ = client.post(
                    "/api/v1/service/control",
                    serde_json::json!({"action":"stop","client_id":"a".repeat(48)}),
                );
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = temporary_directory();
    let (resources, captures, data) = (
        root.path().join("resources"),
        root.path().join("captures"),
        root.path().join("data"),
    );
    fs::create_dir_all(resources.join("ui")).unwrap();
    fs::write(resources.join("ui/index.html"), b"<title>Synthetic</title>").unwrap();
    fs::create_dir_all(&captures).unwrap();
    // Ten seconds of a poll every second and a heartbeat every four, then the action after packet 14.
    let mut packets = Vec::new();
    for second in 0..10 {
        if second % 4 == 0 {
            packets.push((second, "Heartbeat.Alive"));
        }
        packets.push((second, "Poll.Status"));
    }
    packets.push((10, "Poll.Status"));
    packets.extend([
        (11, "Poll.Status"),
        (11, "Heartbeat.Alive"),
        (12, "Action.Send"),
    ]);
    fs::write(captures.join("timed.qscap"), timed_capture(&packets)).unwrap();
    let descriptor = data.join("service.json");
    let mut command = Command::new(env!("CARGO_BIN_EXE_qscope-cli"));
    command
        .args([
            "web",
            "--no-open",
            "--resources",
            path(&resources),
            "--data-dir",
            path(&data),
            "--port",
            "0",
            "--captures",
            path(&captures),
        ])
        .env("PATH", "")
        .stdout(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut server = Server(command.spawn().unwrap(), descriptor.clone());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !descriptor.is_file() {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "backend exited: {}",
            fs::read_to_string(data.join("logs/service.err.log")).unwrap_or_default()
        );
        assert!(Instant::now() < deadline, "backend startup deadline");
        std::thread::sleep(Duration::from_millis(50));
    }
    let selector = format!("@{}", path(&descriptor));
    let live = |arguments: &[&str]| {
        let mut all = vec!["--json", "live", "--server", &selector];
        all.extend_from_slice(arguments);
        json(&cli(&all))
    };
    let sessions = live(&["sessions"]);
    let session = sessions["sessions"][0]["id"].as_str().unwrap().to_string();
    let commands = |page: &Value| -> Vec<String> {
        page["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["command"].as_str().unwrap().into())
            .collect()
    };
    let all = live(&["events", &session, "--after", "14"]);
    assert_eq!(
        commands(&all),
        ["Poll.Status", "Heartbeat.Alive", "Action.Send"]
    );
    let named = live(&["events", &session, "--after", "14", "--hide-heartbeat"]);
    assert_eq!(commands(&named), ["Poll.Status", "Action.Send"]);
    let quiet = live(&["events", &session, "--after", "14", "--baseline", "30"]);
    assert_eq!(commands(&quiet), ["Action.Send"]);
    assert_eq!(
        quiet["ignored"]["commands"],
        serde_json::json!(["Heartbeat.Alive", "Poll.Status"])
    );
    assert_eq!(quiet["ignored"]["baseline"]["packets"], 14);
    assert_eq!(quiet["ignored"]["baseline"]["complete"], true);
    // A short baseline sees only the poll, so the heartbeat stays.
    let short = live(&["events", &session, "--after", "14", "--baseline", "1"]);
    assert_eq!(commands(&short), ["Heartbeat.Alive", "Action.Send"]);
    let waited = live(&[
        "wait",
        &session,
        "--after",
        "14",
        "--baseline",
        "30",
        "--timeout",
        "1",
    ]);
    assert_eq!(waited["matched"], true);
    assert_eq!(commands(&waited["page"]), ["Action.Send"]);
    let noise = live(&["noise", &session, "--before", "14", "--seconds", "30"]);
    assert_eq!(noise["commands"][0]["command"], "Poll.Status");
    assert_eq!(noise["commands"][0]["count"], 11);
    assert_eq!(noise["commands"][0]["regular"], true);
    assert_eq!(noise["commands"][0]["median_interval_ms"], 1000);
    assert_eq!(noise["exclude"], "=Poll.Status,=Heartbeat.Alive");
    let missing = cli(&[
        "--json",
        "live",
        "--server",
        &selector,
        "events",
        &session,
        "--baseline",
        "30",
    ]);
    assert!(!missing.status.success());
}
