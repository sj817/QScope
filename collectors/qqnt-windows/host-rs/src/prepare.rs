use crate::package::{self, Identity};
use qscope_automation::{Error, runtime};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn progress(
    path: &Path,
    identity: &Identity,
    stage: &str,
    message: &str,
    downloaded: u64,
) -> Result<(), Error> {
    runtime::no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("qq_prepare", "Progress path needs a parent directory"))?;
    runtime::regular_directory(parent)?;
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    let value = json!({"schema_version":1,"stage":stage,"message":message,"downloaded":downloaded.to_string(),"total":identity.installer_bytes.to_string(),"input_sha256":identity.installer_sha256,"time_unix":SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| Error::new("clock",error.to_string()))?.as_secs().to_string()});
    output.write_all(&serde_json::to_vec(&value)?)?;
    output
        .persist(path)
        .map_err(|error| Error::from(error.error))?;
    Ok(())
}

fn signed_url(client: &Client, identity: &Identity) -> Result<Url, Error> {
    let raw = Url::parse(&identity.installer_url)
        .map_err(|error| Error::new("qq_download", error.to_string()))?;
    let response = client
        .post(&identity.installer_sign_url)
        .timeout(Duration::from_secs(20))
        .header(
            "x-oidb",
            "{\"uint32_command\":\"0x9b8e\",\"uint32_service_type\":1}",
        )
        .header("Origin", "https://im.qq.com")
        .header("Referer", "https://im.qq.com/index/")
        .json(&json!({"url":raw.as_str()}))
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::new(
            "qq_download",
            "Download signature response exceeds its limit",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    let signed = value["data"]["url"]
        .as_str()
        .filter(|_| value["retcode"] == 0)
        .ok_or_else(|| {
            Error::new(
                "qq_download",
                "QQ download signature service returned an error",
            )
        })?;
    let url = Url::parse(signed).map_err(|error| Error::new("qq_download", error.to_string()))?;
    let sign = url
        .query_pairs()
        .find(|(key, _)| key == "sign")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    let time = url
        .query_pairs()
        .find(|(key, _)| key == "t")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    if url.scheme() != "https"
        || url.host_str() != raw.host_str()
        || url.path() != raw.path()
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || sign.is_empty()
        || sign.len() > 128
        || !sign.bytes().all(|byte| byte.is_ascii_hexdigit())
        || time.is_empty()
        || time.len() > 20
        || !time.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::new(
            "qq_download",
            "Signed QQ URL does not match the pinned package",
        ));
    }
    Ok(url)
}

pub fn run(
    root: &Path,
    status: &Path,
    provided: Option<&Path>,
    cancelled: &AtomicBool,
) -> Result<Value, Error> {
    let identity = package::identity()?;
    runtime::regular_directory(root)?;
    let lock_path = root.join(".prepare.lock");
    runtime::no_links(&lock_path)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.try_lock()
        .map_err(|_| Error::new("qq_busy", "Another operation is preparing this QQ package"))?;
    let unpacked = root.join("unpacked");
    let result: Result<Value, Error> = (|| {
        progress(status, &identity, "checking", "检查 QQ 安装包", 0)?;
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::new("qq_cancelled", "QQ preparation cancelled"));
        }
        if let Some(path) = provided {
            runtime::no_links(path)?;
            if std::fs::metadata(path)?.len() != identity.installer_bytes
                || package::file_sha256(path)? != identity.installer_sha256
            {
                return Err(Error::new(
                    "qq_installer",
                    "The provided installer does not match the supported QQ build",
                ));
            }
        }
        if !unpacked.exists() {
            let installer = match provided {
                Some(path) => path.to_path_buf(),
                None => {
                    let installer = root.join("QQ_9.9.37_261103_x64_01.exe");
                    runtime::no_links(&installer)?;
                    if !installer.exists() {
                        let client = reqwest::blocking::ClientBuilder::from(
                            reqwest::Client::builder().read_timeout(Duration::from_secs(5)),
                        )
                        .no_proxy()
                        .redirect(Policy::none())
                        .connect_timeout(Duration::from_secs(10))
                        .timeout(Duration::from_secs(900))
                        .build()?;
                        let url = signed_url(&client, &identity)?;
                        let mut response = client.get(url).send()?.error_for_status()?;
                        if response
                            .content_length()
                            .is_some_and(|bytes| bytes != identity.installer_bytes)
                        {
                            return Err(Error::new(
                                "qq_download",
                                "QQ download length differs from its identity",
                            ));
                        }
                        let mut temporary = tempfile::Builder::new()
                            .prefix("qq-download-")
                            .suffix(".partial")
                            .tempfile_in(root)?;
                        let mut buffer = [0u8; 65536];
                        let mut received = 0u64;
                        let mut report = Instant::now();
                        loop {
                            if cancelled.load(Ordering::Acquire) {
                                return Err(Error::new("qq_cancelled", "QQ preparation cancelled"));
                            }
                            let size = response.read(&mut buffer)?;
                            if size == 0 {
                                break;
                            }
                            received += size as u64;
                            if received > identity.installer_bytes {
                                return Err(Error::new(
                                    "qq_download",
                                    "QQ download exceeds its pinned size",
                                ));
                            }
                            temporary.write_all(&buffer[..size])?;
                            if report.elapsed() >= Duration::from_millis(250) {
                                progress(
                                    status,
                                    &identity,
                                    "downloading",
                                    "正在下载官方 QQ 安装包",
                                    received,
                                )?;
                                report = Instant::now();
                            }
                        }
                        temporary.as_file_mut().sync_all()?;
                        if received != identity.installer_bytes
                            || package::file_sha256(temporary.path())? != identity.installer_sha256
                        {
                            return Err(Error::new(
                                "qq_download",
                                "QQ installer length or SHA256 mismatch",
                            ));
                        }
                        temporary
                            .persist_noclobber(&installer)
                            .map_err(|error| Error::from(error.error))?;
                    }
                    installer
                }
            };
            progress(
                status,
                &identity,
                "extracting",
                "正在免安装解包并核验模块",
                identity.installer_bytes,
            )?;
            package::extract_cancellable(&installer, &unpacked, cancelled)?;
        }
        package::verify_client(&unpacked.join("Files"), &identity)?;
        progress(
            status,
            &identity,
            "ready",
            "QQ 安装包已准备",
            identity.installer_bytes,
        )?;
        Ok(
            json!({"schema_version":1,"build":identity.build,"root":unpacked,"installer_sha256":identity.installer_sha256}),
        )
    })();
    if let Err(error) = &result {
        let _ = progress(status, &identity, "error", &error.message, 0);
    }
    result
}
