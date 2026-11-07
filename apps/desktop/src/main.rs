#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "macos"))]
mod desktop;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, target_os = "macos"))]
mod platform;

fn main() {
    #[cfg(any(windows, target_os = "macos"))]
    desktop::run();
    #[cfg(not(any(windows, target_os = "macos")))]
    eprintln!("QScope Desktop currently supports Windows and macOS.");
}
