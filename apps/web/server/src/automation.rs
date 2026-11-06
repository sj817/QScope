use crate::{ApiError, AppState, blocking, contract, lock_error};
use axum::{
    Json,
    extract::{Query, State},
};
use qscope_automation::{devtools, launcher, skills};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

impl From<qscope_automation::Error> for ApiError {
    fn from(error: qscope_automation::Error) -> Self {
        ApiError::new(&error.code, error.message)
    }
}

#[derive(Deserialize)]
pub struct SettingsQuery {
    #[serde(default)]
    scope: skills::Scope,
    project: Option<String>,
}

pub async fn settings(
    State(state): State<AppState>,
    Query(query): Query<SettingsQuery>,
) -> Result<Json<skills::IntegrationSettings>, ApiError> {
    blocking(move || {
        let mut settings = state
            .automation
            .settings(query.scope, query.project.as_deref())
            .map_err(ApiError::from)?;
        settings.research_launcher = state
            .runtime
            .host()
            .ok()
            .map(|path| qscope_automation::display_path(&path));
        Ok(Json(settings))
    })
    .await
}

pub async fn install(
    State(state): State<AppState>,
    Json(request): Json<skills::SkillRequest>,
) -> Result<Json<skills::SkillStatus>, ApiError> {
    blocking(move || {
        state
            .automation
            .install(&request)
            .map(Json)
            .map_err(Into::into)
    })
    .await
}

pub async fn uninstall(
    State(state): State<AppState>,
    Json(request): Json<skills::SkillRequest>,
) -> Result<Json<skills::SkillStatus>, ApiError> {
    blocking(move || {
        state
            .automation
            .uninstall(&request)
            .map(Json)
            .map_err(Into::into)
    })
    .await
}

pub(crate) fn saved_connections(
    state: &AppState,
) -> Result<Vec<devtools::SavedConnection>, ApiError> {
    let mut sessions = state
        .library
        .lock()
        .map_err(|_| lock_error())?
        .snapshot()
        .sessions;
    state
        .processes
        .blocking_lock()
        .annotate_cached(&mut sessions, &state.imports);
    let identities: BTreeSet<_> = sessions
        .iter()
        .filter(|session| session.producer_alive != Some(false))
        .map(|session| (session.pid, session.process_birth_filetime.as_str()))
        .collect();
    let mut paths = BTreeSet::new();
    for session in &sessions {
        let path = Path::new(&session.path);
        if path.starts_with(&state.imports) {
            continue;
        }
        if let Some(directory) = path
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name == ".qscope"))
        {
            paths.insert(directory.join("devtools.json"));
        }
    }
    let mut connections = BTreeMap::new();
    for path in paths {
        if let Ok(connection) = devtools::saved_connection(&path)
            && identities.contains(&(connection.pid, connection.birth_filetime.as_str()))
        {
            connections.insert(connection.id.clone(), connection);
        }
    }
    Ok(connections.into_values().collect())
}

fn selected_connection(state: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    saved_connections(state)?
        .into_iter()
        .find(|connection| connection.id == id)
        .map(|connection| PathBuf::from(connection.status_path))
        .ok_or_else(|| ApiError::new("not_found", "研究副本连接不存在，请刷新连接列表"))
}

pub async fn connections(
    State(state): State<AppState>,
) -> Result<Json<Vec<devtools::SavedConnection>>, ApiError> {
    blocking(move || saved_connections(&state).map(Json)).await
}

pub async fn connect(
    State(state): State<AppState>,
    Json(request): Json<devtools::ConnectRequest>,
) -> Result<Json<devtools::Status>, ApiError> {
    blocking(move || {
        let connection = devtools::Connection::connect_file(&selected_connection(
            &state,
            &request.connection_id,
        )?)?;
        connection.status().map(Json).map_err(Into::into)
    })
    .await
}

pub async fn open(
    State(state): State<AppState>,
    Json(request): Json<devtools::OpenRequest>,
) -> Result<Json<contract::DevtoolsFrontend>, ApiError> {
    blocking(move || open_window(&state, request).map(Json)).await
}

fn open_window(
    state: &AppState,
    request: devtools::OpenRequest,
) -> Result<contract::DevtoolsFrontend, ApiError> {
    let connection =
        devtools::Connection::connect_file(&selected_connection(state, &request.connection_id)?)?;
    if connection.info.pid != request.identity.pid
        || connection.info.birth_filetime != request.identity.birth_filetime
    {
        return Err(ApiError::new(
            "process_identity",
            "QQ 进程身份已改变，请刷新连接",
        ));
    }
    let frontend = state.inspector.lock().map_err(|_| lock_error())?.ensure(
        &state.runtime,
        &state.host,
        state.assets.as_ref(),
    )?;
    let bytes = std::fs::read(&frontend.client_path)
        .map_err(|error| ApiError::new("inspector", error.to_string()))?;
    let result = connection.run(
        json!({
            "action": "inspect", "window_id": request.window_id,
            "frontend_url": format!("{}{}", frontend.origin, frontend.base_path),
            "client_path": frontend.client_path,
            "client_sha256": qscope_core::data::sha256(&bytes),
        }),
        Some(&request.identity),
    )?;
    let target = result["target_id"]
        .as_str()
        .filter(|value| {
            value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
        .ok_or_else(|| ApiError::new("inspector", "调试目标未返回有效 ID"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match frontend.response(target) {
            Ok(response) => return Ok(response),
            Err(error)
                if error.code == "inspector_pending" && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100))
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn current_connection(
    state: &AppState,
) -> Result<Option<devtools::SavedConnection>, ApiError> {
    let mut connections = saved_connections(state)?
        .into_iter()
        .filter(|connection| connection.ready);
    let selected = connections.next();
    if connections.next().is_some() {
        return Err(ApiError::new(
            "ambiguous_target",
            "多个研究 QQ 正在运行，请明确选择连接",
        ));
    }
    Ok(selected)
}

pub async fn open_current(
    State(state): State<AppState>,
) -> Result<Json<contract::DevtoolsFrontend>, ApiError> {
    blocking(move || {
        let saved = current_connection(&state)?
            .ok_or_else(|| ApiError::new("qq_not_running", "请先启动 QQ"))?;
        let connection = devtools::Connection::connect_file(Path::new(&saved.status_path))?;
        let status = connection.status()?;
        let window = devtools::preferred_window(&status.windows)?;
        open_window(
            &state,
            devtools::OpenRequest {
                connection_id: saved.id,
                window_id: window.id,
                identity: devtools::Identity {
                    pid: status.info.pid,
                    birth_filetime: status.info.birth_filetime,
                },
            },
        )
        .map(Json)
    })
    .await
}

fn default_profile() -> String {
    "automation".into()
}
fn default_port() -> u16 {
    19860
}

#[derive(Deserialize)]
pub struct LaunchQuery {
    #[serde(default = "default_profile")]
    profile: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn launch_specification(
    state: &AppState,
    profile: String,
    port: u16,
) -> Result<contract::DevtoolsLaunch, ApiError> {
    launcher::validate_profile(&profile, port)?;
    let host = state.runtime.host()?;
    let request = state.runtime.request(&profile, port)?;
    let connection = state
        .runtime
        .package()
        .join("portable/profiles")
        .join(&profile)
        .join(".qscope/devtools.json");
    let command = format!(
        "& '{}' --json devtools --server '{}' launch --profile '{}' --port {port}",
        qscope_automation::display_path(&state.automation.cli).replace('\'', "''"),
        state.automation.server_url.replace('\'', "''"),
        profile
    );
    Ok(contract::DevtoolsLaunch {
        launch_version: 3,
        profile,
        endpoint: format!("http://127.0.0.1:{port}"),
        host_path: qscope_automation::display_path(&host),
        command,
        connection_file: qscope_automation::display_path(&connection),
        log_directory: qscope_automation::display_path(
            &state.runtime.storage.join("automation/launches"),
        ),
        resources_directory: request.resources_directory,
        data_directory: request.data_directory,
        package_directory: request.package_directory,
        prebuilt_directory: request.prebuilt_directory,
    })
}

pub async fn launch_command(
    State(state): State<AppState>,
    Query(query): Query<LaunchQuery>,
) -> Result<Json<contract::DevtoolsLaunch>, ApiError> {
    blocking(move || launch_specification(&state, query.profile, query.port).map(Json)).await
}

pub async fn launch(
    State(state): State<AppState>,
    Json(request): Json<contract::DevtoolsLaunchRequest>,
) -> Result<Json<Value>, ApiError> {
    if let Some(assets) = &state.assets {
        assets.prepare_native(&state.runtime.storage)?;
    }
    let specification = launch_specification(&state, request.profile, request.port)?;
    blocking(move || {
        launcher::launch(&specification, 90, &state.stopping)
            .map(Json)
            .map_err(Into::into)
    })
    .await
}
