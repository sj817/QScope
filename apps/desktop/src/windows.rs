use clap::Parser;
use qscope_automation::{runtime, service};
use sha2::Digest;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::Duration,
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, webview::NewWindowResponse};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
#[command(version, about = "QScope Desktop for Windows x64")]
struct Options {
    #[arg(value_name = "CAPTURE")]
    capture: Option<PathBuf>,
    #[arg(long, hide = true)]
    resources: Option<PathBuf>,
    #[arg(long, conflicts_with = "portable")]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    portable: bool,
    #[arg(long, alias = "captures")]
    capture_root: Vec<PathBuf>,
}

struct Desktop {
    stopped: Arc<AtomicBool>,
    connection: Arc<service::Connection>,
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

fn record_error(data: &Path, message: &str) {
    let logs = data.join("logs");
    if fs::create_dir_all(&logs).is_ok()
        && let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("desktop.log"))
    {
        let _ = writeln!(file, "{:?} {message}", std::time::SystemTime::now());
    }
}

fn inspector_url(url: &tauri::Url) -> bool {
    let mut segments = url.path().split('/').skip(1);
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.username().is_empty()
        && url.password().is_none()
        && segments
            .next()
            .is_some_and(|key| key.len() == 48 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && segments.next() == Some("front_end")
}

fn capture_url(
    data: &Path,
    descriptor: &runtime::Service,
    path: &Path,
) -> Result<tauri::Url, Error> {
    let path = path.canonicalize()?;
    let response = qscope_automation::client::Client::new(&format!(
        "@{}",
        qscope_automation::display_path(&data.join("service.json"))
    ))?
    .post("/api/v1/service/roots", serde_json::json!({"roots":[path]}))?;
    let id = response["opened"]
        .as_array()
        .and_then(|sessions| sessions.first())
        .and_then(|session| session["id"].as_str())
        .ok_or("抓包文件未能打开")?;
    let mut url: tauri::Url = descriptor.browser_url().parse()?;
    url.query_pairs_mut().append_pair("session", id);
    Ok(url)
}

fn initialize(
    app: &tauri::App,
    options: Options,
    data: PathBuf,
    files: Receiver<PathBuf>,
) -> Result<(), Error> {
    runtime::regular_directory(&data)?;
    if let Some(directory) = std::env::current_exe()?.parent() {
        runtime::cleanup_replaced(directory);
    }
    let cli = std::env::current_exe()?.with_file_name("qscope-cli.exe");
    if !cli.is_file() {
        return Err("同目录缺少 qscope-cli.exe，请完整解压桌面包".into());
    }
    let captures = options
        .capture_root
        .into_iter()
        .map(fs::canonicalize)
        .collect::<Result<Vec<_>, _>>()?;
    let descriptor = service::ensure(
        &cli,
        &data,
        &service::Options {
            captures,
            resources: options.resources.map(fs::canonicalize).transpose()?,
            port: None,
            workspace: None,
        },
    )?;
    let origin = descriptor.endpoint.clone();
    let connection = Arc::new(service::Connection::acquire(&data, descriptor)?);
    let address = match options.capture {
        Some(path) => capture_url(&data, &connection.service, &path)?,
        None => connection.service.browser_url().parse()?,
    };
    let stopped = Arc::new(AtomicBool::new(false));
    let heartbeat_connection = connection.clone();
    let heartbeat_stopped = stopped.clone();
    let heartbeat_data = data.clone();
    let heartbeat_app = app.handle().clone();
    std::thread::Builder::new()
        .name("qscope-desktop-connection".into())
        .spawn(move || {
            while !heartbeat_stopped.load(Ordering::Acquire) {
                for _ in 0..50 {
                    if heartbeat_stopped.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                if heartbeat_connection.heartbeat().is_err() {
                    // An update stops the backend and closes the desktop so the new copy can open.
                    if runtime::restart_requested(&heartbeat_data) {
                        heartbeat_app.exit(0);
                    }
                    return;
                }
            }
        })?;
    let file_stopped = stopped.clone();
    let file_service = connection.service.clone();
    let file_data = data.clone();
    let file_app = app.handle().clone();
    app.manage(Desktop {
        stopped,
        connection,
    });
    let navigation_app = app.handle().clone();
    let popup_app = app.handle().clone();
    WebviewWindowBuilder::new(app, "main", WebviewUrl::External(address))
        .title("QScope")
        .inner_size(1440.0, 900.0)
        .min_inner_size(720.0, 520.0)
        .data_directory(data.join("webview"))
        .initialization_script(
            "Object.defineProperty(window, '__QSCOPE_DESKTOP__', {value: true});",
        )
        .on_navigation(move |url| {
            if url.origin().ascii_serialization() == origin || inspector_url(url) {
                return true;
            }
            if url.scheme() == "https" {
                let _ = navigation_app.opener().open_url(url.as_str(), None::<&str>);
            }
            false
        })
        .on_new_window(move |url, _features| {
            if inspector_url(&url) {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let label = format!("devtools-{}", NEXT.fetch_add(1, Ordering::Relaxed));
                let _ = WebviewWindowBuilder::new(&popup_app, label, WebviewUrl::External(url))
                    .title("QQ DevTools · QScope")
                    .inner_size(1280.0, 850.0)
                    .data_directory(data.join("webview"))
                    .on_navigation(|target| {
                        target.as_str() == "about:blank" || inspector_url(target)
                    })
                    .build();
            } else if url.scheme() == "https" {
                let _ = popup_app.opener().open_url(url.as_str(), None::<&str>);
            }
            NewWindowResponse::Deny
        })
        .build()?;
    std::thread::Builder::new()
        .name("qscope-open-capture".into())
        .spawn(move || {
            while !file_stopped.load(Ordering::Acquire) {
                let path = match files.recv_timeout(Duration::from_millis(200)) {
                    Ok(path) => path,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let result = capture_url(&file_data, &file_service, &path);
                if file_stopped.load(Ordering::Acquire) {
                    break;
                }
                match result {
                    Ok(url) => {
                        if let Some(window) = file_app.get_webview_window("main") {
                            let _ = window.navigate(url);
                        }
                    }
                    Err(error) => {
                        record_error(&file_data, &error.to_string());
                        file_app
                            .dialog()
                            .message(error.to_string())
                            .title("QScope")
                            .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                            .show(|_| {});
                    }
                }
            }
        })?;
    Ok(())
}

pub fn run() {
    let options = Options::parse();
    let data = if options.portable {
        std::env::current_exe()
            .map(|path| path.with_file_name("data"))
            .map_err(Into::into)
    } else {
        options
            .data_dir
            .clone()
            .map(Ok)
            .unwrap_or_else(runtime::user_data)
    }
    .and_then(|path| std::path::absolute(path).map_err(Into::into));
    let mut context = tauri::generate_context!();
    if let Ok(path) = &data {
        let normalized = qscope_automation::display_path(path).to_lowercase();
        let identity = format!("{:x}", sha2::Sha256::digest(normalized.as_bytes()));
        context.config_mut().identifier =
            format!("{}.{}", context.config().identifier, &identity[..24]);
    }
    let (file_sender, file_receiver) = mpsc::sync_channel(4);
    let callback_data = data.as_ref().ok().cloned();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            move |app, arguments, cwd| {
                if let Ok(options) = Options::try_parse_from(arguments)
                    && let Some(path) = options.capture
                    && file_sender.try_send(PathBuf::from(cwd).join(path)).is_err()
                    && let Some(data) = &callback_data
                {
                    record_error(data, "抓包打开队列已满或窗口正在关闭，请重新打开文件");
                }
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ))
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            if window.label() == "main"
                && matches!(event, tauri::WindowEvent::CloseRequested { .. })
            {
                if let Some(state) = window.app_handle().try_state::<Desktop>() {
                    state.stopped.store(true, Ordering::Release);
                    let _ = state.connection.action("release");
                }
                window.app_handle().exit(0);
            }
        })
        .setup(move |app| {
            let error_data = data.as_ref().ok().cloned();
            let result = match data {
                Ok(data) => initialize(app, options, data, file_receiver),
                Err(error) => Err(error.into()),
            };
            if let Err(error) = result {
                let message = error.to_string();
                if let Some(data) = error_data {
                    record_error(&data, &message);
                }
                let handle = app.handle().clone();
                app.dialog()
                    .message(format!(
                        "QScope 启动失败\n\n{message}\n\n日志位于用户数据目录 logs/desktop.log。"
                    ))
                    .title("QScope")
                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                    .show(move |_| handle.exit(1));
            }
            Ok(())
        })
        .build(context)
        .expect("Unable to initialize QScope Desktop");
    app.run(|_, _| {});
}
