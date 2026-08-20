//! Showing a file or folder in the platform's file manager.
//!
//! Three panes grew three copies of `Command::new("open")`, which is the kind
//! of macOS-ism that compiles everywhere and works on exactly one platform.
//! The Windows and Linux arms are written to each platform's convention and
//! marked: they have compiled here, not run there.

use std::path::Path;
use std::process::Command;

/// Open the file manager with `path` selected — Finder's "Reveal", Explorer's
/// "/select". Falls back to opening the parent folder where selection is not a
/// concept the platform offers.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-R").arg(path).spawn();

    #[cfg(target_os = "windows")]
    // Explorer wants `/select,PATH` as a single argument, comma included.
    let _ = Command::new("explorer")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = path.parent().map(|dir| Command::new("xdg-open").arg(dir).spawn());
}

/// Open a folder in the file manager.
pub fn open_folder(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();

    #[cfg(target_os = "windows")]
    let _ = Command::new("explorer").arg(path).spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt as _;
