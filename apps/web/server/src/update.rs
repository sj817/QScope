// Online updates through the backend, so the desktop and the browser share one download and
// one state. Recordings and research QQ are closed only after the new package is verified.
use crate::{
    ApiError, AppState, blocking,
    contract::{UpdateSettingsRequest, UpdateStatus},
    control, lock_error, onboarding,
};
use axum::{Json, extract::State};
use qscope_automation::update;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Manager {
    stage: String,
    manifest: Option<update::Manifest>,
    package: Option<PathBuf>,
    prepared: Option<Vec<(String, PathBuf)>>,
    downloaded: u64,
    total: u64,
    error: Option<String>,
    busy: bool,
}

fn unsupported(install: update::Install) -> Option<String> {
    if !update::supported() {
        Some("在线更新支持 Windows x64 和 macOS Apple Silicon".into())
    } else if install == update::Install::Development {
        Some("开发构建不支持在线更新，请用源码重新构建".into())
    } else if update::RELEASE_KEY.is_empty() {
        Some("此构建没有配置发布签名公钥，不能在线更新".into())
    } else {
        None
    }
}

fn snapshot(state: &AppState) -> Result<UpdateStatus, ApiError> {
    let (install, _) = update::locate()?;
    let settings = update::load(&state.runtime.storage)?;
    let manager = state.update.lock().map_err(|_| lock_error())?;
    Ok(UpdateStatus {
        current: env!("CARGO_PKG_VERSION").into(),
        install: install.name().into(),
        unsupported: unsupported(install),
        stage: if manager.stage.is_empty() {
            "idle".into()
        } else {
            manager.stage.clone()
        },
        latest: manager
            .manifest
            .as_ref()
            .map(|manifest| manifest.version.clone()),
        notes_url: manager
            .manifest
            .as_ref()
            .map(|manifest| manifest.notes_url.clone()),
        downloaded: manager.downloaded.to_string(),
        total: manager.total.to_string(),
        error: manager.error.clone(),
        checked_unix: settings.last_check_unix.map(|time| time.to_string()),
        check_on_launch: settings.check_on_launch,
        proxy: settings.proxy,
    })
}

pub async fn status(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    blocking(move || snapshot(&state).map(Json)).await
}

fn set(manager: &Arc<Mutex<Manager>>, change: impl FnOnce(&mut Manager)) {
    if let Ok(mut manager) = manager.lock() {
        change(&mut manager);
    }
}

#[derive(Deserialize, Default)]
pub struct CheckRequest {
    /// A launch check, skipped when one ran within the last day or launch checks are off.
    #[serde(default)]
    auto: bool,
}

pub async fn check(
    State(state): State<AppState>,
    request: Option<Json<CheckRequest>>,
) -> Result<Json<UpdateStatus>, ApiError> {
    let auto = request.is_some_and(|request| request.auto);
    let current = blocking({
        let state = state.clone();
        move || snapshot(&state)
    })
    .await?;
    if current.unsupported.is_some() {
        if auto {
            return Ok(Json(current));
        }
        return Err(ApiError::new(
            "update_unsupported",
            current.unsupported.unwrap_or_default(),
        ));
    }
    let settings = update::load(&state.runtime.storage)?;
    let skip = {
        let mut manager = state.update.lock().map_err(|_| lock_error())?;
        let skip = manager.busy || (auto && !update::due(&settings));
        if !skip {
            manager.busy = true;
            manager.stage = "checking".into();
            manager.error = None;
        }
        skip
    };
    if skip {
        return blocking(move || snapshot(&state).map(Json)).await;
    }
    let task = state.clone();
    tokio::task::spawn_blocking(move || {
        let data = task.runtime.storage.clone();
        let result = (|| {
            let mut settings = update::load(&data)?;
            let manifest = update::check(settings.proxy.as_deref())?;
            settings.last_check_unix = Some(update::now());
            update::save(&data, &settings)?;
            let newer = update::newer(&manifest.version, env!("CARGO_PKG_VERSION"))?;
            Ok::<_, qscope_automation::Error>((manifest, newer))
        })();
        set(&task.update, |manager| {
            manager.busy = false;
            match result {
                Ok((manifest, newer)) => {
                    manager.stage = if newer { "available" } else { "latest" }.into();
                    manager.manifest = Some(manifest);
                }
                Err(error) => {
                    manager.stage = "error".into();
                    manager.error = Some(error.message);
                }
            }
        });
    });
    blocking(move || snapshot(&state).map(Json)).await
}

pub async fn settings(
    State(state): State<AppState>,
    Json(request): Json<UpdateSettingsRequest>,
) -> Result<Json<UpdateStatus>, ApiError> {
    blocking(move || {
        let mut settings = update::load(&state.runtime.storage)?;
        settings.proxy = update::proxy(&request.proxy)?;
        settings.check_on_launch = request.check_on_launch;
        update::save(&state.runtime.storage, &settings)?;
        snapshot(&state).map(Json)
    })
    .await
}

fn preparing(state: &AppState) -> Result<(), ApiError> {
    if state.onboarding.lock().map_err(|_| lock_error())?.busy() {
        return Err(ApiError::new(
            "setup_busy",
            "QQ 正在下载或启动，请等它完成后再更新",
        ));
    }
    Ok(())
}

/// Ends what keeps the old build in use before it is replaced: recordings are saved and each
/// running research QQ is closed through its owner, so its capture ends with a footer.
pub(crate) async fn close_tasks(state: &AppState) -> Result<Value, ApiError> {
    preparing(state)?;
    let recordings = {
        let manager = state.recordings.lock().map_err(|_| lock_error())?;
        let active = manager
            .list()
            .iter()
            .filter(|recording| recording.state == "recording")
            .count();
        manager.stop_all();
        active
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while state
        .recordings
        .lock()
        .map_err(|_| lock_error())?
        .list()
        .iter()
        .any(|recording| recording.state == "recording")
    {
        if Instant::now() >= deadline {
            return Err(ApiError::new(
                "recording_timeout",
                "录制没有在 10 秒内保存完成",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Only copies whose bridge still answers are running; a stale connection file is left alone.
    let running = blocking({
        let state = state.clone();
        move || {
            Ok(crate::automation::saved_connections(&state)?
                .into_iter()
                .filter(|saved| {
                    saved.ready
                        && qscope_automation::devtools::Connection::connect_file(Path::new(
                            &saved.status_path,
                        ))
                        .is_ok()
                })
                .map(|saved| PathBuf::from(saved.status_path))
                .collect::<Vec<_>>())
        }
    })
    .await?;
    let mut closed = Vec::new();
    for connection in running {
        let profile = onboarding::profile_of(&connection)?;
        closed.push(onboarding::close(state, connection, profile).await?);
    }
    if control::active_tasks(state.clone()).await? {
        return Err(ApiError::new(
            "active_tasks",
            "研究 QQ 或录制仍在运行，请先结束对应任务再更新",
        ));
    }
    Ok(json!({"schema_version":1,"recordings":recordings,"qq":closed}))
}

/// Downloads and verifies the new release while QQ keeps running, then closes the running
/// tasks and starts installing it. A setup package takes over from there; a portable package
/// waits for `restart`.
pub async fn install(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    preparing(&state)?;
    let (install, directory) = update::locate()?;
    if let Some(reason) = unsupported(install) {
        return Err(ApiError::new("update_unsupported", reason));
    }
    let manifest = {
        let mut manager = state.update.lock().map_err(|_| lock_error())?;
        if manager.busy {
            return Err(ApiError::new("update_busy", "更新正在进行"));
        }
        let manifest = manager
            .manifest
            .clone()
            .filter(|_| manager.stage == "available" || manager.stage == "error")
            .ok_or_else(|| ApiError::new("update_missing", "请先检查更新"))?;
        if !update::newer(&manifest.version, env!("CARGO_PKG_VERSION"))? {
            return Err(ApiError::new("update_missing", "当前已是最新版本"));
        }
        let asset = update::asset(&manifest, install)?;
        manager.busy = true;
        manager.stage = "downloading".into();
        manager.error = None;
        manager.prepared = None;
        manager.downloaded = 0;
        manager.prepared = None;
        manager.total = asset.bytes;
        manifest
    };
    let task = state.clone();
    tokio::spawn(async move {
        let result = apply(&task, manifest, install, directory).await;
        set(&task.update, |manager| {
            manager.busy = false;
            match result {
                Ok((stage, package)) => {
                    manager.stage = stage.into();
                    manager.package = Some(package);
                }
                Err(error) => {
                    manager.stage = "error".into();
                    manager.error = Some(error.message);
                }
            }
        });
    });
    blocking(move || snapshot(&state).map(Json)).await
}

async fn apply(
    state: &AppState,
    manifest: update::Manifest,
    install: update::Install,
    directory: PathBuf,
) -> Result<(&'static str, PathBuf), ApiError> {
    let version = manifest.version.clone();
    let package = blocking({
        let state = state.clone();
        move || {
            let data = state.runtime.storage.clone();
            let proxy = update::load(&data)?.proxy;
            Ok(update::download(
                &manifest,
                install,
                &data,
                proxy.as_deref(),
                &mut |received| set(&state.update, |manager| manager.downloaded = received),
                &state.stopping,
            )?)
        }
    })
    .await?;
    let prepared = if install != update::Install::Installer {
        let package = package.clone();
        let directory = directory.clone();
        Some(
            blocking(move || {
                let files = update::unpack(&package, install)?;
                update::preflight(install, &directory, &files, &version)?;
                Ok(files)
            })
            .await?,
        )
    } else {
        None
    };
    // Desktop bundles are replaced on restart, which closes their tasks then.
    if !install.needs_restart() {
        set(&state.update, |manager| manager.stage = "closing".into());
        close_tasks(state).await?;
    }
    let update_state = state.update.clone();
    blocking(move || {
        Ok(match install {
            update::Install::Installer => {
                update::start_installer(&package)?;
                ("installing", package)
            }
            update::Install::Cli => {
                // The CLI copy runs this backend; the next start uses the new files.
                let files = prepared.expect("verified CLI files");
                update::replace(&directory, &files)?;
                update::refresh_global_command(&directory);
                ("ready", package)
            }
            _ => {
                set(&update_state, |manager| manager.prepared = prepared);
                ("ready", package)
            }
        })
    })
    .await
}

/// Replaces a portable copy, then stops this backend; the desktop closes and the new one opens.
pub async fn restart(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    let (install, directory) = update::locate()?;
    let (files, version) = {
        let mut manager = state.update.lock().map_err(|_| lock_error())?;
        if manager.busy {
            return Err(ApiError::new("update_busy", "更新正在进行"));
        }
        match (&manager.prepared, &manager.manifest) {
            (Some(files), Some(manifest))
                if manager.stage == "ready" && install.needs_restart() =>
            {
                let prepared = (files.clone(), manifest.version.clone());
                manager.busy = true;
                manager.stage = "closing".into();
                prepared
            }
            _ => return Err(ApiError::new("update_missing", "没有等待重启的更新")),
        }
    };
    let result = async {
        let check_directory = directory.clone();
        let check_files = files.clone();
        let check_version = version.clone();
        blocking(move || {
            update::preflight(install, &check_directory, &check_files, &check_version)
                .map_err(Into::into)
        })
        .await?;
        close_tasks(&state).await?;
        let data = state.runtime.storage.clone();
        blocking(move || {
            update::replace(&directory, &files)?;
            update::refresh_global_command(&directory);
            update::mark_restart(&data, &version)?;
            update::start_relaunch(&directory, &data)?;
            Ok(())
        })
        .await
    }
    .await;
    set(&state.update, |manager| {
        manager.busy = false;
        if let Err(error) = &result {
            manager.stage = "error".into();
            manager.error = Some(error.message.clone());
        } else {
            manager.stage = "restarting".into();
        }
    });
    result?;
    state.control.lock().map_err(|_| lock_error())?.stop();
    blocking(move || snapshot(&state).map(Json)).await
}
