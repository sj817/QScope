//! 只读验证生产者身份，抓包文件与运行进程分别建模。
use crate::contract::Session;
use std::path::Path;

#[derive(Default, Clone)]
pub struct Probe {
    #[cfg(any(windows, target_os = "macos"))]
    entries: std::collections::HashMap<String, Cached>,
    #[cfg(any(windows, target_os = "macos"))]
    host: Option<std::path::PathBuf>,
}

#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone)]
struct Cached {
    birth: String,
    alive: Option<bool>,
    checked: std::time::Instant,
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(serde::Deserialize)]
struct Observed {
    id: String,
    birth: Option<String>,
    name: Option<String>,
    missing: bool,
}

#[cfg(any(windows, target_os = "macos", test))]
fn alive(expected_birth: &str, observed: &Observed) -> Option<bool> {
    if observed.missing {
        return Some(false);
    }
    match (&observed.birth, &observed.name) {
        (Some(birth), Some(name)) => {
            Some(birth == expected_birth && name.eq_ignore_ascii_case("QQ"))
        }
        _ => None,
    }
}

impl Probe {
    pub fn new(layout: &qscope_automation::runtime::Layout) -> Self {
        #[cfg(any(windows, target_os = "macos"))]
        {
            Self {
                entries: Default::default(),
                host: layout.host().ok(),
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = layout;
            Self::default()
        }
    }
    pub fn annotate_cached(&self, sessions: &mut [Session], imports: &Path) {
        for session in sessions.iter_mut() {
            session.source_kind = if Path::new(&session.path).starts_with(imports) {
                let root = imports;
                if Path::new(&session.path).starts_with(root.join("recordings")) {
                    "recording".into()
                } else if Path::new(&session.path).starts_with(root.join("http-proxy")) {
                    "http".into()
                } else {
                    "imported".into()
                }
            } else {
                "capture".into()
            };
            session.producer_alive = None;
            #[cfg(any(windows, target_os = "macos"))]
            if session.source_kind == "capture" {
                session.producer_alive = self
                    .entries
                    .get(&session.id)
                    .filter(|entry| entry.birth == session.process_birth_filetime)
                    .and_then(|entry| entry.alive);
            }
        }
    }

    /// 记录 QScope 刚观察到退出的生产者，例如由界面关闭的研究副本。
    pub fn exited(&mut self, id: &str, birth: &str) {
        #[cfg(any(windows, target_os = "macos"))]
        self.entries.insert(
            id.into(),
            Cached {
                birth: birth.into(),
                alive: Some(false),
                checked: std::time::Instant::now(),
            },
        );
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = (id, birth);
    }

    pub async fn annotate(&mut self, sessions: &mut [Session], imports: &Path) {
        self.annotate_cached(sessions, imports);
        #[cfg(any(windows, target_os = "macos"))]
        self.native(sessions).await;
    }

    #[cfg(any(windows, target_os = "macos"))]
    async fn native(&mut self, sessions: &mut [Session]) {
        let Some(host) = &self.host else {
            return;
        };
        use std::{
            collections::HashMap,
            process::Stdio,
            time::{Duration, Instant},
        };
        use tokio::io::AsyncWriteExt;

        #[derive(serde::Serialize)]
        struct Request<'a> {
            id: &'a str,
            pid: u32,
        }
        let requests: Vec<_> = sessions
            .iter()
            .filter(|s| {
                s.source_kind == "capture"
                    && self.entries.get(&s.id).is_none_or(|cached| {
                        cached.birth != s.process_birth_filetime
                            || (cached.alive != Some(false)
                                && cached.checked.elapsed() >= Duration::from_secs(5))
                    })
            })
            .map(|s| Request {
                id: &s.id,
                pid: s.pid,
            })
            .collect();
        if !requests.is_empty() {
            let query = async {
                let mut command = tokio::process::Command::new(host);
                command
                    .arg("__collector")
                    .arg("identify")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                #[cfg(windows)]
                command.creation_flags(0x0800_0000); // 使用 CREATE_NO_WINDOW 隐藏窗口
                let mut child = command.spawn().ok()?;
                let mut stdin = child.stdin.take()?;
                stdin
                    .write_all(&serde_json::to_vec(&requests).ok()?)
                    .await
                    .ok()?;
                drop(stdin);
                let output = child.wait_with_output().await.ok()?;
                if !output.status.success() || output.stdout.len() > 128 * 1024 {
                    return None;
                }
                serde_json::from_slice::<Vec<Observed>>(&output.stdout).ok()
            };
            let observed = tokio::time::timeout(Duration::from_secs(3), query)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let observations: HashMap<_, _> =
                observed.into_iter().map(|o| (o.id.clone(), o)).collect();
            for request in &requests {
                if let Some(session) = sessions.iter().find(|s| s.id == request.id) {
                    self.entries.insert(
                        session.id.clone(),
                        Cached {
                            birth: session.process_birth_filetime.clone(),
                            alive: observations
                                .get(&session.id)
                                .and_then(|o| alive(&session.process_birth_filetime, o)),
                            checked: Instant::now(),
                        },
                    );
                }
            }
            // 库的会话数量上限为 512。
            if self.entries.len() > 512 {
                self.entries
                    .retain(|id, _| sessions.iter().any(|s| &s.id == id));
            }
        }
        for session in sessions {
            if session.source_kind == "capture" {
                session.producer_alive =
                    self.entries.get(&session.id).and_then(|entry| entry.alive);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_identity_uses_exact_birth_time_not_pid_or_missing_footer() {
        let mut observed = Observed {
            id: "sample".into(),
            birth: Some("134383145243658810".into()),
            name: Some("QQ".into()),
            missing: false,
        };
        assert_eq!(observed.id, "sample");
        assert_eq!(alive("134383145243658810", &observed), Some(true));
        assert_eq!(alive("134383145243658811", &observed), Some(false));
        observed.name = Some("other".into());
        assert_eq!(alive("134383145243658810", &observed), Some(false));
        observed.name = None;
        observed.birth = None;
        assert_eq!(alive("134383145243658810", &observed), None);
        observed.missing = true;
        assert_eq!(alive("134383145243658810", &observed), Some(false));
    }
}
