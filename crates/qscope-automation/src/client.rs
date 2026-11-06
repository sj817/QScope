use crate::Error;
pub use reqwest::Url;
use reqwest::{blocking::Client as HttpClient, redirect::Policy};
use serde_json::Value;
use std::{io::Read, time::Duration};

pub const DEFAULT_SERVER: &str = "auto";
pub const LOCAL_SERVER: &str = "http://127.0.0.1:19840";
pub const DEFAULT_DEVTOOLS: &str = "http://127.0.0.1:19860";
const MAX_RESPONSE: u64 = 32 * 1024 * 1024;

pub fn loopback_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|error| Error::new("endpoint", error.to_string()))?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err(Error::new(
            "endpoint",
            "Use an explicit http://127.0.0.1:port endpoint",
        ));
    }
    Ok(url)
}

pub struct Client {
    base: Url,
    http: HttpClient,
    token: Option<String>,
}

impl Client {
    pub fn new(endpoint: &str) -> Result<Self, Error> {
        let descriptor = if endpoint == DEFAULT_SERVER {
            let path = crate::runtime::user_data()?.join("service.json");
            if path.exists() {
                Some(crate::runtime::Service::read(&path)?)
            } else {
                None
            }
        } else if let Some(path) = endpoint.strip_prefix('@') {
            Some(crate::runtime::Service::read(std::path::Path::new(path))?)
        } else {
            None
        };
        let address = descriptor
            .as_ref()
            .map(|service| service.endpoint.as_str())
            .unwrap_or(if endpoint == DEFAULT_SERVER {
                LOCAL_SERVER
            } else {
                endpoint
            });
        let client = Self {
            base: loopback_url(address)?,
            http: HttpClient::builder()
                .no_proxy()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(12))
                .build()?,
            token: descriptor
                .as_ref()
                .and_then(|service| (!service.token.is_empty()).then(|| service.token.clone())),
        };
        if let Some(service) = descriptor {
            let info = client.get("/api/v1/service", &[])?;
            if info["instance_id"] != service.instance_id {
                return Err(Error::new(
                    "service_identity",
                    "QScope Desktop service changed. Open the desktop app and retry",
                ));
            }
        }
        Ok(client)
    }

    pub fn endpoint(&self) -> &Url {
        &self.base
    }

    pub fn with_token(endpoint: &str, token: &str) -> Result<Self, Error> {
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::new(
                "devtools_token",
                "Invalid research bridge token",
            ));
        }
        let mut client = Self::new(endpoint)?;
        client.token = Some(token.into());
        Ok(client)
    }

    pub fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value, Error> {
        self.request("GET", path, query, None, None)
    }

    pub fn post(&self, path: &str, value: Value) -> Result<Value, Error> {
        self.request("POST", path, &[], Some(value), None)
    }

    pub fn post_with_timeout(
        &self,
        path: &str,
        value: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        self.request("POST", path, &[], Some(value), Some(timeout))
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<Value, Error> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#', '\\']) {
            return Err(Error::new("endpoint", "Invalid local API path"));
        }
        let mut url = self.base.clone();
        url.set_path(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|error| Error::new("request", error.to_string()))?;
        let mut request = self
            .http
            .request(method, url)
            .header("X-QScope-Client", "web-v1")
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request.send()?;
        let status = response.status();
        let mut bytes = Vec::new();
        response.take(MAX_RESPONSE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(Error::new(
                "response_limit",
                "Local response exceeds 32 MiB",
            ));
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        if !status.is_success() {
            return Err(Error::new(
                value["code"]
                    .as_str()
                    .filter(|code| {
                        code.len() <= 64
                            && code
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    })
                    .unwrap_or("api"),
                value["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
            ));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_endpoints_reject_remote_hosts_credentials_and_embedded_paths() {
        for endpoint in [
            "https://127.0.0.1:19840",
            "http://example.com:19840",
            "http://127.0.0.1:0",
            "http://user:pass@127.0.0.1:19840",
            "http://127.0.0.1:19840/path",
            "http://127.0.0.1:19840/?secret=x",
        ] {
            assert!(loopback_url(endpoint).is_err(), "{endpoint}");
        }
        assert!(loopback_url(LOCAL_SERVER).is_ok());
        assert!(Client::with_token(DEFAULT_DEVTOOLS, "not-a-token").is_err());
    }

    #[test]
    fn local_json_requests_explicitly_disable_unavailable_compression() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 4096);
            }
            let headers = String::from_utf8(headers).unwrap().to_lowercase();
            assert!(headers.contains("accept-encoding: identity\r\n"));
            assert!(headers.contains("x-qscope-client: web-v1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        assert_eq!(
            Client::new(&endpoint).unwrap().get("/health", &[]).unwrap(),
            serde_json::json!({})
        );
        server.join().unwrap();
    }
}
