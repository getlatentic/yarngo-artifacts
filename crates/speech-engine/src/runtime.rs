//! Installing the speech runtime — the Python interpreter and `mlx-speech`.
//!
//! The runtime is not bundled inside the `.app`. It is roughly a gigabyte of
//! interpreter and native wheels, it updates on a different cadence to the UI,
//! and embedding it would put every user's download on the critical path of
//! every app update. So it is fetched once into application support, which is
//! also the directory `paths::runtime_dir()` already looks in first.
//!
//! Two steps, both resumable by virtue of being re-runnable:
//!
//! 1. a relocatable CPython from python-build-standalone (Astral's builds —
//!    ordinary CPython, just built to run from any directory)
//! 2. `pip install mlx-speech`, which pulls MLX and the model runtime
//!
//! Progress is reported through a callback so the caller can drive a UI without
//! this module knowing one exists.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::paths;

/// Pinned so an install is reproducible; bumping this is a deliberate act.
const PYTHON_VERSION: &str = "3.13.15";
const PYTHON_RELEASE: &str = "20260814";

/// What the runtime actually is, named for the user. Not a marketing version:
/// the interpreter pin above is the thing that decides reproducibility, so it
/// is what gets shown.
pub const NAME: &str = "Yarngo Runtime";
pub const VERSION: &str = PYTHON_VERSION;
/// Installed size, measured on macOS arm64 rather than estimated: interpreter
/// plus MLX and its dependencies.
pub const APPROX_BYTES: u64 = 350_000_000;

/// Where a running generation reports what it has written, and where a stop is
/// signalled. Files rather than messages: the sidecar is blocking on one
/// request while it generates, so nothing it is reading would arrive.
pub fn progress_path() -> PathBuf {
    paths::data_dir().join("generating.json")
}

pub fn cancel_path() -> PathBuf {
    paths::data_dir().join("cancel")
}

/// Ask the running generation to stop at the next chunk boundary.
pub fn request_cancel() {
    let path = cancel_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, b"1");
}

/// What the current generation has produced so far, if one is running.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Progress2 {
    pub written_s: f32,
    pub elapsed_s: f32,
    pub chunks_done: u32,
    pub chunks: u32,
}

pub fn read_progress() -> Option<Progress2> {
    let raw = std::fs::read_to_string(progress_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The exact archive the installer fetches, so a machine with no connection can
/// be given the real URL rather than a branded one that redirects.
pub fn download_url() -> Option<String> {
    python_url()
}

#[derive(Debug, Clone)]
pub enum Progress {
    /// A human-readable step, for a status line.
    Step(String),
    /// Fraction of the whole install, 0.0 to 1.0.
    Fraction(f32),
    Done,
    Failed(String),
}

/// Whether this machine can run the speech stack at all, and why not if it
/// cannot.
///
/// The runtime installer is generic — python-build-standalone publishes an
/// interpreter for Intel Macs, Linux and Windows — but the thing it exists to
/// run is not: `mlx-speech` is built on MLX, which is Apple-silicon only. Left
/// ungated, an Intel Mac downloads several hundred megabytes, installs them
/// happily, and then fails at `import mlx_speech` with a Python traceback that
/// names none of this. Refusing at the start costs nothing and explains itself.
pub fn host_supported() -> Result<(), String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(()),
        ("macos", _) => Err(
            "Yarngo Studio needs an Apple silicon Mac — M1 or later. \
             The speech models run on Apple's MLX, which Intel Macs cannot use."
                .into(),
        ),
        (os, _) => Err(format!(
            "Yarngo Studio runs on Apple silicon Macs. This is {os}, and the \
             speech models have no runtime here yet."
        )),
    }
}

/// The python-build-standalone asset for this host.
fn python_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

fn python_url() -> Option<String> {
    let target = python_asset()?;
    Some(format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/\
         {PYTHON_RELEASE}/cpython-{PYTHON_VERSION}+{PYTHON_RELEASE}-{target}-install_only.tar.gz"
    ))
}

/// The interpreter inside an installed runtime.
pub fn interpreter(runtime: &Path) -> PathBuf {
    if cfg!(windows) {
        runtime.join("python").join("python.exe")
    } else {
        runtime.join("python").join("bin").join("python3")
    }
}

/// An interpreter already on this machine that can run the speech stack.
///
/// Asked before offering to download 350 MB, because a machine that already
/// has MLX and `mlx-speech` — a developer's, or someone who installed it for
/// something else — does not need a second copy. `YARNGO_PYTHON` names one
/// explicitly; otherwise whatever `python3` resolves to is tried.
pub fn existing_interpreter() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("YARNGO_PYTHON") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.push(PathBuf::from("python3"));

    candidates.into_iter().find(|python| can_speak(python))
}

/// Whether this interpreter has the speech package. The import is the test:
/// a version number or a path would only be a guess about it.
fn can_speak(python: &Path) -> bool {
    Command::new(python)
        .args(["-c", "import mlx_speech"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether a usable runtime is already installed.

pub fn is_installed() -> bool {
    let python = interpreter(&paths::runtime_dir());
    // Presence of the interpreter is not enough — the speech package is what
    // makes it a *speech* runtime, and a half-finished install has one but not
    // the other.
    (python.exists() && can_speak(&python))
        // Or the machine already had one, in which case there is nothing to
        // install and setup has nothing to ask for.
        || existing_interpreter().is_some()
}

fn run_streaming(
    mut command: Command,
    on_line: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {:?}: {e}", command.get_program()))?;

    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(std::result::Result::ok) {
            on_line(&line);
        }
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        return Ok(());
    }

    // Failure output goes to stderr, which is only worth reading when it failed.
    let mut detail = String::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(std::result::Result::ok).take(20) {
            detail.push_str(&line);
            detail.push('\n');
        }
    }
    Err(if detail.trim().is_empty() {
        format!("exited with {status}")
    } else {
        detail.trim().to_string()
    })
}

/// Install the runtime, reporting progress. Safe to re-run: an existing
/// interpreter is reused and pip is idempotent.
pub fn install(report: impl FnMut(Progress)) {
    install_from(None, report)
}

/// Install with the interpreter archive already on disk, for a machine that
/// cannot reach the release host. `archive` is the `cpython-…install_only.tar.gz`
/// the link on the setup screen points at; everything after unpacking it is
/// the same path a normal install takes.
pub fn install_from(archive: Option<PathBuf>, mut report: impl FnMut(Progress)) {
    // Before anything is downloaded, not after.
    if let Err(reason) = host_supported() {
        report(Progress::Failed(reason));
        return;
    }

    let runtime = paths::runtime_dir();
    let python_dir = runtime.join("python");

    if let Err(err) = std::fs::create_dir_all(&runtime) {
        report(Progress::Failed(format!("cannot create {}: {err}", runtime.display())));
        return;
    }

    // --- 1. interpreter ---
    if !interpreter(&runtime).exists() {
        let Some(url) = python_url() else {
            report(Progress::Failed(format!(
                "no prebuilt Python for {} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )));
            return;
        };

        // A file the user supplied is used where it lies; a downloaded one is
        // fetched into the runtime directory and removed after unpacking.
        let supplied = archive.clone();
        let source = match &supplied {
            Some(path) => path.clone(),
            None => {
                report(Progress::Step("Downloading Python…".into()));
                report(Progress::Fraction(0.05));
                let into = runtime.join("python.tar.gz");
                let mut curl = Command::new("curl");
                curl.args(["-fL", "--retry", "3", "-o"]).arg(&into).arg(&url);
                if let Err(err) = run_streaming(curl, &mut |_| {}) {
                    report(Progress::Failed(format!("downloading Python failed: {err}")));
                    return;
                }
                into
            }
        };

        report(Progress::Step("Unpacking Python…".into()));
        report(Progress::Fraction(0.25));
        let _ = std::fs::create_dir_all(&python_dir);
        let mut tar = Command::new("tar");
        // The archive holds a top-level `python/` directory already.
        tar.arg("-xzf").arg(&source).arg("-C").arg(&runtime);
        if let Err(err) = run_streaming(tar, &mut |_| {}) {
            report(Progress::Failed(format!("unpacking Python failed: {err}")));
            return;
        }
        if supplied.is_none() {
            let _ = std::fs::remove_file(&source);
        }
    }

    let python = interpreter(&runtime);
    if !python.exists() {
        report(Progress::Failed(format!("interpreter missing after unpack: {}", python.display())));
        return;
    }

    // --- 2. speech packages ---
    report(Progress::Step("Installing the speech engine…".into()));
    report(Progress::Fraction(0.35));

    let mut pip = Command::new(&python);
    pip.args(["-m", "pip", "install", "--upgrade", "--no-input", "mlx-speech"]);
    let mut seen = 0usize;
    let result = run_streaming(pip, &mut |line| {
        // pip prints one line per package collected; enough to move a bar
        // without parsing its output format, which is not a stable interface.
        if line.starts_with("Collecting") || line.starts_with("Downloading") {
            seen += 1;
            report(Progress::Fraction((0.35 + seen as f32 * 0.01).min(0.95)));
        }
    });
    if let Err(err) = result {
        report(Progress::Failed(format!("installing the speech engine failed: {err}")));
        return;
    }

    if !is_installed() {
        report(Progress::Failed("install finished but the engine did not import".into()));
        return;
    }

    report(Progress::Fraction(1.0));
    report(Progress::Done);
}
