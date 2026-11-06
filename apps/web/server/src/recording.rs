//! Record a time window from an existing append-only capture. Never controls the QQ probe.
use crate::contract::{ApiError, Recording, Session};
use qscope_core::{
    data::{U64, sha256},
    native,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Job {
    status: Arc<Mutex<Recording>>,
    stop: Arc<AtomicBool>,
}
#[derive(Default)]
pub struct Manager {
    jobs: Vec<Job>,
}
impl Drop for Manager {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.stop.store(true, Ordering::Release);
        }
    }
}
fn io(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("recording_io", e.to_string())
}
fn now() -> u64 {
    116444736000000000
        + SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
            / 100
}

pub fn encode_event(event: &native::Event, id: u64, dropped: u64) -> Vec<u8> {
    let command = &event.command_hex;
    let command: Vec<u8> = (0..command.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&command[i..i + 2], 16).expect("native command hex"))
        .collect();
    let mut bytes = vec![0; native::RECORD_BYTES];
    bytes[..4].copy_from_slice(b"QSR1");
    bytes[4..8].copy_from_slice(&((80 + command.len() + event.payload.len()) as u32).to_le_bytes());
    let kind = match event.kind {
        "packet" => 1u16,
        "status" => 2,
        "loss" => 3,
        _ => 4,
    };
    bytes[8..10].copy_from_slice(&kind.to_le_bytes());
    bytes[10] = match event.direction {
        "inbound" => 1,
        "outbound" => 2,
        _ => 0,
    };
    bytes[11] = u8::from(event.truncated);
    for (at, value) in [
        (12, event.pid),
        (16, event.tid),
        (20, event.error_code),
        (64, event.payload.len() as u32),
        (68, command.len() as u32),
    ] {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (at, value) in [
        (24, id),
        (32, event.timestamp_filetime.0),
        (40, event.qpc.0),
        (48, event.protocol_sequence.0),
        (56, event.original_length.0),
        (72, dropped),
    ] {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes.extend(command);
    bytes.extend(&event.payload);
    bytes
}
fn marker(pid: u32, kind: &'static str, command: &str) -> native::Event {
    native::Event {
        event_id: U64(0),
        kind,
        direction: "unknown",
        pid,
        tid: 0,
        error_code: 0,
        timestamp_filetime: U64(now()),
        qpc: U64(0),
        protocol_sequence: U64(0),
        original_length: U64(0),
        truncated: false,
        dropped_total: U64(0),
        command: Some(command.into()),
        command_hex: qscope_core::data::to_hex(command.as_bytes()),
        payload_hex: String::new(),
        payload: vec![],
    }
}

impl Manager {
    pub fn stop_all(&self) {
        for job in &self.jobs {
            job.stop.store(true, Ordering::Release);
        }
    }
    pub fn list(&self) -> Vec<Recording> {
        self.jobs
            .iter()
            .filter_map(|j| j.status.lock().ok().map(|s| s.clone()))
            .collect()
    }
    pub fn stop(&self, id: &str) -> Result<(), ApiError> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.status.lock().is_ok_and(|s| s.id == id))
            .ok_or_else(|| ApiError::new("not_found", "录制不存在"))?;
        job.stop.store(true, Ordering::Release);
        Ok(())
    }
    pub fn start(&mut self, session: &Session, imports: &Path) -> Result<Recording, ApiError> {
        if session.clean_end
            || session.error.is_some()
            || session.source_kind == "imported"
            || session.source_kind == "recording"
        {
            return Err(ApiError::new("query", "请选择仍在追加的原始采集记录"));
        }
        if self
            .list()
            .iter()
            .any(|r| r.state == "recording" && r.source_session == session.id)
        {
            return Err(ApiError::new("query", "该会话已在录制"));
        }
        if self
            .list()
            .iter()
            .filter(|r| r.state == "recording")
            .count()
            >= 4
        {
            return Err(ApiError::new("query", "最多同时录制 4 段"));
        }
        // Establish the last complete source boundary. Do not start in the middle of a record.
        let mut reader =
            native::Reader::new(BufReader::new(File::open(&session.path).map_err(io)?))
                .map_err(io)?;
        let (mut baseline_drop, mut last_qpc) = (0, 0);
        let mut account = None;
        let scan_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if reader.offset > 512 * 1024 * 1024 || Instant::now() > scan_deadline {
                return Err(ApiError::new("file_limit", "录制起点扫描超过限额"));
            }
            match reader.next_event() {
                Ok(Some(e)) => {
                    baseline_drop = e.dropped_total.0;
                    last_qpc = e.qpc.0;
                    if e.kind == "status"
                        && let Some(observed) = e
                            .command
                            .as_deref()
                            .and_then(native::AccountObservation::parse)
                    {
                        account = Some(observed);
                    }
                }
                Ok(None) => break,
                Err(e) if e.code == "native_partial_tail" => {
                    reader.retry_partial_tail().map_err(io)?;
                    break;
                }
                Err(e) => return Err(io(e)),
            }
        }
        if reader.ended {
            return Err(ApiError::new("query", "原始采集已经结束"));
        }
        let start_offset = reader.offset;
        let started = now();
        let id = sha256(format!("{}:{started}:{}", session.id, self.jobs.len()).as_bytes())[..24]
            .to_string();
        let directory = imports.join("recordings").join(&id);
        fs::create_dir_all(&directory).map_err(io)?;
        let path = directory.join("capture.qscap");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io)?;
        let mut header = [0; 64];
        let mut source = File::open(&session.path).map_err(io)?;
        source.read_exact(&mut header).map_err(io)?;
        header[32..40].copy_from_slice(&started.to_le_bytes());
        header[40..48].copy_from_slice(&last_qpc.to_le_bytes());
        output.write_all(&header).map_err(io)?;
        let mut first = marker(reader.header.pid, "status", "recording_started");
        first.qpc = U64(last_qpc);
        output.write_all(&encode_event(&first, 1, 0)).map_err(io)?;
        // A window keeps the account its source observed before the window began.
        let mut first_id = 1;
        if let Some(observed) = &account {
            let mut status = marker(reader.header.pid, "status", &observed.status_text());
            status.qpc = U64(last_qpc);
            first_id = 2;
            output
                .write_all(&encode_event(&status, first_id, 0))
                .map_err(io)?;
        }
        output.flush().map_err(io)?;
        let status = Recording {
            id,
            source_session: session.id.clone(),
            state: "recording".into(),
            path: path.to_string_lossy().into_owned(),
            started_filetime: started.to_string(),
            packets: "0".into(),
            bytes: output.stream_position().map_err(io)?.to_string(),
            source_first_event: None,
            source_last_event: None,
            error: None,
        };
        fs::write(directory.join("manifest.json"), serde_json::to_vec_pretty(&serde_json::json!({"schema_version":1,"source_type":"recorded_window","recording":status,"source_path":session.path,"source_start_offset":start_offset,"source_dropped_before_window":baseline_drop,"raw_input_modified":false})).map_err(io)?).map_err(io)?;
        let shared = Arc::new(Mutex::new(status.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        if self.jobs.len() >= 32 {
            self.jobs
                .retain(|j| j.status.lock().is_ok_and(|s| s.state == "recording"));
        }
        let worker_status = shared.clone();
        let worker_stop = stop.clone();
        let source_path = PathBuf::from(&session.path);
        std::thread::Builder::new().name("qscope-recording".into()).spawn(move || {
            let result = record_loop(&mut reader, &mut output, &worker_status, &worker_stop, (baseline_drop,last_qpc,first_id), &source_path);
            if let Ok(mut current) = worker_status.lock() {
                if let Err(error) = result { current.error = Some(error.message); current.state = "error".into(); }
                else { current.state = "stopped".into(); }
                let manifest = serde_json::json!({"schema_version":1,"source_type":"recorded_window","recording":*current,"source_path":source_path,"source_start_offset":start_offset,"source_end_offset":reader.offset,"source_dropped_before_window":baseline_drop,"event_ids":"window-local consecutive IDs; first/last source IDs retained","payload":"copied without modification","source_ended":reader.ended,"raw_input_modified":false});
                let _ = fs::write(directory.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap_or_default());
            }
        }).map_err(io)?;
        self.jobs.push(Job {
            status: shared,
            stop,
        });
        Ok(status)
    }
}

fn record_loop(
    reader: &mut native::Reader<BufReader<File>>,
    output: &mut File,
    status: &Mutex<Recording>,
    stop: &AtomicBool,
    baseline: (u64, u64, u64),
    source: &Path,
) -> Result<(), ApiError> {
    let (mut id, mut packets, mut dropped) = (baseline.2, 0u64, 0);
    let mut latest_qpc = baseline.1;
    let deadline = Instant::now() + Duration::from_secs(3600);
    let mut stop_at = None;
    loop {
        // Drain complete data already on disk when Stop is clicked, but never wait for a partial tail.
        let stopping = stop.load(Ordering::Acquire);
        if stopping && stop_at.is_none() {
            stop_at = Some(fs::metadata(source).map_err(io)?.len());
        }
        reader.retry_partial_tail().map_err(io)?;
        let mut progressed = false;
        for _ in 0..128 {
            if stop_at.is_some_and(|at| reader.offset >= at) {
                break;
            }
            match reader.next_event() {
                Ok(Some(event)) => {
                    progressed = true;
                    if event.kind == "end" {
                        break;
                    }
                    id += 1;
                    dropped = event.dropped_total.0.saturating_sub(baseline.0);
                    latest_qpc = event.qpc.0;
                    output
                        .write_all(&encode_event(&event, id, dropped))
                        .map_err(io)?;
                    if event.kind == "packet" {
                        packets += 1;
                    }
                    if let Ok(mut state) = status.lock() {
                        state
                            .source_first_event
                            .get_or_insert_with(|| event.event_id.0.to_string());
                        state.source_last_event = Some(event.event_id.0.to_string());
                    }
                }
                Ok(None) => break,
                Err(e) if e.code == "native_partial_tail" => {
                    reader.retry_partial_tail().map_err(io)?;
                    break;
                }
                Err(e) => return Err(io(e)),
            }
            if output.stream_position().map_err(io)? >= 256 * 1024 * 1024 {
                return Err(ApiError::new(
                    "file_limit",
                    "录制达到 256 MiB 上限，已保留现场",
                ));
            }
        }
        output.flush().map_err(io)?;
        if let Ok(mut state) = status.lock() {
            state.packets = packets.to_string();
            state.bytes = output.stream_position().map_err(io)?.to_string();
        }
        if reader.ended
            || (stopping && (!progressed || stop_at.is_some_and(|at| reader.offset >= at)))
            || Instant::now() >= deadline
        {
            break;
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    id += 1;
    let mut end = marker(reader.header.pid, "end", "recording_stopped");
    end.qpc = U64(latest_qpc);
    output
        .write_all(&encode_event(&end, id, dropped))
        .map_err(io)?;
    output.sync_all().map_err(io)?;
    if let Ok(mut state) = status.lock() {
        state.bytes = output.stream_position().map_err(io)?.to_string();
    }
    Ok(())
}
