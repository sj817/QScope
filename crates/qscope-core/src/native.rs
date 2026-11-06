//! Append-only native probe transport. A missing footer or partial tail is
//! reported explicitly; complete records remain available after a target crash.
use crate::data::{U64, to_hex};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, SeekFrom};

pub const HEADER_BYTES: usize = 64;
pub const RECORD_BYTES: usize = 80;
pub const MAX_PAYLOAD: usize = 1024 * 1024;
pub const MAX_COMMAND: usize = 512;

#[derive(Debug, Clone, Serialize)]
pub struct Header {
    pub version: u32,
    pub pid: u32,
    pub process_birth_filetime: U64,
    pub started_filetime: U64,
    pub started_qpc: U64,
    pub qpc_frequency: U64,
    pub payload_limit: u32,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub event_id: U64,
    pub kind: &'static str,
    pub direction: &'static str,
    pub pid: u32,
    pub tid: u32,
    pub error_code: u32,
    pub timestamp_filetime: U64,
    pub qpc: U64,
    pub protocol_sequence: U64,
    pub original_length: U64,
    pub truncated: bool,
    pub dropped_total: U64,
    pub command: Option<String>,
    pub command_hex: String,
    pub payload_hex: String,
    #[serde(skip)]
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub offset: u64,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at native byte {}: {}",
            self.code, self.offset, self.message
        )
    }
}
impl std::error::Error for Error {}

pub struct Reader<R> {
    input: R,
    pub header: Header,
    pub offset: u64,
    pub ended: bool,
    last_id: u64,
    last_dropped: u64,
}

/// An opaque, validated record boundary for random access on the same input.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Checkpoint {
    offset: u64,
    ended: bool,
    last_id: u64,
    last_dropped: u64,
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().expect("fixed header"))
}
fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("fixed header"))
}
fn error(code: &'static str, offset: u64, message: impl Into<String>) -> Error {
    Error {
        code,
        offset,
        message: message.into(),
    }
}
fn exact(input: &mut impl Read, data: &mut [u8], offset: u64) -> Result<(), Error> {
    input.read_exact(data).map_err(|e| {
        error(
            if e.kind() == io::ErrorKind::UnexpectedEof {
                "native_partial_tail"
            } else {
                "io"
            },
            offset,
            e.to_string(),
        )
    })
}

impl<R: Read> Reader<R> {
    pub fn new(mut input: R) -> Result<Self, Error> {
        let mut bytes = [0; HEADER_BYTES];
        exact(&mut input, &mut bytes, 0)?;
        if &bytes[..8] != b"QSCAP01\0"
            || u32_at(&bytes, 8) != 1
            || u32_at(&bytes, 12) != HEADER_BYTES as u32
        {
            return Err(error(
                "native_header",
                0,
                "unsupported native transport header",
            ));
        }
        if u32_at(&bytes, 20) != 0 || u32_at(&bytes, 60) != 0 {
            return Err(error(
                "native_header",
                20,
                "unsupported native header flags",
            ));
        }
        let header = Header {
            version: 1,
            pid: u32_at(&bytes, 16),
            process_birth_filetime: U64(u64_at(&bytes, 24)),
            started_filetime: U64(u64_at(&bytes, 32)),
            started_qpc: U64(u64_at(&bytes, 40)),
            qpc_frequency: U64(u64_at(&bytes, 48)),
            payload_limit: u32_at(&bytes, 56),
        };
        if header.pid == 0
            || header.qpc_frequency.0 == 0
            || header.payload_limit == 0
            || header.payload_limit as usize > MAX_PAYLOAD
        {
            return Err(error(
                "native_header",
                16,
                "invalid native identity or capture limit",
            ));
        }
        Ok(Self {
            input,
            header,
            offset: HEADER_BYTES as u64,
            ended: false,
            last_id: 0,
            last_dropped: 0,
        })
    }

    pub fn next_event(&mut self) -> Result<Option<Event>, Error> {
        let mut bytes = [0; RECORD_BYTES];
        // EOF between records is distinct from a record interrupted by a crash.
        loop {
            match self.input.read(&mut bytes[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(error("io", self.offset, e.to_string())),
            }
        }
        if self.ended {
            return Err(error(
                "native_after_end",
                self.offset,
                "record after native footer",
            ));
        }
        exact(&mut self.input, &mut bytes[1..], self.offset)?;
        if &bytes[..4] != b"QSR1" {
            return Err(error("native_record", self.offset, "invalid record magic"));
        }
        let kind = match u16::from_le_bytes([bytes[8], bytes[9]]) {
            1 => "packet",
            2 => "status",
            3 => "loss",
            4 => "end",
            _ => return Err(error("native_record", self.offset, "unknown record kind")),
        };
        let direction = match bytes[10] {
            0 => "unknown",
            1 => "inbound",
            2 => "outbound",
            _ => return Err(error("native_record", self.offset, "unknown direction")),
        };
        let captured = u32_at(&bytes, 64) as usize;
        let command_len = u32_at(&bytes, 68) as usize;
        let original = u64_at(&bytes, 56);
        let total = u32_at(&bytes, 4) as usize;
        let id = u64_at(&bytes, 24);
        let dropped = u64_at(&bytes, 72);
        if captured > self.header.payload_limit as usize
            || command_len > MAX_COMMAND
            || total != RECORD_BYTES + captured + command_len
            || bytes[11] > 1
            || original < captured as u64
            || (original != captured as u64) != (bytes[11] == 1)
            || id != self.last_id + 1
            || dropped < self.last_dropped
            || u32_at(&bytes, 12) != self.header.pid
        {
            return Err(error(
                "native_record",
                self.offset,
                "invalid lengths, flags, identity or ordering",
            ));
        }
        if kind != "packet" && (captured != 0 || original != 0 || direction != "unknown") {
            return Err(error(
                "native_record",
                self.offset,
                "control record contains a packet",
            ));
        }
        let mut command = vec![0; command_len];
        let mut payload = vec![0; captured];
        exact(&mut self.input, &mut command, self.offset)?;
        exact(&mut self.input, &mut payload, self.offset)?;
        self.offset += total as u64;
        self.last_id = id;
        self.last_dropped = dropped;
        self.ended = kind == "end";
        Ok(Some(Event {
            event_id: U64(id),
            kind,
            direction,
            pid: self.header.pid,
            tid: u32_at(&bytes, 16),
            error_code: u32_at(&bytes, 20),
            timestamp_filetime: U64(u64_at(&bytes, 32)),
            qpc: U64(u64_at(&bytes, 40)),
            protocol_sequence: U64(u64_at(&bytes, 48)),
            original_length: U64(original),
            truncated: bytes[11] == 1,
            dropped_total: U64(dropped),
            command: String::from_utf8(command.clone()).ok(),
            command_hex: to_hex(&command),
            payload_hex: to_hex(&payload),
            payload,
        }))
    }
}

impl<R: Read + Seek> Reader<R> {
    /// Resume a derived index after its input prefix hash has been verified.
    pub fn resume_index(&mut self, checkpoint: Checkpoint) -> Result<(), Error> {
        let length = self
            .input
            .seek(SeekFrom::End(0))
            .map_err(|e| error("io", self.offset, e.to_string()))?;
        self.input
            .seek(SeekFrom::Start(self.offset))
            .map_err(|e| error("io", self.offset, e.to_string()))?;
        if checkpoint.offset < HEADER_BYTES as u64 || checkpoint.offset > length {
            return Err(error(
                "native_index",
                checkpoint.offset,
                "Index boundary lies outside its input",
            ));
        }
        self.restore(checkpoint)
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            offset: self.offset,
            ended: self.ended,
            last_id: self.last_id,
            last_dropped: self.last_dropped,
        }
    }

    /// Read a previously indexed record without changing the append cursor.
    pub fn event_at(&mut self, checkpoint: Checkpoint) -> Result<Option<Event>, Error> {
        let saved = self.checkpoint();
        self.restore(checkpoint)?;
        let result = self.next_event();
        self.restore(saved)?;
        result
    }

    fn restore(&mut self, checkpoint: Checkpoint) -> Result<(), Error> {
        self.input
            .seek(SeekFrom::Start(checkpoint.offset))
            .map_err(|e| error("io", checkpoint.offset, e.to_string()))?;
        self.offset = checkpoint.offset;
        self.ended = checkpoint.ended;
        self.last_id = checkpoint.last_id;
        self.last_dropped = checkpoint.last_dropped;
        Ok(())
    }

    /// A live writer may not have finished its latest record yet.
    pub fn retry_partial_tail(&mut self) -> Result<(), Error> {
        self.input
            .seek(SeekFrom::Start(self.offset))
            .map(|_| ())
            .map_err(|e| error("io", self.offset, e.to_string()))
    }
}

/// Status text the probe writes when the account value it reads from the MSF
/// send task changes. The value stays a string; its meaning is a hypothesis
/// bound to the collector profile until validated against a real login.
pub const ACCOUNT_STATUS: &str = "account_observed";
pub const MAX_ACCOUNT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountObservation {
    pub uin: String,
    pub uin_type: u32,
}

impl AccountObservation {
    /// Parse `account_observed type=<u32> uin=<1..=32 of [0-9A-Za-z_-]>`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(' ');
        if parts.next()? != ACCOUNT_STATUS {
            return None;
        }
        let uin_type = parts.next()?.strip_prefix("type=")?;
        let uin = parts.next()?.strip_prefix("uin=")?;
        if parts.next().is_some()
            || uin_type.is_empty()
            || !uin_type.bytes().all(|b| b.is_ascii_digit())
            || uin.is_empty()
            || uin.len() > MAX_ACCOUNT
            || !uin
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return None;
        }
        Some(Self {
            uin: uin.into(),
            uin_type: uin_type.parse().ok()?,
        })
    }

    pub fn status_text(&self) -> String {
        format!("{ACCOUNT_STATUS} type={} uin={}", self.uin_type, self.uin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<u8> {
        let mut b = vec![0; HEADER_BYTES];
        b[..8].copy_from_slice(b"QSCAP01\0");
        for (o, v) in [(8, 1u32), (12, 64), (16, 123), (56, MAX_PAYLOAD as u32)] {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[48..56].copy_from_slice(&10_000_000u64.to_le_bytes());
        let mut r = [0; RECORD_BYTES];
        r[..4].copy_from_slice(b"QSR1");
        r[4..8].copy_from_slice(&85u32.to_le_bytes());
        r[8] = 1;
        r[10] = 2;
        r[12..16].copy_from_slice(&123u32.to_le_bytes());
        r[24..32].copy_from_slice(&1u64.to_le_bytes());
        r[56..64].copy_from_slice(&2u64.to_le_bytes());
        r[64..68].copy_from_slice(&2u32.to_le_bytes());
        r[68..72].copy_from_slice(&3u32.to_le_bytes());
        b.extend(r);
        b.extend(b"cmd\x08\x01");
        b
    }
    #[test]
    fn random_access_restores_the_live_append_cursor() {
        let mut reader = Reader::new(io::Cursor::new(sample())).unwrap();
        let first = reader.checkpoint();
        let event = reader.next_event().unwrap().unwrap();
        let end = reader.offset;
        assert_eq!(
            reader.event_at(first).unwrap().unwrap().payload,
            event.payload
        );
        assert_eq!(reader.offset, end);
        assert!(reader.next_event().unwrap().is_none());
        assert_eq!(
            reader.event_at(first).unwrap().unwrap().event_id,
            event.event_id
        );
        assert_eq!(reader.offset, end);
    }

    #[test]
    fn preserves_non_pb_packets_and_identity() {
        let mut r = Reader::new(io::Cursor::new(sample())).unwrap();
        let e = r.next_event().unwrap().unwrap();
        assert_eq!(e.command.as_deref(), Some("cmd"));
        assert_eq!(e.payload, [8, 1]);
        assert_eq!(e.direction, "outbound");
        assert!(r.next_event().unwrap().is_none());
        assert!(!r.ended);
    }
    #[test]
    fn interrupted_tail_is_explicit_and_retains_last_complete_offset() {
        let mut b = sample();
        b.pop();
        let mut r = Reader::new(io::Cursor::new(b)).unwrap();
        assert_eq!(r.next_event().unwrap_err().code, "native_partial_tail");
        assert_eq!(r.offset, 64);
    }
    #[test]
    fn rejects_unbounded_lengths_before_allocation() {
        let mut b = sample();
        b[128..132].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut r = Reader::new(io::Cursor::new(b)).unwrap();
        assert_eq!(r.next_event().unwrap_err().code, "native_record");
    }
    #[test]
    fn rejects_pid_and_event_id_mismatch() {
        for off in [76, 88] {
            let mut b = sample();
            b[off] = 0;
            let mut r = Reader::new(io::Cursor::new(b)).unwrap();
            assert_eq!(r.next_event().unwrap_err().code, "native_record");
        }
    }
    #[test]
    fn live_partial_record_can_be_retried_without_replaying_prior_records() {
        let mut bytes = sample();
        let tail = bytes.pop().unwrap();
        let mut reader = Reader::new(io::Cursor::new(bytes)).unwrap();
        assert_eq!(reader.next_event().unwrap_err().code, "native_partial_tail");
        reader.input.get_mut().push(tail);
        reader.retry_partial_tail().unwrap();
        assert_eq!(reader.next_event().unwrap().unwrap().payload, [8, 1]);
        assert!(reader.next_event().unwrap().is_none());
    }
    #[test]
    fn non_utf8_commands_remain_available_as_original_bytes() {
        let mut bytes = sample();
        bytes[HEADER_BYTES + RECORD_BYTES] = 0xff;
        let event = Reader::new(io::Cursor::new(bytes))
            .unwrap()
            .next_event()
            .unwrap()
            .unwrap();
        assert!(event.command.is_none());
        assert_eq!(event.command_hex, "ff6d64");
    }
    #[test]
    fn account_status_accepts_only_the_bounded_probe_text() {
        let observed = AccountObservation::parse("account_observed type=1 uin=10001").unwrap();
        assert_eq!(observed.uin, "10001");
        assert_eq!(observed.uin_type, 1);
        assert_eq!(observed.status_text(), "account_observed type=1 uin=10001");
        assert_eq!(
            AccountObservation::parse("account_observed type=0 uin=u_Ab-9")
                .unwrap()
                .uin,
            "u_Ab-9"
        );
        for invalid in [
            "account_observed type=1 uin=",
            "account_observed type= uin=10001",
            "account_observed type=x uin=10001",
            "account_observed type=99999999999 uin=10001",
            "account_observed uin=10001 type=1",
            "account_observed type=1 uin=10001 extra",
            "account_observed type=1 uin=10 001",
            "account_observed type=1 uin=1000\u{1}",
            "account_observed  type=1 uin=10001",
            "hooks_ready",
        ] {
            assert!(AccountObservation::parse(invalid).is_none(), "{invalid}");
        }
        let long = format!(
            "account_observed type=1 uin={}",
            "9".repeat(MAX_ACCOUNT + 1)
        );
        assert!(AccountObservation::parse(&long).is_none());
    }
}
