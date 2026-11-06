use crate::{
    ApiError, AppState, automation, blocking,
    contract::{QQSetup, RuntimeComponent, RuntimeHealth},
    control, lock_error,
};
use axum::{Json, extract::State};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct Manager {
    progress: Option<PathBuf>,
    busy: bool,
    error: Option<String>,
    task: Option<tokio::task::JoinHandle<()>>,
    stopping: bool,
    // QQ started from a QScope page leaves with the desktop; the CLI and AI keep their own.
    page_task: bool,
    page_owned: Option<(u32, String)>,
    released: bool,
}

impl Manager {
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn cancel(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        self.stopping = true;
        let task = self.task.take()?;
        task.abort();
        Some(task)
    }
}

fn read_progress(path: &Path) -> Option<Value> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

// The probe comes from the build's native directory (a development build) or the embedded
// resources (a release), which are written to the data directory on launch.
fn probe_source(state: &AppState) -> bool {
    state
        .runtime
        .native
        .as_ref()
        .is_some_and(|native| native.join("qscope-portable.dll").is_file())
        || state
            .assets
            .as_ref()
            .is_some_and(|assets| assets.contains("native/qscope-portable.dll"))
}

fn snapshot(state: &AppState) -> Result<QQSetup, ApiError> {
    let identity: Value =
        serde_json::from_str(include_str!("../../../../tools/qq-portable/client.json"))
            .map_err(|error| ApiError::new("setup", error.to_string()))?;
    let root = state.runtime.package();
    let manager = state.onboarding.lock().map_err(|_| lock_error())?;
    let mut status = QQSetup {
        build: identity["build"].as_str().unwrap_or_default().into(),
        installed: root.join("unpacked/Files/versions/config.json").is_file(),
        available: cfg!(all(windows, target_arch = "x86_64"))
            && state.runtime.host().is_ok()
            && probe_source(state),
        stage: if manager.busy { "preparing" } else { "idle" }.into(),
        message: if manager.busy {
            "正在准备 QQ"
        } else {
            "启动后扫码登录，流量会自动记录"
        }
        .into(),
        downloaded: "0".into(),
        total: identity["installer_bytes"].to_string(),
        pid: None,
        session_id: None,
        error: manager.error.clone(),
    };
    if manager.busy
        && let Some(path) = &manager.progress
        && let Some(progress) = read_progress(path)
    {
        for (target, key) in [
            (&mut status.stage, "stage"),
            (&mut status.message, "message"),
            (&mut status.downloaded, "downloaded"),
        ] {
            if let Some(value) = progress[key].as_str() {
                *target = value.chars().take(2000).collect();
            }
        }
    }
    if status.error.is_some() {
        status.stage = "error".into();
    }
    let idle = !manager.busy && manager.error.is_none();
    drop(manager);
    if idle && let Ok(Some(connection)) = automation::current_connection(state) {
        status.stage = "running".into();
        status.message = "QQ 已运行，流量正在记录".into();
        status.pid = Some(connection.pid);
        status.error = None;
        status.session_id = state
            .library
            .lock()
            .map_err(|_| lock_error())?
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| {
                session.pid == connection.pid
                    && session.process_birth_filetime == connection.birth_filetime
            })
            .map(|session| session.id);
    }
    Ok(status)
}

pub async fn status(State(state): State<AppState>) -> Result<Json<QQSetup>, ApiError> {
    blocking(move || snapshot(&state).map(Json)).await
}

#[derive(serde::Deserialize)]
pub struct SetupRequest {
    #[serde(default)]
    prepare_only: bool,
    installer: Option<PathBuf>,
    #[serde(default = "default_profile")]
    profile: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(skip)]
    from_page: bool,
}
fn default_profile() -> String {
    "automation".into()
}
fn default_port() -> u16 {
    19860
}
impl Default for SetupRequest {
    fn default() -> Self {
        Self {
            prepare_only: false,
            installer: None,
            profile: default_profile(),
            port: default_port(),
            from_page: false,
        }
    }
}

pub async fn start(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Option<Json<SetupRequest>>,
) -> Result<Json<QQSetup>, ApiError> {
    let mut request = request.map(|value| value.0).unwrap_or_default();
    qscope_automation::launcher::validate_profile(&request.profile, request.port)?;
    request.from_page = headers.contains_key("origin");
    if request.installer.is_some() && request.from_page {
        return Err(ApiError::new(
            "forbidden",
            "本地安装包路径需要通过 CLI 指定",
        ));
    }
    let mut current = blocking({
        let state = state.clone();
        move || snapshot(&state)
    })
    .await?;
    let saved = state
        .runtime
        .package()
        .join("portable/profiles")
        .join(&request.profile)
        .join(".qscope/devtools.json");
    let existing = blocking(move || {
        Ok(
            qscope_automation::devtools::Connection::connect_file(&saved)
                .ok()
                .map(|connection| connection.info.pid),
        )
    })
    .await?;
    if request.installer.is_none()
        && (existing.is_some() || (request.prepare_only && current.installed))
    {
        if let Some(pid) = existing {
            current.pid = Some(pid);
        }
        return Ok(Json(current));
    }
    if !current.available {
        return Err(ApiError::new(
            "setup_missing",
            "QQ 运行组件不完整，请在设置中运行环境检查并修复",
        ));
    }
    spawn_prepare(&state, request)?;
    blocking(move || snapshot(&state).map(Json)).await
}

fn spawn_prepare(state: &AppState, request: SetupRequest) -> Result<(), ApiError> {
    let mut manager = state.onboarding.lock().map_err(|_| lock_error())?;
    if manager.stopping {
        return Err(ApiError::new("setup_stopping", "桌面服务正在关闭"));
    }
    if manager.busy {
        return Ok(());
    }
    manager.busy = true;
    manager.error = None;
    manager.progress = None;
    manager.page_task = request.from_page;
    manager.released = false;
    let task_state = state.clone();
    manager.task = Some(tokio::spawn(async move {
        let result = prepare(&task_state, request).await;
        if let Ok(mut manager) = task_state.onboarding.lock() {
            manager.busy = false;
            manager.error = result.err().map(|error| error.message);
        }
    }));
    Ok(())
}

fn component(
    id: &str,
    label: &str,
    state: &str,
    detail: impl Into<String>,
    repair: &str,
) -> RuntimeComponent {
    RuntimeComponent {
        id: id.into(),
        label: label.into(),
        state: state.into(),
        detail: detail.into(),
        repair: repair.into(),
    }
}

fn platform() -> RuntimeComponent {
    if cfg!(all(windows, target_arch = "x86_64")) {
        component("platform", "系统", "ok", "Windows x64", "none")
    } else {
        component(
            "platform",
            "系统",
            "unsupported",
            "QQ 采集只支持 Windows x64",
            "none",
        )
    }
}

fn host(state: &AppState) -> RuntimeComponent {
    match state.runtime.host() {
        Ok(path) => component(
            "host",
            "QScope CLI",
            "ok",
            qscope_automation::display_path(&path),
            "none",
        ),
        Err(_) if state.assets.is_some() => component(
            "host",
            "QScope CLI",
            "missing",
            "安装目录缺少 qscope-cli.exe，需要重新安装 QScope",
            "reinstall",
        ),
        Err(_) => component(
            "host",
            "QScope CLI",
            "missing",
            "开发构建缺少 qscope-cli，先运行 cargo build -p qscope-cli",
            "build",
        ),
    }
}

fn probe(state: &AppState) -> RuntimeComponent {
    const LABEL: &str = "采集探针";
    let Some(assets) = &state.assets else {
        return match state
            .runtime
            .native
            .as_ref()
            .map(|native| native.join("qscope-portable.dll"))
        {
            Some(path) if path.is_file() => component(
                "probe",
                LABEL,
                "ok",
                format!("开发构建 {}", qscope_automation::display_path(&path)),
                "none",
            ),
            _ => component(
                "probe",
                LABEL,
                "missing",
                "开发构建还没有编译采集器，运行 node tools/build-native.ts 后重新检查",
                "build",
            ),
        };
    };
    if !assets.contains("native/qscope-portable.dll") {
        return component(
            "probe",
            LABEL,
            "missing",
            "这个 QScope 安装包不含采集探针，需要重新下载安装",
            "reinstall",
        );
    }
    match assets.check_native(&state.runtime.storage) {
        Ok(check) if !check.changed.is_empty() => component(
            "probe",
            LABEL,
            "damaged",
            format!(
                "{} 个文件与安装包不一致：{}",
                check.changed.len(),
                check.changed[0]
            ),
            "local",
        ),
        Ok(check) if check.written && !check.missing.is_empty() => component(
            "probe",
            LABEL,
            "missing",
            format!(
                "{} 个文件已被移除：{}",
                check.missing.len(),
                check.missing[0]
            ),
            "local",
        ),
        Ok(check) if !check.written => component(
            "probe",
            LABEL,
            "pending",
            "首次启动 QQ 时写入数据目录",
            "none",
        ),
        Ok(_) => component("probe", LABEL, "ok", "与安装包一致", "none"),
        Err(error) => component("probe", LABEL, "damaged", error.message, "local"),
    }
}

fn installer_name(identity: &qscope_qqnt::package::Identity) -> Option<String> {
    identity
        .installer_url
        .rsplit('/')
        .next()
        .filter(|name| name.ends_with(".exe") && !name.contains(['\\', ':']))
        .map(str::to_string)
}

fn package(state: &AppState) -> RuntimeComponent {
    const LABEL: &str = "QQ 客户端";
    let identity = match qscope_qqnt::package::identity() {
        Ok(identity) => identity,
        Err(error) => return component("qq", LABEL, "damaged", error.message, "reinstall"),
    };
    let root = state.runtime.package();
    let unpacked = root.join("unpacked");
    if !unpacked.exists() {
        let downloaded = installer_name(&identity).is_some_and(|name| root.join(name).is_file());
        return component(
            "qq",
            LABEL,
            "pending",
            if downloaded {
                "官方安装包已下载，首次启动时解包"
            } else {
                "首次启动时下载官方安装包（约 319 MiB）并校验"
            },
            "none",
        );
    }
    match qscope_qqnt::package::verify_client(&unpacked.join("Files"), &identity) {
        Ok(()) => component(
            "qq",
            LABEL,
            "ok",
            format!("QQ {} 模块校验通过", identity.build),
            "none",
        ),
        Err(error) => component(
            "qq",
            LABEL,
            "damaged",
            format!("解包文件校验失败（{}），可以重新解包", error.message),
            "local",
        ),
    }
}

fn inspect(state: &AppState) -> RuntimeHealth {
    let components = vec![platform(), host(state), probe(state), package(state)];
    RuntimeHealth {
        healthy: components
            .iter()
            .all(|item| matches!(item.state.as_str(), "ok" | "pending")),
        repairable: components.iter().any(|item| item.repair == "local"),
        preparing: false,
        components,
    }
}

pub async fn health(State(state): State<AppState>) -> Result<Json<RuntimeHealth>, ApiError> {
    blocking(move || Ok(Json(inspect(&state)))).await
}

const SECURITY_HINT: &str = "写入后又被移除，通常是安全软件拦截了采集探针。请在 Windows 安全中心的「保护历史记录」中还原，并把 QScope 数据目录加入排除项后再修复。";

/// Fixes what can be fixed in place: rewrites the collector files from the bundle, and removes
/// damaged QQ files so the official installer is checked and unpacked again.
pub async fn repair(State(state): State<AppState>) -> Result<Json<RuntimeHealth>, ApiError> {
    if control::active_tasks(state.clone()).await? {
        return Err(ApiError::new(
            "active_tasks",
            "请先停止研究 QQ 和录制，再修复运行组件",
        ));
    }
    let before = blocking({
        let state = state.clone();
        move || Ok(inspect(&state))
    })
    .await?;
    let local = |id: &str| {
        before
            .components
            .iter()
            .any(|item| item.id == id && item.repair == "local")
    };
    let (fix_probe, fix_package) = (local("probe"), local("qq"));
    let task = state.clone();
    let probe_error = blocking(move || {
        let mut probe_error = None;
        if fix_probe && let Some(assets) = &task.assets {
            probe_error = assets.repair_native(&task.runtime.storage).err();
            // Security software usually removes the file shortly after it is written.
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        if fix_package {
            let identity = qscope_qqnt::package::identity()?;
            let root = task.runtime.package();
            let unpacked = root.join("unpacked");
            qscope_automation::runtime::no_links(&unpacked)?;
            if unpacked.exists() {
                std::fs::remove_dir_all(&unpacked).map_err(|error| {
                    ApiError::new("repair", format!("无法删除损坏的解包目录：{error}"))
                })?;
            }
            if let Some(name) = installer_name(&identity) {
                let installer = root.join(name);
                qscope_automation::runtime::no_links(&installer)?;
                let io = qscope_automation::Error::from;
                if installer.is_file()
                    && (std::fs::metadata(&installer).map_err(io)?.len()
                        != identity.installer_bytes
                        || qscope_qqnt::package::file_sha256(&installer)?
                            != identity.installer_sha256)
                {
                    std::fs::remove_file(&installer).map_err(io)?;
                }
            }
        }
        Ok(probe_error)
    })
    .await?;
    if fix_package {
        spawn_prepare(
            &state,
            SetupRequest {
                prepare_only: true,
                ..SetupRequest::default()
            },
        )?;
    }
    let mut after = blocking({
        let state = state.clone();
        move || Ok(inspect(&state))
    })
    .await?;
    after.preparing = fix_package;
    if fix_probe
        && let Some(item) = after.components.iter_mut().find(|item| item.id == "probe")
        && item.state != "ok"
    {
        item.state = "blocked".into();
        item.detail = match probe_error {
            Some(error) => format!("{SECURITY_HINT}（{}）", error.message),
            None => SECURITY_HINT.into(),
        };
        after.healthy = false;
    }
    Ok(Json(after))
}

async fn prepare(state: &AppState, request: SetupRequest) -> Result<(), ApiError> {
    let root = state.runtime.storage.join("automation/setup");
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|error| ApiError::new("setup", error.to_string()))?;
    let directory = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(root)
        .map_err(|error| ApiError::new("setup", error.to_string()))?
        .keep();
    let progress = directory.join("manifest.json");
    state.onboarding.lock().map_err(|_| lock_error())?.progress = Some(progress.clone());
    let package = state.runtime.package();
    let status = progress.clone();
    let cancelled = state.stopping.clone();
    blocking(move || {
        qscope_qqnt::prepare::run(&package, &status, request.installer.as_deref(), &cancelled)
            .map_err(Into::into)
    })
    .await?;
    if request.prepare_only || released(state)? {
        return Ok(());
    }
    let mut record = read_progress(&progress).unwrap_or_else(|| json!({}));
    record["stage"] = json!("starting");
    record["message"] = json!("正在启动隔离 QQ 并启用抓包");
    tokio::fs::write(
        &progress,
        serde_json::to_vec(&record).expect("progress JSON"),
    )
    .await
    .map_err(|error| ApiError::new("setup", error.to_string()))?;
    let Json(launched) = automation::launch(
        State(state.clone()),
        Json(crate::contract::DevtoolsLaunchRequest {
            profile: request.profile.clone(),
            port: request.port,
        }),
    )
    .await?;
    if request.from_page
        && let (Some(pid), Some(birth)) = (
            launched["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok()),
            launched["process_birth_filetime"].as_str(),
        )
    {
        state
            .onboarding
            .lock()
            .map_err(|_| lock_error())?
            .page_owned = Some((pid, birth.into()));
    }
    // The page that asked for this copy closed while it was starting.
    if released(state)? {
        let connection = state
            .runtime
            .package()
            .join("portable/profiles")
            .join(&request.profile)
            .join(".qscope/devtools.json");
        close(state, connection, request.profile).await?;
        return Ok(());
    }
    record["stage"] = json!("running");
    record["launch"] = launched;
    tokio::fs::write(
        &progress,
        serde_json::to_vec(&record).expect("progress JSON"),
    )
    .await
    .map_err(|error| ApiError::new("setup", error.to_string()))?;
    Ok(())
}

// The connection file sits at profiles/<profile>/.qscope/devtools.json.
pub(crate) fn profile_of(connection: &Path) -> Result<String, ApiError> {
    connection
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::new("qq_identity", "无法确定研究 QQ 的 profile"))
}

fn released(state: &AppState) -> Result<bool, ApiError> {
    Ok(state.onboarding.lock().map_err(|_| lock_error())?.released)
}

// Closes one research copy through its owner and records the exit, so the status shown next
// does not wait for the producer check to notice.
pub(crate) async fn close(
    state: &AppState,
    connection: PathBuf,
    profile: String,
) -> Result<Value, ApiError> {
    let launches = state.runtime.storage.join("automation/launches");
    let stopped = blocking(move || {
        qscope_automation::launcher::stop(&launches, &connection, &profile).map_err(Into::into)
    })
    .await?;
    if let (Some(pid), Some(birth)) = (stopped["pid"].as_u64(), stopped["birth_filetime"].as_str())
    {
        let sessions = state
            .library
            .lock()
            .map_err(|_| lock_error())?
            .snapshot()
            .sessions;
        let mut processes = state.processes.lock().await;
        for session in sessions.iter().filter(|session| {
            u64::from(session.pid) == pid && session.process_birth_filetime == birth
        }) {
            processes.exited(&session.id, birth);
        }
    }
    Ok(stopped)
}

#[derive(Default, serde::Deserialize)]
pub struct StopRequest {
    /// Only a copy started from a QScope page; QQ started by the CLI or an AI keeps running.
    #[serde(default)]
    page_only: bool,
}

/// Closes the research QQ shown in the settings. A startup still in progress is not launched.
pub async fn stop(
    State(state): State<AppState>,
    request: Option<Json<StopRequest>>,
) -> Result<Json<QQSetup>, ApiError> {
    let page_only = request.map(|value| value.0).unwrap_or_default().page_only;
    let owned = {
        let mut manager = state.onboarding.lock().map_err(|_| lock_error())?;
        if manager.busy && (!page_only || manager.page_task) {
            manager.released = true;
        }
        manager.page_owned.clone()
    };
    let target = blocking({
        let state = state.clone();
        move || {
            let saved = if page_only {
                let Some((pid, birth)) = owned else {
                    return Ok(None);
                };
                automation::saved_connections(&state)?
                    .into_iter()
                    .find(|saved| saved.ready && saved.pid == pid && saved.birth_filetime == birth)
            } else {
                automation::current_connection(&state)?
            };
            let Some(saved) = saved else {
                return Ok(None);
            };
            let path = PathBuf::from(saved.status_path);
            let profile = profile_of(&path)?;
            Ok(Some((path, profile)))
        }
    })
    .await?;
    if let Some((connection, profile)) = target {
        close(&state, connection, profile).await?;
    }
    blocking(move || snapshot(&state).map(Json)).await
}
