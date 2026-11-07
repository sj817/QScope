// 由 qscope-web types 生成，修改 Rust 契约后重新生成。

export type SkillAgent = "codex" | "claude-code" | "cursor";

export type SkillScope = "project" | "user";

export type SkillRequest = { agent: SkillAgent, scope: SkillScope, project: string | null, };

export type SkillStatus = { agent: SkillAgent, name: string, scope: SkillScope, path: string, state: string, detail: string, permissions_path: string | null, permissions_ready: boolean, version: string, installed_version: string | null, template_sha256: string, installed_template_sha256: string | null, content_sha256: string | null, installed_content_sha256: string | null, };

export type IntegrationSettings = { workspace: string, target_directory: string, scope: SkillScope, home_directory: string, cli_path: string, cli_available: boolean, server_url: string, targets: Array<SkillStatus>, install_prompt: string, devtools_endpoint: string, research_launcher: string | null, };

export type DevtoolsIdentity = { pid: number, birth_filetime: string, };

export type DevtoolsInfo = { version: number, pid: number, birth_filetime: string, build: string, endpoint: string, };

export type DevtoolsWindow = { id: number, title: string, visible: boolean, web_contents_id: number, url: string, devtools_open: boolean, focused: boolean, width: number, height: number, };

export type DevtoolsStatus = { info: DevtoolsInfo, windows: Array<DevtoolsWindow>, };

export type ConnectRequest = { connection_id: string, };

export type OpenRequest = { connection_id: string, window_id: number, identity: DevtoolsIdentity, };

export type SavedConnection = { id: string, endpoint: string, pid: number, birth_filetime: string, status_path: string, ready: boolean, error: string | null, };

export type DevtoolsLaunchRequest = { profile: string, port: number, };

export type DevtoolsLaunch = { launch_version: number, profile: string, endpoint: string, host_path: string, command: string, connection_file: string, log_directory: string, resources_directory: string, data_directory: string, package_directory: string, prebuilt_directory: string, };

export type DevtoolsFrontend = { opened: boolean, frontend_url: string, frontend: string, };

export type QQSetup = { platform: string, capture_supported: boolean, unavailable_reason: string | null, build: string, installed: boolean, available: boolean, stage: string, message: string, downloaded: string, total: string, pid: number | null, session_id: string | null, error: string | null, };

export type RuntimeComponent = { 
/**
 * 组件类型为 platform、host、probe 或 qq。
 */
id: string, label: string, 
/**
 * 状态为 ok、pending、missing、damaged、blocked 或 unsupported，pending 表示首次启动时准备。
 */
state: string, detail: string, 
/**
 * 处理方式为 none、local、reinstall 或 build，分别表示无需操作、本机修复、重装产品或开发构建。
 */
repair: string, };

export type RuntimeHealth = { components: Array<RuntimeComponent>, healthy: boolean, repairable: boolean, 
/**
 * 修复已开始重新准备 QQ 时为 true，进度由 QQ 状态提供。
 */
preparing: boolean, };

export type UpdateStatus = { current: string, 
/**
 * 安装类型为 installer、portable、cli 或 development。
 */
install: string, 
/**
 * 当前副本无法在线更新的原因。
 */
unsupported: string | null, 
/**
 * 更新状态为 idle、checking、available、latest、downloading、installing、ready、restarting 或 error。
 */
stage: string, latest: string | null, notes_url: string | null, downloaded: string, total: string, error: string | null, checked_unix: string | null, check_on_launch: boolean, proxy: string | null, };

export type UpdateSettingsRequest = { check_on_launch: boolean, 
/**
 * 代理格式为 http://host:port，空值表示直连。
 */
proxy: string, };

export type ApiError = { code: string, message: string, offset: number | null, };

export type Session = { id: string, name: string, path: string, pid: number, process_birth_filetime: string, source_kind: string, producer_alive: boolean | null, started_filetime: string, records: string, packets: string, inbound: string, outbound: string, dropped_total: string, truncated_packets: string, file_bytes: string, complete_bytes: string, clean_end: boolean, partial_tail: boolean, has_more: boolean, error: ApiError | null, 
/**
 * 从本文件 account_observed 状态记录读取的最近账号值，可从原始记录重建。
 */
account: string | null, 
/**
 * 按首次出现顺序保存不同观察值，最多 8 个。
 */
accounts: Array<string>, };

export type Catalog = { api_version: number, sessions: Array<Session>, discovering: boolean, discovery_limited: boolean, discovery_errors: Array<string>, };

export type EventSummary = { event_id: string, kind: string, direction: string, command: string, command_hex: string, sequence: string, timestamp_filetime: string, captured_length: number, original_length: string, truncated: boolean, dropped_total: string, error_code: number, tid: number, };

export type EventPage = { session: Session, events: Array<EventSummary>, cursor: string, has_earlier: boolean, has_more: boolean, };

export type CommandCount = { command: string, count: string, inbound: string, outbound: string, };

export type CommandList = { commands: Array<CommandCount>, 
/**
 * 实际不同命令数量超过本次返回数量。
 */
truncated: boolean, };

export type MergedEvent = { session_id: string, event: EventSummary, };

export type MergedPage = { account: string, segments: Array<Session>, events: Array<MergedEvent>, cursor: string, has_earlier: boolean, has_more: boolean, indexing: boolean, };

export type ByteRange = { start: number, end: number, };

export type FieldPresentation = { key: string, kind: string, type_name: string, value: string, value_range: ByteRange, value_length: number, preview_limited: boolean, };

export type FieldNode = { id: string, label: string, value: string, range: ByteRange, candidate: boolean, children: Array<FieldNode>, presentation: FieldPresentation, };

export type FieldText = { text: string, complete: boolean, content_type: string, };

export type PacketDetail = { event: EventSummary, payload_hex: string, payload_sha256: string, format: string, decode_range: ByteRange, nodes: Array<FieldNode>, complete: boolean | null, error: ApiError | null, undecoded: ByteRange | null, view_limited: boolean, 
/**
 * 还有字段时，保存下一页的 node_offset。
 */
next_node_offset: number | null, };

export type PacketRef = { session_id: string, event_id: string, };

export type CompareRequest = { items: Array<PacketRef>, format: string | null, };

export type CompareColumn = { source: PacketRef, event: EventSummary, payload_hex: string, payload_sha256: string, error: ApiError | null, };

export type CompareRow = { path: string, values: Array<string | null>, fingerprints: Array<string | null>, changed: boolean, };

export type Comparison = { columns: Array<CompareColumn>, rows: Array<CompareRow>, view_limited: boolean, };

export type RecordRequest = { session_id: string, };

export type Recording = { id: string, source_session: string, state: string, path: string, started_filetime: string, packets: string, bytes: string, source_first_event: string | null, source_last_event: string | null, error: string | null, };
