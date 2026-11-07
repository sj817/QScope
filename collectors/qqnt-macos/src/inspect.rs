//! 只读检查所选应用的身份和签名。
use qscope_automation::Error;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub state: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub schema_version: u32,
    pub app: String,
    pub version: String,
    pub build: String,
    pub bundle_identifier: String,
    pub checks: Vec<Check>,
    pub scan: crate::Scan,
}

#[cfg(not(target_os = "macos"))]
pub fn inspect(_app: &Path) -> Result<Inspection, Error> {
    Err(Error::new(
        "unsupported_platform",
        "App permissions can only be inspected on macOS; use qq scan --module for offline discovery",
    ))
}

#[cfg(target_os = "macos")]
pub fn inspect(app: &Path) -> Result<Inspection, Error> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    fn command(program: &str, args: &[&std::ffi::OsStr]) -> Result<(bool, String, String), Error> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout pipe");
        let stderr = child.stderr.take().expect("stderr pipe");
        fn read(
            pipe: impl Read + Send + 'static,
        ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.take(65537).read_to_end(&mut bytes)?;
                Ok(bytes)
            })
        }
        let out = read(stdout);
        let err = read(stderr);
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(match result {
                        Err(error) => Error::from(error),
                        _ => {
                            Error::new("inspection_timeout", "macOS inspection exceeded 10 seconds")
                        }
                    });
                }
            }
        };
        let stdout = out
            .join()
            .map_err(|_| Error::new("inspection", "Inspection output reader failed"))??;
        let stderr = err
            .join()
            .map_err(|_| Error::new("inspection", "Inspection error reader failed"))??;
        let status = status?;
        if stdout.len() > 65536 || stderr.len() > 65536 {
            return Err(Error::new(
                "inspection_limit",
                "macOS inspection output exceeds 64 KiB",
            ));
        }
        Ok((
            status.success(),
            String::from_utf8_lossy(&stdout).trim().into(),
            String::from_utf8_lossy(&stderr).trim().into(),
        ))
    }

    let app = app.canonicalize()?;
    if !app.is_dir() || app.extension().is_none_or(|extension| extension != "app") {
        return Err(Error::new("qq_app", "Select a QQ.app bundle"));
    }
    let info = app.join("Contents/Info.plist");
    if std::fs::metadata(&info)?.len() > 1024 * 1024 {
        return Err(Error::new("qq_app", "App metadata exceeds 1 MiB"));
    }
    let property = |name: &str| -> Result<String, Error> {
        let (success, value, detail) = command(
            "/usr/bin/plutil",
            &[
                "-extract".as_ref(),
                name.as_ref(),
                "raw".as_ref(),
                "-o".as_ref(),
                "-".as_ref(),
                info.as_os_str(),
            ],
        )?;
        if !success || value.len() > 256 {
            return Err(Error::new(
                "qq_app",
                format!("Cannot read app property {name}: {detail}"),
            ));
        }
        Ok(value)
    };
    let identifier = property("CFBundleIdentifier")?;
    if identifier != "com.tencent.qq" {
        return Err(Error::new(
            "qq_app",
            "The selected source app is not com.tencent.qq",
        ));
    }
    let version = property("CFBundleShortVersionString")?;
    let build = property("CFBundleVersion")?;
    let (signature_ok, _, signature_detail) = command(
        "/usr/bin/codesign",
        &["--verify".as_ref(), "--strict".as_ref(), app.as_os_str()],
    )?;
    let (entitlements_ok, entitlements, entitlements_error) = command(
        "/usr/bin/codesign",
        &[
            "-d".as_ref(),
            "--arch".as_ref(),
            "arm64".as_ref(),
            "--entitlements".as_ref(),
            "-".as_ref(),
            app.as_os_str(),
        ],
    )?;
    let (display_ok, _, signature_info) = command(
        "/usr/bin/codesign",
        &[
            "-dv".as_ref(),
            "--verbose=4".as_ref(),
            "--arch".as_ref(),
            "arm64".as_ref(),
            app.as_os_str(),
        ],
    )?;
    let mut checks = vec![Check {
        id: "code_signature",
        state: if signature_ok { "ok" } else { "unverified" },
        detail: if signature_ok {
            "codesign --verify --strict passed for this source app".into()
        } else {
            signature_detail
        },
    }];
    let hardened = display_ok.then(|| runtime_flag(&signature_info)).flatten();
    checks.push(Check {
        id: "hardened_runtime",
        state: if hardened.is_some() {
            "observed"
        } else {
            "unknown"
        },
        detail: hardened
            .map(|enabled| format!("runtime flag: {enabled}"))
            .unwrap_or(signature_info),
    });
    for (id, key) in [
        ("app_sandbox", "com.apple.security.app-sandbox"),
        (
            "library_validation_exception",
            "com.apple.security.cs.disable-library-validation",
        ),
        (
            "dyld_environment_exception",
            "com.apple.security.cs.allow-dyld-environment-variables",
        ),
        ("debug_target", "com.apple.security.get-task-allow"),
    ] {
        let value = if entitlements_ok {
            entitlement(&entitlements, key)
        } else {
            None
        };
        checks.push(Check {
            id,
            state: if value.is_some() {
                "observed"
            } else {
                "unknown"
            },
            detail: match value {
                Some(true) => format!("{key} = true"),
                Some(false) => format!("{key} is absent or false"),
                None => format!("Entitlements could not be interpreted: {entitlements_error}"),
            },
        });
    }
    checks.push(Check { id: "research_isolation", state: "unverified", detail: "A separate research copy, data isolation and startup injection have not been validated. No app or permissions were modified.".into() });
    Ok(Inspection {
        schema_version: 1,
        app: qscope_automation::display_path(&app),
        version,
        build,
        bundle_identifier: identifier,
        checks,
        scan: crate::scan(&app.join("Contents/Resources/app/wrapper.node"))?,
    })
}

/// codesign 解码字典可处理 DER 权限声明，未知格式保留 unknown 状态。
#[cfg(any(target_os = "macos", test))]
fn runtime_flag(output: &str) -> Option<bool> {
    let line = output
        .lines()
        .find(|line| line.starts_with("CodeDirectory "))?;
    let word = line
        .split_whitespace()
        .find_map(|word| word.strip_prefix("flags=0x"))?;
    u32::from_str_radix(word.split('(').next()?, 16)
        .ok()
        .map(|flags| flags & 0x10000 != 0)
}

#[cfg(any(target_os = "macos", test))]
fn entitlement(output: &str, key: &str) -> Option<bool> {
    let lines: Vec<_> = output.lines().map(str::trim).collect();
    if lines.first().copied() != Some("[Dict]") {
        return None;
    }
    let expected = format!("[Key] {key}");
    let Some(index) = lines.iter().position(|line| *line == expected) else {
        return Some(false);
    };
    match (lines.get(index + 1), lines.get(index + 2)) {
        (Some(&"[Value]"), Some(&"[Bool] true")) => Some(true),
        (Some(&"[Value]"), Some(&"[Bool] false")) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_entitlements_are_not_reported_as_absent() {
        assert_eq!(
            runtime_flag("CodeDirectory flags=0x10002(adhoc,runtime)"),
            Some(true)
        );
        assert_eq!(runtime_flag("CodeDirectory flags=0x2(adhoc)"), Some(false));
        assert_eq!(runtime_flag("unrecognized"), None);
        assert_eq!(entitlement("", "a"), None);
        assert_eq!(entitlement("<plist/>", "a"), None);
        assert_eq!(
            entitlement("[Dict]\n [Key] a\n [Value]\n [Bool] true", "a"),
            Some(true)
        );
        assert_eq!(
            entitlement("[Dict]\n [Key] a\n [Value]\n [String] true", "a"),
            None
        );
        assert_eq!(entitlement("[Dict]", "a"), Some(false));
    }
}
