use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const API_VERSION: u32 = 1;
pub use qscope_automation::{
    devtools::{
        BridgeInfo as DevtoolsInfo, ConnectRequest, Identity as DevtoolsIdentity, OpenRequest,
        SavedConnection, Status as DevtoolsStatus, Window as DevtoolsWindow,
    },
    launcher::Specification as DevtoolsLaunch,
    skills::{
        Agent as SkillAgent, IntegrationSettings, Scope as SkillScope, SkillRequest, SkillStatus,
    },
};

#[derive(Debug, Deserialize, Serialize, TS)]
pub struct DevtoolsLaunchRequest {
    pub profile: String,
    pub port: u16,
}

#[derive(Debug, Serialize, TS)]
pub struct DevtoolsFrontend {
    pub opened: bool,
    pub frontend_url: String,
    pub frontend: String,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct QQSetup {
    pub platform: String,
    pub capture_supported: bool,
    pub unavailable_reason: Option<String>,
    pub build: String,
    pub installed: bool,
    pub available: bool,
    pub stage: String,
    pub message: String,
    pub downloaded: String,
    pub total: String,
    pub pid: Option<u32>,
    pub session_id: Option<String>,
    pub error: Option<String>,
}

/// One part the research QQ needs, and what can be done when it is not usable.
#[derive(Debug, Clone, Serialize, TS)]
pub struct RuntimeComponent {
    /// platform, host, probe or qq.
    pub id: String,
    pub label: String,
    /// ok, pending (prepared on first launch), missing, damaged, blocked or unsupported.
    pub state: String,
    pub detail: String,
    /// none, local (fixed in place), reinstall (a new QScope package) or build (a development build step).
    pub repair: String,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct RuntimeHealth {
    pub components: Vec<RuntimeComponent>,
    pub healthy: bool,
    pub repairable: bool,
    /// True when a repair started preparing QQ again; its progress follows the QQ status.
    pub preparing: bool,
}

/// Online update state of this copy of QScope.
#[derive(Debug, Clone, Serialize, TS)]
pub struct UpdateStatus {
    pub current: String,
    /// installer, portable, cli or development.
    pub install: String,
    /// Why this copy cannot update online.
    pub unsupported: Option<String>,
    /// idle, checking, available, latest, downloading, installing, ready, restarting or error.
    pub stage: String,
    pub latest: Option<String>,
    pub notes_url: Option<String>,
    pub downloaded: String,
    pub total: String,
    pub error: Option<String>,
    pub checked_unix: Option<String>,
    pub check_on_launch: bool,
    pub proxy: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
pub struct UpdateSettingsRequest {
    pub check_on_launch: bool,
    /// http://host:port, or empty for a direct connection.
    pub proxy: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub offset: Option<usize>,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            offset: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub path: String,
    pub pid: u32,
    pub process_birth_filetime: String,
    pub source_kind: String,
    pub producer_alive: Option<bool>,
    pub started_filetime: String,
    pub records: String,
    pub packets: String,
    pub inbound: String,
    pub outbound: String,
    pub dropped_total: String,
    pub truncated_packets: String,
    pub file_bytes: String,
    pub complete_bytes: String,
    pub clean_end: bool,
    pub partial_tail: bool,
    pub has_more: bool,
    pub error: Option<ApiError>,
    /// Latest account value read from `account_observed` status records in
    /// this file. Derived and rebuildable; the raw status record is the source.
    #[serde(default)]
    pub account: Option<String>,
    /// Distinct observed values in first-seen order, at most 8.
    #[serde(default)]
    pub accounts: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct Catalog {
    pub api_version: u32,
    pub sessions: Vec<Session>,
    pub discovering: bool,
    pub discovery_limited: bool,
    pub discovery_errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
pub struct EventSummary {
    pub event_id: String,
    pub kind: String,
    pub direction: String,
    pub command: String,
    pub command_hex: String,
    pub sequence: String,
    pub timestamp_filetime: String,
    pub captured_length: usize,
    pub original_length: String,
    pub truncated: bool,
    pub dropped_total: String,
    pub error_code: u32,
    pub tid: u32,
}

#[derive(Debug, Serialize, TS)]
pub struct EventPage {
    pub session: Session,
    pub events: Vec<EventSummary>,
    pub cursor: String,
    pub has_earlier: bool,
    pub has_more: bool,
}

/// Packets with one command in a capture's current index.
#[derive(Debug, Serialize, TS)]
pub struct CommandCount {
    pub command: String,
    pub count: String,
    pub inbound: String,
    pub outbound: String,
}

/// Distinct packet commands, most frequent first, offered by the command filter.
#[derive(Debug, Serialize, TS)]
pub struct CommandList {
    pub commands: Vec<CommandCount>,
    /// More distinct commands exist than were returned.
    pub truncated: bool,
}

/// One event of an account view. Every row keeps its original capture identity.
#[derive(Debug, Serialize, TS)]
pub struct MergedEvent {
    pub session_id: String,
    pub event: EventSummary,
}

/// Launches of one account concatenated in start order. `cursor` is
/// `<session_id>:<event_id>`; `indexing` means an earlier launch is not yet
/// fully indexed and no rows are returned until it is.
#[derive(Debug, Serialize, TS)]
pub struct MergedPage {
    pub account: String,
    pub segments: Vec<Session>,
    pub events: Vec<MergedEvent>,
    pub cursor: String,
    pub has_earlier: bool,
    pub has_more: bool,
    pub indexing: bool,
}

#[derive(Debug, Default, Clone, Copy, Serialize, TS)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default, Serialize, TS)]
pub struct FieldPresentation {
    pub key: String,
    pub kind: String,
    pub type_name: String,
    pub value: String,
    pub value_range: ByteRange,
    pub value_length: usize,
    pub preview_limited: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct FieldNode {
    pub id: String,
    pub label: String,
    pub value: String,
    pub range: ByteRange,
    pub candidate: bool,
    pub children: Vec<FieldNode>,
    pub presentation: FieldPresentation,
}

#[derive(Debug, Serialize, TS)]
pub struct FieldText {
    pub text: String,
    pub complete: bool,
    pub content_type: String,
}

#[derive(Debug, Serialize, TS)]
pub struct PacketDetail {
    pub event: EventSummary,
    pub payload_hex: String,
    pub payload_sha256: String,
    pub format: String,
    pub decode_range: ByteRange,
    pub nodes: Vec<FieldNode>,
    pub complete: Option<bool>,
    pub error: Option<ApiError>,
    pub undecoded: Option<ByteRange>,
    pub view_limited: bool,
    /// `node_offset` of the next page when more fields follow this one.
    pub next_node_offset: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
pub struct PacketRef {
    pub session_id: String,
    pub event_id: String,
}

#[derive(Debug, Deserialize, TS)]
pub struct CompareRequest {
    pub items: Vec<PacketRef>,
    pub format: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct CompareColumn {
    pub source: PacketRef,
    pub event: EventSummary,
    pub payload_hex: String,
    pub payload_sha256: String,
    pub error: Option<ApiError>,
}

#[derive(Debug, Serialize, TS)]
pub struct CompareRow {
    pub path: String,
    pub values: Vec<Option<String>>,
    pub fingerprints: Vec<Option<String>>,
    pub changed: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct Comparison {
    pub columns: Vec<CompareColumn>,
    pub rows: Vec<CompareRow>,
    pub view_limited: bool,
}

#[derive(Debug, Clone, Deserialize, TS)]
pub struct RecordRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct Recording {
    pub id: String,
    pub source_session: String,
    pub state: String,
    pub path: String,
    pub started_filetime: String,
    pub packets: String,
    pub bytes: String,
    pub source_first_event: Option<String>,
    pub source_last_event: Option<String>,
    pub error: Option<String>,
}

pub fn typescript() -> String {
    let config = ts_rs::Config::default();
    let declarations = [
        SkillAgent::decl(&config),
        SkillScope::decl(&config),
        SkillRequest::decl(&config),
        SkillStatus::decl(&config),
        IntegrationSettings::decl(&config),
        DevtoolsIdentity::decl(&config),
        DevtoolsInfo::decl(&config),
        DevtoolsWindow::decl(&config),
        DevtoolsStatus::decl(&config),
        ConnectRequest::decl(&config),
        OpenRequest::decl(&config),
        SavedConnection::decl(&config),
        DevtoolsLaunchRequest::decl(&config),
        DevtoolsLaunch::decl(&config),
        DevtoolsFrontend::decl(&config),
        QQSetup::decl(&config),
        RuntimeComponent::decl(&config),
        RuntimeHealth::decl(&config),
        UpdateStatus::decl(&config),
        UpdateSettingsRequest::decl(&config),
        ApiError::decl(&config),
        Session::decl(&config),
        Catalog::decl(&config),
        EventSummary::decl(&config),
        EventPage::decl(&config),
        CommandCount::decl(&config),
        CommandList::decl(&config),
        MergedEvent::decl(&config),
        MergedPage::decl(&config),
        ByteRange::decl(&config),
        FieldPresentation::decl(&config),
        FieldNode::decl(&config),
        FieldText::decl(&config),
        PacketDetail::decl(&config),
        PacketRef::decl(&config),
        CompareRequest::decl(&config),
        CompareColumn::decl(&config),
        CompareRow::decl(&config),
        Comparison::decl(&config),
        RecordRequest::decl(&config),
        Recording::decl(&config),
    ];
    format!(
        "// Generated by qscope-web types. Edit the Rust contract, then regenerate.\n\n{}\n",
        declarations
            .iter()
            .map(|d| format!("export {d}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    )
}
