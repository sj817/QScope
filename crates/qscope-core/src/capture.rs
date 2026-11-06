use crate::data::{U64, sha256};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{self, BufRead, Write},
};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_PACKET_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_line_bytes: usize,
    pub max_file_bytes: usize,
    pub max_records: usize,
    pub max_packet_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_line_bytes: 2 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024,
            max_records: 100_000,
            max_packet_bytes: MAX_PACKET_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Manual,
    Synthetic,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationLayer {
    Wire,
    Plaintext,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Frame,
    Chunk,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    ControllerStopped,
    SourceFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LossReason {
    QueueFull,
    PayloadLimit,
    SourceReported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleIdentity {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIdentity {
    pub platform: String,
    pub architecture: String,
    pub build: String,
    pub modules: Vec<ModuleIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_unix_ns: U64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHeader {
    pub format_version: u32,
    pub session_id: String,
    pub created_unix_ns: U64,
    pub source: SourceKind,
    pub collector: String,
    pub collector_version: String,
    pub capture_point: String,
    pub client: Option<ClientIdentity>,
    pub process: Option<ProcessIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    pub event_id: U64,
    pub timestamp_unix_ns: U64,
    pub monotonic_ns: U64,
    pub direction: Direction,
    pub layer: ObservationLayer,
    pub boundary: Boundary,
    pub command: Option<String>,
    pub protocol_sequence: Option<U64>,
    pub stream_id: Option<String>,
    pub original_length: U64,
    pub captured_length: u32,
    pub truncated: bool,
    pub payload_sha256: String,
    pub payload_base64: String,
}

impl Packet {
    pub fn from_bytes(bytes: &[u8], timestamp_unix_ns: U64) -> Result<Self, CaptureError> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(CaptureError::new("packet_limit", "packet exceeds 1 MiB"));
        }
        Ok(Self {
            event_id: U64(1),
            timestamp_unix_ns,
            monotonic_ns: U64(0),
            direction: Direction::Unknown,
            layer: ObservationLayer::Unknown,
            boundary: Boundary::Unknown,
            command: None,
            protocol_sequence: None,
            stream_id: None,
            original_length: U64(bytes.len() as u64),
            captured_length: bytes.len() as u32,
            truncated: false,
            payload_sha256: sha256(bytes),
            payload_base64: STANDARD.encode(bytes),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loss {
    pub event_id: U64,
    pub timestamp_unix_ns: U64,
    pub monotonic_ns: U64,
    pub dropped_packets: U64,
    pub reason: LossReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnd {
    pub emitted_events: U64,
    pub packets: U64,
    pub dropped_packets: U64,
    pub captured_bytes: U64,
    pub monotonic_ns: U64,
    pub reason: EndReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    Session(SessionHeader),
    Packet(Packet),
    Loss(Loss),
    End(SessionEnd),
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub session_id: Option<String>,
    pub emitted_events: U64,
    pub packets: U64,
    pub dropped_packets: U64,
    pub truncated_packets: U64,
    pub captured_bytes: U64,
    pub stream_closed: bool,
    pub end_reason: Option<EndReason>,
}

#[derive(Debug)]
pub struct CaptureError {
    pub code: &'static str,
    pub line: usize,
    pub message: String,
}

impl CaptureError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            line: 0,
            message: message.into(),
        }
    }
}
impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(formatter, "{}: {}", self.code, self.message)
        } else {
            write!(
                formatter,
                "{} at line {}: {}",
                self.code, self.line, self.message
            )
        }
    }
}
impl std::error::Error for CaptureError {}
impl From<io::Error> for CaptureError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

pub struct Validator {
    limits: Limits,
    summary: Summary,
    records: usize,
    last_monotonic: u64,
}

fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Validator {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            summary: Summary::default(),
            records: 0,
            last_monotonic: 0,
        }
    }
    pub fn summary(&self) -> &Summary {
        &self.summary
    }

    fn event(&mut self, id: U64, monotonic: U64) -> Result<(), CaptureError> {
        if Some(id.0) != self.summary.emitted_events.0.checked_add(1)
            || monotonic.0 < self.last_monotonic
        {
            return Err(CaptureError::new(
                "event_order",
                "event IDs must be consecutive from 1 and monotonic time cannot decrease",
            ));
        }
        self.summary.emitted_events = id;
        self.last_monotonic = monotonic.0;
        Ok(())
    }

    /// Returns unchanged payload bytes only after checking length and SHA256.
    pub fn push(&mut self, record: &Record) -> Result<Option<Vec<u8>>, CaptureError> {
        self.records += 1;
        if self.records > self.limits.max_records {
            return Err(CaptureError::new(
                "record_limit",
                "record count limit exceeded",
            ));
        }
        if self.summary.stream_closed {
            return Err(CaptureError::new(
                "record_order",
                "record after session end",
            ));
        }
        if let Record::Session(header) = record {
            if self.summary.session_id.is_some() {
                return Err(CaptureError::new(
                    "record_order",
                    "duplicate session header",
                ));
            }
            if header.format_version != FORMAT_VERSION {
                return Err(CaptureError::new(
                    "unsupported_version",
                    "unsupported capture format version",
                ));
            }
            if !text(&header.session_id, 128)
                || !text(&header.collector, 128)
                || !text(&header.collector_version, 128)
                || !text(&header.capture_point, 512)
            {
                return Err(CaptureError::new(
                    "identity",
                    "invalid session or collector identity",
                ));
            }
            if header.source == SourceKind::Native
                && (header.client.is_none() || header.process.is_none())
            {
                return Err(CaptureError::new(
                    "identity",
                    "native capture requires client modules and process birth identity",
                ));
            }
            if header
                .process
                .as_ref()
                .is_some_and(|process| process.pid == 0 || process.started_unix_ns.0 == 0)
            {
                return Err(CaptureError::new("identity", "invalid process identity"));
            }
            if let Some(client) = &header.client
                && (!text(&client.platform, 64)
                    || !text(&client.architecture, 64)
                    || !text(&client.build, 128)
                    || client.modules.is_empty()
                    || client.modules.len() > 64
                    || client
                        .modules
                        .iter()
                        .any(|module| !text(&module.path, 1024) || !digest(&module.sha256)))
            {
                return Err(CaptureError::new(
                    "identity",
                    "invalid client or module identity",
                ));
            }
            self.summary.session_id = Some(header.session_id.clone());
            return Ok(None);
        }
        if self.summary.session_id.is_none() {
            return Err(CaptureError::new(
                "record_order",
                "session header must be first",
            ));
        }
        match record {
            Record::Packet(packet) => {
                if packet.captured_length as usize > self.limits.max_packet_bytes
                    || packet.payload_base64.len()
                        > self
                            .limits
                            .max_packet_bytes
                            .saturating_add(2)
                            .saturating_div(3)
                            .saturating_mul(4)
                {
                    return Err(CaptureError::new(
                        "packet_limit",
                        "captured payload limit exceeded",
                    ));
                }
                if packet
                    .command
                    .as_ref()
                    .is_some_and(|value| !text(value, 256))
                    || packet
                        .stream_id
                        .as_ref()
                        .is_some_and(|value| !text(value, 128))
                {
                    return Err(CaptureError::new(
                        "metadata",
                        "invalid command or stream metadata",
                    ));
                }
                let payload = STANDARD.decode(&packet.payload_base64).map_err(|_| {
                    CaptureError::new("payload_encoding", "invalid padded base64 payload")
                })?;
                if payload.len() != packet.captured_length as usize
                    || packet.original_length.0 < u64::from(packet.captured_length)
                    || packet.truncated
                        != (packet.original_length.0 > u64::from(packet.captured_length))
                {
                    return Err(CaptureError::new(
                        "payload_length",
                        "inconsistent captured/original length or truncation flag",
                    ));
                }
                if !digest(&packet.payload_sha256) || sha256(&payload) != packet.payload_sha256 {
                    return Err(CaptureError::new("payload_hash", "payload SHA256 mismatch"));
                }
                self.event(packet.event_id, packet.monotonic_ns)?;
                self.summary.packets.0 += 1;
                self.summary.truncated_packets.0 += u64::from(packet.truncated);
                self.summary.captured_bytes.0 = self
                    .summary
                    .captured_bytes
                    .0
                    .checked_add(payload.len() as u64)
                    .ok_or_else(|| {
                        CaptureError::new("counter_overflow", "captured byte counter overflow")
                    })?;
                Ok(Some(payload))
            }
            Record::Loss(loss) => {
                if loss.dropped_packets.0 == 0 {
                    return Err(CaptureError::new(
                        "loss_count",
                        "loss count must be positive",
                    ));
                }
                self.event(loss.event_id, loss.monotonic_ns)?;
                self.summary.dropped_packets.0 = self
                    .summary
                    .dropped_packets
                    .0
                    .checked_add(loss.dropped_packets.0)
                    .ok_or_else(|| CaptureError::new("counter_overflow", "loss count overflow"))?;
                Ok(None)
            }
            Record::End(end) => {
                if end.emitted_events != self.summary.emitted_events
                    || end.packets != self.summary.packets
                    || end.dropped_packets != self.summary.dropped_packets
                    || end.captured_bytes != self.summary.captured_bytes
                    || end.monotonic_ns.0 < self.last_monotonic
                {
                    return Err(CaptureError::new(
                        "end_counters",
                        "end record disagrees with observed event counters or time",
                    ));
                }
                self.summary.stream_closed = true;
                self.summary.end_reason = Some(end.reason);
                Ok(None)
            }
            Record::Session(_) => unreachable!("session handled before event dispatch"),
        }
    }

    pub fn finish(&self) -> Result<(), CaptureError> {
        if !self.summary.stream_closed {
            return Err(CaptureError::new(
                "incomplete_stream",
                "session end record missing; do not treat an interrupted stream as complete",
            ));
        }
        Ok(())
    }

    pub fn end_record(&self, monotonic_ns: U64, reason: EndReason) -> Record {
        Record::End(SessionEnd {
            emitted_events: self.summary.emitted_events,
            packets: self.summary.packets,
            dropped_packets: self.summary.dropped_packets,
            captured_bytes: self.summary.captured_bytes,
            monotonic_ns,
            reason,
        })
    }
}

pub fn write_record(writer: &mut impl Write, record: &Record) -> Result<(), CaptureError> {
    serde_json::to_writer(&mut *writer, record)
        .map_err(|error| CaptureError::new("json_write", error.to_string()))?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub struct ValidatedRecord {
    pub record: Record,
    pub payload: Option<Vec<u8>>,
}

pub struct Reader<R> {
    input: R,
    validator: Validator,
    line: usize,
    bytes: usize,
    finished: bool,
}

impl<R: BufRead> Reader<R> {
    pub fn new(input: R, limits: Limits) -> Self {
        Self {
            input,
            validator: Validator::new(limits),
            line: 0,
            bytes: 0,
            finished: false,
        }
    }
    pub fn summary(&self) -> &Summary {
        self.validator.summary()
    }

    fn line(&mut self) -> Result<Option<Vec<u8>>, CaptureError> {
        let mut line = Vec::new();
        loop {
            let available = self.input.fill_buf()?;
            if available.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(line))
                };
            }
            let count = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if count
                > self
                    .validator
                    .limits
                    .max_line_bytes
                    .saturating_sub(line.len())
            {
                return Err(CaptureError::new(
                    "line_limit",
                    "JSONL line size limit exceeded",
                ));
            }
            if count
                > self
                    .validator
                    .limits
                    .max_file_bytes
                    .saturating_sub(self.bytes)
            {
                return Err(CaptureError::new(
                    "file_limit",
                    "capture byte limit exceeded",
                ));
            }
            let ended = available[count - 1] == b'\n';
            line.extend_from_slice(&available[..count]);
            self.input.consume(count);
            self.bytes += count;
            if ended {
                return Ok(Some(line));
            }
        }
    }
}

impl<R: BufRead> Iterator for Reader<R> {
    type Item = Result<ValidatedRecord, CaptureError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        self.line += 1;
        let result = match self.line() {
            Ok(Some(line)) => serde_json::from_slice::<Record>(&line)
                .map_err(|error| CaptureError::new("invalid_json", error.to_string()))
                .and_then(|record| {
                    self.validator
                        .push(&record)
                        .map(|payload| ValidatedRecord { record, payload })
                }),
            Ok(None) => {
                self.finished = true;
                match self.validator.finish() {
                    Ok(()) => return None,
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        Some(result.map_err(|mut error| {
            self.finished = true;
            error.line = self.line;
            error
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    pub fn header() -> Record {
        Record::Session(SessionHeader {
            format_version: FORMAT_VERSION,
            session_id: "synthetic-test".into(),
            created_unix_ns: U64(1),
            source: SourceKind::Synthetic,
            collector: "test".into(),
            collector_version: "1".into(),
            capture_point: "generated bytes; no client".into(),
            client: None,
            process: None,
        })
    }

    fn encoded(records: &[Record]) -> Vec<u8> {
        let mut bytes = vec![];
        for record in records {
            write_record(&mut bytes, record).unwrap();
        }
        bytes
    }

    #[test]
    fn unchanged_bytes_survive_round_trip_with_loss_and_truncation() {
        let mut packet = Packet::from_bytes(&[8, 150, 1], U64(3)).unwrap();
        packet.original_length = U64(20);
        packet.truncated = true;
        let records = [
            header(),
            Record::Packet(packet),
            Record::Loss(Loss {
                event_id: U64(2),
                timestamp_unix_ns: U64(4),
                monotonic_ns: U64(1),
                dropped_packets: U64(5),
                reason: LossReason::QueueFull,
            }),
        ];
        let mut validator = Validator::new(Limits::default());
        for record in &records {
            validator.push(record).unwrap();
        }
        let mut bytes = encoded(&records);
        write_record(
            &mut bytes,
            &validator.end_record(U64(2), EndReason::ControllerStopped),
        )
        .unwrap();
        let mut reader = Reader::new(Cursor::new(bytes), Limits::default());
        let values = reader.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(values[1].payload.as_deref(), Some([8, 150, 1].as_slice()));
        assert_eq!(reader.summary().dropped_packets, U64(5));
        assert_eq!(reader.summary().truncated_packets, U64(1));
        assert!(reader.summary().stream_closed);
    }

    #[test]
    fn native_identity_and_unknown_versions_are_not_guessed() {
        let Record::Session(mut data) = header() else {
            panic!()
        };
        data.format_version = 2;
        assert_eq!(
            Validator::new(Limits::default())
                .push(&Record::Session(data.clone()))
                .unwrap_err()
                .code,
            "unsupported_version"
        );
        data.format_version = FORMAT_VERSION;
        data.source = SourceKind::Native;
        assert_eq!(
            Validator::new(Limits::default())
                .push(&Record::Session(data))
                .unwrap_err()
                .code,
            "identity"
        );
    }

    #[test]
    fn tampering_and_event_gaps_are_rejected() {
        let mut validator = Validator::new(Limits::default());
        validator.push(&header()).unwrap();
        let mut packet = Packet::from_bytes(b"original", U64(2)).unwrap();
        packet.payload_base64 = STANDARD.encode(b"changed");
        assert_eq!(
            validator.push(&Record::Packet(packet)).unwrap_err().code,
            "payload_length"
        );
        let mut packet = Packet::from_bytes(b"original", U64(2)).unwrap();
        packet.payload_sha256 = "0".repeat(64);
        assert_eq!(
            validator.push(&Record::Packet(packet)).unwrap_err().code,
            "payload_hash"
        );
        let mut packet = Packet::from_bytes(b"original", U64(2)).unwrap();
        packet.event_id = U64(2);
        assert_eq!(
            validator.push(&Record::Packet(packet)).unwrap_err().code,
            "event_order"
        );
    }

    #[test]
    fn broken_stream_never_becomes_complete_and_lines_are_bounded() {
        let bytes = encoded(&[header()]);
        let mut reader = Reader::new(Cursor::new(bytes.clone()), Limits::default());
        assert!(reader.next().unwrap().is_ok());
        assert_eq!(
            reader.next().unwrap().err().unwrap().code,
            "incomplete_stream"
        );
        assert!(reader.next().is_none());
        let mut reader = Reader::new(
            Cursor::new(bytes),
            Limits {
                max_line_bytes: 10,
                ..Limits::default()
            },
        );
        assert_eq!(reader.next().unwrap().err().unwrap().code, "line_limit");
    }

    #[test]
    fn wrong_footer_or_trailing_records_are_rejected() {
        let mut validator = Validator::new(Limits::default());
        validator.push(&header()).unwrap();
        let end = validator.end_record(U64(0), EndReason::Completed);
        let Record::End(mut wrong) = end.clone() else {
            panic!()
        };
        wrong.packets = U64(1);
        assert_eq!(
            validator.push(&Record::End(wrong)).unwrap_err().code,
            "end_counters"
        );
        validator.push(&end).unwrap();
        assert_eq!(validator.push(&header()).unwrap_err().code, "record_order");
    }
}
