use crate::{
    common,
    package::{self, Identity},
};
use qscope_automation::{Error, runtime};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::FileExt,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const CHUNK: u64 = 8 * 1024 * 1024;

fn download_client() -> Result<Client, Error> {
    // The blocking timeout bounds each send/read through reqwest's blocking
    // waiter. An async read_timeout creates a Tokio Sleep when the body is
    // polled on our plain range threads, which have no reactor context.
    Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| Error::from(error.without_url()))
}

fn progress(
    path: &Path,
    identity: &Identity,
    stage: &str,
    message: &str,
    downloaded: u64,
) -> Result<(), Error> {
    common::write_json(
        path,
        &json!({"schema_version":1,"stage":stage,"message":message,"downloaded":downloaded.to_string(),"total":identity.installer_bytes.to_string(),"input_sha256":identity.installer_sha256,"time_unix":runtime::unix_now().to_string()}),
    )
}

fn validate_signed(raw: &Url, value: &Value) -> Result<Url, Error> {
    let value = value["data"]["url"]
        .as_str()
        .filter(|_| value["retcode"] == 0)
        .ok_or_else(|| {
            Error::new(
                "qq_download",
                "Official QQ URL-sign service rejected the request",
            )
        })?;
    let signed = Url::parse(value).map_err(|error| Error::new("qq_download", error.to_string()))?;
    let pairs: Vec<_> = signed.query_pairs().collect();
    let sign: Vec<_> = pairs.iter().filter(|(key, _)| key == "sign").collect();
    let time: Vec<_> = pairs.iter().filter(|(key, _)| key == "t").collect();
    if signed.scheme() != "https"
        || signed.host_str() != raw.host_str()
        || signed.path() != raw.path()
        || signed.port_or_known_default() != Some(443)
        || !signed.username().is_empty()
        || signed.password().is_some()
        || signed.fragment().is_some()
        || pairs.len() != 2
        || sign.len() != 1
        || time.len() != 1
        || sign[0].1.is_empty()
        || sign[0].1.len() > 128
        || !sign[0].1.bytes().all(|byte| byte.is_ascii_hexdigit())
        || time[0].1.is_empty()
        || time[0].1.len() > 20
        || !time[0].1.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::new(
            "qq_download",
            "Signed QQ URL escaped the pinned HTTPS package",
        ));
    }
    Ok(signed)
}

fn signed_url(client: &Client, identity: &Identity) -> Result<Url, Error> {
    let raw = Url::parse(&identity.installer_url)
        .map_err(|error| Error::new("qq_download", error.to_string()))?;
    let response = client
        .post(&identity.installer_sign_url)
        .header(
            "x-oidb",
            "{\"uint32_command\":\"0x9b8e\",\"uint32_service_type\":1}",
        )
        .header("Origin", "https://im.qq.com")
        .header("Referer", "https://im.qq.com/index/")
        .json(&json!({"url":raw.as_str()}))
        .send()
        .map_err(|error| Error::from(error.without_url()))?
        .error_for_status()
        .map_err(|error| Error::from(error.without_url()))?;
    let mut bytes = Vec::new();
    response
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("qq_download", "Cannot read the official URL-sign response"))?;
    if bytes.len() > 65536 {
        return Err(Error::new(
            "qq_download",
            "URL-sign response exceeds 64 KiB",
        ));
    }
    validate_signed(&raw, &serde_json::from_slice(&bytes)?)
}

#[derive(Clone, Copy)]
struct ByteRange {
    start: u64,
    end: u64,
    total: u64,
}

fn download_range(
    client: &Client,
    url: &Url,
    output: &File,
    range: ByteRange,
    stop: &AtomicBool,
    failed: &AtomicBool,
) -> Result<(), Error> {
    common::cancelled(stop)?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let ByteRange { start, end, total } = range;
    let mut response = client
        .get(url.clone())
        .header("Range", format!("bytes={start}-{end}"))
        .header("Accept-Encoding", "identity")
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://im.qq.com/")
        .send()
        .map_err(|error| Error::from(error.without_url()))?;
    let expected = end - start + 1;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
        || response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            != Some(format!("bytes {start}-{end}/{total}").as_str())
        || response
            .content_length()
            .is_some_and(|length| length != expected)
    {
        return Err(Error::new(
            "qq_download",
            "Official QQ server returned an unexpected byte range",
        ));
    }
    let mut received = 0u64;
    let mut bytes = [0u8; 65536];
    loop {
        common::cancelled(stop)?;
        if failed.load(Ordering::Acquire) {
            return Err(Error::new(
                "qq_cancelled",
                "Another QQ download range failed",
            ));
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "qq_download",
                "QQ download range exceeded 90 seconds",
            ));
        }
        let length = match response.read(&mut bytes) {
            Ok(length) => length,
            Err(_) => {
                common::cancelled(stop)?;
                return Err(Error::new(
                    "qq_download",
                    "Failed to read a QQ download range",
                ));
            }
        };
        if length == 0 {
            break;
        }
        if received + length as u64 > expected {
            return Err(Error::new(
                "qq_download",
                "QQ range exceeds its declared length",
            ));
        }
        output.write_all_at(&bytes[..length], start + received)?;
        received += length as u64;
    }
    if received != expected {
        return Err(Error::new("qq_download", "Truncated QQ download range"));
    }
    Ok(())
}

fn download(
    root: &Path,
    status: &Path,
    identity: &Identity,
    stop: &AtomicBool,
) -> Result<std::path::PathBuf, Error> {
    let destination = root.join(package::INSTALLER_NAME);
    if destination.exists() {
        package::verify_installer(&destination, identity)?;
        return Ok(destination);
    }
    let client = download_client()?;
    let url = signed_url(&client, identity)?;
    download_ranges(root, status, identity, stop, &client, &url)
}

fn download_ranges(
    root: &Path,
    status: &Path,
    identity: &Identity,
    stop: &AtomicBool,
    client: &Client,
    url: &Url,
) -> Result<std::path::PathBuf, Error> {
    let destination = root.join(package::INSTALLER_NAME);
    let temporary = tempfile::Builder::new()
        .prefix("qq-download-")
        .suffix(".partial")
        .tempfile_in(root)?;
    temporary.as_file().set_len(identity.installer_bytes)?;
    let next = AtomicUsize::new(0);
    let downloaded = AtomicU64::new(0);
    let failed = AtomicBool::new(false);
    let error = Mutex::new(None);
    let begin = Instant::now();
    std::thread::scope(|scope| -> Result<(), Error> {
        let mut workers = Vec::new();
        for _ in 0..4 {
            let file = temporary.as_file();
            let next = &next;
            let downloaded = &downloaded;
            let failed = &failed;
            let error = &error;
            workers.push(scope.spawn(move || {
                loop {
                    if failed.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
                        break;
                    }
                    let start = next.fetch_add(1, Ordering::Relaxed) as u64 * CHUNK;
                    if start >= identity.installer_bytes {
                        break;
                    }
                    let end = (start + CHUNK).min(identity.installer_bytes) - 1;
                    let result = if begin.elapsed() >= Duration::from_secs(900) {
                        Err(Error::new("qq_download", "QQ download exceeded 15 minutes"))
                    } else {
                        let mut last =
                            Err(Error::new("qq_download", "QQ range retry limit reached"));
                        for attempt in 0..3 {
                            last = download_range(
                                client,
                                url,
                                file,
                                ByteRange {
                                    start,
                                    end,
                                    total: identity.installer_bytes,
                                },
                                stop,
                                failed,
                            );
                            if last.is_ok()
                                || common::cancelled(stop).is_err()
                                || failed.load(Ordering::Acquire)
                            {
                                break;
                            }
                            if attempt < 2 {
                                std::thread::sleep(Duration::from_millis(250));
                            }
                        }
                        last
                    };
                    match result {
                        Ok(()) => {
                            downloaded.fetch_add(end - start + 1, Ordering::Relaxed);
                        }
                        Err(value) => {
                            if let Ok(mut first) = error.lock()
                                && first.is_none()
                            {
                                *first = Some(value);
                            }
                            failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            }));
        }
        let mut result = Ok(());
        while workers.iter().any(|worker| !worker.is_finished()) {
            if let Err(value) = progress(
                status,
                identity,
                "downloading",
                "正在分段下载官方 QQ 安装包",
                downloaded.load(Ordering::Relaxed),
            ) {
                failed.store(true, Ordering::Release);
                result = Err(value);
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        for worker in workers {
            if worker.join().is_err() {
                failed.store(true, Ordering::Release);
                if result.is_ok() {
                    result = Err(common::failure("Download worker failed"));
                }
            }
        }
        result
    })?;
    common::cancelled(stop)?;
    if let Some(error) = error
        .into_inner()
        .map_err(|_| common::failure("Download state lock failed"))?
    {
        return Err(error);
    }
    if downloaded.load(Ordering::Relaxed) != identity.installer_bytes {
        return Err(Error::new("qq_download", "QQ download is incomplete"));
    }
    temporary.as_file().sync_all()?;
    package::verify_installer(temporary.path(), identity)?;
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| error.error)?;
    Ok(destination)
}

struct Mount {
    directory: Option<tempfile::TempDir>,
    point: std::path::PathBuf,
}
impl Mount {
    fn detach(&mut self) -> Result<(), Error> {
        let result = common::command(
            "/usr/bin/hdiutil",
            &common::args(&["detach".as_ref(), self.point.as_os_str()]),
            Duration::from_secs(30),
            &AtomicBool::new(false),
        );
        if result.is_err()
            && let Some(directory) = self.directory.take()
        {
            let _ = directory.keep();
        }
        result.map(|_| ())
    }
}
impl Drop for Mount {
    fn drop(&mut self) {
        if self.directory.is_some() {
            let _ = self.detach();
        }
    }
}

fn extract(
    installer: &Path,
    root: &Path,
    identity: &Identity,
    stop: &AtomicBool,
) -> Result<(), Error> {
    common::command(
        "/usr/bin/hdiutil",
        &common::args(&["verify".as_ref(), installer.as_os_str()]),
        Duration::from_secs(120),
        stop,
    )?;
    let owned = tempfile::Builder::new()
        .prefix("qq-mount-")
        .tempdir_in(root)?;
    let point = owned.path().join("volume");
    common::directory(&point)?;
    let mut mount = Mount {
        directory: Some(owned),
        point,
    };
    common::command(
        "/usr/bin/hdiutil",
        &common::args(&[
            "attach".as_ref(),
            "-readonly".as_ref(),
            "-nobrowse".as_ref(),
            "-noautoopen".as_ref(),
            "-mountpoint".as_ref(),
            mount.point.as_os_str(),
            installer.as_os_str(),
        ]),
        Duration::from_secs(120),
        stop,
    )?;
    let source = mount.point.join("QQ.app");
    package::verify_client(&source, identity)?;
    let stage = tempfile::Builder::new()
        .prefix("qq-unpack-")
        .tempdir_in(root)?;
    let output = stage.path().join("QQ.app");
    common::command(
        "/usr/bin/ditto",
        &common::args(&[source.as_os_str(), output.as_os_str()]),
        Duration::from_secs(180),
        stop,
    )?;
    package::verify_client(&output, identity)?;
    mount.detach()?;
    mount.directory.take();
    common::cancelled(stop)?;
    fs::rename(stage.path(), root.join("unpacked"))?;
    Ok(())
}

fn prepare(
    root: &Path,
    status: &Path,
    provided: Option<&Path>,
    stop: &AtomicBool,
    repair: bool,
) -> Result<Value, Error> {
    let identity = package::identity()?;
    common::directory(root)?;
    runtime::no_links(status)?;
    let _lock = common::lock(&root.join(".prepare.lock"))?;
    let result = (|| {
        common::cancelled(stop)?;
        progress(status, &identity, "checking", "检查官方 QQ 安装包", 0)?;
        let unpacked = root.join("unpacked");
        if unpacked.exists()
            && let Err(error) = package::verify_client(&unpacked.join("QQ.app"), &identity)
        {
            if !repair {
                return Err(error);
            }
            fs::rename(
                &unpacked,
                root.join(format!("rejected-unpacked-{}", runtime::instance_id()?)),
            )?;
        }
        if !unpacked.exists() {
            let cached = root.join(package::INSTALLER_NAME);
            if repair && cached.exists() && package::verify_installer(&cached, &identity).is_err() {
                runtime::no_links(&cached)?;
                fs::rename(
                    &cached,
                    root.join(format!("rejected-dmg-{}", runtime::instance_id()?)),
                )?;
            }
            let installer = match provided {
                Some(path) => {
                    package::verify_installer(path, &identity)?;
                    path.to_path_buf()
                }
                None => download(root, status, &identity, stop)?,
            };
            progress(
                status,
                &identity,
                "extracting",
                "只读解包并验证官方签名",
                identity.installer_bytes,
            )?;
            extract(&installer, root, &identity, stop)?;
        }
        package::verify_client(&unpacked.join("QQ.app"), &identity)?;
        progress(
            status,
            &identity,
            "ready",
            "QQ 安装包已准备",
            identity.installer_bytes,
        )?;
        Ok(
            json!({"schema_version":1,"build":identity.build,"root":unpacked,"installer_sha256":identity.installer_sha256,"architecture":identity.architecture}),
        )
    })();
    if let Err(error) = &result {
        let _ = progress(status, &identity, "error", &error.message, 0);
    }
    result
}

pub fn run(
    root: &Path,
    status: &Path,
    provided: Option<&Path>,
    cancelled: &AtomicBool,
) -> Result<Value, Error> {
    prepare(root, status, provided, cancelled, false)
}
pub fn repair(
    root: &Path,
    status: &Path,
    provided: Option<&Path>,
    cancelled: &AtomicBool,
) -> Result<Value, Error> {
    prepare(root, status, provided, cancelled, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn production_downloader_reads_four_ranges_on_plain_threads_and_publishes_verified_bytes() {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let bytes: Vec<u8> = (0..CHUNK * 3 + 137)
            .map(|index| (index % 251) as u8)
            .collect();
        let mut identity = package::identity().unwrap();
        identity.build = "synthetic".into();
        identity.installer_bytes = bytes.len() as u64;
        identity.installer_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server_bytes = std::sync::Arc::new(bytes.clone());
        let server = std::thread::spawn(move || {
            let mut responses = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            for _ in 0..4 {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("Synthetic download listener failed: {error}"),
                    }
                };
                let bytes = server_bytes.clone();
                responses.push(std::thread::spawn(move||{
                    // Darwin accepts sockets with the listener's nonblocking
                    // flag; this synthetic HTTP responder uses blocking I/O.
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut request=Vec::new();let mut byte=[0u8;1];while request.len()<8192&&!request.ends_with(b"\r\n\r\n"){stream.read_exact(&mut byte).unwrap();request.push(byte[0]);}
                    let request=String::from_utf8(request).unwrap().to_ascii_lowercase();
                    let range=request.lines().find_map(|line|line.strip_prefix("range: bytes=")).unwrap();let (start,end)=range.split_once('-').unwrap();let start=start.parse::<usize>().unwrap();let end=end.parse::<usize>().unwrap();
                    assert!(end>=start&&end<bytes.len()&&end-start<CHUNK as usize);
                    let headers=format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len(),end-start+1);
                    // Deliver headers separately so plain worker threads must poll
                    // the response body, including after a ready frame is consumed.
                    stream.write_all(headers.as_bytes()).unwrap();
                    std::thread::sleep(Duration::from_millis(5));
                    for chunk in bytes[start..=end].chunks(16384) {if stream.write_all(chunk).is_err(){break;}}
                }));
            }
            for response in responses {
                response.join().unwrap();
            }
        });
        let client = download_client().unwrap();
        let result = download_ranges(
            root.path(),
            &root.path().join("progress.json"),
            &identity,
            &AtomicBool::new(false),
            &client,
            &Url::parse(&format!(
                "http://{address}/synthetic?sign=private-query&t=1"
            ))
            .unwrap(),
        );
        server.join().unwrap();
        let output = result.unwrap();
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(
            package::file_sha256(&output).unwrap(),
            identity.installer_sha256
        );
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".partial")
        }));
    }

    #[test]
    fn production_blocking_timeout_bounds_stalled_body_reads_and_reports_cancellation() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let client = download_client().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (waiting, headers) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("Synthetic stall listener failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1\r\nContent-Length: 1\r\nConnection: close\r\n\r\n").unwrap();
            waiting.send(()).unwrap();
            let _ = released.recv_timeout(Duration::from_secs(7));
        });
        let cancelled = AtomicBool::new(false);
        let elapsed = Instant::now();
        let file = tempfile::tempfile_in(root.path()).unwrap();
        std::thread::scope(|scope| {
            let task = scope.spawn(|| {
                download_range(
                    &client,
                    &Url::parse(&format!(
                        "http://{address}/synthetic?sign=private-query&t=1"
                    ))
                    .unwrap(),
                    &file,
                    ByteRange {
                        start: 0,
                        end: 0,
                        total: 1,
                    },
                    &cancelled,
                    &AtomicBool::new(false),
                )
            });
            headers.recv_timeout(Duration::from_secs(3)).unwrap();
            // Leave time for the plain worker to block in its first body read.
            std::thread::sleep(Duration::from_millis(100));
            cancelled.store(true, Ordering::Release);
            let error = task.join().unwrap().unwrap_err();
            assert_eq!(error.code, "qq_cancelled");
            assert!(!error.message.contains("private-query"));
        });
        release.send(()).unwrap();
        server.join().unwrap();
        assert!(elapsed.elapsed() < Duration::from_secs(7));
    }

    #[test]
    fn ranges_reject_wrong_metadata_and_truncation_without_disclosing_signed_urls() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        for (name, response, valid) in [
            (
                "valid",
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-7/8\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd",
                true,
            ),
            (
                "wrong-range",
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-3/8\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd",
                false,
            ),
            (
                "wrong-status",
                "HTTP/1.1 200 OK\r\nContent-Range: bytes 4-7/8\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd",
                false,
            ),
            (
                "wrong-size",
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-7/8\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
                false,
            ),
            (
                "truncated",
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-7/8\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabc",
                false,
            ),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("Synthetic range listener failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                assert!(
                    String::from_utf8_lossy(&request)
                        .to_ascii_lowercase()
                        .contains("range: bytes=4-7")
                );
                stream.write_all(response.as_bytes()).unwrap();
            });
            let file = File::options()
                .create_new(true)
                .read(true)
                .write(true)
                .open(root.path().join(name))
                .unwrap();
            file.set_len(8).unwrap();
            let url = Url::parse(&format!(
                "http://{address}/fixture?sign=synthetic-private-query&t=1"
            ))
            .unwrap();
            let result = download_range(
                &client,
                &url,
                &file,
                ByteRange {
                    start: 4,
                    end: 7,
                    total: 8,
                },
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            );
            assert_eq!(result.is_ok(), valid, "{name}");
            if let Err(error) = result {
                assert!(!error.message.contains("synthetic-private-query"));
                assert!(!error.message.contains("sign="));
            }
            if valid {
                let mut bytes = [0u8; 8];
                file.read_exact_at(&mut bytes, 0).unwrap();
                assert_eq!(&bytes, b"\0\0\0\0abcd");
            }
            server.join().unwrap();
        }
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        let file = tempfile::tempfile_in(root.path()).unwrap();
        let error = download_range(
            &client,
            &Url::parse(&format!(
                "http://{address}/?sign=synthetic-private-query&t=1"
            ))
            .unwrap(),
            &file,
            ByteRange {
                start: 0,
                end: 0,
                total: 1,
            },
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(!error.message.contains("synthetic-private-query"));
        assert!(!error.message.contains("sign="));
    }
    #[test]
    fn signed_urls_require_the_pinned_origin_path_and_single_bounded_signature() {
        let raw = Url::parse(&package::identity().unwrap().installer_url).unwrap();
        let valid = format!("{raw}?sign=abcdef0123&t=1794000000");
        assert!(validate_signed(&raw, &json!({"retcode":0,"data":{"url":valid}})).is_ok());
        for value in [
            valid.replace("qqdl.gtimg.cn", "example.com"),
            valid.replace("https:", "http:"),
            format!("{valid}&t=2"),
            format!("{valid}#fragment"),
            valid.replace("abcdef0123", "bad-sign"),
            valid.replace("1794000000", "no-time"),
        ] {
            assert!(validate_signed(&raw, &json!({"retcode":0,"data":{"url":value}})).is_err());
        }
    }
    #[test]
    fn cancellation_and_invalid_provided_dmg_do_not_invoke_mac_tools() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let installer = root.path().join("synthetic.dmg");
        fs::write(&installer, b"invalid").unwrap();
        let state = root.path().join("owned");
        assert!(
            run(
                &state,
                &state.join("progress.json"),
                Some(&installer),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(
            run(
                &state,
                &state.join("progress.json"),
                Some(&installer),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(!state.join("unpacked").exists());
        assert!(fs::read_dir(&state).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("qq-mount-")
        }));
    }
}
