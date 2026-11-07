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

/// 研究 QQ 所需组件及不可用时的处理方式。
#[derive(Debug, Clone, Serialize, TS)]
pub struct RuntimeComponent {
    /// 组件类型为 platform、host、probe 或 qq。
    pub id: String,
    pub label: String,
    /// 状态为 ok、pending、missing、damaged、blocked 或 unsupported，pending 表示首次启动时准备。
    pub state: String,
    pub detail: String,
    /// 处理方式为 none、local、reinstall 或 build，分别表示无需操作、本机修复、重装产品或开发构建。
    pub repair: String,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct RuntimeHealth {
    pub components: Vec<RuntimeComponent>,
    pub healthy: bool,
    pub repairable: bool,
    /// 修复已开始重新准备 QQ 时为 true，进度由 QQ 状态提供。
    pub preparing: bool,
}

/// 当前 QScope 副本的在线更新状态。
#[derive(Debug, Clone, Serialize, TS)]
pub struct UpdateStatus {
    pub current: String,
    /// 安装类型为 installer、portable、cli 或 development。
    pub install: String,
    /// 当前副本无法在线更新的原因。
    pub unsupported: Option<String>,
    /// 更新状态为 idle、checking、available、latest、downloading、installing、ready、restarting 或 error。
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
    /// 代理格式为 http://host:port，空值表示直连。
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
    /// 从本文件 account_observed 状态记录读取的最近账号值，可从原始记录重建。
    #[serde(default)]
    pub account: Option<String>,
    /// 按首次出现顺序保存不同观察值，最多 8 个。
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

/// 当前抓包索引中某个命令的包数量。
#[derive(Debug, Serialize, TS)]
pub struct CommandCount {
    pub command: String,
    pub count: String,
    pub inbound: String,
    pub outbound: String,
}

/// 命令筛选提供的不同包命令，按频次降序排列。
#[derive(Debug, Serialize, TS)]
pub struct CommandList {
    pub commands: Vec<CommandCount>,
    /// 实际不同命令数量超过本次返回数量。
    pub truncated: bool,
}

/// 账号视图中的单个事件，每行保留原始抓包身份。
#[derive(Debug, Serialize, TS)]
pub struct MergedEvent {
    pub session_id: String,
    pub event: EventSummary,
}

/// 同一账号的多次启动按时间拼接。cursor 为 <session_id>:<event_id>，indexing 表示前序会话尚未完成索引，完成前暂缓返回行。
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
    /// 还有字段时，保存下一页的 node_offset。
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
        "// 由 qscope-web types 生成，修改 Rust 契约后重新生成。\n\n{}\n",
        declarations
            .iter()
            .map(|d| format!("export {d}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    )
}
