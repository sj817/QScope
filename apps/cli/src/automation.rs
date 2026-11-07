use crate::{Error, print_json};
use clap::{Args, Subcommand};
use qscope_automation::{
    client::{Client, DEFAULT_SERVER},
    devtools, skills,
};
use serde_json::json;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Args)]
pub struct Live {
    /// 后台选择器支持 auto、@path/to/service.json 或回环 URL。
    #[arg(long, default_value = DEFAULT_SERVER, global = true)]
    server: String,
    #[command(subcommand)]
    command: LiveCommand,
}

#[derive(Subcommand)]
enum LiveCommand {
    /// 查询抓包元数据，按需加载包体。
    Sessions,
    /// 使用工作台筛选条件查询有界元数据页。
    Events {
        /// 来自 capture sessions 的抓包会话 ID。
        session: String,
        #[command(flatten)]
        filters: EventFilters,
    },
    /// 复用 Rust 工作台解码单个包。
    Packet {
        /// 来自 capture sessions 的抓包会话 ID。
        session: String,
        /// 该会话中的事件 ID。
        event: u64,
        /// 选择 pb、tlv16be、jce 或 hex 格式。
        #[arg(long, default_value = "pb")]
        format: String,
        /// 包体内解码范围的起点。
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// 从 --offset 起解码的字节数，默认覆盖剩余包体。
        #[arg(long)]
        length: Option<usize>,
    },
    /// 对比 2 至 8 个 session:event 引用。
    Compare {
        /// SESSION:EVENT 参数，重复传入 2 至 8 次。
        #[arg(long, required = true)]
        packet: Vec<String>,
        /// 选择 pb、tlv16be、jce 或 hex 格式。
        #[arg(long, default_value = "pb")]
        format: String,
    },
    /// 从正在运行的生产者保存一个时间窗口。
    Record {
        #[command(subcommand)]
        command: RecordingCommand,
    },
    /// 从显式事件游标之后等待匹配包。
    Wait {
        /// 来自 capture sessions 的抓包会话 ID。
        session: String,
        /// 仅查询此事件 ID 之后的包，填写操作前观察到的最新 ID。
        #[arg(long)]
        after: u64,
        /// 等待的命令规则，以逗号分隔。
        #[arg(long)]
        command: Option<String>,
        /// 排除的命令规则，以逗号分隔。
        #[arg(long)]
        exclude: Option<String>,
        /// 方向选择 all、inbound 或 outbound。
        #[arg(long, default_value = "all")]
        direction: String,
        /// 等待秒数，范围 1 至 60。
        #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..=60))]
        timeout: u64,
        #[command(flatten)]
        noise: Noise,
    },
    /// 汇总安静时间窗口内的命令，例如心跳和轮询。
    Noise {
        /// 来自 capture sessions 的抓包会话 ID。
        session: String,
        /// 窗口的最后事件，默认使用最新事件。
        #[arg(long)]
        before: Option<u64>,
        /// 截至 --before 的窗口长度，范围 1 至 3600 秒。
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
        seconds: u64,
    },
}

// 查询结果可隐藏背景流量，原始抓包和录制保留所有包。
#[derive(Args)]
struct Noise {
    /// 隐藏名称类似心跳和保活的命令。
    #[arg(long)]
    hide_heartbeat: bool,
    /// 排除 --after 之前指定秒数内已出现的命令，例如轮询。
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=3600))]
    baseline: Option<u64>,
}

#[derive(Args)]
struct EventFilters {
    /// 匹配命令或序号子串，或精确事件 ID。
    #[arg(long)]
    q: Option<String>,
    /// 保留命令规则，以逗号分隔，支持 =精确名称、a*glob? 和子串。
    #[arg(long)]
    include: Option<String>,
    /// 按 --include 的语法指定排除命令。
    #[arg(long)]
    exclude: Option<String>,
    /// 方向选择 all、inbound 或 outbound。
    #[arg(long, default_value = "all")]
    direction: String,
    /// 同时列出状态、丢包和结束记录。
    #[arg(long)]
    controls: bool,
    /// 仅查询此事件 ID 之后的事件。
    #[arg(long)]
    after: Option<u64>,
    /// 仅查询此事件 ID 之前的事件。
    #[arg(long)]
    before: Option<u64>,
    /// 每页 1 至 1000 条，has_more 为 true 时将返回游标传给 --after。
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    limit: u16,
    /// 附加服务端筛选，例如 min_size=10 或 error_only=true。
    #[arg(long)]
    filter: Vec<String>,
    #[command(flatten)]
    noise: Noise,
}

impl EventFilters {
    fn query(self) -> Result<Vec<(String, String)>, Error> {
        let mut query = vec![
            ("direction".into(), self.direction),
            ("controls".into(), self.controls.to_string()),
            ("limit".into(), self.limit.to_string()),
        ];
        if self.noise.hide_heartbeat {
            query.push(("hide_heartbeat".into(), "true".into()));
        }
        for (key, value) in [
            ("q", self.q),
            ("include", self.include),
            ("exclude", self.exclude),
            ("after", self.after.map(|v| v.to_string())),
            ("before", self.before.map(|v| v.to_string())),
        ] {
            if let Some(value) = value {
                query.push((key.into(), value));
            }
        }
        for filter in self.filter {
            let (key, value) = filter
                .split_once('=')
                .ok_or_else(|| Error::new("query", "Use --filter key=value"))?;
            if ![
                "kinds",
                "min_size",
                "max_size",
                "time_from",
                "time_to",
                "event_from",
                "event_to",
                "seq",
                "tid",
                "error_only",
                "truncated_only",
                "hide_heartbeat",
                "hide_login",
            ]
            .contains(&key)
            {
                return Err(Error::new(
                    "query",
                    format!("Unsupported additional filter {key}"),
                ));
            }
            query.push((key.into(), value.into()));
        }
        Ok(query)
    }
}

#[derive(Subcommand)]
enum RecordingCommand {
    /// 列出录制状态与输出路径，状态为 recording、stopped 或 error。
    List,
    /// 从当前时刻开始保存包，仅有一个在线会话时可省略会话参数。
    Start { session: Option<String> },
    /// 停止录制，仅有一个运行中的录制时可省略录制参数。
    Stop { recording: Option<String> },
}

const FILETIME_SECOND: u64 = 10_000_000;

fn number(value: &serde_json::Value) -> u64 {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

// 查询截至 last 的指定秒数内的包，最多返回最新 1000 条。
fn quiet_window(
    client: &Client,
    path: &str,
    last: Option<u64>,
    seconds: u64,
) -> Result<serde_json::Value, Error> {
    let mut query = vec![
        ("controls".to_string(), "true".to_string()),
        ("limit".into(), "1".into()),
    ];
    if let Some(last) = last {
        query.push(("event_from".into(), last.to_string()));
        query.push(("event_to".into(), last.to_string()));
    }
    let reference = client.get(path, &query)?["events"][0].clone();
    if reference.is_null() {
        return Err(Error::new(
            "baseline",
            "No event at the window end; pass an existing event ID",
        ));
    }
    let (end, time) = (
        number(&reference["event_id"]),
        number(&reference["timestamp_filetime"]),
    );
    let page = client.get(
        path,
        &[
            ("before".into(), (end + 1).to_string()),
            (
                "time_from".into(),
                time.saturating_sub(seconds * FILETIME_SECOND).to_string(),
            ),
            ("limit".into(), "1000".into()),
        ],
    )?;
    Ok(
        json!({"last_event":end.to_string(),"seconds":seconds,"packets":page["events"].as_array().map_or(0, Vec::len),"complete":page["has_earlier"]==false,"events":page["events"]}),
    )
}

// 每个命令生成一条精确规则，无法用规则语法表示的名称单独报告。
fn exact_rules(commands: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut rules, mut skipped) = (Vec::new(), Vec::new());
    for command in commands {
        if command.is_empty()
            || command.len() > 127
            || command.contains([',', '\n'])
            || command.trim() != command
            || rules.len() >= 200
        {
            skipped.push(command.clone());
        } else {
            rules.push(format!("={command}"));
        }
    }
    (rules, skipped)
}

// 按名称汇总窗口内命令的频率、规律性及大小。
fn summarize(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut groups: std::collections::BTreeMap<&str, Vec<&serde_json::Value>> = Default::default();
    for event in events.iter().filter(|event| event["kind"] == "packet") {
        groups
            .entry(event["command"].as_str().unwrap_or_default())
            .or_default()
            .push(event);
    }
    let mut summary: Vec<_> = groups
        .into_iter()
        .map(|(command, items)| {
            let mut gaps: Vec<u64> = items
                .windows(2)
                .map(|pair| {
                    number(&pair[1]["timestamp_filetime"])
                        .saturating_sub(number(&pair[0]["timestamp_filetime"]))
                })
                .collect();
            gaps.sort_unstable();
            let median = gaps.get(gaps.len() / 2).copied();
            // 至少出现三个包，间隔相对中位数的偏差不超过五分之一时视为规律流量。
            let regular = median.is_some_and(|median| {
                items.len() >= 3
                    && median > 0
                    && gaps.iter().all(|gap| gap.abs_diff(median) * 5 <= median)
            });
            let sizes = items
                .iter()
                .map(|item| item["captured_length"].as_u64().unwrap_or(0));
            json!({
                "command": command,
                "count": items.len(),
                "inbound": items.iter().filter(|item| item["direction"] == "inbound").count(),
                "outbound": items.iter().filter(|item| item["direction"] == "outbound").count(),
                "median_interval_ms": median.map(|gap| gap / 10_000),
                "regular": regular,
                "bytes_min": sizes.clone().min(),
                "bytes_max": sizes.max(),
            })
        })
        .collect();
    summary.sort_by(|a, b| b["count"].as_u64().cmp(&a["count"].as_u64()));
    summary
}

// 为查询补充去噪选项，并报告被排除的内容。
fn quiet(
    client: &Client,
    path: &str,
    after: Option<u64>,
    noise: &Noise,
    query: &mut Vec<(String, String)>,
) -> Result<serde_json::Value, Error> {
    if noise.hide_heartbeat && !query.iter().any(|(key, _)| key == "hide_heartbeat") {
        query.push(("hide_heartbeat".into(), "true".into()));
    }
    let Some(seconds) = noise.baseline else {
        return Ok(json!({"hide_heartbeat":noise.hide_heartbeat}));
    };
    let after = after.ok_or_else(|| {
        Error::new(
            "query",
            "--baseline compares against the window before --after",
        )
    })?;
    let window = quiet_window(client, path, Some(after), seconds)?;
    let mut commands: Vec<String> = window["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| event["kind"] == "packet")
        .filter_map(|event| event["command"].as_str().map(str::to_string))
        .collect();
    commands.sort();
    commands.dedup();
    let (rules, skipped) = exact_rules(&commands);
    if !rules.is_empty() {
        let joined = rules.join(",");
        match query.iter_mut().find(|(key, _)| key == "exclude") {
            Some((_, value)) => *value = format!("{value},{joined}"),
            None => query.push(("exclude".into(), joined)),
        }
    }
    Ok(json!({
        "hide_heartbeat": noise.hide_heartbeat,
        "baseline": {"seconds":seconds,"last_event":window["last_event"],"packets":window["packets"],"complete":window["complete"]},
        "commands": rules.iter().map(|rule| &rule[1..]).collect::<Vec<_>>(),
        "not_excluded": skipped,
    }))
}

fn identifier(value: &str) -> Result<&str, Error> {
    if value.len() != 24
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::new(
            "identifier",
            "Expected a 24-character lowercase capture or recording ID",
        ));
    }
    Ok(value)
}

pub fn live(args: Live) -> Result<(), Error> {
    let _lease = ensure_backend(&args.server)?;
    let client = Client::new(&args.server)?;
    let value = match args.command {
        LiveCommand::Sessions => client.get("/api/v1/sessions", &[])?,
        LiveCommand::Events { session, filters } => {
            let path = format!("/api/v1/sessions/{}/events", identifier(&session)?);
            let after = filters.after;
            let noise = Noise {
                hide_heartbeat: filters.noise.hide_heartbeat,
                baseline: filters.noise.baseline,
            };
            let mut query = filters.query()?;
            let ignored = quiet(&client, &path, after, &noise, &mut query)?;
            let mut page = client.get(&path, &query)?;
            if noise.hide_heartbeat || noise.baseline.is_some() {
                page["ignored"] = ignored;
            }
            page
        }
        LiveCommand::Noise {
            session,
            before,
            seconds,
        } => {
            let path = format!("/api/v1/sessions/{}/events", identifier(&session)?);
            let mut window = quiet_window(&client, &path, before, seconds)?;
            let events = window["events"].take();
            let summary = summarize(events.as_array().map_or(&[], Vec::as_slice));
            let commands: Vec<String> = summary
                .iter()
                .filter_map(|item| item["command"].as_str().map(str::to_string))
                .collect();
            let (rules, _) = exact_rules(&commands);
            window["commands"] = json!(summary);
            window["exclude"] = json!(rules.join(","));
            window
        }
        LiveCommand::Packet {
            session,
            event,
            format,
            offset,
            length,
        } => {
            let mut query = vec![
                ("format".into(), format),
                ("offset".into(), offset.to_string()),
            ];
            if let Some(length) = length {
                query.push(("length".into(), length.to_string()));
            }
            client.get(
                &format!("/api/v1/sessions/{}/events/{event}", identifier(&session)?),
                &query,
            )?
        }
        LiveCommand::Compare { packet, format } => {
            if !(2..=8).contains(&packet.len()) {
                return Err(Error::new(
                    "query",
                    "Choose 2–8 --packet session:event references",
                ));
            }
            let items = packet
                .iter()
                .map(|reference| {
                    let (session, event) = reference
                        .split_once(':')
                        .ok_or_else(|| Error::new("query", "Use --packet session:event"))?;
                    let event: u64 = event
                        .parse()
                        .map_err(|_| Error::new("query", "Invalid event ID"))?;
                    Ok(json!({"session_id":identifier(session)?,"event_id":event.to_string()}))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            client.post("/api/v1/compare", json!({"items":items,"format":format}))?
        }
        LiveCommand::Record { command } => match command {
            RecordingCommand::List => client.get("/api/v1/recordings", &[])?,
            RecordingCommand::Start { session } => {
                let selected = match session {
                    Some(value) => value,
                    None => {
                        let catalog = client.get("/api/v1/sessions", &[])?;
                        let candidates: Vec<_> = catalog["sessions"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|session| {
                                session["producer_alive"] == true
                                    && session["source_kind"] == "capture"
                            })
                            .collect();
                        if candidates.len() != 1 {
                            return Err(Error::new(
                                "ambiguous_target",
                                "Choose a session ID from capture sessions",
                            ));
                        }
                        candidates[0]["id"]
                            .as_str()
                            .ok_or_else(|| Error::new("response", "Invalid session identity"))?
                            .into()
                    }
                };
                client.post(
                    "/api/v1/recordings",
                    json!({"session_id":identifier(&selected)?}),
                )?
            }
            RecordingCommand::Stop { recording } => {
                let selected = match recording {
                    Some(value) => value,
                    None => {
                        let recordings = client.get("/api/v1/recordings", &[])?;
                        let candidates: Vec<_> = recordings
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|recording| recording["state"] == "recording")
                            .collect();
                        if candidates.len() != 1 {
                            return Err(Error::new(
                                "ambiguous_target",
                                "Choose a recording ID from capture record list",
                            ));
                        }
                        candidates[0]["id"]
                            .as_str()
                            .ok_or_else(|| Error::new("response", "Invalid recording identity"))?
                            .into()
                    }
                };
                client.post(
                    &format!("/api/v1/recordings/{}/stop", identifier(&selected)?),
                    json!({}),
                )?
            }
        },
        LiveCommand::Wait {
            session,
            after,
            command,
            exclude,
            direction,
            timeout,
            noise,
        } => {
            let path = format!("/api/v1/sessions/{}/events", identifier(&session)?);
            let deadline = Instant::now() + Duration::from_secs(timeout);
            let mut cursor = after.to_string();
            let mut filters = vec![("direction".to_string(), direction.clone())];
            if let Some(command) = &command {
                filters.push(("include".into(), command.clone()));
            }
            if let Some(exclude) = &exclude {
                filters.push(("exclude".into(), exclude.clone()));
            }
            let ignored = quiet(&client, &path, Some(after), &noise, &mut filters)?;
            loop {
                let mut query = vec![
                    ("after".into(), cursor.clone()),
                    ("limit".into(), "100".into()),
                ];
                query.extend(filters.iter().cloned());
                let page = client.get(&path, &query)?;
                if page["events"]
                    .as_array()
                    .is_some_and(|events| !events.is_empty())
                {
                    break json!({"matched":true,"page":page,"ignored":ignored});
                }
                if Instant::now() >= deadline {
                    break json!({"matched":false,"page":page,"ignored":ignored});
                }
                if let Some(next) = page["cursor"].as_str() {
                    cursor = next.into();
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    };
    print_json(&value)
}

#[derive(Args)]
pub struct Agent {
    #[command(subcommand)]
    command: AgentCommand,
}

#[derive(Args)]
struct Installation {
    /// 等同于 --scope user。
    #[arg(long = "global", conflicts_with = "project")]
    global_install: bool,
    /// project 写入项目目录，user 写入用户主目录。
    #[arg(long, default_value = "project")]
    scope: skills::Scope,
    /// 项目目录，默认为当前目录。
    #[arg(long)]
    project: Option<PathBuf>,
    /// 写入技能的后台选择器。
    #[arg(long, default_value = DEFAULT_SERVER)]
    server: String,
}

#[derive(Subcommand)]
enum AgentCommand {
    /// 以原子方式将生成的技能安装到指定 AI 工具目录。
    Install {
        /// 选择 claude-code、codex 或 cursor。
        #[arg(long)]
        agent: skills::Agent,
        #[command(flatten)]
        options: Installation,
    },
    /// 移除完整的托管技能及 QScope 新增的 CLI 权限。
    Uninstall {
        #[arg(long)]
        agent: skills::Agent,
        #[command(flatten)]
        options: Installation,
    },
    /// 检查所有支持的 AI 工具安装路径、版本与哈希。
    Status {
        #[command(flatten)]
        options: Installation,
    },
    /// 生成可粘贴给 AI 助手的安装请求。
    Prompt {
        #[command(flatten)]
        options: Installation,
    },
    /// 输出生成的 SKILL.md，供其他 Agent Skills 客户端安装。
    Skill {
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
    },
}

fn environment(server: String) -> Result<skills::Environment, Error> {
    Ok(skills::Environment {
        workspace: std::env::current_dir()?,
        home: skills::user_home()?,
        cli: std::env::current_exe()?,
        server_url: server,
    })
}

pub fn agent(args: Agent, json_output: bool) -> Result<(), Error> {
    let value = match args.command {
        AgentCommand::Install { agent, options } => {
            let project = options
                .project
                .map(|path| path.to_string_lossy().into_owned());
            serde_json::to_value(environment(options.server)?.install(&skills::SkillRequest {
                agent,
                scope: if options.global_install {
                    skills::Scope::User
                } else {
                    options.scope
                },
                project,
            })?)?
        }
        AgentCommand::Uninstall { agent, options } => {
            let project = options
                .project
                .map(|path| path.to_string_lossy().into_owned());
            serde_json::to_value(environment(options.server)?.uninstall(
                &skills::SkillRequest {
                    agent,
                    scope: if options.global_install {
                        skills::Scope::User
                    } else {
                        options.scope
                    },
                    project,
                },
            )?)?
        }
        AgentCommand::Status { options } => {
            let project = options
                .project
                .map(|path| path.to_string_lossy().into_owned());
            serde_json::to_value(environment(options.server)?.settings(
                if options.global_install {
                    skills::Scope::User
                } else {
                    options.scope
                },
                project.as_deref(),
            )?)?
        }
        AgentCommand::Prompt { options } => {
            let project = options
                .project
                .map(|path| path.to_string_lossy().into_owned());
            let prompt = environment(options.server)?.prompt(
                if options.global_install {
                    skills::Scope::User
                } else {
                    options.scope
                },
                project.as_deref(),
            )?;
            if !json_output {
                println!("{prompt}");
                return Ok(());
            }
            json!({"prompt":prompt})
        }
        AgentCommand::Skill { server } => {
            let content = environment(server)?.skill()?;
            if !json_output {
                print!("{content}");
                return Ok(());
            }
            json!({"content":content})
        }
    };
    print_json(&value)
}

#[derive(Args)]
pub struct Devtools {
    /// 配合 open 使用时返回调试链接。
    #[arg(long, global = true)]
    no_open: bool,
    /// 后台选择器支持 auto、@path/to/service.json 或回环 URL。
    #[arg(long, default_value = DEFAULT_SERVER, global = true)]
    server: String,
    /// 指定研究控制桥，例如 http://127.0.0.1:19860。
    #[arg(long, global = true)]
    endpoint: Option<String>,
    /// 指定本机连接描述文件，其中的令牌按私密数据处理。
    #[arg(long, global = true)]
    connection: Option<PathBuf>,
    /// 仅对仍匹配此 PID 的研究 QQ 操作，PID 来自 devtools windows。
    #[arg(long, global = true, requires = "birth")]
    pid: Option<u32>,
    /// 与 --pid 配套的进程创建时间 FILETIME。
    #[arg(long, global = true, requires = "pid")]
    birth: Option<u64>,
    #[command(subcommand)]
    command: DevtoolsCommand,
}

#[derive(Subcommand)]
enum DevtoolsCommand {
    /// 所选研究控制桥的身份。
    Info,
    /// 列出研究 QQ 窗口的 ID、标题和 URL。
    Windows,
    /// 读取窗口可见文本、交互元素及其选择器。
    Snapshot {
        #[arg(long)]
        window: u32,
    },
    /// 点击 CSS 选择器唯一匹配的可见元素。
    Click {
        #[arg(long)]
        window: u32,
        #[arg(long)]
        selector: String,
    },
    /// 替换 CSS 选择器匹配的输入框内容。
    Fill {
        #[arg(long)]
        window: u32,
        #[arg(long)]
        selector: String,
        #[arg(long)]
        text: String,
    },
    /// 发送单个按键，例如 Enter、Tab 或 Escape。
    Press {
        #[arg(long)]
        window: u32,
        #[arg(long)]
        key: String,
    },
    /// 在指定窗口的渲染进程执行有界表达式。
    Eval {
        #[arg(long)]
        window: u32,
        #[arg(long)]
        expression: String,
    },
    /// 在研究 Electron 主进程执行有界表达式。
    MainEval {
        #[arg(long)]
        expression: String,
    },
    /// 打开窗口的 Chii DevTools，默认选择当前可见窗口。
    Open {
        #[arg(long)]
        window: Option<u32>,
    },
    /// 保存指定窗口的 PNG 截图。
    Screenshot {
        #[arg(long)]
        window: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// 输出研究副本的启动命令与连接文件路径。
    LaunchCommand {
        #[arg(long, default_value = "automation")]
        profile: String,
        #[arg(long, default_value_t = 19860)]
        port: u16,
    },
    /// 启动带抓包及控制桥的受控研究副本。
    Launch {
        #[arg(long, default_value = "automation")]
        profile: String,
        #[arg(long, default_value_t = 19860)]
        port: u16,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=120))]
        wait: u64,
    },
}

fn ensure_backend(server: &str) -> Result<Option<qscope_automation::service::Connection>, Error> {
    if server != DEFAULT_SERVER {
        return Ok(None);
    }
    let service = crate::web::BackendOptions::default().ensure()?;
    Ok(Some(qscope_automation::service::Connection::acquire(
        &qscope_automation::runtime::user_data()?,
        service,
    )?))
}

fn connect(args: &Devtools) -> Result<(devtools::Connection, devtools::SavedConnection), Error> {
    if let Some(path) = &args.connection {
        return Ok((
            devtools::Connection::connect_file(path)?,
            devtools::saved_connection(path)?,
        ));
    }
    let records: Vec<devtools::SavedConnection> = serde_json::from_value(
        Client::new(&args.server)?.get("/api/v1/settings/devtools/connections", &[])?,
    )?;
    let endpoint = args
        .endpoint
        .as_deref()
        .map(qscope_automation::client::loopback_url)
        .transpose()?;
    let mut candidates = records.iter().filter(|record| {
        record.ready
            && endpoint.as_ref().is_none_or(|endpoint| {
                qscope_automation::client::loopback_url(&record.endpoint)
                    .is_ok_and(|value| value == *endpoint)
            })
            && args.pid.is_none_or(|pid| record.pid == pid)
            && args
                .birth
                .is_none_or(|birth| record.birth_filetime == birth.to_string())
    });
    let selected = candidates.next().ok_or_else(|| {
        Error::new(
            "devtools_unavailable",
            "No ready research bridge. Use devtools launch-command or --connection PATH",
        )
    })?;
    if candidates.next().is_some() {
        return Err(Error::new(
            "ambiguous_target",
            "Multiple research bridges are ready. Choose --connection or --endpoint",
        ));
    }
    Ok((
        devtools::Connection::connect_file(Path::new(&selected.status_path))?,
        selected.clone(),
    ))
}

pub fn devtools(args: Devtools) -> Result<(), Error> {
    let _lease =
        if args.connection.is_none() || matches!(args.command, DevtoolsCommand::Open { .. }) {
            ensure_backend(&args.server)?
        } else {
            None
        };
    if let DevtoolsCommand::Open { window } = &args.command {
        let client = Client::new(&args.server)?;
        let value = if window.is_none()
            && args.connection.is_none()
            && args.endpoint.is_none()
            && args.pid.is_none()
        {
            client.post("/api/v1/settings/devtools/current/open", json!({}))?
        } else {
            let (connection, saved) = connect(&args)?;
            let status = connection.status()?;
            let identity = match (args.pid, args.birth) {
                (Some(pid), Some(birth)) => devtools::Identity {
                    pid,
                    birth_filetime: birth.to_string(),
                },
                _ if window.is_none() => devtools::Identity {
                    pid: status.info.pid,
                    birth_filetime: status.info.birth_filetime.clone(),
                },
                _ => {
                    return Err(Error::new(
                        "process_identity",
                        "Supply --pid and --birth from the current bridge info",
                    ));
                }
            };
            let window = match window {
                Some(id) => *id,
                None => devtools::preferred_window(&status.windows)?.id,
            };
            client.post(
                "/api/v1/settings/devtools/open",
                json!({"connection_id":saved.id,"window_id":window,"identity":identity}),
            )?
        };
        if !args.no_open
            && let Some(url) = value["frontend_url"].as_str()
        {
            open::that_detached(url)
                .map_err(|error| Error::new("devtools_open", error.to_string()))?;
        }
        return print_json(&value);
    }
    let launch_options = match &args.command {
        DevtoolsCommand::LaunchCommand { profile, port } => Some((profile, port, None)),
        DevtoolsCommand::Launch {
            profile,
            port,
            wait,
        } => Some((profile, port, Some(*wait))),
        _ => None,
    };
    if let Some((profile, port, wait)) = launch_options {
        let command = Client::new(&args.server)?.get(
            "/api/v1/settings/devtools/launch",
            &[
                ("profile".into(), profile.clone()),
                ("port".into(), port.to_string()),
            ],
        )?;
        return print_json(&if let Some(wait) = wait {
            Client::new(&args.server)?.post_with_timeout(
                "/api/v1/settings/devtools/launch",
                json!({"profile":profile,"port":port}),
                Duration::from_secs(wait + 100),
            )?
        } else {
            command
        });
    }
    let (connection, _) = connect(&args)?;
    let identity = args
        .pid
        .zip(args.birth)
        .map(|(pid, birth)| devtools::Identity {
            pid,
            birth_filetime: birth.to_string(),
        });
    let value = match args.command {
        DevtoolsCommand::Info => serde_json::to_value(&connection.info)?,
        DevtoolsCommand::Windows => serde_json::to_value(connection.status()?)?,
        DevtoolsCommand::Snapshot { window } => {
            connection.run(json!({"action":"snapshot","window_id":window}), None)?
        }
        DevtoolsCommand::Click { window, selector } => connection.run(
            json!({"action":"click","window_id":window,"selector":selector}),
            identity.as_ref(),
        )?,
        DevtoolsCommand::Fill {
            window,
            selector,
            text,
        } => connection.run(
            json!({"action":"fill","window_id":window,"selector":selector,"text":text}),
            identity.as_ref(),
        )?,
        DevtoolsCommand::Press { window, key } => connection.run(
            json!({"action":"press","window_id":window,"key":key}),
            identity.as_ref(),
        )?,
        DevtoolsCommand::Eval { window, expression } => connection.run(
            json!({"action":"eval","window_id":window,"expression":expression}),
            identity.as_ref(),
        )?,
        DevtoolsCommand::MainEval { expression } => connection.run(
            json!({"action":"main-eval","expression":expression}),
            identity.as_ref(),
        )?,
        DevtoolsCommand::Screenshot { window, out } => {
            use base64::Engine;
            let image = connection.run(json!({"action":"screenshot","window_id":window}), None)?;
            let encoded = image["png_base64"]
                .as_str()
                .ok_or_else(|| Error::new("screenshot", "Bridge did not return PNG bytes"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| Error::new("screenshot", error.to_string()))?;
            if bytes.len() > 8 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(Error::new("screenshot", "Invalid or oversized PNG"));
            }
            let parent = out
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&bytes)?;
            temporary
                .persist_noclobber(&out)
                .map_err(|error| Error::from(error.error))?;
            json!({"path":out,"width":image["width"],"height":image["height"],"bytes":bytes.len()})
        }
        DevtoolsCommand::LaunchCommand { .. }
        | DevtoolsCommand::Launch { .. }
        | DevtoolsCommand::Open { .. } => {
            unreachable!("handled above")
        }
    };
    print_json(&value)
}
