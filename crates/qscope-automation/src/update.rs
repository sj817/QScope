//! Online updates from GitHub Releases. Each release publishes `qscope-update.json`, which
//! lists the size and SHA256 of every package, and `qscope-update.json.sig`, an Ed25519
//! signature over the exact manifest bytes. Only a manifest signed by `RELEASE_KEY` is used.

use crate::{Error, runtime};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const REPOSITORY: &str = "sj817/QScope";
/// Base64 of the raw 32-byte Ed25519 public key that signs release manifests. While it is
/// empty this build has no trusted key and online updates stay off.
pub const RELEASE_KEY: &str = "T8/Uu63zwJsaS0Hl3dsiFZzHSBs937j22pMdAFnZjJ4=";
pub const MANIFEST: &str = "qscope-update.json";
const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_PACKAGE: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub product: String,
    pub version: String,
    pub notes_url: String,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    pub kind: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Installed by the setup program, which can replace a running copy.
    Installer,
    /// The desktop and CLI unpacked side by side.
    Portable,
    /// The CLI alone.
    Cli,
    /// A build in the source tree.
    Development,
}

impl Install {
    pub fn name(self) -> &'static str {
        match self {
            Self::Installer => "installer",
            Self::Portable => "portable",
            Self::Cli => "cli",
            Self::Development => "development",
        }
    }
    fn files(self) -> &'static [&'static str] {
        match self {
            Self::Portable => &["QScope.exe", "qscope-cli.exe"],
            Self::Cli => &["qscope-cli.exe"],
            Self::Installer | Self::Development => &[],
        }
    }
}

/// The install type of a product directory.
pub fn classify(directory: &Path) -> Install {
    let name = |path: Option<&Path>| {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().to_lowercase())
    };
    // Test and example binaries live below target/<profile>/, so any such ancestor counts.
    let development = directory.ancestors().any(|path| {
        matches!(name(Some(path)).as_deref(), Some("debug" | "release"))
            && name(path.parent()).as_deref() == Some("target")
    });
    if development {
        Install::Development
    } else if directory.join("qscope-install.json").is_file() {
        Install::Installer
    } else if directory.join("QScope.exe").is_file() {
        Install::Portable
    } else {
        Install::Cli
    }
}

/// The directory and install type of the running product. The global `qs` command is a copy
/// whose ownership record names the CLI it came from.
pub fn locate() -> Result<(Install, PathBuf), Error> {
    let executable = std::env::current_exe()?;
    let mut directory = executable
        .parent()
        .ok_or_else(|| Error::new("update_location", "Executable has no directory"))?
        .to_path_buf();
    if executable
        .file_stem()
        .is_some_and(|stem| stem.eq_ignore_ascii_case("qs"))
    {
        let record = runtime::read_settings(&directory.join(".qs-command.json"))?;
        directory = record["target"]
            .as_str()
            .map(PathBuf::from)
            .and_then(|target| target.parent().map(Path::to_path_buf))
            .ok_or_else(|| Error::new("update_location", "qs does not name its source CLI"))?;
    }
    Ok((classify(&directory), directory))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Settings {
    pub schema_version: u32,
    #[serde(default = "enabled")]
    pub check_on_launch: bool,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub last_check_unix: Option<u64>,
}

fn enabled() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            check_on_launch: true,
            proxy: None,
            last_check_unix: None,
        }
    }
}

fn settings_path(data: &Path) -> PathBuf {
    data.join("settings/update.json")
}

pub fn load(data: &Path) -> Result<Settings, Error> {
    let path = settings_path(data);
    if !path.exists() {
        return Ok(Settings::default());
    }
    let settings: Settings = serde_json::from_value(runtime::read_settings(&path)?)?;
    if settings.schema_version != 1 {
        return Err(Error::new("update_settings", "Unsupported update settings"));
    }
    Ok(settings)
}

pub fn save(data: &Path, settings: &Settings) -> Result<(), Error> {
    runtime::write_settings(&settings_path(data), settings)
}

pub fn now() -> u64 {
    runtime::unix_now()
}

/// A launch checks again once a day at most.
pub fn due(settings: &Settings) -> bool {
    settings.check_on_launch
        && settings
            .last_check_unix
            .is_none_or(|last| now().saturating_sub(last) >= 24 * 60 * 60)
}

/// Normalizes an update proxy. Only plain `http://host:port` proxies are accepted; HTTPS
/// downloads go through them with CONNECT.
pub fn proxy(text: &str) -> Result<Option<String>, Error> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let invalid = |message: &str| Error::new("update_proxy", message);
    let url = reqwest::Url::parse(text)
        .map_err(|_| invalid("代理地址应为 http://主机:端口，例如 http://127.0.0.1:7890"))?;
    if url.scheme() != "http" {
        return Err(invalid("更新代理只支持 http://，不支持 https 或 socks"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("暂不支持带用户名或密码的代理"));
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("代理地址只包含主机和端口"));
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| invalid("代理地址缺少主机"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    Ok(Some(format!("http://{host}:{port}")))
}

// Release files come from github.com and its download host only.
fn trusted(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host == "github.com" || host.ends_with(".githubusercontent.com"))
}

fn client(proxy: Option<&str>) -> Result<reqwest::blocking::Client, Error> {
    let builder = reqwest::blocking::ClientBuilder::from(
        reqwest::Client::builder().read_timeout(Duration::from_secs(30)),
    )
    .connect_timeout(Duration::from_secs(15))
    .timeout(Duration::from_secs(30 * 60))
    .user_agent(concat!("QScope/", env!("CARGO_PKG_VERSION")))
    .redirect(reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if trusted(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("redirect left GitHub")
        }
    }));
    let builder = match proxy {
        Some(proxy) => builder.proxy(reqwest::Proxy::all(proxy)?),
        None => builder.no_proxy(),
    };
    Ok(builder.build()?)
}

fn response(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<reqwest::blocking::Response, Error> {
    let response = client
        .get(url)
        .send()
        .map_err(|error| Error::new("update_network", format!("无法连接 GitHub：{error}")))?;
    if !response.status().is_success() {
        return Err(Error::new(
            "update_network",
            format!("GitHub 返回 {}（{url}）", response.status()),
        ));
    }
    Ok(response)
}

fn fetch(client: &reqwest::blocking::Client, url: &str, limit: u64) -> Result<Vec<u8>, Error> {
    let response = response(client, url)?;
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(Error::new(
            "update_manifest",
            "Release file exceeds its limit",
        ));
    }
    let mut bytes = Vec::new();
    response.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            "update_manifest",
            "Release file exceeds its limit",
        ));
    }
    Ok(bytes)
}

fn semver(value: &str) -> Result<semver::Version, Error> {
    semver::Version::parse(value)
        .map_err(|_| Error::new("update_manifest", format!("Invalid version {value}")))
}

fn validate(manifest: &Manifest) -> Result<(), Error> {
    let invalid = |message: &str| Error::new("update_manifest", message);
    if manifest.schema_version != 1 || manifest.product != "qscope" {
        return Err(invalid("Unsupported update manifest"));
    }
    semver(&manifest.version)?;
    if !manifest.notes_url.is_empty()
        && !reqwest::Url::parse(&manifest.notes_url).is_ok_and(|url| trusted(&url))
    {
        return Err(invalid("Release notes must link to GitHub"));
    }
    if manifest.assets.len() > 8 {
        return Err(invalid("Too many release assets"));
    }
    for (index, asset) in manifest.assets.iter().enumerate() {
        if !["installer", "portable", "cli"].contains(&asset.kind.as_str())
            || manifest.assets[..index]
                .iter()
                .any(|other| other.kind == asset.kind)
            || asset.name.is_empty()
            || asset.name.len() > 128
            || !asset
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || asset.name.starts_with('.')
            || asset.bytes == 0
            || asset.bytes > MAX_PACKAGE
            || asset.sha256.len() != 64
            || !asset
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("Invalid release asset"));
        }
    }
    Ok(())
}

/// Checks the signature over the manifest bytes with a base64 Ed25519 public key.
pub fn verify(key: &str, manifest: &[u8], signature: &[u8]) -> Result<Manifest, Error> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key = engine
        .decode(key.trim())
        .ok()
        .filter(|key| key.len() == 32)
        .ok_or_else(|| Error::new("update_key", "发布签名公钥无效"))?;
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| engine.decode(text.trim()).ok())
        .filter(|signature| signature.len() == 64)
        .ok_or_else(|| Error::new("update_signature", "更新清单签名格式无效"))?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &key)
        .verify(manifest, &signature)
        .map_err(|_| Error::new("update_signature", "更新清单签名校验失败，已拒绝这个版本"))?;
    let manifest: Manifest = serde_json::from_slice(manifest)?;
    validate(&manifest)?;
    Ok(manifest)
}

pub fn newer(candidate: &str, current: &str) -> Result<bool, Error> {
    Ok(semver(candidate)? > semver(current)?)
}

/// Reads and verifies the latest release manifest.
pub fn check(proxy: Option<&str>) -> Result<Manifest, Error> {
    if RELEASE_KEY.is_empty() {
        return Err(Error::new(
            "update_unsigned",
            "此构建没有配置发布签名公钥，不能在线更新",
        ));
    }
    let client = client(proxy)?;
    let base = format!("https://github.com/{REPOSITORY}/releases/latest/download");
    let manifest = fetch(&client, &format!("{base}/{MANIFEST}"), MAX_MANIFEST)?;
    let signature = fetch(&client, &format!("{base}/{MANIFEST}.sig"), 1024)?;
    verify(RELEASE_KEY, &manifest, &signature)
}

pub fn file_sha256(path: &Path) -> Result<String, Error> {
    runtime::no_links(path)?;
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn asset(manifest: &Manifest, install: Install) -> Result<&Asset, Error> {
    let kind = match install {
        Install::Installer => "installer",
        Install::Portable => "portable",
        Install::Cli => "cli",
        Install::Development => {
            return Err(Error::new("update_unsupported", "开发构建不支持在线更新"));
        }
    };
    manifest
        .assets
        .iter()
        .find(|asset| asset.kind == kind)
        .ok_or_else(|| Error::new("update_asset", "这个版本没有适合当前安装方式的更新包"))
}

/// Downloads the package for this install type into `data/updates/<version>/` and checks it
/// against the signed manifest. `progress` receives the bytes received so far.
pub fn download(
    manifest: &Manifest,
    install: Install,
    data: &Path,
    proxy: Option<&str>,
    progress: &mut dyn FnMut(u64),
    cancelled: &AtomicBool,
) -> Result<PathBuf, Error> {
    let asset = asset(manifest, install)?;
    let directory = data.join("updates").join(&manifest.version);
    runtime::regular_directory(&directory)?;
    let target = directory.join(&asset.name);
    runtime::no_links(&target)?;
    if target.is_file()
        && fs::metadata(&target)?.len() == asset.bytes
        && file_sha256(&target)? == asset.sha256
    {
        progress(asset.bytes);
        return Ok(target);
    }
    let client = client(proxy)?;
    let url = format!(
        "https://github.com/{REPOSITORY}/releases/download/v{}/{}",
        manifest.version, asset.name
    );
    let mut response = response(&client, &url)?;
    if response
        .content_length()
        .is_some_and(|length| length != asset.bytes)
    {
        return Err(Error::new("update_download", "更新包大小与签名清单不一致"));
    }
    let mut file = tempfile::Builder::new()
        .prefix(".download-")
        .suffix(".partial")
        .tempfile_in(&directory)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::new("update_cancelled", "更新下载已取消"));
        }
        let size = response.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        received += size as u64;
        if received > asset.bytes {
            return Err(Error::new("update_download", "更新包超过签名清单中的大小"));
        }
        hash.update(&buffer[..size]);
        file.write_all(&buffer[..size])?;
        progress(received);
    }
    if received != asset.bytes || format!("{:x}", hash.finalize()) != asset.sha256 {
        return Err(Error::new(
            "update_download",
            "更新包大小或 SHA256 与签名清单不一致，已丢弃",
        ));
    }
    file.as_file_mut().sync_all()?;
    if target.exists() {
        fs::remove_file(&target)?;
    }
    file.persist_noclobber(&target)
        .map_err(|error| Error::from(error.error))?;
    Ok(target)
}

/// Unpacks a portable or CLI package. The archive must hold exactly the product executables.
pub fn unpack(package: &Path, install: Install) -> Result<Vec<(String, PathBuf)>, Error> {
    let expected = install.files();
    let invalid = |message: &str| Error::new("update_package", message);
    let mut archive = zip::ZipArchive::new(fs::File::open(package)?)
        .map_err(|error| invalid(&error.to_string()))?;
    if expected.is_empty() || archive.len() != expected.len() {
        return Err(invalid("更新包内容与当前安装方式不符"));
    }
    let output = package.with_extension("files");
    runtime::regular_directory(&output)?;
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| invalid(&error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| invalid(&error.to_string()))?
            .to_string();
        if !expected.contains(&name.as_str())
            || files.iter().any(|(seen, _)| *seen == name)
            || !entry.is_file()
            || entry.size() > MAX_PACKAGE
        {
            return Err(invalid("更新包含有意外的文件"));
        }
        let destination = output.join(&name);
        runtime::no_links(&destination)?;
        let mut file = tempfile::NamedTempFile::new_in(&output)?;
        let copied = std::io::copy(&mut (&mut entry).take(MAX_PACKAGE + 1), &mut file)?;
        if copied != entry.size() {
            return Err(invalid("更新包文件长度不一致"));
        }
        file.as_file_mut().sync_all()?;
        if destination.exists() {
            fs::remove_file(&destination)?;
        }
        file.persist_noclobber(&destination)
            .map_err(|error| Error::from(error.error))?;
        files.push((name, destination));
    }
    Ok(files)
}

/// Replaces executables in `directory`. Windows lets a running executable be renamed, so each
/// one moves aside to `<name>.old` and the new copy takes its name; any failure restores the
/// previous files.
pub fn replace(directory: &Path, files: &[(String, PathBuf)]) -> Result<(), Error> {
    let mut done: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    let result = (|| {
        for (name, source) in files {
            let target = directory.join(name);
            let old = directory.join(format!("{name}.old"));
            let fresh = directory.join(format!("{name}.new"));
            for path in [&target, &old, &fresh] {
                runtime::no_links(path)?;
            }
            if old.exists() {
                fs::remove_file(&old).map_err(|error| {
                    Error::new(
                        "update_in_use",
                        format!(
                            "上次更新留下的 {name}.old 仍在使用，请重启 QScope 后再试（{error}）"
                        ),
                    )
                })?;
            }
            if fresh.exists() {
                fs::remove_file(&fresh)?;
            }
            fs::copy(source, &fresh)?;
            let moved = target.exists();
            if moved {
                fs::rename(&target, &old)?;
            }
            if let Err(error) = fs::rename(&fresh, &target) {
                if moved {
                    let _ = fs::rename(&old, &target);
                }
                let _ = fs::remove_file(&fresh);
                return Err(error.into());
            }
            done.push((target, old, moved));
        }
        Ok(())
    })();
    if result.is_err() {
        for (target, old, moved) in done.into_iter().rev() {
            if moved {
                let _ = fs::remove_file(&target);
                let _ = fs::rename(&old, &target);
            }
        }
    }
    result
}

fn same_file(left: &Path, right: &Path) -> bool {
    let normal = |path: &Path| {
        crate::display_path(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
            .to_lowercase()
    };
    normal(left) == normal(right)
}

// The global qs command is a copy of the installed CLI; register the new one in its place.
// A running qs can only be renamed, so it moves aside first.
pub fn refresh_global_command(directory: &Path) -> serde_json::Value {
    let Ok(home) = crate::skills::user_home() else {
        return serde_json::json!(null);
    };
    let bin = home.join(".local/bin");
    let Ok(record) = runtime::read_settings(&bin.join(".qs-command.json")) else {
        return serde_json::json!(null);
    };
    let cli = directory.join("qscope-cli.exe");
    if !record["target"]
        .as_str()
        .is_some_and(|target| same_file(Path::new(target), &cli))
    {
        return serde_json::json!(null);
    }
    let command = bin.join("qs.exe");
    let parked = bin.join("qs.exe.old");
    let running = std::env::current_exe().is_ok_and(|current| same_file(&current, &command));
    if running {
        let _ = std::fs::remove_file(&parked);
        if std::fs::rename(&command, &parked).is_err() {
            return serde_json::json!({"refreshed":false,"hint":"关闭正在运行的 qs 后执行 qscope-cli cli install"});
        }
    }
    let refreshed = Command::new(&cli)
        .args(["cli", "install"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !refreshed && running {
        let _ = std::fs::rename(&parked, &command);
    }
    serde_json::json!({"refreshed":refreshed})
}

/// Starts the downloaded setup program. It stops an idle backend, closes the desktop, installs
/// over the current copy and opens QScope again.
pub fn start_installer(package: &Path) -> Result<(), Error> {
    Command::new(package)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/RELAUNCH=1"])
        .spawn()
        .map_err(|error| Error::new("update_install", format!("无法启动安装程序：{error}")))?;
    Ok(())
}

/// Asks the desktop to close once its backend stops for an update.
pub fn mark_restart(data: &Path, version: &str) -> Result<(), Error> {
    runtime::write_settings(
        &data.join(runtime::RESTART_MARKER),
        &serde_json::json!({"schema_version":1,"version":version,"time_unix":now()}),
    )
}

/// Waits until the replaced desktop and backend have exited, then opens the new desktop.
pub fn relaunch(directory: &Path, data: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let waiting = ["QScope.exe.old", "qscope-cli.exe.old"]
            .iter()
            .map(|name| directory.join(name))
            .filter(|path| path.exists() && fs::remove_file(path).is_err())
            .count();
        if waiting == 0 || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = fs::remove_file(data.join(runtime::RESTART_MARKER));
    Command::new(directory.join("QScope.exe"))
        .spawn()
        .map_err(|error| Error::new("update_restart", format!("无法打开新版 QScope：{error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair as _;

    fn signed(manifest: &serde_json::Value) -> (String, Vec<u8>, Vec<u8>) {
        let random = ring::rand::SystemRandom::new();
        let document = ring::signature::Ed25519KeyPair::generate_pkcs8(&random).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap();
        let engine = base64::engine::general_purpose::STANDARD;
        let bytes = serde_json::to_vec(manifest).unwrap();
        let signature = engine.encode(pair.sign(&bytes).as_ref()).into_bytes();
        (engine.encode(pair.public_key().as_ref()), bytes, signature)
    }

    fn manifest() -> serde_json::Value {
        serde_json::json!({"schema_version":1,"product":"qscope","version":"0.2.0","notes_url":"https://github.com/sj817/QScope/releases/tag/v0.2.0","assets":[{"kind":"cli","name":"QScope-0.2.0-windows-x64-cli-webui.zip","bytes":10,"sha256":"a".repeat(64)}]})
    }

    #[test]
    fn manifests_need_the_release_signature_over_their_exact_bytes() {
        let (key, bytes, signature) = signed(&manifest());
        assert_eq!(verify(&key, &bytes, &signature).unwrap().version, "0.2.0");
        let mut changed = bytes.clone();
        let at = changed.iter().position(|byte| *byte == b'2').unwrap();
        changed[at] = b'9';
        assert_eq!(
            verify(&key, &changed, &signature).unwrap_err().code,
            "update_signature"
        );
        let (other, _, _) = signed(&manifest());
        assert_eq!(
            verify(&other, &bytes, &signature).unwrap_err().code,
            "update_signature"
        );
        assert_eq!(
            verify(&key, &bytes, b"not base64").unwrap_err().code,
            "update_signature"
        );
        assert_eq!(
            verify("short", &bytes, &signature).unwrap_err().code,
            "update_key"
        );
    }

    // Signed by tools/release-sign.ts with a throwaway key, so both sides agree on the format.
    #[test]
    fn manifests_signed_by_the_release_tool_verify() {
        let key = "TvfTaDVqFJyKZP7nI/paRoe2a2LzwrFuS7+3RxVAjPQ=";
        let manifest = r#"{
  "schema_version": 1,
  "product": "qscope",
  "version": "0.2.0",
  "notes_url": "https://github.com/sj817/QScope/releases/tag/v0.2.0",
  "assets": [
    {
      "kind": "cli",
      "name": "QScope-0.2.0-windows-x64-cli-webui.zip",
      "bytes": 3,
      "sha256": "99bb88401742848e032fd6f51709415fb6be169a72d2e5d7fc44289255160d3c"
    }
  ]
}
"#;
        let signature = b"l+aIq3DvkPkJzFh4zP5lb4DrH9BAhHLGE4h45CFSglbQnPY4Wzv13jvovcmqU1zLcuOGxV9MpI8B4lKbEQzjBA==\n";
        let verified = verify(key, manifest.as_bytes(), signature).unwrap();
        assert_eq!(verified.assets[0].bytes, 3);
        let reformatted = manifest.replace("  ", "    ");
        assert!(verify(key, reformatted.as_bytes(), signature).is_err());
    }

    #[test]
    fn signed_manifests_still_reject_unsafe_assets_and_links() {
        for change in [
            serde_json::json!({"name":"../qscope-cli.exe"}),
            serde_json::json!({"name":".hidden"}),
            serde_json::json!({"kind":"driver"}),
            serde_json::json!({"sha256":"A".repeat(64)}),
            serde_json::json!({"bytes":0}),
        ] {
            let mut value = manifest();
            for (key, field) in change.as_object().unwrap() {
                value["assets"][0][key] = field.clone();
            }
            let (key, bytes, signature) = signed(&value);
            assert_eq!(
                verify(&key, &bytes, &signature).unwrap_err().code,
                "update_manifest"
            );
        }
        let mut value = manifest();
        value["notes_url"] = "https://example.com/notes".into();
        let (key, bytes, signature) = signed(&value);
        assert_eq!(
            verify(&key, &bytes, &signature).unwrap_err().code,
            "update_manifest"
        );
        value["notes_url"] = "".into();
        value["version"] = "next".into();
        let (key, bytes, signature) = signed(&value);
        assert_eq!(
            verify(&key, &bytes, &signature).unwrap_err().code,
            "update_manifest"
        );
    }

    #[test]
    fn versions_compare_by_semver_and_proxies_must_be_plain_http() {
        assert!(newer("0.10.0", "0.9.9").unwrap());
        assert!(!newer("0.2.0", "0.2.0").unwrap());
        assert!(newer("0.2.0", "0.2.0-rc.1").unwrap());
        assert_eq!(proxy(" ").unwrap(), None);
        assert_eq!(
            proxy("http://127.0.0.1:7890/").unwrap().as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            proxy("http://proxy.lan").unwrap().as_deref(),
            Some("http://proxy.lan:80")
        );
        for bad in [
            "https://127.0.0.1:7890",
            "socks5://127.0.0.1:1080",
            "http://user:secret@127.0.0.1:7890",
            "http://127.0.0.1:7890/path",
            "127.0.0.1:7890",
        ] {
            assert_eq!(proxy(bad).unwrap_err().code, "update_proxy", "{bad}");
        }
        assert!(trusted(
            &reqwest::Url::parse("https://objects.githubusercontent.com/x").unwrap()
        ));
        assert!(!trusted(
            &reqwest::Url::parse("http://github.com/x").unwrap()
        ));
        assert!(!trusted(
            &reqwest::Url::parse("https://github.com.example.com/x").unwrap()
        ));
    }

    #[test]
    fn install_types_follow_the_files_beside_the_product() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let directory = root.path().join("QScope");
        fs::create_dir(&directory).unwrap();
        assert_eq!(classify(&directory), Install::Cli);
        fs::write(directory.join("QScope.exe"), b"desktop").unwrap();
        assert_eq!(classify(&directory), Install::Portable);
        fs::write(directory.join("qscope-install.json"), b"{}").unwrap();
        assert_eq!(classify(&directory), Install::Installer);
        let build = root.path().join("target/release");
        fs::create_dir_all(&build).unwrap();
        assert_eq!(classify(&build), Install::Development);
        assert_eq!(classify(&build.join("deps")), Install::Development);
    }

    #[test]
    fn packages_replace_running_files_by_renaming_and_roll_back_on_failure() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let product = root.path().join("product");
        fs::create_dir(&product).unwrap();
        fs::write(product.join("QScope.exe"), b"old desktop").unwrap();
        fs::write(product.join("qscope-cli.exe"), b"old cli").unwrap();
        let package = root.path().join("QScope-0.2.0-windows-x64-portable.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&package).unwrap());
        for (name, bytes) in [
            ("QScope.exe", b"new desktop".as_slice()),
            ("qscope-cli.exe", b"new cli"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let files = unpack(&package, Install::Portable).unwrap();
        assert!(unpack(&package, Install::Cli).is_err());
        replace(&product, &files).unwrap();
        assert_eq!(
            fs::read(product.join("QScope.exe")).unwrap(),
            b"new desktop"
        );
        assert_eq!(
            fs::read(product.join("qscope-cli.exe.old")).unwrap(),
            b"old cli"
        );
        runtime::cleanup_replaced(&product);
        assert!(!product.join("qscope-cli.exe.old").exists());
        // A second file that cannot move restores the first.
        fs::create_dir(product.join("qscope-cli.exe.old")).unwrap();
        let next = vec![
            ("QScope.exe".to_string(), files[0].1.clone()),
            ("qscope-cli.exe".to_string(), files[1].1.clone()),
        ];
        fs::write(&files[0].1, b"newer desktop").unwrap();
        assert!(replace(&product, &next).is_err());
        assert_eq!(
            fs::read(product.join("QScope.exe")).unwrap(),
            b"new desktop"
        );
    }

    #[test]
    fn downloads_reuse_only_a_package_that_matches_the_manifest() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let mut value = manifest();
        let directory = root.path().join("updates/0.2.0");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("QScope-0.2.0-windows-x64-cli-webui.zip");
        fs::write(&path, b"0123456789").unwrap();
        value["assets"][0]["sha256"] = format!("{:x}", Sha256::digest(b"0123456789")).into();
        let manifest: Manifest = serde_json::from_value(value).unwrap();
        let mut seen = 0;
        let found = download(
            &manifest,
            Install::Cli,
            root.path(),
            Some("http://127.0.0.1:9"),
            &mut |bytes| seen = bytes,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!((found, seen), (path, 10));
        assert_eq!(
            asset(&manifest, Install::Portable).unwrap_err().code,
            "update_asset"
        );
        assert_eq!(
            asset(&manifest, Install::Development).unwrap_err().code,
            "update_unsupported"
        );
    }
}
