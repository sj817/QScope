fn main() {
    #[cfg(any(windows, target_os = "macos"))]
    tauri_build::build();
}
