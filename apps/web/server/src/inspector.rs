use crate::{ApiError, AppState, contract::DevtoolsFrontend, lock_error};
use axum::{
    Json,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt, future::join_all, stream::SplitSink};
use qscope_automation::{client::Client, display_path};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot};

const MAX_TARGETS: usize = 16;
const MAX_CLIENTS: usize = 4;
const MAX_MESSAGE: usize = 1024 * 1024;
const EVENT_QUEUE: usize = 16;

#[derive(Debug, Clone, Serialize)]
pub struct Frontend {
    pub origin: String,
    pub base_path: String,
    pub client_path: String,
    pub version: String,
}

impl Frontend {
    pub fn response(&self, target: &str) -> Result<DevtoolsFrontend, ApiError> {
        let targets = Client::new(&self.origin)?.get(&format!("{}targets", self.base_path), &[])?;
        if !targets["targets"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == target))
        {
            return Err(ApiError::new(
                "inspector_pending",
                "调试目标尚未连接，请重试打开 DevTools",
            ));
        }
        let host = self.origin.trim_start_matches("http://");
        let client = qscope_automation::runtime::instance_id()?;
        let socket = format!("{host}{}client/{client}?target={target}", self.base_path);
        let mut url = qscope_automation::client::Url::parse(&format!(
            "{}{}front_end/chii_app.html",
            self.origin, self.base_path
        ))
        .map_err(|error| ApiError::new("inspector", error.to_string()))?;
        url.query_pairs_mut().append_pair("ws", &socket);
        Ok(DevtoolsFrontend {
            opened: true,
            frontend_url: url.into(),
            frontend: "Chii 1.15.5".into(),
        })
    }
}

struct Target {
    generation: String,
    metadata: serde_json::Value,
    commands: mpsc::Sender<Message>,
    clients: BTreeMap<String, InspectorClient>,
}

struct InspectorClient {
    events: mpsc::Sender<Message>,
    // 移除客户端时同时取消中继，避免超时前端持续消耗已排队事件。
    _lifetime: oneshot::Sender<()>,
}

type Registry = Arc<Mutex<BTreeMap<String, Target>>>;

#[derive(Default)]
pub struct Manager {
    frontend: Option<Frontend>,
    public: PathBuf,
    targets: Registry,
}

impl Manager {
    pub fn ensure(
        &mut self,
        runtime: &qscope_automation::runtime::Layout,
        host: &str,
        assets: Option<&qscope_automation::bundle::Resources>,
    ) -> Result<Frontend, ApiError> {
        if let Some(frontend) = &self.frontend {
            return Ok(frontend.clone());
        }
        let public = runtime
            .resources
            .join("collectors/devtools/node_modules/chii/public");
        if let Some(assets) = assets {
            assets.materialize(
                &runtime.storage,
                "collectors/devtools/node_modules/chii/public/target.js",
            )?;
        }
        if !public.join("target.js").is_file()
            || !assets.map_or_else(
                || public.join("front_end/chii_app.html").is_file(),
                |assets| {
                    assets.contains(
                        "collectors/devtools/node_modules/chii/public/front_end/chii_app.html",
                    )
                },
            )
        {
            return Err(ApiError::new(
                "inspector_missing",
                "当前资源缺少固定版本的 Chii 前端",
            ));
        }
        let settings = runtime.storage.join("automation/inspector.json");
        let key = if settings.exists() {
            let value = qscope_automation::runtime::read_settings(&settings)?;
            value["key"]
                .as_str()
                .filter(|key| {
                    value["schema_version"] == 1
                        && key.len() == 48
                        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .ok_or_else(|| {
                    ApiError::new("inspector_settings", "Invalid saved DevTools capability")
                })?
                .to_owned()
        } else {
            let key = qscope_automation::runtime::instance_id()?;
            qscope_automation::runtime::write_settings(
                &settings,
                &serde_json::json!({"schema_version":1,"key":key}),
            )?;
            key
        };
        let frontend = Frontend {
            origin: format!("http://{host}"),
            base_path: format!("/{key}/"),
            client_path: display_path(&public.join("target.js")),
            version: "1.15.5".into(),
        };
        self.public = public;
        self.frontend = Some(frontend.clone());
        Ok(frontend)
    }

    pub fn accepts(&self, path: &str) -> bool {
        self.frontend
            .as_ref()
            .is_some_and(|frontend| path.starts_with(&frontend.base_path))
    }

    pub fn public_path(&self, path: &str) -> Option<PathBuf> {
        let relative = path.strip_prefix(&self.frontend.as_ref()?.base_path)?;
        if relative != "target.js" && !relative.starts_with("front_end/") {
            return None;
        }
        let file = self.public.join(relative).canonicalize().ok()?;
        file.starts_with(self.public.canonicalize().ok()?)
            .then_some(file)
    }

    pub fn public_resource(&self, path: &str) -> Option<String> {
        let relative = path.strip_prefix(&self.frontend.as_ref()?.base_path)?;
        (relative == "target.js" || relative.starts_with("front_end/"))
            .then(|| format!("collectors/devtools/node_modules/chii/public/{relative}"))
    }
}

pub async fn targets(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let manager = state.inspector.lock().map_err(|_| lock_error())?;
    if !manager.accepts(&format!("/{key}/targets")) {
        return Err(ApiError::new("forbidden", "调试连接已失效"));
    }
    let targets = manager.targets.lock().map_err(|_| lock_error())?;
    Ok(Json(
        serde_json::json!({"targets": targets.values().map(|target| target.metadata.clone()).collect::<Vec<_>>()}),
    ))
}

pub async fn target_socket(
    State(state): State<AppState>,
    Path((key, id)): Path<(String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    socket(state, key, "target".into(), id, query, upgrade)
}

pub async fn client_socket(
    State(state): State<AppState>,
    Path((key, id)): Path<(String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    socket(state, key, "client".into(), id, query, upgrade)
}

fn socket(
    state: AppState,
    key: String,
    kind: String,
    id: String,
    query: BTreeMap<String, String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let manager = match state.inspector.lock() {
        Ok(manager) => manager,
        Err(_) => return lock_error().into_response(),
    };
    if !manager.accepts(&format!("/{key}/{kind}/{id}"))
        || !matches!(kind.as_str(), "target" | "client")
        || id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if query.values().any(|value| value.len() > 4096) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let registry = manager.targets.clone();
    drop(manager);
    upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| relay(socket, registry, kind, id, query))
        .into_response()
}

async fn send(socket: &mut SplitSink<WebSocket, Message>, message: Message) -> bool {
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), socket.send(message)).await,
        Ok(Ok(()))
    )
}

fn remove_client(registry: &Registry, target_id: &str, generation: &str, client_id: &str) -> bool {
    registry.lock().is_ok_and(|mut targets| {
        targets
            .get_mut(target_id)
            .filter(|target| target.generation == generation)
            .is_some_and(|target| target.clients.remove(client_id).is_some())
    })
}

async fn forward_event(
    registry: &Registry,
    target_id: &str,
    generation: &str,
    message: Message,
) -> Result<(), &'static str> {
    let clients = {
        let targets = registry.lock().map_err(|_| "target_registry_lock")?;
        let target = targets
            .get(target_id)
            .filter(|target| target.generation == generation)
            .ok_or("target_generation_changed")?;
        target
            .clients
            .iter()
            .map(|(id, client)| (id.clone(), client.events.clone()))
            .collect::<Vec<_>>()
    };
    // 每个客户端最多保留 EVENT_QUEUE 帧，每帧受 MAX_MESSAGE 限制。在注册锁外并行等待，隔离慢速前端的投递延迟。
    let results = join_all(clients.into_iter().map(|(id, sender)| {
        let message = message.clone();
        async move {
            match tokio::time::timeout(std::time::Duration::from_secs(5), sender.send(message))
                .await
            {
                Ok(Ok(())) => None,
                result => Some((id, result.is_err())),
            }
        }
    }))
    .await;
    for (client, timed_out) in results.into_iter().flatten() {
        if remove_client(registry, target_id, generation, &client) && timed_out {
            eprintln!(
                "{}",
                serde_json::json!({"component":"devtools","code":"client_event_queue_timeout"})
            );
        }
    }
    Ok(())
}

async fn relay(
    socket: WebSocket,
    registry: Registry,
    kind: String,
    id: String,
    query: BTreeMap<String, String>,
) {
    let (mut writer, mut reader) = socket.split();
    let (controls, mut control_messages) = mpsc::channel(4);
    let result;
    if kind == "target" {
        let generation = match qscope_automation::runtime::instance_id() {
            Ok(value) => value,
            Err(_) => return,
        };
        let (commands, mut receiver) = mpsc::channel(16);
        let metadata = serde_json::json!({"id": id, "title": query.get("title"), "url": query.get("url"), "favicon": query.get("favicon"), "rtc": false});
        {
            let Ok(mut targets) = registry.lock() else {
                return;
            };
            if targets.len() >= MAX_TARGETS || targets.contains_key(&id) {
                return;
            }
            targets.insert(
                id.clone(),
                Target {
                    generation: generation.clone(),
                    metadata,
                    commands,
                    clients: BTreeMap::new(),
                },
            );
        }
        result = {
            let incoming = async {
                while let Some(message) = reader.next().await {
                    match message {
                        Ok(message @ (Message::Text(_) | Message::Binary(_))) => {
                            forward_event(&registry, &id, &generation, message).await?;
                        }
                        Ok(Message::Ping(data)) => controls
                            .try_send(Message::Pong(data))
                            .map_err(|_| "control_queue_full")?,
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(_)) => break,
                        Err(_) => return Err("target_socket_read"),
                    }
                }
                Ok(())
            };
            let outgoing = async {
                loop {
                    let message = tokio::select! {
                        message = receiver.recv() => message,
                        message = control_messages.recv() => message,
                    };
                    let Some(message) = message else {
                        return Ok(());
                    };
                    if !send(&mut writer, message).await {
                        return Err("target_socket_write");
                    }
                }
            };
            tokio::select! { result = incoming => result, result = outgoing => result }
        };
        if let Ok(mut targets) = registry.lock()
            && targets
                .get(&id)
                .is_some_and(|target| target.generation == generation)
        {
            targets.remove(&id);
        }
    } else {
        let Some(target_id) = query.get("target") else {
            return;
        };
        // 每次连接生成内部 ID，区分使用同一 URL 的多个标签页，并防止旧连接移除新连接。
        let client_id = match qscope_automation::runtime::instance_id() {
            Ok(value) => value,
            Err(_) => return,
        };
        let (generation, commands, mut events, disconnected) = {
            let Ok(mut targets) = registry.lock() else {
                return;
            };
            let Some(target) = targets.get_mut(target_id) else {
                return;
            };
            if target.clients.len() >= MAX_CLIENTS {
                return;
            }
            let (events, receiver) = mpsc::channel(EVENT_QUEUE);
            let (lifetime, disconnected) = oneshot::channel();
            target.clients.insert(
                client_id.clone(),
                InspectorClient {
                    events,
                    _lifetime: lifetime,
                },
            );
            (
                target.generation.clone(),
                target.commands.clone(),
                receiver,
                disconnected,
            )
        };
        result = {
            let incoming = async {
                while let Some(message) = reader.next().await {
                    match message {
                        Ok(message @ (Message::Text(_) | Message::Binary(_))) => {
                            if !matches!(
                                tokio::time::timeout(
                                    std::time::Duration::from_secs(5),
                                    commands.send(message)
                                )
                                .await,
                                Ok(Ok(()))
                            ) {
                                return Err("command_queue_timeout");
                            }
                        }
                        Ok(Message::Ping(data)) => controls
                            .try_send(Message::Pong(data))
                            .map_err(|_| "control_queue_full")?,
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(_)) => break,
                        Err(_) => return Err("client_socket_read"),
                    }
                }
                Ok(())
            };
            let outgoing = async {
                loop {
                    let message = tokio::select! {
                        message = events.recv() => match message { Some(message) => message, None => return Ok(()) },
                        message = control_messages.recv() => match message { Some(message) => message, None => return Ok(()) },
                    };
                    if !send(&mut writer, message).await {
                        return Err("client_socket_write");
                    }
                }
            };
            tokio::select! {
                result = incoming => result,
                result = outgoing => result,
                _ = disconnected => Ok(()),
            }
        };
        remove_client(&registry, target_id, &generation, &client_id);
    }
    if let Err(code) = result {
        eprintln!(
            "{}",
            serde_json::json!({"component":"devtools","code":code})
        );
    }
    let _ = send(&mut writer, Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(clients: BTreeMap<String, mpsc::Sender<Message>>) -> Registry {
        let (commands, _) = mpsc::channel(1);
        let clients = clients
            .into_iter()
            .map(|(id, events)| {
                let (lifetime, _) = oneshot::channel();
                (
                    id,
                    InspectorClient {
                        events,
                        _lifetime: lifetime,
                    },
                )
            })
            .collect();
        Arc::new(Mutex::new(BTreeMap::from([(
            "fixture".into(),
            Target {
                generation: "current-target".into(),
                metadata: serde_json::Value::Null,
                commands,
                clients,
            },
        )])))
    }

    #[tokio::test]
    async fn slow_client_is_bounded_and_removed_without_delaying_healthy_delivery() {
        let (slow, mut unread) = mpsc::channel(EVENT_QUEUE);
        let (healthy, mut received) = mpsc::channel(EVENT_QUEUE);
        let (closed, closed_receiver) = mpsc::channel(EVENT_QUEUE);
        drop(closed_receiver);
        let registry = registry(BTreeMap::from([
            ("slow".into(), slow),
            ("healthy".into(), healthy),
            ("closed".into(), closed),
        ]));
        let (delivered, ready) = tokio::sync::oneshot::channel();
        let consumer = tokio::spawn(async move {
            for index in 0..=EVENT_QUEUE {
                assert_eq!(
                    received.recv().await.unwrap().into_text().unwrap(),
                    index.to_string()
                );
            }
            delivered.send(()).unwrap();
        });
        for index in 0..EVENT_QUEUE {
            forward_event(
                &registry,
                "fixture",
                "current-target",
                Message::Text(index.to_string().into()),
            )
            .await
            .unwrap();
        }
        assert_eq!(unread.len(), EVENT_QUEUE);
        assert!(
            !registry.lock().unwrap()["fixture"]
                .clients
                .contains_key("closed")
        );
        let pending_registry = registry.clone();
        let pending = tokio::spawn(async move {
            forward_event(
                &pending_registry,
                "fixture",
                "current-target",
                Message::Text(EVENT_QUEUE.to_string().into()),
            )
            .await
        });
        // 健康前端需在慢速前端的五秒超时前收到最后事件，慢速队列始终保持固定上限。
        tokio::time::timeout(std::time::Duration::from_secs(1), ready)
            .await
            .unwrap()
            .unwrap();
        assert!(!pending.is_finished());
        assert_eq!(unread.len(), EVENT_QUEUE);
        tokio::time::timeout(std::time::Duration::from_secs(6), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        consumer.await.unwrap();
        let clients = &registry.lock().unwrap()["fixture"].clients;
        assert_eq!(clients.len(), 1);
        assert!(clients.contains_key("healthy"));
        for index in 0..EVENT_QUEUE {
            assert_eq!(
                unread.try_recv().unwrap().into_text().unwrap(),
                index.to_string()
            );
        }
        assert!(matches!(
            unread.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn stale_client_cleanup_preserves_the_reconnected_target() {
        let (events, _receiver) = mpsc::channel(EVENT_QUEUE);
        let registry = registry(BTreeMap::from([("client".into(), events)]));
        assert!(!remove_client(&registry, "fixture", "old-target", "client"));
        assert!(
            registry.lock().unwrap()["fixture"]
                .clients
                .contains_key("client")
        );
        assert!(remove_client(
            &registry,
            "fixture",
            "current-target",
            "client"
        ));
        assert!(registry.lock().unwrap()["fixture"].clients.is_empty());
    }
}
