use crate::{ApiError, AppState, automation, blocking, lock_error};
use axum::{Json, extract::State, http::HeaderMap};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub struct Manager {
    clients: BTreeMap<String, Instant>,
    pinned: bool,
    stopped: bool,
    idle_since: Instant,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            pinned: false,
            stopped: false,
            idle_since: Instant::now(),
        }
    }
}

impl Manager {
    pub fn idle(&mut self) -> bool {
        self.clients
            .retain(|_, seen| seen.elapsed() < Duration::from_secs(45));
        if self.pinned || !self.clients.is_empty() {
            self.idle_since = Instant::now();
            return false;
        }
        self.idle_since.elapsed() >= Duration::from_secs(30)
    }
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    pub fn stop(&mut self) {
        self.stopped = true;
    }
}

#[derive(Deserialize)]
pub struct Command {
    action: String,
    client_id: String,
}

fn trusted_controller(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.contains_key("origin") {
        return Err(ApiError::new("forbidden", "此操作需要本机控制入口"));
    }
    Ok(())
}

pub async fn active_tasks(state: AppState) -> Result<bool, ApiError> {
    if state.onboarding.lock().map_err(|_| lock_error())?.busy()
        || state
            .recordings
            .lock()
            .map_err(|_| lock_error())?
            .list()
            .iter()
            .any(|job| job.state == "recording")
    {
        return Ok(true);
    }
    blocking(move || {
        for saved in automation::saved_connections(&state)?
            .into_iter()
            .filter(|saved| saved.ready)
            .take(16)
        {
            if qscope_automation::devtools::Connection::connect_file(std::path::Path::new(
                &saved.status_path,
            ))
            .is_ok()
            {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
}

pub async fn command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Command>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(
        request.action.as_str(),
        "acquire" | "heartbeat" | "release" | "status"
    ) {
        trusted_controller(&headers)?;
    }
    if request.client_id.len() != 48
        || !request
            .client_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::new("query", "Invalid backend client identity"));
    }
    // Updates and the installer end recordings and research QQ before replacing the program.
    if request.action == "close_tasks" {
        return crate::update::close_tasks(&state).await.map(Json);
    }
    if request.action == "stop" && active_tasks(state.clone()).await? {
        return Err(ApiError::new(
            "active_tasks",
            "研究 QQ 或录制仍在运行，请先结束对应任务",
        ));
    }
    let mut control = state.control.lock().map_err(|_| lock_error())?;
    match request.action.as_str() {
        "acquire" => {
            if control.clients.len() >= 64 {
                return Err(ApiError::new(
                    "client_limit",
                    "Backend client limit exceeded",
                ));
            }
            control.clients.insert(request.client_id, Instant::now());
        }
        "heartbeat" => {
            let client = control
                .clients
                .get_mut(&request.client_id)
                .ok_or_else(|| ApiError::new("client_expired", "Backend connection expired"))?;
            *client = Instant::now();
        }
        "release" => {
            control.clients.remove(&request.client_id);
        }
        "pin" => control.pinned = true,
        "stop" => control.stopped = true,
        "status" => {}
        _ => return Err(ApiError::new("query", "Unsupported backend control action")),
    }
    Ok(Json(
        json!({"schema_version":1,"clients":control.clients.len(),"pinned":control.pinned,"stopping":control.stopped}),
    ))
}

#[derive(Deserialize)]
pub struct Roots {
    roots: Vec<std::path::PathBuf>,
}

pub async fn roots(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Roots>,
) -> Result<Json<Value>, ApiError> {
    trusted_controller(&headers)?;
    if request.roots.len() > 32 {
        return Err(ApiError::new(
            "query",
            "At most 32 capture roots may be added",
        ));
    }
    blocking(move || {
        let paths = request
            .roots
            .into_iter()
            .map(|path| {
                path.canonicalize()
                    .map_err(|error| ApiError::new("capture_root", error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if paths.iter().any(|path| {
            !path.is_dir()
                && !(path.is_file()
                    && path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("qscap")))
        }) {
            return Err(ApiError::new(
                "capture_root",
                "Capture roots must be directories or .qscap files",
            ));
        }
        let files = paths
            .iter()
            .filter(|path| path.is_file())
            .map(|path| crate::capture::Capture::open(path.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut library = state.library.lock().map_err(|_| lock_error())?;
        library.check_capacity(&files)?;
        let additions: Vec<_> = paths
            .into_iter()
            .filter(|path| !library.roots.contains(path))
            .collect();
        if library.roots.len() + additions.len() > 64 {
            return Err(ApiError::new(
                "capture_root",
                "Backend capture root limit exceeded",
            ));
        }
        let mut roots = library.roots.clone();
        roots.extend(additions);
        let settings = state.runtime.storage.join("capture-roots.json");
        qscope_automation::runtime::no_links(&settings)?;
        qscope_automation::runtime::write_settings(
            &settings,
            &json!({"schema_version":1,"roots":roots}),
        )?;
        library.roots = roots;
        let mut opened = Vec::new();
        for capture in files {
            opened.push(
                library
                    .insert(capture)?
                    .lock()
                    .map_err(|_| lock_error())?
                    .summary(),
            );
        }
        Ok(Json(json!({"roots":library.roots,"opened":opened})))
    })
    .await
}
