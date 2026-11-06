use super::*;
use axum::http::Request as HttpRequest;
use std::io::Write;
use tower::ServiceExt;

fn header() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..8].copy_from_slice(b"QSCAP01\0");
    for (at, value) in [(8, 1u32), (12, 64), (16, 123), (56, 1024 * 1024)] {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (at, value) in [
        (24, 134383000000000000u64),
        (32, 134383000000000000),
        (48, 10_000_000),
    ] {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[test]
fn sqlite_index_resumes_appends_and_rebuilds_changed_input() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("live.qscap");
    let mut bytes = header();
    for id in 1..=5000 {
        bytes.extend(record(id, 1, 2, "Test.Cache", &[8, 1], 0));
    }
    fs::write(&source, &bytes).unwrap();
    let path = source.canonicalize().unwrap();
    let index = index::Store::open(&directory.path().join("index")).unwrap();
    let mut first = capture::Capture::open(path.clone()).unwrap();
    first.attach_index(Some(index.clone()));
    first.refresh();
    assert_eq!(first.summary().records, "2048");
    drop(first);
    let mut reopened = capture::Capture::open(path.clone()).unwrap();
    reopened.attach_index(Some(index.clone()));
    reopened.refresh();
    assert_eq!(reopened.summary().records, "4096");
    assert_eq!(
        reopened.event(3000).unwrap().command.as_deref(),
        Some("Test.Cache")
    );
    drop(reopened);
    bytes[64 + 80 + "Test.Cache".len() + 1] = 2;
    fs::write(&source, &bytes).unwrap();
    let mut changed = capture::Capture::open(path).unwrap();
    changed.attach_index(Some(index));
    changed.refresh();
    assert_eq!(changed.summary().records, "2048");
    assert_eq!(changed.event(1).unwrap().payload, [8, 2]);
    assert_eq!(fs::read(source).unwrap(), bytes);
}

#[test]
fn corrupt_sqlite_cache_is_preserved_and_recreated() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("captures-v1.sqlite"), b"broken cache").unwrap();
    let index = index::Store::open(directory.path()).unwrap();
    assert!(index.load("missing").unwrap().is_none());
    let invalid = fs::read_dir(directory.path())
        .unwrap()
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("captures-invalid-")
        })
        .unwrap();
    assert_eq!(fs::read(invalid.path()).unwrap(), b"broken cache");
}
fn record(
    id: u64,
    kind: u16,
    direction: u8,
    command: &str,
    payload: &[u8],
    dropped: u64,
) -> Vec<u8> {
    let mut bytes = vec![0; 80];
    bytes[..4].copy_from_slice(b"QSR1");
    bytes[4..8]
        .copy_from_slice(&(80u32 + command.len() as u32 + payload.len() as u32).to_le_bytes());
    bytes[8..10].copy_from_slice(&kind.to_le_bytes());
    bytes[10] = direction;
    bytes[12..16].copy_from_slice(&123u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&id.to_le_bytes());
    bytes[32..40].copy_from_slice(&(134383000000000000u64 + id * 10000).to_le_bytes());
    bytes[48..56].copy_from_slice(&(9007199254740993u64 + id).to_le_bytes());
    bytes[56..64].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes[64..68].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes[68..72].copy_from_slice(&(command.len() as u32).to_le_bytes());
    bytes[72..80].copy_from_slice(&dropped.to_le_bytes());
    bytes.extend_from_slice(command.as_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
fn setup() -> (tempfile::TempDir, AppState, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("sample.qscap");
    let mut bytes = header();
    bytes.extend(record(1, 1, 2, "Test.PB", &[8, 1, 18, 3, 10, 1, 65], 0));
    bytes.extend(record(2, 1, 1, "Test.TLV", &[1, 6, 0, 2, 8, 1], 2));
    bytes.extend(record(3, 4, 0, "controller_stopped", &[], 2));
    fs::write(&capture, bytes).unwrap();
    fs::write(dir.path().join("index.html"), "<h1>QScope</h1>").unwrap();
    let imports = dir.path().join("imports");
    fs::create_dir(&imports).unwrap();
    let mut library = catalog::Library::new(vec![dir.path().canonicalize().unwrap()]);
    library.discover();
    let state = AppState {
        library: Arc::new(Mutex::new(library)),
        ui: dir.path().canonicalize().unwrap(),
        assets: None,
        token: String::new(),
        control: Arc::new(Mutex::new(control::Manager::default())),
        imports,
        host: "127.0.0.1:19840".into(),
        processes: Arc::new(tokio::sync::Mutex::new(process::Probe::default())),
        recordings: Arc::new(Mutex::new(recording::Manager::default())),
        inspector: Arc::new(Mutex::new(crate::inspector::Manager::default())),
        onboarding: Arc::new(Mutex::new(crate::onboarding::Manager::default())),
        update: Arc::new(Mutex::new(crate::update::Manager::default())),
        runtime: qscope_automation::runtime::Layout::development(dir.path()),
        instance_id: "test-service".into(),
        stopping: Arc::new(AtomicBool::new(false)),
        automation: qscope_automation::skills::Environment {
            workspace: dir.path().to_path_buf(),
            home: dir.path().to_path_buf(),
            cli: std::env::current_exe().unwrap(),
            server_url: "http://127.0.0.1:19840".into(),
        },
    };
    (dir, state, capture)
}
async fn api(state: AppState, uri: &str) -> (StatusCode, serde_json::Value) {
    let request = HttpRequest::builder()
        .uri(uri)
        .header("host", state.host.clone())
        .header("x-qscope-client", "web-v1")
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
fn id(state: &AppState) -> String {
    state.library.lock().unwrap().snapshot().sessions[0]
        .id
        .clone()
}

#[tokio::test]
async fn rust_inspector_relays_both_directions_and_removes_disconnected_targets() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let (root, mut state, _) = setup();
    let public = root
        .path()
        .join("collectors/devtools/node_modules/chii/public");
    fs::create_dir_all(public.join("front_end")).unwrap();
    fs::write(public.join("target.js"), b"/* synthetic target */").unwrap();
    fs::write(
        public.join("front_end/chii_app.html"),
        b"synthetic frontend",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    state.host = listener.local_addr().unwrap().to_string();
    let frontend = state
        .inspector
        .lock()
        .unwrap()
        .ensure(&state.runtime, &state.host, None)
        .unwrap();
    let server_state = state.clone();
    let server =
        tokio::spawn(async move { axum::serve(listener, router(server_state)).await.unwrap() });
    let socket_base = format!("ws://{}{}", state.host, frontend.base_path);
    assert!(
        connect_async(format!("ws://{}/wrong/target/fixture", state.host))
            .await
            .is_err()
    );
    let metadata = format!("{}targets", frontend.base_path);
    assert_eq!(
        api(state.clone(), &metadata).await.1["targets"],
        serde_json::json!([])
    );
    let (mut target, _) = connect_async(format!("{socket_base}target/fixture?title=Synthetic"))
        .await
        .unwrap();
    let (mut inspector, _) = connect_async(format!("{socket_base}client/viewer?target=fixture"))
        .await
        .unwrap();
    inspector
        .send(Message::Text(
            "{\"id\":1,\"method\":\"DOM.getDocument\"}".into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), target.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap(),
        "{\"id\":1,\"method\":\"DOM.getDocument\"}"
    );
    target
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), inspector.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data(),
        vec![1, 2, 3]
    );
    let (mut writer, mut reader) = inspector.split();
    let burst = tokio::spawn(async move {
        for index in 0..64u8 {
            writer
                .send(Message::Binary(vec![index; 256 * 1024].into()))
                .await
                .unwrap();
        }
        writer
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    for index in 0..64u8 {
        let packet = tokio::time::timeout(std::time::Duration::from_secs(5), target.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data();
        assert_eq!(packet.len(), 256 * 1024);
        assert!(packet.iter().all(|byte| *byte == index));
    }
    let _writer = burst.await.unwrap();
    let replies = tokio::spawn(async move {
        for index in 0..64u8 {
            let packet = tokio::time::timeout(std::time::Duration::from_secs(5), reader.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_data();
            assert_eq!(packet.as_ref(), &[index; 128]);
        }
    });
    for index in 0..64u8 {
        target
            .send(Message::Binary(vec![index; 128].into()))
            .await
            .unwrap();
    }
    replies.await.unwrap();
    target.close(None).await.unwrap();
    for _ in 0..50 {
        if api(state.clone(), &metadata).await.1["targets"] == serde_json::json!([]) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        api(state.clone(), &metadata).await.1["targets"],
        serde_json::json!([])
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn embedded_service_uses_descriptor_identity_and_releases_its_listener() {
    let (directory, state, _) = setup();
    let instance_id = qscope_automation::runtime::instance_id().unwrap();
    let service = start(ServeOptions {
        captures: vec![directory.path().to_path_buf()],
        imports: state.imports,
        ui: state.ui,
        assets: None,
        token: None,
        port: 0,
        fallback_port: false,
        automation: state.automation,
        runtime: state.runtime,
        instance_id: instance_id.clone(),
    })
    .await
    .unwrap();
    let descriptor = qscope_automation::runtime::Service {
        schema_version: 1,
        owner: "qscope-desktop".into(),
        instance_id,
        endpoint: service.endpoint.clone(),
        pid: std::process::id(),
        token: String::new(),
        version: String::new(),
        build_id: String::new(),
        resources: None,
        workspace: None,
    };
    let path = directory.path().join("service.json");
    descriptor.publish(&path).unwrap();
    let selector = format!("@{}", path.display());
    tokio::task::spawn_blocking(move || {
        let client = qscope_automation::client::Client::new(&selector).unwrap();
        assert!(client.get("/api/v1/sessions", &[]).unwrap()["sessions"].is_array());
        let mut wrong = descriptor;
        wrong.instance_id = qscope_automation::runtime::instance_id().unwrap();
        wrong.publish(&path).unwrap();
        assert!(qscope_automation::client::Client::new(&selector).is_err());
    })
    .await
    .unwrap();
    let address = service.endpoint.trim_start_matches("http://").to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(std::net::TcpListener::bind(address).is_ok());
}

#[tokio::test]
async fn catalog_does_not_index_unopened_files_or_wait_for_busy_capture() {
    fn occupy(
        capture: catalog::SharedCapture,
    ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (ready, acquired) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = capture.lock().unwrap();
            ready.send(()).unwrap();
            wait.recv().unwrap();
        });
        acquired.recv().unwrap();
        (release, worker)
    }
    let (_dir, state, _) = setup();
    let session_id = id(&state);
    let capture = state.library.lock().unwrap().get(&session_id).unwrap();
    let (release, worker) = occupy(capture.clone());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        api(state.clone(), "/api/v1/sessions"),
    )
    .await;
    release.send(()).unwrap();
    worker.join().unwrap();
    let (status, catalog) = result.expect("catalog must not wait for payload indexing");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["sessions"][0]["records"], "0");
    assert_eq!(catalog["sessions"][0]["has_more"], true);
    assert!(catalog["sessions"][0]["error"].is_null());

    let (_, page) = api(
        state.clone(),
        &format!("/api/v1/sessions/{session_id}/events"),
    )
    .await;
    assert_eq!(page["session"]["records"], "3");
    let (_, indexed) = api(state.clone(), "/api/v1/sessions").await;
    assert_eq!(indexed["sessions"][0]["records"], "3");
    let (release, worker) = occupy(capture);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        api(state, "/api/v1/sessions"),
    )
    .await;
    release.send(()).unwrap();
    worker.join().unwrap();
    let (_, busy) = result.expect("busy capture must retain its last catalog metadata");
    assert_eq!(busy["sessions"][0]["records"], "3");
    assert_eq!(busy["sessions"][0]["clean_end"], true);
}

#[tokio::test]
async fn advanced_filters_apply_to_complete_index_and_validate_ranges() {
    let (_dir, state, _) = setup();
    let id = id(&state);
    for (query, expected) in [
        ("include=Test.*&exclude=*TLV", 1),
        ("seq=9007199254740995", 1),
        ("min_size=7&max_size=7", 1),
        ("event_from=2&event_to=2&tid=9", 0),
        ("controls=true&kinds=end", 1),
        ("error_only=true", 0),
        ("truncated_only=true", 0),
        ("time_from=134383000000020000&time_to=134383000000020000", 1),
    ] {
        let (status, page) = api(
            state.clone(),
            &format!("/api/v1/sessions/{id}/events?{query}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(
            page["events"].as_array().unwrap().len(),
            expected,
            "{query}"
        );
    }
    let (status, _) = api(
        state,
        &format!("/api/v1/sessions/{id}/events?min_size=8&max_size=2"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn comparison_aligns_repeated_fields_and_missing_values_across_three_packets() {
    let (directory, state, path) = setup();
    let mut bytes = header();
    for (i, payload) in [&[8, 1, 8, 2][..], &[8, 1, 8, 3][..], &[8, 1][..]]
        .iter()
        .enumerate()
    {
        bytes.extend(record(i as u64 + 1, 1, 2, "Compare.PB", payload, 0));
    }
    bytes.extend(record(4, 4, 0, "end", &[], 0));
    fs::write(&path, bytes).unwrap();
    let mut library = catalog::Library::new(vec![directory.path().canonicalize().unwrap()]);
    library.discover();
    let source = library.snapshot().sessions[0].id.clone();
    let result = compare::run(
        &library,
        contract::CompareRequest {
            items: (1..=3)
                .map(|i| contract::PacketRef {
                    session_id: source.clone(),
                    event_id: i.to_string(),
                })
                .collect(),
            format: Some("pb".into()),
        },
    )
    .unwrap();
    assert_eq!(result.columns.len(), 3);
    assert_eq!(result.rows.len(), 2);
    assert!(!result.rows[0].changed);
    assert!(result.rows[1].changed);
    assert_eq!(
        result.rows[1].values,
        vec![Some("2".into()), Some("3".into()), None]
    );
    assert!(
        compare::run(
            &state.library.lock().unwrap(),
            contract::CompareRequest {
                items: vec![],
                format: None
            }
        )
        .is_err()
    );
}

#[test]
fn comparison_detects_different_unknown_bytes_despite_identical_display_values() {
    let (directory, _, path) = setup();
    let mut bytes = header();
    bytes.extend(record(1, 1, 2, "Bytes", &[18, 2, 255, 254], 0));
    bytes.extend(record(2, 1, 2, "Bytes", &[18, 2, 255, 253], 0));
    bytes.extend(record(3, 4, 0, "end", &[], 0));
    fs::write(path, bytes).unwrap();
    let mut library = catalog::Library::new(vec![directory.path().canonicalize().unwrap()]);
    library.discover();
    let id = library.snapshot().sessions[0].id.clone();
    let comparison = compare::run(
        &library,
        contract::CompareRequest {
            items: (1..=2)
                .map(|i| contract::PacketRef {
                    session_id: id.clone(),
                    event_id: i.to_string(),
                })
                .collect(),
            format: Some("pb".into()),
        },
    )
    .unwrap();
    assert_eq!(comparison.rows[0].values[0], comparison.rows[0].values[1]);
    assert!(comparison.rows[0].changed);
    assert_ne!(
        comparison.rows[0].fingerprints[0],
        comparison.rows[0].fingerprints[1]
    );
}

#[test]
fn recording_window_reopens_without_old_packets_and_preserves_input_and_loss_delta() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("live.qscap");
    let mut original = header();
    original.extend(record(1, 1, 2, "Before", &[8, 1], 3));
    fs::write(&source, &original).unwrap();
    let mut capture = capture::Capture::open(source.clone()).unwrap();
    capture.refresh();
    let session = capture.summary();
    let imports = directory.path().join("imports");
    fs::create_dir(&imports).unwrap();
    let mut manager = recording::Manager::default();
    let job = manager.start(&session, &imports).unwrap();
    assert!(manager.start(&session, &imports).is_err());
    let next = record(2, 1, 1, "During", &[8, 2], 5);
    original.extend(&next);
    original.extend(&record(3, 1, 2, "Partial", &[8, 3], 5)[..30]);
    fs::write(&source, &original).unwrap();
    for _ in 0..100 {
        if manager.list()[0].packets == "1" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    manager.stop(&job.id).unwrap();
    for _ in 0..100 {
        if manager.list()[0].state != "recording" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let saved = &manager.list()[0];
    assert_eq!(saved.state, "stopped");
    assert_eq!(saved.packets, "1");
    assert_eq!(fs::read(&source).unwrap(), original);
    let mut reader = qscope_core::native::Reader::new(std::io::BufReader::new(
        fs::File::open(&saved.path).unwrap(),
    ))
    .unwrap();
    let mut packets = vec![];
    while let Some(event) = reader.next_event().unwrap() {
        if event.kind == "packet" {
            assert_eq!(event.payload, [8, 2]);
            assert_eq!(event.dropped_total.0, 2);
            packets.push(event);
        }
    }
    assert_eq!(packets.len(), 1);
    assert!(reader.ended);
    assert_eq!(saved.source_first_event.as_deref(), Some("2"));
}

#[test]
fn http_view_retains_body_offsets_for_protocol_decoding() {
    let bytes = record(
        1,
        1,
        2,
        "HTTP POST https://example.test/upload",
        b"POST /upload HTTP/1.1\r\nContent-Type: application/octet-stream\r\n\r\n\x08\x01",
        0,
    );
    let input = [header(), bytes].concat();
    let event = qscope_core::native::Reader::new(std::io::Cursor::new(input))
        .unwrap()
        .next_event()
        .unwrap()
        .unwrap();
    let result = decode::detail(
        event,
        &decode::DecodeQuery {
            format: Some("http".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.complete, Some(true));
    let body = result.nodes.last().unwrap();
    assert_eq!(body.range.end - body.range.start, 2);
    assert_eq!(&result.payload_hex[body.range.start * 2..], "0801");
}

#[tokio::test]
async fn filters_history_and_preserves_64bit_values() {
    let (_dir, state, _) = setup();
    let id = id(&state);
    let (status, page) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events?direction=inbound&q=tlv"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["events"][0]["sequence"], "9007199254740995");
    assert_eq!(page["session"]["packets"], "2");
    assert_eq!(page["session"]["dropped_total"], "2");
    assert_eq!(page["session"]["clean_end"], true);
    let (_, page) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events?limit=1"),
    )
    .await;
    assert_eq!(page["events"][0]["event_id"], "2");
    assert_eq!(page["has_earlier"], true);
    let (_, page) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events?before=2&limit=1"),
    )
    .await;
    assert_eq!(page["events"][0]["event_id"], "1");
    assert_eq!(page["has_earlier"], false);
    let (_, page) = api(
        state,
        &format!("/api/v1/sessions/{id}/events?after=1&controls=true&limit=1"),
    )
    .await;
    assert_eq!(page["cursor"], "2");
    assert_eq!(page["has_more"], true);
}

#[tokio::test]
async fn full_field_copy_reads_original_unicode_and_json_groups_repeats_without_losing_u64() {
    let (directory, _, path) = setup();
    let text = "前缀\"\\".to_owned() + &"中文0123456789".repeat(180) + "END-完整尾部";
    let mut length = text.len();
    let mut payload = vec![10];
    loop {
        let mut byte = (length & 127) as u8;
        length >>= 7;
        if length != 0 {
            byte |= 128;
        }
        payload.push(byte);
        if length == 0 {
            break;
        }
    }
    payload.extend(text.as_bytes());
    let mut bytes = header();
    bytes.extend(record(1, 1, 2, "Long.Text", &payload, 0));
    bytes.extend(record(
        2,
        1,
        1,
        "Repeats",
        &[
            8, 1, 8, 2, 16, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1,
        ],
        0,
    ));
    bytes.extend(record(3, 4, 0, "end", &[], 0));
    fs::write(&path, bytes).unwrap();
    let before = qscope_core::data::sha256(&fs::read(&path).unwrap());
    let mut library = catalog::Library::new(vec![directory.path().canonicalize().unwrap()]);
    library.discover();
    let id = library.snapshot().sessions[0].id.clone();
    let imports = directory.path().join("imports");
    fs::create_dir_all(&imports).unwrap();
    let state = AppState {
        library: Arc::new(Mutex::new(library)),
        ui: directory.path().canonicalize().unwrap(),
        assets: None,
        token: String::new(),
        control: Arc::new(Mutex::new(control::Manager::default())),
        imports,
        host: "127.0.0.1:19840".into(),
        processes: Arc::new(tokio::sync::Mutex::new(process::Probe::default())),
        recordings: Arc::new(Mutex::new(recording::Manager::default())),
        inspector: Arc::new(Mutex::new(crate::inspector::Manager::default())),
        onboarding: Arc::new(Mutex::new(crate::onboarding::Manager::default())),
        update: Arc::new(Mutex::new(crate::update::Manager::default())),
        runtime: qscope_automation::runtime::Layout::development(directory.path()),
        instance_id: "test-service".into(),
        stopping: Arc::new(AtomicBool::new(false)),
        automation: qscope_automation::skills::Environment {
            workspace: directory.path().to_path_buf(),
            home: directory.path().to_path_buf(),
            cli: std::env::current_exe().unwrap(),
            server_url: "http://127.0.0.1:19840".into(),
        },
    };
    let (_, detail) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/1?format=pb"),
    )
    .await;
    assert_eq!(detail["nodes"][0]["presentation"]["key"], "1");
    assert_eq!(detail["nodes"][0]["presentation"]["preview_limited"], true);
    let (status, value) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/1/text?format=pb&mode=value&node_id=pb.0"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["text"], text);
    let (_, structure) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/2/text?format=pb&mode=structure"),
    )
    .await;
    let json: serde_json::Value =
        serde_json::from_str(structure["text"].as_str().unwrap()).unwrap();
    assert_eq!(json["1"], serde_json::json!(["1", "2"]));
    assert_eq!(json["2"], "18446744073709551615");
    let (status, _) = api(
        state,
        &format!("/api/v1/sessions/{id}/events/1/text?mode=value&node_id=missing"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(qscope_core::data::sha256(&fs::read(&path).unwrap()), before);
}

#[tokio::test]
async fn detail_uses_core_decoders_and_absolute_byte_ranges() {
    let (_dir, state, _) = setup();
    let id = id(&state);
    let (_, detail) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/1?format=pb"),
    )
    .await;
    assert_eq!(detail["payload_hex"], "080112030a0141");
    assert_eq!(detail["nodes"][0]["value"], "1");
    assert_eq!(detail["nodes"][1]["children"][0]["candidate"], true);
    let (_, detail) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/2?format=pb&offset=4&length=2"),
    )
    .await;
    assert_eq!(detail["nodes"][0]["range"]["start"], 4);
    assert_eq!(detail["nodes"][0]["range"]["end"], 6);
    let (_, detail) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/2?format=tlv16be"),
    )
    .await;
    assert_eq!(detail["complete"], true);
    assert_eq!(detail["nodes"][0]["label"], "tag 0x0106 · TLV");
    let (status, _) = api(
        state.clone(),
        &format!("/api/v1/sessions/{id}/events/1?offset=9"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = api(
        state,
        &format!("/api/v1/sessions/{id}/events/1?format=unknown"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn partial_tail_is_retried_without_duplication() {
    let (dir, _state, _) = setup();
    let path = dir.path().join("live.qscap");
    let tail = record(2, 1, 1, "Live.Second", &[8, 2], 0);
    let mut bytes = header();
    bytes.extend(record(1, 1, 2, "Live.First", &[8, 1], 0));
    bytes.extend(&tail[..83]);
    fs::write(&path, bytes).unwrap();
    let mut capture = capture::Capture::open(path.clone()).unwrap();
    let first = capture.list(&capture::ListQuery::default()).unwrap();
    assert!(first.session.partial_tail);
    assert_eq!(first.events.len(), 1);
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&tail[83..])
        .unwrap();
    let second = capture
        .list(&capture::ListQuery {
            after: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert!(!second.session.partial_tail);
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].event_id, "2");
    assert_eq!(capture.event(1).unwrap().payload, [8, 1]);
    assert_eq!(
        capture
            .list(&capture::ListQuery::default())
            .unwrap()
            .events
            .len(),
        2
    );
}

#[tokio::test]
async fn rejects_cross_origin_rebinding_and_unmarked_requests() {
    let (_dir, state, _) = setup();
    for (host, origin, marker) in [
        ("attacker.test:19840", None, true),
        ("127.0.0.1:19840", Some("https://attacker.test"), true),
        ("127.0.0.1:19840", None, false),
    ] {
        let mut request = HttpRequest::builder()
            .uri("/api/v1/sessions")
            .header("host", host);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if marker {
            request = request.header("x-qscope-client", "web-v1");
        }
        let response = router(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let (status, _) = api(state, "/api/v1/sessions/arbitrary-path/events").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn imports_are_preserved_and_invalid_uploads_leave_no_temporary_files() {
    let (_dir, state, _) = setup();
    for (body, expected) in [
        (b"invalid".to_vec(), StatusCode::BAD_REQUEST),
        (header(), StatusCode::OK),
    ] {
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/api/v1/import")
            .header("host", "127.0.0.1:19840")
            .header("x-qscope-client", "web-v1")
            .body(Body::from(body))
            .unwrap();
        let response = router(state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status, expected, "{}", String::from_utf8_lossy(&body));
        if status == StatusCode::OK {
            let imported: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(imported["source_kind"], "imported");
            assert!(imported["producer_alive"].is_null());
        }
    }
    let files: Vec<_> = fs::read_dir(&state.imports)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(fs::read(&files[0]).unwrap(), header());
}

#[test]
fn view_keeps_partial_decodes_and_marks_display_limits() {
    let (dir, _state, _) = setup();
    let path = dir.path().join("view.qscap");
    let mut bytes = header();
    let payload: Vec<u8> = [8, 1].repeat(2100);
    bytes.extend(record(1, 1, 1, "Large.Fields", &payload, 0));
    bytes.extend(record(2, 1, 1, "Non.PB", &[0xff], 0));
    fs::write(&path, bytes).unwrap();
    let mut capture = capture::Capture::open(path).unwrap();
    capture.refresh();
    let detail =
        decode::detail(capture.event(1).unwrap(), &decode::DecodeQuery::default()).unwrap();
    assert!(detail.view_limited);
    assert_eq!(detail.nodes.len(), 2000);
    assert_eq!(detail.next_node_offset, Some(2000));
    assert_eq!(detail.payload_hex.len(), 8400);
    let rest = decode::detail(
        capture.event(1).unwrap(),
        &decode::DecodeQuery {
            node_offset: Some(2000),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!rest.view_limited);
    assert_eq!(rest.next_node_offset, None);
    assert_eq!(rest.nodes.len(), 100);
    assert_eq!(rest.nodes[0].id, "pb.2000");
    let detail =
        decode::detail(capture.event(2).unwrap(), &decode::DecodeQuery::default()).unwrap();
    assert_eq!(detail.complete, Some(false));
    assert!(detail.undecoded.is_some());
    assert_eq!(detail.payload_hex, "ff");
}

#[tokio::test]
async fn local_fonts_and_license_notices_are_served_with_their_real_content_types() {
    let (_dir, state, _) = setup();
    for (filename, content_type) in [
        ("fixture.otf", "font/otf"),
        ("fixture.woff2", "font/woff2"),
        ("LICENSE.txt", "text/plain; charset=utf-8"),
    ] {
        fs::write(state.ui.join(filename), b"synthetic asset").unwrap();
        let request = HttpRequest::builder()
            .uri(format!("/{filename}"))
            .header("host", "127.0.0.1:19840")
            .body(Body::empty())
            .unwrap();
        let response = router(state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn qq_setup_status_is_metadata_only_and_reports_missing_runtime_components() {
    let (_root, state, _) = setup();
    let (status, body) = api(state, "/api/v1/settings/qq").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["build"], "9.9.37-54395");
    assert_eq!(body["total"], "334442672");
    assert_eq!(body["available"], false);
    assert_eq!(body["installed"], false);
    assert!(body["pid"].is_null());
}

#[tokio::test]
async fn qq_setup_requires_the_cli_worker_and_probe() {
    let (root, mut state, _) = setup();
    let worker = root.path().join("qscope-cli.exe");
    fs::write(&worker, b"metadata fixture only").unwrap();
    state.automation.cli = worker.clone();
    state.runtime.worker = Some(worker);
    let native = root.path().join("native");
    fs::create_dir(&native).unwrap();
    fs::write(native.join("qscope-portable.dll"), b"metadata fixture only").unwrap();
    state.runtime.native = Some(native);
    let (status, body) = api(state.clone(), "/api/v1/settings/qq").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["available"],
        cfg!(all(windows, target_arch = "x86_64"))
    );
    let (status, body) = api(state, "/api/v1/settings/devtools/launch").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["launch_version"], 3);
    assert!(body["command"].as_str().unwrap().contains("qscope-cli.exe"));
}

/// A synthetic launch: own PID and start time, records rewritten to that PID.
fn launch(pid: u32, started: u64, records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = header();
    bytes[16..20].copy_from_slice(&pid.to_le_bytes());
    bytes[32..40].copy_from_slice(&started.to_le_bytes());
    for record in records {
        let mut record = record.clone();
        record[12..16].copy_from_slice(&pid.to_le_bytes());
        bytes.extend(record);
    }
    bytes
}
fn account(id: u64, uin: &str) -> Vec<u8> {
    record(
        id,
        2,
        0,
        &format!("account_observed type=1 uin={uin}"),
        &[],
        0,
    )
}
fn names(page: &serde_json::Value, field: &str) -> Vec<String> {
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"][field].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn account_is_derived_from_status_records_and_survives_index_restore() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("live.qscap");
    let mut bytes = launch(
        123,
        134383000000000000,
        &[
            record(1, 1, 2, "Before.Login", &[8, 1], 0),
            account(2, "10001"),
            record(3, 1, 2, "After.Login", &[8, 1], 0),
            record(4, 2, 0, "account_observed type=1 uin=bad!", &[], 0),
        ],
    );
    fs::write(&source, &bytes).unwrap();
    let path = source.canonicalize().unwrap();
    let index = index::Store::open(&directory.path().join("index")).unwrap();
    let mut first = capture::Capture::open(path.clone()).unwrap();
    first.attach_index(Some(index.clone()));
    first.refresh();
    assert_eq!(first.summary().account.as_deref(), Some("10001"));
    assert_eq!(first.summary().accounts, ["10001"]);
    let id = first.summary().id;
    drop(first);
    // A snapshot written before account fields existed restores them from status metadata.
    let (mut snapshot, entries) = index.load(&id).unwrap().unwrap();
    snapshot.summary.account = None;
    snapshot.summary.accounts.clear();
    index.save(&id, 0, &entries, snapshot).unwrap();
    bytes.extend(account(5, "10002"));
    fs::write(&source, &bytes).unwrap();
    let mut restored = capture::Capture::open(path).unwrap();
    restored.attach_index(Some(index));
    restored.refresh();
    let summary = restored.summary();
    assert_eq!(summary.account.as_deref(), Some("10002"));
    assert_eq!(summary.accounts, ["10001", "10002"]);
    assert_eq!(summary.records, "5");
}

#[test]
fn unindexed_history_gets_a_bounded_account_look_ahead_once() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("old.qscap");
    fs::write(
        &source,
        launch(
            200,
            134383000000000000,
            &[record(1, 1, 2, "A", &[8, 1], 0), account(2, "10001")],
        ),
    )
    .unwrap();
    let mut capture = capture::Capture::open(source.canonicalize().unwrap()).unwrap();
    assert!(capture.summary().account.is_none());
    assert!(capture.scan_account());
    assert_eq!(capture.summary().account.as_deref(), Some("10001"));
    assert_eq!(capture.summary().records, "0");
    assert!(!capture.scan_account());
    let none = directory.path().join("none.qscap");
    fs::write(
        &none,
        launch(201, 134383000000000000, &[record(1, 1, 2, "A", &[8, 1], 0)]),
    )
    .unwrap();
    let mut capture = capture::Capture::open(none.canonicalize().unwrap()).unwrap();
    assert!(capture.scan_account());
    assert!(capture.summary().account.is_none());
}

#[test]
fn recording_window_inherits_the_account_observed_before_it_started() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("live.qscap");
    let mut original = launch(
        123,
        134383000000000000,
        &[account(1, "10001"), record(2, 1, 2, "Before", &[8, 1], 0)],
    );
    fs::write(&source, &original).unwrap();
    let mut capture = capture::Capture::open(source.clone()).unwrap();
    capture.refresh();
    let imports = directory.path().join("imports");
    fs::create_dir(&imports).unwrap();
    let mut manager = recording::Manager::default();
    let job = manager.start(&capture.summary(), &imports).unwrap();
    original.extend(record(3, 1, 1, "During", &[8, 2], 0));
    fs::write(&source, &original).unwrap();
    for _ in 0..200 {
        if manager.list()[0].packets == "1" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    manager.stop(&job.id).unwrap();
    for _ in 0..200 {
        if manager.list()[0].state != "recording" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let saved = &manager.list()[0];
    assert_eq!(saved.state, "stopped");
    let mut recorded = capture::Capture::open(PathBuf::from(&saved.path)).unwrap();
    recorded.refresh();
    let summary = recorded.summary();
    assert_eq!(summary.account.as_deref(), Some("10001"));
    assert_eq!(summary.packets, "1");
    assert!(summary.clean_end);
}

#[tokio::test]
async fn account_view_concatenates_launches_with_original_identities_and_cursors() {
    let (dir, mut state, _) = setup();
    // Production canonicalizes the imports root before serving.
    state.imports = state.imports.canonicalize().unwrap();
    let base = 134383000000000000u64;
    let packets = |first: u64, count: u64, prefix: &str| -> Vec<Vec<u8>> {
        (first..first + count)
            .map(|id| record(id, 1, 2, &format!("{prefix}.{id}"), &[8, 1], 0))
            .collect()
    };
    let mut first = vec![account(1, "10001")];
    first.extend(packets(2, 3, "First"));
    let mut second = vec![account(1, "10001")];
    second.extend(packets(2, 2, "Second"));
    let mut other = vec![account(1, "10002")];
    other.extend(packets(2, 1, "Other"));
    fs::write(
        dir.path().join("first.qscap"),
        launch(301, base + 1_000, &first),
    )
    .unwrap();
    fs::write(
        dir.path().join("second.qscap"),
        launch(302, base + 2_000, &second),
    )
    .unwrap();
    fs::write(
        dir.path().join("other.qscap"),
        launch(303, base + 3_000, &other),
    )
    .unwrap();
    // An imported copy with the same account never joins the launch view.
    fs::write(
        state.imports.join("copy.qscap"),
        launch(304, base + 4_000, &first),
    )
    .unwrap();
    state.library.lock().unwrap().discover();
    catalog::scan_accounts_shared(&state.library, 16).unwrap();
    let (_, catalog) = api(state.clone(), "/api/v1/sessions").await;
    let sessions = catalog["sessions"].as_array().unwrap();
    let by = |name: &str| -> String {
        sessions
            .iter()
            .find(|s| s["path"].as_str().unwrap().ends_with(name))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        sessions.iter().filter(|s| s["account"] == "10001").count(),
        3
    );

    let (status, page) = api(state.clone(), "/api/v1/accounts/10001/events?limit=3").await;
    assert_eq!(status, StatusCode::OK);
    let segments: Vec<_> = page["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(segments, [by("first.qscap"), by("second.qscap")]);
    let rows: Vec<_> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["session_id"].as_str().unwrap().to_string(),
                e["event"]["event_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (by("first.qscap"), "4".to_string()),
            (by("second.qscap"), "2".to_string()),
            (by("second.qscap"), "3".to_string())
        ]
    );
    assert_eq!(page["has_earlier"], true);
    assert_eq!(page["cursor"], format!("{}:3", by("second.qscap")));

    let (_, earlier) = api(
        state.clone(),
        &format!(
            "/api/v1/accounts/10001/events?limit=10&before_cursor={}:4",
            by("first.qscap")
        ),
    )
    .await;
    assert_eq!(names(&earlier, "command"), ["First.2", "First.3"]);
    assert_eq!(earlier["has_earlier"], false);

    let (_, after) = api(
        state.clone(),
        &format!(
            "/api/v1/accounts/10001/events?limit=2&after_cursor={}:2",
            by("first.qscap")
        ),
    )
    .await;
    assert_eq!(names(&after, "command"), ["First.3", "First.4"]);
    assert_eq!(after["has_more"], true);
    assert_eq!(after["cursor"], format!("{}:4", by("first.qscap")));

    let (_, filtered) = api(
        state.clone(),
        &format!(
            "/api/v1/accounts/10001/events?q=second&after_cursor={}:0",
            by("first.qscap")
        ),
    )
    .await;
    assert_eq!(names(&filtered, "command"), ["Second.2", "Second.3"]);
    assert_eq!(filtered["has_more"], false);
    assert_eq!(filtered["cursor"], format!("{}:3", by("second.qscap")));
    let (_, controls) = api(
        state.clone(),
        "/api/v1/accounts/10001/events?controls=true&kinds=status",
    )
    .await;
    assert_eq!(controls["events"].as_array().unwrap().len(), 2);

    for bad in [
        "/api/v1/accounts/bad!/events",
        "/api/v1/accounts/10001/events?after_cursor=x",
        "/api/v1/accounts/10001/events?after_cursor=missing:1",
        "/api/v1/accounts/10001/events?after=1",
    ] {
        assert_eq!(
            api(state.clone(), bad).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        api(state, "/api/v1/accounts/99999/events").await.0,
        StatusCode::NOT_FOUND
    );
}

#[test]
fn command_suggestions_count_packets_by_frequency_and_skip_control_records() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("commands.qscap");
    fs::write(
        &source,
        launch(
            210,
            134383000000000000,
            &[
                record(1, 1, 1, "B", &[8, 1], 0),
                record(2, 1, 2, "A", &[8, 1], 0),
                record(3, 1, 1, "A", &[8, 1], 0),
                account(4, "10001"),
            ],
        ),
    )
    .unwrap();
    let mut capture = capture::Capture::open(source.canonicalize().unwrap()).unwrap();
    let list = capture.commands();
    let rows: Vec<_> = list
        .commands
        .iter()
        .map(|c| {
            (
                c.command.as_str(),
                c.count.as_str(),
                c.inbound.as_str(),
                c.outbound.as_str(),
            )
        })
        .collect();
    assert_eq!(rows, [("A", "2", "1", "1"), ("B", "1", "1", "0")]);
    assert!(!list.truncated);
}

#[test]
fn field_pages_continue_inside_nested_messages_and_stop_at_the_view_cap() {
    let (dir, _state, _) = setup();
    let path = dir.path().join("pages.qscap");
    // 1500 top-level varints, then one message candidate holding 600 more fields.
    let inner = [8u8, 0x80, 1].repeat(600);
    let mut payload = [8u8, 1].repeat(1500);
    payload.extend([0x12, 0x88, 0x0e]);
    payload.extend(&inner);
    let mut bytes = header();
    bytes.extend(record(1, 1, 1, "Nested.Pages", &payload, 0));
    fs::write(&path, bytes).unwrap();
    let mut capture = capture::Capture::open(path).unwrap();
    capture.refresh();
    let query = |node_offset| decode::DecodeQuery {
        node_offset,
        ..Default::default()
    };
    let first = decode::detail(capture.event(1).unwrap(), &query(None)).unwrap();
    assert_eq!(first.next_node_offset, Some(2000));
    assert_eq!(first.nodes.len(), 1501);
    assert_eq!(first.nodes[1500].children[0].children.len(), 498);
    let second = decode::detail(capture.event(1).unwrap(), &query(Some(2000))).unwrap();
    assert_eq!(second.next_node_offset, None);
    assert!(!second.view_limited);
    // Only the parent field and its candidate return as shells around the remaining fields.
    assert_eq!(second.nodes.len(), 1);
    assert_eq!(second.nodes[0].id, "pb.1500");
    let candidate = &second.nodes[0].children[0];
    assert_eq!(candidate.id, "pb.1500.candidate");
    assert_eq!(candidate.children.len(), 102);
    assert_eq!(candidate.children[0].id, "pb.1500.candidate.498");
    let error = decode::detail(capture.event(1).unwrap(), &query(Some(20_000))).unwrap_err();
    assert_eq!(error.code, "range");
}

async fn post(
    state: AppState,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let request = HttpRequest::builder()
        .method("POST")
        .uri(uri)
        .header("host", state.host.clone())
        .header("x-qscope-client", "web-v1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn runtime_health_names_what_is_missing_and_how_each_part_can_be_fixed() {
    let (dir, state, _) = setup();
    let parts = |health: &serde_json::Value| -> Vec<(String, String, String)> {
        health["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["id"].as_str().unwrap().into(),
                    item["state"].as_str().unwrap().into(),
                    item["repair"].as_str().unwrap().into(),
                )
            })
            .collect()
    };
    // A development layout without a build or a QQ download.
    let (status, health) = api(state.clone(), "/api/v1/settings/qq/health").await;
    assert_eq!(status, StatusCode::OK);
    let found = parts(&health);
    assert_eq!(found[1], ("host".into(), "missing".into(), "build".into()));
    assert_eq!(found[2], ("probe".into(), "missing".into(), "build".into()));
    assert_eq!(found[3], ("qq".into(), "pending".into(), "none".into()));
    assert_eq!(health["healthy"], false);
    assert_eq!(health["repairable"], false);
    // Unpacked QQ files that differ from the pinned modules can be unpacked again.
    let files = dir
        .path()
        .join("artifacts/qq/9.9.37-261103-x64/unpacked/Files");
    fs::create_dir_all(&files).unwrap();
    fs::write(files.join("QQ.exe"), b"changed").unwrap();
    let (_, health) = api(state.clone(), "/api/v1/settings/qq/health").await;
    assert_eq!(
        parts(&health)[3],
        ("qq".into(), "damaged".into(), "local".into())
    );
    assert_eq!(health["repairable"], true);
}

#[tokio::test]
async fn update_settings_keep_only_plain_http_proxies_and_unsigned_builds_do_not_check() {
    let (dir, state, _) = setup();
    let (status, update) = api(state.clone(), "/api/v1/update").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(update["current"], env!("CARGO_PKG_VERSION"));
    assert!(update["unsupported"].is_string());
    assert_eq!(update["check_on_launch"], true);
    let (status, error) = post(
        state.clone(),
        "/api/v1/update/settings",
        serde_json::json!({"check_on_launch":false,"proxy":"socks5://127.0.0.1:1080"}),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(error["code"], "update_proxy");
    let (status, update) = post(
        state.clone(),
        "/api/v1/update/settings",
        serde_json::json!({"check_on_launch":false,"proxy":"http://127.0.0.1:7890/"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(update["proxy"], "http://127.0.0.1:7890");
    assert_eq!(update["check_on_launch"], false);
    assert!(dir.path().join("artifacts/settings/update.json").is_file());
    // A launch check stays quiet; an explicit one says why this build cannot update.
    let (status, update) = post(
        state.clone(),
        "/api/v1/update/check",
        serde_json::json!({"auto":true}),
    )
    .await;
    assert_eq!(
        (status, update["stage"].as_str()),
        (StatusCode::OK, Some("idle"))
    );
    let (_, error) = post(state.clone(), "/api/v1/update/check", serde_json::json!({})).await;
    assert_eq!(error["code"], "update_unsupported");
    let (_, error) = post(state, "/api/v1/update/install", serde_json::json!({})).await;
    assert_eq!(error["code"], "update_unsupported");
}
