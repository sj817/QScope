mod capture;
mod catalog;
mod compare;
mod contract;
mod control;
mod decode;
mod field;
mod filters;
mod index;
mod inspector;
mod merged;
mod onboarding;
mod process;
mod recording;
#[cfg(test)]
mod tests;
mod update;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contract::{ApiError, Catalog, EventPage, PacketDetail, Session};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::BufReader,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
mod automation;
use tokio::io::AsyncWriteExt;

#[derive(Clone)]
struct AppState {
    library: Arc<Mutex<catalog::Library>>,
    ui: PathBuf,
    assets: Option<qscope_automation::bundle::Resources>,
    token: String,
    control: Arc<Mutex<control::Manager>>,
    imports: PathBuf,
    host: String,
    processes: Arc<tokio::sync::Mutex<process::Probe>>,
    recordings: Arc<Mutex<recording::Manager>>,
    automation: qscope_automation::skills::Environment,
    inspector: Arc<Mutex<inspector::Manager>>,
    onboarding: Arc<Mutex<onboarding::Manager>>,
    update: Arc<Mutex<update::Manager>>,
    runtime: qscope_automation::runtime::Layout,
    instance_id: String,
    stopping: Arc<AtomicBool>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.code.as_str() {
            "not_found" => StatusCode::NOT_FOUND,
            "forbidden" => StatusCode::FORBIDDEN,
            "file_limit" | "integration_limit" => StatusCode::PAYLOAD_TOO_LARGE,
            "skill_conflict" | "skill_changed" | "install_busy" | "permissions_config"
            | "launch_busy" => StatusCode::CONFLICT,
            "query"
            | "range"
            | "format"
            | "native_header"
            | "native_partial_tail"
            | "empty_upload" => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self)).into_response()
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ApiError::new("worker", e.to_string()))?
}
fn lock_error() -> ApiError {
    ApiError::new("worker", "会话锁不可用，请重新启动本地服务")
}

async fn comparison(
    State(state): State<AppState>,
    Json(query): Json<contract::CompareRequest>,
) -> Result<Json<contract::Comparison>, ApiError> {
    blocking(move || {
        let library = state.library.lock().map_err(|_| lock_error())?;
        compare::run(&library, query).map(Json)
    })
    .await
}

async fn recordings(
    State(state): State<AppState>,
) -> Result<Json<Vec<contract::Recording>>, ApiError> {
    Ok(Json(
        state.recordings.lock().map_err(|_| lock_error())?.list(),
    ))
}
async fn record_start(
    State(state): State<AppState>,
    Json(query): Json<contract::RecordRequest>,
) -> Result<Json<contract::Recording>, ApiError> {
    blocking(move || {
        let capture = state
            .library
            .lock()
            .map_err(|_| lock_error())?
            .get(&query.session_id)?;
        let mut capture = capture.lock().map_err(|_| lock_error())?;
        capture.refresh();
        let session = capture.summary();
        if std::path::Path::new(&session.path).starts_with(&state.imports) {
            return Err(ApiError::new("query", "不能再次录制导入或派生文件"));
        }
        state
            .recordings
            .lock()
            .map_err(|_| lock_error())?
            .start(&session, &state.imports)
            .map(Json)
    })
    .await
}
async fn record_stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<contract::Recording>>, ApiError> {
    state
        .recordings
        .lock()
        .map_err(|_| lock_error())?
        .stop(&id)?;
    // Wait for the small worker to flush; the existing QQ collector continues running.
    for _ in 0..100 {
        let jobs = state.recordings.lock().map_err(|_| lock_error())?.list();
        if jobs.iter().any(|r| r.id == id && r.state != "recording") {
            return Ok(Json(jobs));
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    recordings(State(state)).await
}

async fn sessions(State(state): State<AppState>) -> Result<Json<Catalog>, ApiError> {
    let library = state.library.clone();
    let mut catalog = blocking(move || {
        let mut library = library.lock().map_err(|_| lock_error())?;
        Ok(library.snapshot())
    })
    .await?;
    state
        .processes
        .lock()
        .await
        .annotate_cached(&mut catalog.sessions, &state.imports);
    Ok(Json(catalog))
}

async fn refresh_workspace(state: AppState) {
    loop {
        let library = state.library.clone();
        if let Err(error) = blocking(move || catalog::discover_shared(&library)).await {
            eprintln!("Workspace discovery: {}", error.code);
        }
        let library = state.library.clone();
        if let Err(error) = blocking(move || catalog::scan_accounts_shared(&library, 8)).await {
            eprintln!("Account look-ahead: {}", error.code);
        }
        let library = state.library.clone();
        if let Ok(mut catalog) =
            blocking(move || Ok(library.lock().map_err(|_| lock_error())?.snapshot())).await
        {
            // A slow OS query must never hold the cache lock used by HTTP reads.
            let mut probe = state.processes.lock().await.clone();
            probe.annotate(&mut catalog.sessions, &state.imports).await;
            *state.processes.lock().await = probe;
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<capture::ListQuery>,
) -> Result<Json<EventPage>, ApiError> {
    let library = state.library.clone();
    let mut result = blocking(move || {
        let capture = library.lock().map_err(|_| lock_error())?.get(&id)?;
        let result = capture.lock().map_err(|_| lock_error())?.list(&query)?;
        Ok(result)
    })
    .await?;
    state
        .processes
        .lock()
        .await
        .annotate_cached(std::slice::from_mut(&mut result.session), &state.imports);
    Ok(Json(result))
}

async fn commands(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<contract::CommandList>, ApiError> {
    blocking(move || {
        let capture = state.library.lock().map_err(|_| lock_error())?.get(&id)?;
        let list = capture.lock().map_err(|_| lock_error())?.commands();
        Ok(Json(list))
    })
    .await
}

async fn account_events(
    State(state): State<AppState>,
    Path(account): Path<String>,
    Query(query): Query<capture::ListQuery>,
    Query(cursor): Query<merged::Cursor>,
) -> Result<Json<contract::MergedPage>, ApiError> {
    let library = state.library.clone();
    let imports = state.imports.clone();
    let mut page =
        blocking(move || merged::page(&library, &imports, &account, &query, &cursor)).await?;
    state
        .processes
        .lock()
        .await
        .annotate_cached(&mut page.segments, &state.imports);
    Ok(Json(page))
}

async fn packet(
    State(state): State<AppState>,
    Path((id, event_id)): Path<(String, u64)>,
    Query(query): Query<decode::DecodeQuery>,
) -> Result<Json<PacketDetail>, ApiError> {
    blocking(move || {
        let capture = state.library.lock().map_err(|_| lock_error())?.get(&id)?;
        let event = capture.lock().map_err(|_| lock_error())?.event(event_id)?;
        Ok(Json(decode::detail(event, &query)?))
    })
    .await
}

async fn field_text(
    State(state): State<AppState>,
    Path((id, event_id)): Path<(String, u64)>,
    Query(query): Query<field::TextQuery>,
) -> Result<Json<contract::FieldText>, ApiError> {
    blocking(move || {
        let capture = state.library.lock().map_err(|_| lock_error())?.get(&id)?;
        let event = capture.lock().map_err(|_| lock_error())?.event(event_id)?;
        let detail = decode::detail(
            event,
            &decode::DecodeQuery {
                format: query.format.clone(),
                offset: query.offset,
                length: query.length,
                node_offset: None,
            },
        )?;
        field::text(&detail, &query).map(Json)
    })
    .await
}

async fn upload(
    State(state): State<AppState>,
    request: Request,
) -> Result<Json<Session>, ApiError> {
    let mut temporary = tempfile::NamedTempFile::new_in(&state.imports)
        .map_err(|e| ApiError::new("upload_io", e.to_string()))?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temporary.path())
        .await
        .map_err(|e| ApiError::new("upload_io", e.to_string()))?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut body = request.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| ApiError::new("upload_io", e.to_string()))?;
        if let Ok(chunk) = frame.into_data() {
            bytes += chunk.len() as u64;
            if bytes > capture::MAX_FILE_BYTES {
                return Err(ApiError::new("file_limit", "导入文件最多 512 MiB"));
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| ApiError::new("upload_io", e.to_string()))?;
        }
    }
    file.flush()
        .await
        .map_err(|e| ApiError::new("upload_io", e.to_string()))?;
    drop(file);
    if bytes == 0 {
        return Err(ApiError::new("empty_upload", "请选择 .qscap 文件"));
    }
    qscope_core::native::Reader::new(BufReader::new(temporary.as_file_mut()))
        .map_err(|e| ApiError::new(e.code, e.message))?;
    let filename = format!("{}.qscap", qscope_core::data::to_hex(&hash.finalize()));
    let target = state.imports.join(filename);
    match temporary.persist_noclobber(&target) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(ApiError::new("upload_io", e.error.to_string())),
    }
    blocking(move || {
        let target = target
            .canonicalize()
            .map_err(|e| ApiError::new("upload_io", e.to_string()))?;
        let opened = capture::Capture::open(target)?;
        let capture = state
            .library
            .lock()
            .map_err(|_| lock_error())?
            .insert(opened)?;
        let capture = capture.lock().map_err(|_| lock_error())?;
        let mut summary = capture.summary();
        summary.source_kind = "imported".into();
        Ok(Json(summary))
    })
    .await
}

async fn access(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let expected_origin = format!("http://{}", state.host);
    let api = request.uri().path().starts_with("/api/");
    if api
        && !state.token.is_empty()
        && request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some(format!("Bearer {}", state.token).as_str())
    {
        return ApiError::new("forbidden", "请通过 qs web 或桌面程序打开此工作台").into_response();
    }
    let inspector = state
        .inspector
        .lock()
        .is_ok_and(|manager| manager.accepts(request.uri().path()));
    let target_socket = inspector && request.uri().path().split('/').nth(2) == Some("target");
    if host != Some(state.host.as_str())
        || origin.is_some_and(|origin| {
            origin != expected_origin
                && !(target_socket
                    && (origin == "null" || origin.starts_with("app://") || origin == "file://"))
        })
        || (api
            && request
                .headers()
                .get("x-qscope-client")
                .and_then(|v| v.to_str().ok())
                != Some("web-v1"))
    {
        return ApiError::new("forbidden", "仅允许本机工作台请求").into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().expect("header"));
    headers.insert("x-content-type-options", "nosniff".parse().expect("header"));
    headers.insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().expect("header"),
    );
    let policy = if inspector {
        "frame-ancestors 'self'; base-uri 'self'"
    } else {
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-src http://127.0.0.1:*; frame-ancestors 'none'; base-uri 'none'"
    };
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        policy.parse().expect("header"),
    );
    response
}

async fn static_file(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if path.contains(['\\', ':', '%']) || path.split('/').any(|part| part == "..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let content_type =
        |path: &std::path::Path| match path.extension().and_then(|value| value.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("json") | Some("map") => "application/json",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("ico") => "image/x-icon",
            Some("woff2") => "font/woff2",
            Some("woff") => "font/woff",
            Some("otf") => "font/otf",
            Some("txt") => "text/plain; charset=utf-8",
            Some("wasm") => "application/wasm",
            _ => "application/octet-stream",
        };
    if let Some(assets) = state.assets.clone() {
        let resource = state
            .inspector
            .lock()
            .ok()
            .and_then(|manager| manager.public_resource(request.uri().path()))
            .unwrap_or_else(|| format!("ui/{path}"));
        let mime = content_type(std::path::Path::new(&resource));
        return match blocking(move || assets.read(&resource, 8 * 1024 * 1024).map_err(Into::into))
            .await
        {
            Ok(bytes) => ([(header::CONTENT_TYPE, mime)], Body::from(bytes)).into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        };
    }
    let inspector_file = state
        .inspector
        .lock()
        .ok()
        .and_then(|manager| manager.public_path(request.uri().path()));
    let Ok(file) = inspector_file
        .clone()
        .map(Ok)
        .unwrap_or_else(|| state.ui.join(path).canonicalize())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if inspector_file.is_none() && !file.starts_with(&state.ui) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let content_type = match file.extension().and_then(|s| s.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("otf") => "font/otf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("png") => "image/png",
        Some("wasm") => "application/wasm",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match tokio::fs::metadata(&file).await {
        Ok(meta) if meta.is_file() && meta.len() <= 8 * 1024 * 1024 => {}
        _ => return StatusCode::NOT_FOUND.into_response(),
    }
    match tokio::fs::read(file).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], Body::from(bytes)).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/{key}/targets", get(inspector::targets))
        .route("/{key}/target/{socket_id}", get(inspector::target_socket))
        .route("/{key}/client/{socket_id}", get(inspector::client_socket))
        .route("/api/v1/service", get(service_identity))
        .route("/api/v1/service/control", post(control::command))
        .route("/api/v1/service/roots", post(control::roots))
        .route("/api/v1/sessions", get(sessions))
        .route("/api/v1/settings/integrations", get(automation::settings))
        .route(
            "/api/v1/settings/skills",
            post(automation::install).delete(automation::uninstall),
        )
        .route(
            "/api/v1/settings/qq",
            get(onboarding::status).post(onboarding::start),
        )
        .route("/api/v1/settings/qq/stop", post(onboarding::stop))
        .route("/api/v1/settings/qq/health", get(onboarding::health))
        .route("/api/v1/settings/qq/repair", post(onboarding::repair))
        .route("/api/v1/update", get(update::status))
        .route("/api/v1/update/check", post(update::check))
        .route("/api/v1/update/install", post(update::install))
        .route("/api/v1/update/restart", post(update::restart))
        .route("/api/v1/update/settings", post(update::settings))
        .route(
            "/api/v1/settings/devtools/connections",
            get(automation::connections),
        )
        .route(
            "/api/v1/settings/devtools/connect",
            post(automation::connect),
        )
        .route("/api/v1/settings/devtools/open", post(automation::open))
        .route(
            "/api/v1/settings/devtools/current/open",
            post(automation::open_current),
        )
        .route(
            "/api/v1/settings/devtools/launch",
            get(automation::launch_command).post(automation::launch),
        )
        .route("/api/v1/import", post(upload))
        .route("/api/v1/compare", post(comparison))
        .route("/api/v1/recordings", get(recordings).post(record_start))
        .route("/api/v1/recordings/{id}/stop", post(record_stop))
        .route("/api/v1/sessions/{id}/events", get(events))
        .route("/api/v1/sessions/{id}/commands", get(commands))
        .route("/api/v1/accounts/{account}/events", get(account_events))
        .route("/api/v1/sessions/{id}/events/{event_id}", get(packet))
        .route(
            "/api/v1/sessions/{id}/events/{event_id}/text",
            get(field_text),
        )
        .fallback(static_file)
        .layer(middleware::from_fn_with_state(state.clone(), access))
        .with_state(state)
}

async fn service_identity(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"api_version": contract::API_VERSION, "instance_id": state.instance_id,"version":env!("CARGO_PKG_VERSION"),"workspace":state.automation.workspace}),
    )
}

pub fn typescript() -> String {
    contract::typescript()
}

pub struct ServeOptions {
    pub captures: Vec<PathBuf>,
    pub imports: PathBuf,
    pub ui: PathBuf,
    pub assets: Option<qscope_automation::bundle::Resources>,
    pub token: Option<String>,
    pub port: u16,
    pub fallback_port: bool,
    pub automation: qscope_automation::skills::Environment,
    pub runtime: qscope_automation::runtime::Layout,
    pub instance_id: String,
}

pub struct Service {
    pub endpoint: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    worker: tokio::task::JoinHandle<std::io::Result<()>>,
    discovery: tokio::task::JoinHandle<()>,
    state: AppState,
}

impl Service {
    pub async fn should_exit(&self) -> bool {
        let (stopped, idle) = match self.state.control.lock() {
            Ok(mut control) => (control.stopped(), control.idle()),
            Err(_) => return false,
        };
        stopped || (idle && matches!(control::active_tasks(self.state.clone()).await, Ok(false)))
    }
    pub async fn shutdown(mut self) -> std::io::Result<()> {
        if let Some(signal) = self.shutdown.take() {
            let _ = signal.send(());
        }
        match tokio::time::timeout(std::time::Duration::from_secs(10), &mut self.worker).await {
            Ok(result) => result.map_err(std::io::Error::other)?,
            Err(_) => {
                self.worker.abort();
                let _ = (&mut self.worker).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Local HTTP shutdown exceeded 10 seconds",
                ))
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(signal) = self.shutdown.take() {
            let _ = signal.send(());
        }
        self.discovery.abort();
    }
}

pub async fn start(
    mut options: ServeOptions,
) -> Result<Service, Box<dyn std::error::Error + Send + Sync>> {
    fs::create_dir_all(&options.imports)?;
    let imports = options.imports.canonicalize()?;
    let mut roots = options
        .captures
        .iter()
        .map(|path| path.canonicalize())
        .collect::<Result<Vec<_>, _>>()?;
    roots.push(imports.clone());
    roots.sort();
    roots.dedup();
    let ui = if options.assets.is_some() {
        options.ui
    } else {
        options.ui.canonicalize()?
    };
    if !options.assets.as_ref().map_or_else(
        || ui.join("index.html").is_file(),
        |assets| assets.contains("ui/index.html"),
    ) {
        return Err("WebUI index.html is missing".into());
    }
    let listener = match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, options.port)).await {
        Err(error) if options.fallback_port && error.kind() == std::io::ErrorKind::AddrInUse => {
            tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?
        }
        result => result?,
    };
    let host = listener.local_addr()?.to_string();
    let endpoint = format!("http://{host}");
    if options.automation.server_url.is_empty() {
        options.automation.server_url = endpoint.clone();
    }
    let mut inspector = inspector::Manager::default();
    if options
        .runtime
        .storage
        .join("automation/inspector.json")
        .exists()
    {
        inspector
            .ensure(&options.runtime, &host, options.assets.as_ref())
            .map_err(|error| error.message)?;
    }
    let library = match index::Store::open(&options.runtime.storage.join("cache/index")) {
        Ok(index) => catalog::Library::indexed(roots, index),
        Err(error) => {
            eprintln!("Derived capture index unavailable: {error}");
            catalog::Library::new(roots)
        }
    };
    let state = AppState {
        library: Arc::new(Mutex::new(library)),
        ui,
        assets: options.assets,
        token: options.token.unwrap_or_default(),
        control: Arc::new(Mutex::new(control::Manager::default())),
        imports,
        host,
        processes: Arc::new(tokio::sync::Mutex::new(process::Probe::new(
            &options.runtime,
        ))),
        recordings: Arc::new(Mutex::new(recording::Manager::default())),
        inspector: Arc::new(Mutex::new(inspector)),
        onboarding: Arc::new(Mutex::new(onboarding::Manager::default())),
        update: Arc::new(Mutex::new(update::Manager::default())),
        automation: options.automation,
        runtime: options.runtime,
        instance_id: options.instance_id,
        stopping: Arc::new(AtomicBool::new(false)),
    };
    let shutdown_state = state.clone();
    let control_state = state.clone();
    let discovery = tokio::spawn(refresh_workspace(state.clone()));
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
                shutdown_state.stopping.store(true, Ordering::Release);
                let preparation = shutdown_state
                    .onboarding
                    .lock()
                    .ok()
                    .and_then(|mut manager| manager.cancel());
                if let Some(preparation) = preparation {
                    let _ = preparation.await;
                }
                if let Ok(recordings) = shutdown_state.recordings.lock() {
                    recordings.stop_all();
                }
                for _ in 0..100 {
                    if shutdown_state
                        .recordings
                        .lock()
                        .is_ok_and(|m| m.list().iter().all(|r| r.state != "recording"))
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
            })
            .await
    });
    Ok(Service {
        endpoint,
        shutdown: Some(shutdown),
        worker,
        discovery,
        state: control_state,
    })
}
