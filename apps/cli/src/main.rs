use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
mod automation;
mod command_path;
mod qq;
mod resources;
mod update;
mod web;
use qscope_core::{
    capture::{
        self, Boundary, CaptureError, Direction, EndReason, ObservationLayer, Packet, Record,
        SessionHeader, SourceKind, Validator,
    },
    data::{U64, from_hex, sha256},
    native,
    wire::{self, Message, Value},
};
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(
    name = "qscope-cli",
    version,
    about = "QScope raw QQ capture and binary protocol inspection"
)]
struct Cli {
    /// Use a separate QScope data directory and backend instance.
    #[arg(long, global = true, conflicts_with = "portable")]
    data_dir: Option<PathBuf>,
    /// Store data beside the executable.
    #[arg(long, global = true)]
    portable: bool,
    /// Write machine-readable results to stdout and structured errors to stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Print selected runtime paths to stderr.
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect local resources, backend state and supported QQ capabilities.
    Doctor,
    /// Prepare or manage an isolated research QQ.
    Qq(qq::Options),
    #[command(name = "__collector", hide = true)]
    Collector {
        #[arg(num_args = 1.., allow_hyphen_values = true)]
        arguments: Vec<std::ffi::OsString>,
    },
    /// Install, inspect or remove the global qs command.
    Cli(command_path::Options),
    #[command(name = "__service", hide = true)]
    Engine { config: PathBuf },
    /// Manage the shared local backend.
    Service(web::Service),
    #[command(hide = true)]
    ExtractQq {
        installer: PathBuf,
        destination: PathBuf,
    },
    /// Serve the embedded WebUI with the local Rust API.
    #[command(alias = "serve")]
    Web(web::Options),
    /// Verify and prepare the embedded auxiliary components in the user data directory.
    #[command(hide = true)]
    Runtime {},
    /// Query, record and analyze a running local QScope workbench.
    #[command(name = "capture", visible_alias = "live")]
    Live(automation::Live),
    /// Install or inspect the QScope skill for coding agents.
    Agent(automation::Agent),
    /// Inspect and control an explicitly enabled QQ research bridge.
    Devtools(automation::Devtools),
    /// Inspect bytes without assuming field names or protobuf declaration types.
    Decode(Decode),
    /// Read a native probe file, including complete records from an interrupted session.
    Native {
        path: PathBuf,
        /// Follow the append-only probe file until its footer or Ctrl+C.
        #[arg(long)]
        follow: bool,
        /// Maximum displayed records when not following; all records are still checked.
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Check GitHub for a newer QScope release and install it.
    Update(update::Options),
    /// Save user-supplied bytes as a new, verifiable capture session.
    Import(Import),
    /// Validate every record, payload hash, ordering rule and closing counter.
    Inspect { path: PathBuf },
    /// Validate a collector JSONL stream and publish a new capture on clean end.
    Record {
        /// JSONL file, or - for stdin. No process is attached automatically.
        #[arg(long, default_value = "-")]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("decode_input").required(true).multiple(false)))]
#[command(group(ArgGroup::new("packet_source").args(["capture", "native"])))]
struct Decode {
    /// Hex bytes; spaces are allowed.
    #[arg(long, group = "decode_input")]
    hex: Option<String>,
    /// File containing hex text.
    #[arg(long, group = "decode_input")]
    hex_file: Option<PathBuf>,
    /// Raw binary file, read without modifying it.
    #[arg(long, group = "decode_input")]
    file: Option<PathBuf>,
    /// JSONL capture written by import or record.
    #[arg(long, group = "decode_input")]
    capture: Option<PathBuf>,
    /// Native .qscap probe file.
    #[arg(long, group = "decode_input")]
    native: Option<PathBuf>,
    /// Capture event ID; defaults to 1. IDs and protocol sequence are distinct.
    #[arg(long, requires = "packet_source")]
    event_id: Option<u64>,
    #[arg(long, value_enum, default_value = "pb")]
    format: DecodeFormat,
}

#[derive(Clone, Copy, ValueEnum)]
enum DecodeFormat {
    Pb,
    Hex,
    Tlv16be,
    Jce,
}

#[derive(Args)]
#[command(group(ArgGroup::new("import_input").required(true).multiple(false)))]
struct Import {
    #[arg(long, group = "import_input")]
    hex: Option<String>,
    #[arg(long, group = "import_input")]
    hex_file: Option<PathBuf>,
    #[arg(long, group = "import_input")]
    file: Option<PathBuf>,
    /// New JSONL capture file; an existing file is never replaced.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, value_enum, default_value = "unknown")]
    direction: InputDirection,
    #[arg(long, value_enum, default_value = "unknown")]
    layer: InputLayer,
    #[arg(long, value_enum, default_value = "unknown")]
    boundary: InputBoundary,
    #[arg(long)]
    command: Option<String>,
    #[arg(long)]
    protocol_sequence: Option<u64>,
}

#[derive(Clone, Copy, ValueEnum)]
enum InputDirection {
    Inbound,
    Outbound,
    Unknown,
}
#[derive(Clone, Copy, ValueEnum)]
enum InputLayer {
    Wire,
    Plaintext,
    Unknown,
}
#[derive(Clone, Copy, ValueEnum)]
enum InputBoundary {
    Frame,
    Chunk,
    Unknown,
}

struct Error {
    code: String,
    message: String,
    line: Option<usize>,
}
impl Error {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            line: None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}
impl From<qscope_automation::Error> for Error {
    fn from(error: qscope_automation::Error) -> Self {
        Self::new(error.code, error.message)
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::new("json", error.to_string())
    }
}
impl From<CaptureError> for Error {
    fn from(error: CaptureError) -> Self {
        Self {
            code: error.code.into(),
            message: error.message,
            line: (error.line != 0).then_some(error.line),
        }
    }
}
impl From<native::Error> for Error {
    fn from(error: native::Error) -> Self {
        Self::new(error.code, error.to_string())
    }
}

fn bounded_read(path: &Path, max: usize) -> Result<Vec<u8>, Error> {
    let mut result = Vec::new();
    File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut result)?;
    if result.len() > max {
        return Err(Error::new(
            "input_limit",
            "input file exceeds configured byte limit",
        ));
    }
    Ok(result)
}

fn raw_input(
    hex: Option<&str>,
    hex_file: Option<&Path>,
    file: Option<&Path>,
) -> Result<Vec<u8>, Error> {
    if let Some(path) = file {
        return bounded_read(path, capture::MAX_PACKET_BYTES);
    }
    let buffer;
    let hex = if let Some(path) = hex_file {
        buffer = bounded_read(path, capture::MAX_PACKET_BYTES * 4 + 2)?;
        std::str::from_utf8(&buffer)
            .map_err(|_| Error::new("hex", "hex file must be UTF-8 text"))?
    } else {
        hex.ok_or_else(|| Error::new("input", "provide exactly one byte source"))?
    };
    from_hex(hex, capture::MAX_PACKET_BYTES).map_err(|error| Error::new("hex", error.to_string()))
}

fn unix_ns() -> Result<U64, Error> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::new("clock", "system clock predates Unix epoch"))?
        .as_nanos();
    Ok(U64(nanos.try_into().map_err(|_| {
        Error::new("clock", "Unix nanosecond timestamp overflow")
    })?))
}

fn print_json(value: &serde_json::Value) -> Result<(), Error> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer_pretty(&mut output, value)
        .map_err(|error| Error::new("json_write", error.to_string()))?;
    output.write_all(b"\n")?;
    Ok(())
}

fn print_message(message: &Message, depth: usize, output: &mut impl Write) -> io::Result<()> {
    for field in &message.fields {
        let indent = "  ".repeat(depth);
        write!(
            output,
            "{indent}{} @{}..{} ",
            field.number, field.raw.start, field.raw.end
        )?;
        match &field.value {
            Value::Varint {
                unsigned,
                canonical,
                ..
            } => writeln!(
                output,
                "VARINT {}{}",
                unsigned.0,
                if *canonical { "" } else { " (noncanonical)" }
            )?,
            Value::Fixed64 {
                little_endian_bits, ..
            } => writeln!(output, "FIXED64 bits=0x{:016x}", little_endian_bits.0)?,
            Value::Fixed32 {
                little_endian_bits, ..
            } => writeln!(output, "FIXED32 bits=0x{little_endian_bits:08x}")?,
            Value::LengthDelimited {
                bytes, candidates, ..
            } => {
                writeln!(
                    output,
                    "LEN {} bytes @{}..{}",
                    bytes.end - bytes.start,
                    bytes.start,
                    bytes.end
                )?;
                if let Some(text) = &candidates.utf8 {
                    writeln!(
                        output,
                        "{indent}  UTF-8 candidate: \"{}\"{}",
                        text.escape_debug(),
                        if candidates.utf8_preview_truncated {
                            " (preview truncated)"
                        } else {
                            ""
                        }
                    )?;
                }
                if let Some(nested) = &candidates.message {
                    writeln!(output, "{indent}  Message candidate:")?;
                    print_message(nested, depth + 2, output)?;
                }
                if candidates.message_analysis_limited {
                    writeln!(output, "{indent}  Message candidate analysis limited")?;
                }
            }
            Value::Group { message, .. } => {
                writeln!(output, "GROUP")?;
                print_message(message, depth + 1, output)?;
            }
        }
    }
    if let Some(error) = &message.error {
        writeln!(
            output,
            "{:?} at byte {}, field starts at {}",
            error.code, error.offset, error.field_start
        )?;
    }
    Ok(())
}

/// A failed or disconnected producer never publishes an apparently complete file.
fn publish(
    out: &Path,
    write: impl FnOnce(&mut File) -> Result<serde_json::Value, Error>,
) -> Result<serde_json::Value, Error> {
    let parent = out
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".qscope-")
        .suffix(".partial")
        .tempfile_in(parent)?;
    let result = write(temporary.as_file_mut())?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(out)
        .map_err(|error| Error::new("output_exists_or_io", error.error.to_string()))?;
    Ok(result)
}

fn import(args: Import) -> Result<serde_json::Value, Error> {
    let bytes = raw_input(
        args.hex.as_deref(),
        args.hex_file.as_deref(),
        args.file.as_deref(),
    )?;
    let now = unix_ns()?;
    let header = Record::Session(SessionHeader {
        format_version: capture::FORMAT_VERSION,
        session_id: format!("manual-{:x}-{:x}", now.0, std::process::id()),
        created_unix_ns: now,
        source: SourceKind::Manual,
        collector: "qscope-cli".into(),
        collector_version: env!("CARGO_PKG_VERSION").into(),
        capture_point: "manual raw-byte import; layer metadata supplied by the user".into(),
        client: None,
        process: None,
    });
    let mut packet = Packet::from_bytes(&bytes, now)?;
    packet.direction = match args.direction {
        InputDirection::Inbound => Direction::Inbound,
        InputDirection::Outbound => Direction::Outbound,
        InputDirection::Unknown => Direction::Unknown,
    };
    packet.layer = match args.layer {
        InputLayer::Wire => ObservationLayer::Wire,
        InputLayer::Plaintext => ObservationLayer::Plaintext,
        InputLayer::Unknown => ObservationLayer::Unknown,
    };
    packet.boundary = match args.boundary {
        InputBoundary::Frame => Boundary::Frame,
        InputBoundary::Chunk => Boundary::Chunk,
        InputBoundary::Unknown => Boundary::Unknown,
    };
    packet.command = args.command;
    packet.protocol_sequence = args.protocol_sequence.map(U64);
    let mut validator = Validator::new(capture::Limits::default());
    let records = [header, Record::Packet(packet)];
    for record in &records {
        validator.push(record)?;
    }
    let end = validator.end_record(U64(0), EndReason::Completed);
    validator.push(&end)?;
    validator.finish()?;
    publish(&args.out, |file| {
        for record in &records {
            capture::write_record(file, record)?;
        }
        capture::write_record(file, &end)?;
        Ok(serde_json::json!({"output": args.out, "summary": validator.summary()}))
    })
}

fn record(input: impl BufRead, out: &Path) -> Result<serde_json::Value, Error> {
    publish(out, |file| {
        let mut reader = capture::Reader::new(input, capture::Limits::default());
        for item in reader.by_ref() {
            capture::write_record(file, &item?.record)?;
        }
        Ok(serde_json::json!({"output": out, "summary": reader.summary()}))
    })
}

fn run(cli: Cli) -> Result<(), Error> {
    if let Some(data) = cli.data_dir {
        qscope_automation::runtime::set_data_directory(data)?;
    } else if cli.portable {
        qscope_automation::runtime::set_data_directory(
            std::env::current_exe()?.with_file_name("data"),
        )?;
    }
    // Executables an earlier update moved aside go once nothing runs them.
    if let Ok((_, directory)) = qscope_automation::update::locate() {
        qscope_automation::runtime::cleanup_replaced(&directory);
    }
    if cli.verbose {
        eprintln!(
            "QScope {} · data {}",
            env!("CARGO_PKG_VERSION"),
            qscope_automation::runtime::user_data()?.display()
        );
    }
    match cli.command {
        Command::Doctor => {
            let data = qscope_automation::runtime::user_data()?;
            let service = qscope_automation::service::connect(&data).ok();
            let resources = resources::embedded().ok();
            print_json(
                &serde_json::json!({"schema_version":1,"version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"data_directory":data,"cli":std::env::current_exe()?,"backend":service.map(|service|serde_json::json!({"pid":service.pid,"endpoint":service.endpoint,"version":service.version})),"embedded_webui":resources.as_ref().is_some_and(|resources|resources.contains("ui/index.html")),"embedded_devtools":resources.as_ref().is_some_and(|resources|resources.contains("collectors/devtools/node_modules/chii/public/target.js")),"embedded_probe":resources.as_ref().is_some_and(|resources|resources.contains("native/qscope-portable.dll")),"qq_build":"9.9.37-54395","live_capture_supported":cfg!(all(windows,target_arch="x86_64")),"http_capture_complete":false,"hot_attach_supported":false}),
            )?;
        }
        Command::Qq(options) => qq::run(options)?,
        Command::Collector { arguments } => {
            #[cfg(windows)]
            print_json(&qscope_qqnt::worker::dispatch(arguments)?)?;
            #[cfg(not(windows))]
            {
                let _ = arguments;
                return Err(Error::new(
                    "unsupported_platform",
                    "The QQ collector requires Windows x64",
                ));
            }
        }
        Command::Cli(options) => command_path::run(options)?,
        Command::Engine { config } => web::backend(config)?,
        Command::Service(args) => web::manage(args)?,
        Command::ExtractQq {
            installer,
            destination,
        } => {
            let extraction = qscope_qqnt::package::extract(&installer, &destination)?;
            print_json(&serde_json::to_value(extraction)?)?;
        }
        Command::Web(options) => web::run(options, cli.json)?,
        Command::Runtime {} => {
            let data = qscope_automation::runtime::user_data()?;
            let root = crate::resources::embedded()?.prepare_native(&data)?;
            print_json(&serde_json::json!({"schema_version": 1, "resources": root}))?;
        }
        Command::Live(args) => automation::live(args)?,
        Command::Update(options) => update::run(options, cli.json)?,
        Command::Agent(args) => automation::agent(args, cli.json)?,
        Command::Devtools(args) => automation::devtools(args)?,
        Command::Decode(args) => {
            let (bytes, source) = if let Some(path) = &args.capture {
                let mut reader = capture::Reader::new(
                    BufReader::new(File::open(path)?),
                    capture::Limits::default(),
                );
                let event_id = args.event_id.unwrap_or(1);
                let mut selected = None;
                for item in reader.by_ref() {
                    let item = item?;
                    if let Record::Packet(packet) = &item.record
                        && packet.event_id.0 == event_id
                    {
                        selected = item.payload;
                    }
                }
                (
                    selected.ok_or_else(|| {
                        Error::new("event_not_found", "requested packet event ID was not found")
                    })?,
                    serde_json::json!({"kind":"capture","session_id": reader.summary().session_id,"event_id": U64(event_id)}),
                )
            } else if let Some(path) = &args.native {
                let mut reader = native::Reader::new(BufReader::new(File::open(path)?))?;
                let id = args.event_id;
                let mut selected = None;
                while let Some(event) = reader.next_event()? {
                    if id.is_none_or(|id| event.event_id.0 == id) && event.kind == "packet" {
                        selected = Some(event);
                        break;
                    }
                }
                let event = selected.ok_or_else(|| {
                    Error::new(
                        "event_not_found",
                        "requested native packet event ID was not found",
                    )
                })?;
                (
                    event.payload,
                    serde_json::json!({"kind":"native","header":reader.header,"event_id":event.event_id,"command":event.command,"direction":event.direction,"sequence":event.protocol_sequence,"original_length":event.original_length,"truncated":event.truncated}),
                )
            } else {
                (
                    raw_input(
                        args.hex.as_deref(),
                        args.hex_file.as_deref(),
                        args.file.as_deref(),
                    )?,
                    serde_json::json!({"kind":"raw_input"}),
                )
            };
            if matches!(args.format, DecodeFormat::Hex) {
                print_json(
                    &serde_json::json!({"source":source,"input_sha256":sha256(&bytes),"input_length":bytes.len(),"hex":qscope_core::data::to_hex(&bytes)}),
                )?;
                return Ok(());
            }
            if matches!(args.format, DecodeFormat::Tlv16be | DecodeFormat::Jce) {
                let report = if matches!(args.format, DecodeFormat::Tlv16be) {
                    qscope_core::formats::tlv16be(&bytes)
                } else {
                    qscope_core::formats::jce(&bytes)
                };
                print_json(
                    &serde_json::json!({"source":source,"input_sha256":sha256(&bytes),"input_length":bytes.len(),"message":report}),
                )?;
                if !report.complete {
                    return Err(Error::new(
                        "format_decode",
                        "incomplete structure; original byte ranges are retained",
                    ));
                }
                return Ok(());
            }
            let message = wire::decode(&bytes, wire::Limits::default());
            if cli.json {
                print_json(
                    &serde_json::json!({"source":source,"input_sha256":sha256(&bytes),"input_length":bytes.len(),"message":message}),
                )?;
            } else {
                let stdout = io::stdout();
                let mut output = stdout.lock();
                writeln!(output, "{} bytes; SHA256 {}", bytes.len(), sha256(&bytes))?;
                print_message(&message, 0, &mut output)?;
                writeln!(
                    output,
                    "LEN interpretations are candidates; no schema or field names have been recovered."
                )?;
            }
            if !message.complete {
                return Err(Error::new(
                    "wire_decode",
                    "incomplete wire structure; partial fields and original ranges are retained in the result",
                ));
            }
        }
        Command::Native {
            path,
            follow,
            limit,
        } => {
            let mut reader = native::Reader::new(BufReader::new(File::open(path)?))?;
            let mut records = 0u64;
            let mut packets = 0u64;
            let mut lost = 0u64;
            let mut partial = false;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"type":"native_header","header":reader.header})
                );
            } else {
                println!(
                    "Native capture PID {}; {} ticks/sec",
                    reader.header.pid, reader.header.qpc_frequency.0
                );
            }
            loop {
                match reader.next_event() {
                    Ok(Some(event)) => {
                        records += 1;
                        if event.kind == "packet" {
                            packets += 1;
                        }
                        lost = event.dropped_total.0;
                        if follow || records <= limit as u64 {
                            if cli.json {
                                println!(
                                    "{}",
                                    serde_json::json!({"type":"native_event","event_id":event.event_id,"kind":event.kind,"direction":event.direction,"command":event.command,"sequence":event.protocol_sequence,"original_length":event.original_length,"captured_length":event.payload.len(),"truncated":event.truncated,"error_code":event.error_code,"dropped_total":event.dropped_total,"timestamp_filetime":event.timestamp_filetime,"qpc":event.qpc})
                                );
                            } else {
                                println!(
                                    "#{:<6} {:<7} {:<8} seq={:<12} bytes={:<7} err={:<8} {}{}",
                                    event.event_id.0,
                                    event.kind,
                                    event.direction,
                                    event.protocol_sequence.0,
                                    event.payload.len(),
                                    event.error_code,
                                    event.command.as_deref().unwrap_or("<non-UTF8 command>"),
                                    if event.truncated { " [truncated]" } else { "" }
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        if !follow || reader.ended {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                    Err(error) if error.code == "native_partial_tail" => {
                        if follow {
                            reader.retry_partial_tail()?;
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        } else {
                            partial = true;
                            break;
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            println!(
                "{}",
                serde_json::json!({"type":"native_summary","records":U64(records),"packets":U64(packets),"dropped_total":U64(lost),"clean_end":reader.ended,"partial_tail":partial,"complete_bytes":U64(reader.offset)})
            );
        }
        Command::Import(args) => {
            let value = import(args)?;
            print_json(&value)?;
        }
        Command::Inspect { path } => {
            let mut reader = capture::Reader::new(
                BufReader::new(File::open(path)?),
                capture::Limits::default(),
            );
            let mut header = None;
            for item in reader.by_ref() {
                if let Record::Session(session) = item?.record {
                    header = Some(session);
                }
            }
            print_json(&serde_json::json!({"header":header,"summary":reader.summary()}))?;
        }
        Command::Record { input, out } => {
            let value = if input == Path::new("-") {
                record(io::stdin().lock(), &out)?
            } else {
                record(BufReader::new(File::open(input)?), &out)?
            };
            print_json(&value)?;
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    #[cfg(windows)]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "__guard" || argument == "--qscope-child")
    {
        return match qscope_qqnt::windows::guard() {
            Ok(code) => ExitCode::from(code as u8),
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({"code":error.code,"message":error.message,"line":error.line})
                );
            } else {
                eprintln!(
                    "{}{}: {}",
                    error.code,
                    error
                        .line
                        .map_or(String::new(), |line| format!(" at line {line}")),
                    error.message
                );
            }
            ExitCode::from(2)
        }
    }
}
