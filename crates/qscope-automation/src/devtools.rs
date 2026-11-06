use crate::{Error, client::Client};
use qscope_core::data::sha256;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io::Read, path::Path};
use ts_rs::TS;

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[ts(rename = "DevtoolsIdentity")]
pub struct Identity {
    pub pid: u32,
    pub birth_filetime: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[ts(rename = "DevtoolsInfo")]
pub struct BridgeInfo {
    pub version: u32,
    pub pid: u32,
    pub birth_filetime: String,
    pub build: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[ts(rename = "DevtoolsWindow")]
pub struct Window {
    pub id: u32,
    pub title: String,
    pub visible: bool,
    pub web_contents_id: u32,
    pub url: String,
    pub devtools_open: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

pub fn preferred_window(windows: &[Window]) -> Result<&Window, Error> {
    windows
        .iter()
        .filter(|window| {
            window.visible
                && !window.url.is_empty()
                && !window.url.starts_with("devtools:")
                && window.url != "about:blank"
        })
        .max_by_key(|window| {
            (
                window.focused,
                u64::from(window.width) * u64::from(window.height),
            )
        })
        .ok_or_else(|| {
            Error::new(
                "window_pending",
                "QQ 可见主窗口尚未就绪，请显示 QQ 窗口后再打开 DevTools",
            )
        })
}

#[derive(Debug, Serialize, TS)]
#[ts(rename = "DevtoolsStatus")]
pub struct Status {
    pub info: BridgeInfo,
    pub windows: Vec<Window>,
}

#[derive(Debug, Deserialize, TS)]
pub struct ConnectRequest {
    pub connection_id: String,
}

#[derive(Debug, Deserialize, TS)]
pub struct OpenRequest {
    pub connection_id: String,
    pub window_id: u32,
    pub identity: Identity,
}

#[derive(Deserialize)]
struct Descriptor {
    schema_version: u32,
    pid: u32,
    birth_filetime: String,
    endpoint: String,
    ready: bool,
    token: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SavedConnection {
    pub id: String,
    pub endpoint: String,
    pub pid: u32,
    pub birth_filetime: String,
    pub status_path: String,
    pub ready: bool,
    pub error: Option<String>,
}

fn read_descriptor(path: &Path) -> Result<Descriptor, Error> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(Error::new(
                "devtools_descriptor",
                "Connection file must not be a reparse point",
            ));
        }
    }
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(Error::new(
            "devtools_descriptor",
            "Invalid research connection file",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(Error::new(
            "devtools_descriptor",
            "Connection file exceeds 16 KiB",
        ));
    }
    let descriptor: Descriptor = serde_json::from_slice(&bytes)?;
    crate::client::loopback_url(&descriptor.endpoint)?;
    if descriptor.schema_version != 1 || descriptor.birth_filetime.parse::<u64>().is_err() {
        return Err(Error::new(
            "devtools_version",
            "Unsupported connection format or process identity",
        ));
    }
    Ok(descriptor)
}

pub fn saved_connection(path: &Path) -> Result<SavedConnection, Error> {
    let descriptor = read_descriptor(path)?;
    let path = path.canonicalize()?;
    Ok(SavedConnection {
        id: sha256(path.to_string_lossy().as_bytes())[..24].into(),
        endpoint: descriptor.endpoint,
        pid: descriptor.pid,
        birth_filetime: descriptor.birth_filetime,
        status_path: crate::display_path(&path),
        ready: descriptor.ready,
        error: descriptor.error,
    })
}

pub struct Connection {
    client: Client,
    pub info: BridgeInfo,
}

impl Connection {
    pub fn connect_file(path: &Path) -> Result<Self, Error> {
        let descriptor = read_descriptor(path)?;
        if !descriptor.ready {
            return Err(Error::new(
                "devtools_unavailable",
                descriptor
                    .error
                    .unwrap_or_else(|| "Research bridge is starting".into()),
            ));
        }
        let token = descriptor
            .token
            .ok_or_else(|| Error::new("devtools_token", "Connection file has no bridge token"))?;
        let client = Client::with_token(&descriptor.endpoint, &token)?;
        let info: BridgeInfo = serde_json::from_value(client.get("/qscope/v1/info", &[])?)?;
        if info.version != 1
            || info.pid != descriptor.pid
            || info.birth_filetime != descriptor.birth_filetime
            || info.endpoint != descriptor.endpoint
        {
            return Err(Error::new(
                "process_identity",
                "Bridge does not match the saved QQ process identity",
            ));
        }
        Ok(Self { client, info })
    }

    pub fn run(&self, mut request: Value, identity: Option<&Identity>) -> Result<Value, Error> {
        let action = request["action"].as_str().unwrap_or("");
        if !matches!(action, "windows" | "snapshot" | "screenshot") {
            let identity = identity.ok_or_else(|| Error::new("process_identity", "Supply --pid and --birth from the current bridge info before controlling a window"))?;
            if identity.pid != self.info.pid || identity.birth_filetime != self.info.birth_filetime
            {
                return Err(Error::new(
                    "process_identity",
                    "QQ process identity changed. Refresh its windows and choose the target again",
                ));
            }
        }
        if let Some(identity) = identity {
            request["identity"] = serde_json::to_value(identity)?;
        }
        self.client.post("/qscope/v1/action", request)
    }

    pub fn status(&self) -> Result<Status, Error> {
        let windows = self.run(json!({"action":"windows"}), None)?;
        Ok(Status {
            info: self.info.clone(),
            windows: serde_json::from_value(windows)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_target_uses_visible_window_and_handles_legacy_descriptors() {
        let hidden = json!({"id":1,"title":"hidden","visible":false,"web_contents_id":1,"url":"app://hidden","devtools_open":false});
        let visible = json!({"id":2,"title":"QQ","visible":true,"web_contents_id":2,"url":"app://main","devtools_open":false});
        let windows: Vec<Window> = serde_json::from_value(json!([hidden, visible])).unwrap();
        assert_eq!(preferred_window(&windows).unwrap().id, 2);
        assert!(preferred_window(&windows[..1]).is_err());
        let mut windows = windows;
        windows[0].visible = true;
        windows[0].focused = true;
        assert_eq!(preferred_window(&windows).unwrap().id, 1);
        windows[0].url = "about:blank".into();
        assert_eq!(preferred_window(&windows).unwrap().id, 2);
    }
}
