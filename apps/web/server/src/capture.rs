use crate::contract::{ApiError, CommandCount, CommandList, EventPage, EventSummary, Session};
use qscope_core::{data::sha256, native};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_INDEX: usize = 250_000;
const INDEX_BATCH: usize = 2048;
// Bounded look-ahead for the account of a file that has not been indexed yet.
const ACCOUNT_SCAN_RECORDS: usize = 20_000;
const ACCOUNT_SCAN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 8;
// Command suggestions list the most frequent names only.
const MAX_COMMANDS: usize = 2_000;

#[derive(Serialize, Deserialize)]
pub(crate) struct Entry {
    pub(crate) checkpoint: native::Checkpoint,
    pub(crate) id: u64,
    pub(crate) meta: EventSummary,
}

pub struct Capture {
    path: PathBuf,
    reader: native::Reader<BufReader<File>>,
    entries: Vec<Entry>,
    summary: Session,
    index: Option<crate::index::Store>,
    cache_checked: bool,
    saved_entries: usize,
    saved_offset: u64,
    prefix: Sha256,
    hashed_bytes: u64,
    account_scanned: bool,
}

pub type ListQuery = crate::filters::Filter;

pub fn summarize(e: &native::Event) -> EventSummary {
    EventSummary {
        event_id: e.event_id.0.to_string(),
        kind: e.kind.into(),
        direction: e.direction.into(),
        command: e
            .command
            .clone()
            .unwrap_or_else(|| format!("hex:{}", e.command_hex)),
        command_hex: e.command_hex.clone(),
        sequence: e.protocol_sequence.0.to_string(),
        timestamp_filetime: e.timestamp_filetime.0.to_string(),
        captured_length: e.payload.len(),
        original_length: e.original_length.0.to_string(),
        truncated: e.truncated,
        dropped_total: e.dropped_total.0.to_string(),
        error_code: e.error_code,
        tid: e.tid,
    }
}

fn native_error(e: native::Error) -> ApiError {
    ApiError {
        code: e.code.into(),
        message: e.message,
        offset: usize::try_from(e.offset).ok(),
    }
}

impl Capture {
    pub fn open(path: PathBuf) -> Result<Self, ApiError> {
        let file = File::open(&path).map_err(|e| ApiError::new("capture_io", e.to_string()))?;
        let length = file
            .metadata()
            .map_err(|e| ApiError::new("capture_io", e.to_string()))?
            .len();
        if length > MAX_FILE_BYTES {
            return Err(ApiError::new("file_limit", "采集文件超过 512 MiB 查看上限"));
        }
        let reader = native::Reader::new(BufReader::new(file)).map_err(native_error)?;
        let id = sha256(path.to_string_lossy().as_bytes())[..24].to_string();
        let summary = Session {
            id,
            name: path
                .parent()
                .and_then(Path::file_name)
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            pid: reader.header.pid,
            process_birth_filetime: reader.header.process_birth_filetime.0.to_string(),
            source_kind: "capture".into(),
            producer_alive: None,
            started_filetime: reader.header.started_filetime.0.to_string(),
            records: "0".into(),
            packets: "0".into(),
            inbound: "0".into(),
            outbound: "0".into(),
            dropped_total: "0".into(),
            truncated_packets: "0".into(),
            file_bytes: length.to_string(),
            complete_bytes: reader.offset.to_string(),
            clean_end: false,
            partial_tail: false,
            has_more: length > reader.offset,
            error: None,
            account: None,
            accounts: vec![],
        };
        Ok(Self {
            path,
            reader,
            entries: vec![],
            summary,
            index: None,
            cache_checked: false,
            saved_entries: 0,
            saved_offset: 0,
            prefix: Sha256::new(),
            hashed_bytes: 0,
            account_scanned: false,
        })
    }

    pub fn attach_index(&mut self, index: Option<crate::index::Store>) {
        self.index = index;
    }

    fn prefix_hash(&mut self, end: u64) -> Result<String, ApiError> {
        let result = (|| -> Result<String, std::io::Error> {
            let mut file = File::open(&self.path)?;
            file.seek(SeekFrom::Start(self.hashed_bytes))?;
            let mut bytes = [0u8; 65536];
            while self.hashed_bytes < end {
                let length = usize::try_from((end - self.hashed_bytes).min(bytes.len() as u64))
                    .expect("Bounded hash read");
                file.read_exact(&mut bytes[..length])?;
                self.prefix.update(&bytes[..length]);
                self.hashed_bytes += length as u64;
            }
            Ok(format!("{:x}", self.prefix.clone().finalize()))
        })();
        result.map_err(|error| ApiError::new("capture_io", error.to_string()))
    }

    fn restore_index(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(index) = &self.index else {
            return Ok(());
        };
        let Some((snapshot, entries)) = index.load(&self.summary.id)? else {
            return Ok(());
        };
        let end: u64 = snapshot.summary.complete_bytes.parse()?;
        if end > fs::metadata(&self.path)?.len()
            || end > MAX_FILE_BYTES
            || snapshot.summary.pid != self.summary.pid
            || snapshot.summary.process_birth_filetime != self.summary.process_birth_filetime
            || snapshot.summary.started_filetime != self.summary.started_filetime
            || snapshot.summary.path != self.summary.path
        {
            return Err("Capture identity changed".into());
        }
        if self.prefix_hash(end).map_err(|error| error.message)? != snapshot.prefix_sha256 {
            return Err("Capture prefix changed".into());
        }
        let original = self.reader.checkpoint();
        self.reader.resume_index(snapshot.checkpoint)?;
        if self.reader.offset != end {
            self.reader.resume_index(original)?;
            return Err("Index cursor mismatch".into());
        }
        self.entries = entries;
        self.saved_entries = self.entries.len();
        self.saved_offset = end;
        let scanned = (
            self.summary.account.take(),
            std::mem::take(&mut self.summary.accounts),
        );
        self.summary = snapshot.summary;
        // Older snapshots have no account fields; the restored status metadata does.
        self.summary.account = None;
        self.summary.accounts.clear();
        for entry in &self.entries {
            observe_account(&mut self.summary, &entry.meta);
        }
        if self.summary.account.is_none() {
            (self.summary.account, self.summary.accounts) = scanned;
        }
        Ok(())
    }

    fn save_index(&mut self) {
        let Some(index) = self.index.clone() else {
            return;
        };
        if self.summary.error.is_some() || self.saved_offset == self.reader.offset {
            return;
        }
        let result = self
            .prefix_hash(self.reader.offset)
            .map_err(|error| error.message)
            .and_then(|prefix_sha256| {
                index
                    .save(
                        &self.summary.id,
                        self.saved_entries,
                        &self.entries,
                        crate::index::Snapshot {
                            checkpoint: self.reader.checkpoint(),
                            summary: self.summary(),
                            prefix_sha256,
                        },
                    )
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                self.saved_entries = self.entries.len();
                self.saved_offset = self.reader.offset;
            }
            Err(error) => {
                eprintln!("Derived capture index disabled: {error}");
                self.index = None;
            }
        }
    }

    pub fn summary(&self) -> Session {
        self.summary.clone()
    }

    pub fn refresh(&mut self) {
        if !self.cache_checked {
            self.cache_checked = true;
            if let Err(error) = self.restore_index() {
                eprintln!("Rebuilding derived capture index: {error}");
                self.prefix = Sha256::new();
                self.hashed_bytes = 0;
            }
        }
        if self.summary.error.is_some() {
            return;
        }
        let length = match fs::metadata(&self.path) {
            Ok(meta) => meta.len(),
            Err(e) => {
                self.summary.error = Some(ApiError::new("capture_io", e.to_string()));
                return;
            }
        };
        self.summary.file_bytes = length.to_string();
        if length < self.reader.offset || length > MAX_FILE_BYTES {
            self.summary.error = Some(ApiError::new(
                "source_changed_or_limit",
                "文件被截短或已超过 512 MiB 上限",
            ));
            return;
        }
        self.summary.partial_tail = false;
        self.summary.has_more = false;
        if let Err(e) = self.reader.retry_partial_tail() {
            self.summary.error = Some(native_error(e));
            return;
        }
        let start = self.reader.offset;
        for _ in 0..INDEX_BATCH {
            if self.entries.len() >= MAX_INDEX {
                if self.reader.offset < length {
                    self.summary.error = Some(ApiError::new(
                        "index_limit",
                        "已达到 250,000 条记录的索引上限，原始文件保持不变",
                    ));
                }
                break;
            }
            let checkpoint = self.reader.checkpoint();
            match self.reader.next_event() {
                Ok(Some(event)) => {
                    let meta = summarize(&event);
                    if event.kind == "packet" {
                        increment(&mut self.summary.packets);
                        if event.direction == "inbound" {
                            increment(&mut self.summary.inbound);
                        }
                        if event.direction == "outbound" {
                            increment(&mut self.summary.outbound);
                        }
                        if event.truncated {
                            increment(&mut self.summary.truncated_packets);
                        }
                    }
                    self.summary.dropped_total = meta.dropped_total.clone();
                    observe_account(&mut self.summary, &meta);
                    self.entries.push(Entry {
                        checkpoint,
                        id: event.event_id.0,
                        meta,
                    });
                }
                Ok(None) => break,
                Err(e) if e.code == "native_partial_tail" => {
                    self.summary.partial_tail = true;
                    if let Err(e) = self.reader.retry_partial_tail() {
                        self.summary.error = Some(native_error(e));
                    }
                    break;
                }
                Err(e) => {
                    self.summary.error = Some(native_error(e));
                    break;
                }
            }
            if self.reader.offset.saturating_sub(start) >= 8 * 1024 * 1024 {
                break;
            }
        }
        self.summary.records = self.entries.len().to_string();
        self.summary.complete_bytes = self.reader.offset.to_string();
        self.summary.clean_end = self.reader.ended && self.summary.error.is_none();
        self.summary.has_more = self.reader.offset < length
            && !self.summary.partial_tail
            && self.summary.error.is_none();
        self.save_index();
    }

    /// Account of a file whose index has not reached its status records yet.
    /// Returns whether a bounded scan ran.
    pub fn scan_account(&mut self) -> bool {
        if self.account_scanned {
            return false;
        }
        self.account_scanned = true;
        if self.summary.account.is_some() || !self.entries.is_empty() {
            return false;
        }
        let Ok(file) = File::open(&self.path) else {
            return true;
        };
        let Ok(mut reader) = native::Reader::new(BufReader::new(file)) else {
            return true;
        };
        for _ in 0..ACCOUNT_SCAN_RECORDS {
            if reader.offset > ACCOUNT_SCAN_BYTES || self.summary.account.is_some() {
                break;
            }
            match reader.next_event() {
                Ok(Some(event)) => observe_account(&mut self.summary, &summarize(&event)),
                _ => break,
            }
        }
        true
    }

    /// Distinct packet commands in the current index, most frequent first.
    pub fn commands(&mut self) -> CommandList {
        self.refresh();
        let mut counts: HashMap<&str, [u64; 3]> = HashMap::new();
        for meta in self
            .entries
            .iter()
            .map(|e| &e.meta)
            .filter(|m| m.kind == "packet")
        {
            let count = counts.entry(meta.command.as_str()).or_default();
            count[0] += 1;
            match meta.direction.as_str() {
                "inbound" => count[1] += 1,
                "outbound" => count[2] += 1,
                _ => {}
            }
        }
        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1[0].cmp(&a.1[0]).then_with(|| a.0.cmp(b.0)));
        let truncated = sorted.len() > MAX_COMMANDS;
        let commands = sorted
            .into_iter()
            .take(MAX_COMMANDS)
            .map(|(command, [count, inbound, outbound])| CommandCount {
                command: command.into(),
                count: count.to_string(),
                inbound: inbound.to_string(),
                outbound: outbound.to_string(),
            })
            .collect();
        CommandList {
            commands,
            truncated,
        }
    }

    pub fn last_id(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.id)
    }

    /// Matching rows strictly after/before an event ID from the current index.
    /// Returns the rows and whether more matches lie beyond `limit`.
    pub fn select(
        &self,
        query: &ListQuery,
        after: Option<u64>,
        before: Option<u64>,
        limit: usize,
        newest: bool,
    ) -> Result<(Vec<EventSummary>, bool), ApiError> {
        let matcher = Matcher::new(query)?;
        let eligible = self.entries.iter().filter(|e| {
            after.is_none_or(|a| e.id > a)
                && before.is_none_or(|b| e.id < b)
                && matcher.accepts(query, &e.meta)
        });
        if newest {
            let all: Vec<_> = eligible.collect();
            let rows = all[all.len().saturating_sub(limit)..]
                .iter()
                .map(|e| e.meta.clone())
                .collect();
            Ok((rows, all.len() > limit))
        } else {
            let mut rows: Vec<_> = eligible.take(limit + 1).map(|e| e.meta.clone()).collect();
            let more = rows.len() > limit;
            rows.truncate(limit);
            Ok((rows, more))
        }
    }

    pub fn list(&mut self, query: &ListQuery) -> Result<EventPage, ApiError> {
        let matcher = Matcher::new(query)?;
        if query.after.is_some() && query.before.is_some() {
            return Err(ApiError::new("query", "after 和 before 不能同时使用"));
        }
        let limit = query.limit.unwrap_or(500).clamp(1, 1000);
        self.refresh();
        let candidates: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| matcher.accepts(query, &entry.meta))
            .collect();
        let events: Vec<EventSummary>;
        let mut cursor = self.entries.last().map_or(0, |e| e.id);
        let mut has_more = false;
        if let Some(after) = query.after {
            let eligible: Vec<_> = candidates.iter().filter(|e| e.id > after).collect();
            has_more = eligible.len() > limit;
            events = eligible
                .into_iter()
                .take(limit)
                .map(|e| e.meta.clone())
                .collect();
            if has_more {
                cursor = events
                    .last()
                    .map_or(after, |e| e.event_id.parse().expect("native id"));
            }
        } else {
            let eligible: Vec<_> = candidates
                .iter()
                .filter(|e| query.before.is_none_or(|before| e.id < before))
                .collect();
            events = eligible
                .iter()
                .skip(eligible.len().saturating_sub(limit))
                .map(|e| e.meta.clone())
                .collect();
        }
        let has_earlier = events.first().is_some_and(|first| {
            candidates
                .first()
                .is_some_and(|e| e.meta.event_id != first.event_id)
        });
        Ok(EventPage {
            session: self.summary(),
            events,
            cursor: cursor.to_string(),
            has_earlier,
            has_more,
        })
    }

    pub fn event(&mut self, id: u64) -> Result<native::Event, ApiError> {
        self.refresh();
        let index = self
            .entries
            .binary_search_by_key(&id, |e| e.id)
            .map_err(|_| ApiError::new("not_found", "记录不存在或尚未完成索引"))?;
        self.reader
            .event_at(self.entries[index].checkpoint)
            .map_err(native_error)?
            .filter(|e| e.event_id.0 == id)
            .ok_or_else(|| ApiError::new("source_changed", "已索引记录无法读取"))
    }
}

struct Matcher {
    search: String,
    direction: String,
}

impl Matcher {
    fn new(query: &ListQuery) -> Result<Self, ApiError> {
        query.validate()?;
        if query.q.as_ref().is_some_and(|q| q.len() > 256) {
            return Err(ApiError::new("query", "搜索词最多 256 字节"));
        }
        let direction = query.direction.as_deref().unwrap_or("all");
        if !["all", "inbound", "outbound"].contains(&direction) {
            return Err(ApiError::new("query", "未知方向"));
        }
        Ok(Self {
            search: query.q.as_deref().unwrap_or("").to_lowercase(),
            direction: direction.into(),
        })
    }

    fn accepts(&self, query: &ListQuery, e: &EventSummary) -> bool {
        (query.controls.unwrap_or(false) || e.kind == "packet")
            && query.accepts(e)
            && (self.direction == "all" || e.direction == self.direction)
            && (e.command.to_lowercase().contains(&self.search)
                || e.sequence.contains(&self.search)
                || e.event_id == self.search)
    }
}

fn observe_account(summary: &mut Session, meta: &EventSummary) {
    if meta.kind != "status" {
        return;
    }
    let Some(observed) = native::AccountObservation::parse(&meta.command) else {
        return;
    };
    if !summary.accounts.contains(&observed.uin) && summary.accounts.len() < MAX_ACCOUNTS {
        summary.accounts.push(observed.uin.clone());
    }
    summary.account = Some(observed.uin);
}

fn increment(text: &mut String) {
    *text = (text.parse::<u64>().expect("counter") + 1).to_string();
}
