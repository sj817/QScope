// Online updates through the backend, so the desktop and the browser share one download and
// one state. The backend refuses to replace itself while QQ or a recording is active.
use crate::{
    ApiError, AppState, blocking,
    contract::{UpdateSettingsRequest, UpdateStatus},
    control, lock_error,
};
use axum::{Json, extract::State};
use qscope_automation::update;
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Manager {
    stage: String,
    manifest: Option<update::Manifest>,
    package: Option<PathBuf>,
    downloaded: u64,
    total: u64,
    error: Option<String>,
    busy: bool,
}

fn unsupported(install: update::Install) -> Option<String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        Some("在线更新目前只支持 Windows x64".into())
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

async fn idle(state: &AppState) -> Result<(), ApiError> {
    if control::active_tasks(state.clone()).await? {
        return Err(ApiError::new(
            "active_tasks",
            "研究 QQ 或录制仍在运行，请先结束对应任务再更新",
        ));
    }
    Ok(())
}

/// Downloads the new release and starts installing it. A setup package takes over from there;
/// a portable or CLI package waits for `restart`.
pub async fn install(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    idle(&state).await?;
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
        manager.downloaded = 0;
        manager.total = asset.bytes;
        manifest
    };
    let task = state.clone();
    tokio::task::spawn_blocking(move || {
        let data = task.runtime.storage.clone();
        let progress = task.update.clone();
        let result: Result<(&str, PathBuf), qscope_automation::Error> = (|| {
            let proxy = update::load(&data)?.proxy;
            let package = update::download(
                &manifest,
                install,
                &data,
                proxy.as_deref(),
                &mut |received| set(&progress, |manager| manager.downloaded = received),
                &task.stopping,
            )?;
            match install {
                update::Install::Installer => {
                    update::start_installer(&package)?;
                    Ok(("installing", package))
                }
                update::Install::Cli => {
                    // The CLI copy runs this backend; the next start uses the new files.
                    let files = update::unpack(&package, install)?;
                    update::replace(&directory, &files)?;
                    update::refresh_global_command(&directory);
                    Ok(("ready", package))
                }
                _ => {
                    update::unpack(&package, install)?;
                    Ok(("ready", package))
                }
            }
        })();
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

/// Replaces a portable copy, then stops this backend; the desktop closes and the new one opens.
pub async fn restart(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    idle(&state).await?;
    let (install, directory) = update::locate()?;
    let (package, version) = {
        let manager = state.update.lock().map_err(|_| lock_error())?;
        match (&manager.package, &manager.manifest) {
            (Some(package), Some(manifest))
                if manager.stage == "ready" && install == update::Install::Portable =>
            {
                (package.clone(), manifest.version.clone())
            }
            _ => return Err(ApiError::new("update_missing", "没有等待重启的更新")),
        }
    };
    let data = state.runtime.storage.clone();
    blocking(move || {
        let files = update::unpack(&package, install)?;
        update::replace(&directory, &files)?;
        update::refresh_global_command(&directory);
        update::mark_restart(&data, &version)?;
        let mut relaunch = std::process::Command::new(directory.join("qscope-cli.exe"));
        relaunch
            .arg("--data-dir")
            .arg(&data)
            .args(["update", "relaunch", "--directory"])
            .arg(&directory);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            relaunch.creation_flags(0x0800_0000);
        }
        relaunch.spawn().map_err(|error| {
            ApiError::new("update_restart", format!("无法启动新版 QScope：{error}"))
        })?;
        Ok(())
    })
    .await?;
    state.update.lock().map_err(|_| lock_error())?.stage = "restarting".into();
    state.control.lock().map_err(|_| lock_error())?.stop();
    blocking(move || snapshot(&state).map(Json)).await
}
