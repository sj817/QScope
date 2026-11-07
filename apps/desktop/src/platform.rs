use std::path::{Path, PathBuf};

#[cfg(windows)]
pub const CLI_NAME: &str = "qscope-cli.exe";
#[cfg(target_os = "macos")]
pub const CLI_NAME: &str = "qscope-cli";

pub fn cli_path(executable: &Path) -> PathBuf {
    // macOS bundles put both signed product executables in Contents/MacOS.
    executable.with_file_name(CLI_NAME)
}

pub fn portable_data(executable: &Path) -> PathBuf {
    qscope_automation::runtime::portable_data(executable)
}

#[cfg(target_os = "macos")]
pub fn opened_capture(url: &tauri::Url) -> Option<PathBuf> {
    if url.scheme() != "file" || url.host_str().is_some_and(|host| host != "localhost") {
        return None;
    }
    url.to_file_path().ok().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("qscap"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_the_sibling_product_binary() {
        let executable = Path::new("product").join("QScope");
        assert_eq!(cli_path(&executable), Path::new("product").join(CLI_NAME));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn portable_data_does_not_modify_the_app_bundle() {
        assert_eq!(
            portable_data(Path::new("/portable/QScope.app/Contents/MacOS/QScope")),
            Path::new("/portable/data")
        );
        assert_eq!(
            portable_data(Path::new("/development/QScope")),
            Path::new("/development/data")
        );
        assert_eq!(
            portable_data(Path::new("/ordinary/Contents/MacOS/QScope")),
            Path::new("/ordinary/Contents/MacOS/data")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn file_open_accepts_local_capture_urls_only() {
        let file = tauri::Url::from_file_path("/captures/示例 1.qscap").unwrap();
        assert_eq!(
            opened_capture(&file),
            Some(PathBuf::from("/captures/示例 1.qscap"))
        );
        for address in [
            "https://example.com/record.qscap",
            "file://remote/captures/record.qscap",
            "file:///captures/record.txt",
        ] {
            assert!(opened_capture(&address.parse().unwrap()).is_none());
        }
    }
}
