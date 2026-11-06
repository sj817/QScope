pub mod bundle;
pub mod client;
pub mod devtools;
pub mod launcher;
pub mod runtime;
pub mod service;
pub mod skills;
#[cfg(feature = "update")]
pub mod update;

use std::{fmt, io};

pub fn display_path(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(suffix) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{suffix}");
        }
        if let Some(suffix) = value.strip_prefix(r"\\?\") {
            return suffix.into();
        }
    }
    value.into_owned()
}

#[derive(Debug)]
pub struct Error {
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::new("connection", error.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::new("json", error.to_string())
    }
}
