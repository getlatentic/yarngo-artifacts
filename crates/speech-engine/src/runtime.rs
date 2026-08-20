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

/// A Python speech runtime: an interpreter and the packages that make it able
/// to speak.
///
/// This is one *kind* of runtime, not the shape of all of them. A native
/// backend — CrispASR over GGUF, or ONNX Runtime — has no interpreter and no
/// lock, and would not be described by this struct. Nothing here should be
/// mistaken for a general runtime abstraction; there is no need for one until
/// a second kind actually exists.
///
/// There are two because the two inference backends disagree about Python, and
/// the disagreement is total: `mlx-speech` declares `>=3.13`, upstream
/// `dots.tts` declares `>=3.10,<3.13`. No single pin satisfies both, so the
/// version belongs to the pack rather than to the application. A global pin
/// would mean picking one backend and silently breaking the other.
pub struct Pack {
    /// Which backend this pack exists to run.
    pub id: &'static str,
    /// Pinned so an install is reproducible; bumping is a deliberate act.
    pub python: &'static str,
    /// python-build-standalone release the interpreter comes from.
    pub release: &'static str,
    /// Directory under `packs/` holding this pack's `pyproject.toml` and
    /// `uv.lock`. The lock is what makes an install reproducible: resolving
    /// fresh on each machine gave two people installing a week apart two
    /// different dependency trees.
    pub manifest: &'static str,
    /// What the lock resolves to, for the record. Not what gets installed —
    /// `uv sync` reads the lock, not this.
    pub packages: &'static [&'static str],
    /// The import that proves this pack works. A version number or a path would
    /// only be a guess about it.
    pub probe: &'static str,
    /// Installed size, measured rather than estimated.
    pub approx_bytes: u64,
    /// Size of the interpreter archive alone — the only piece that can be
    /// fetched by hand. It is a fraction of `approx_bytes`, and showing the
    /// installed figure beside a link to this one reads as a failed download.
    pub archive_bytes: u64,
}

/// Apple silicon. Measured: interpreter plus MLX and its dependencies.
pub const MLX: Pack = Pack {
    id: "mlx",
    python: "3.13.15",
    release: "20260814",
    manifest: "mlx",
    packages: &["mlx-speech"],
    probe: "import mlx_speech",
    approx_bytes: 350_000_000,
    archive_bytes: 25_304_407,
};

/// Windows and Linux, and unproven — nothing selects it yet.
///
/// It carries the same checkpoints as [`MLX`], which is the point: the product
/// promises dots.tts MF and SOAR on every platform, and a backend with a
/// different catalogue would be a different product wearing the same labels.
///
/// `dots.tts` is deliberately installed without `WeTextProcessing`. That pulls
/// `pynini`, which publishes manylinux wheels only, and would drag conda into a
/// runtime that is otherwise pip into a standalone interpreter. It buys text
/// normalisation, which the upstream runtime defaults to off. Note that
/// dropping the dependency is not sufficient on its own: `dots_tts.utils.text`
/// imports `tn.*` at module scope, so the import has to be made lazy first.
pub const TORCH: Pack = Pack {
    id: "torch",
    python: "3.12.14",
    release: "20260814",
    manifest: "torch",
    packages: &["torch", "torchaudio", "dots.tts"],
    probe: "import dots_tts",
    approx_bytes: 3_000_000_000,
    // Unmeasured: nothing has installed this pack from an archive yet.
    archive_bytes: 0,
};

/// The pack this host runs. Only one is proven, and [`host_supported`] refuses
/// every machine the other would serve, so this cannot silently pick an
/// untested path.
pub const fn pack() -> &'static Pack {
    &MLX
}

/// What the runtime actually is, named for the user. Not a marketing version:
/// the interpreter pin is the thing that decides reproducibility, so it is what
/// gets shown.
pub const NAME: &str = "yarngo runtime";
pub const VERSION: &str = pack().python;
pub const APPROX_BYTES: u64 = pack().approx_bytes;
pub const ARCHIVE_BYTES: u64 = pack().archive_bytes;

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
            "yarngo studio needs an Apple silicon Mac — M1 or later. \
             The speech models run on Apple's MLX, which Intel Macs cannot use."
                .into(),
        ),
        (os, _) => Err(format!(
            "yarngo studio runs on Apple silicon Macs. This is {os}, and the \
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
    let Pack { python, release, .. } = pack();
    Some(format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/\
         {release}/cpython-{python}+{release}-{target}-install_only.tar.gz"
    ))
}

/// Where the CPython we fetched is unpacked. Not what runs the sidecar — the
/// pack environment below is built against it.
///
/// uv cannot fetch this itself: its downloadable versions are compiled into the
/// uv binary, so pinning through uv would tie our Python to uv's release
/// cadence. The pin stays ours; uv installs the packages.
pub fn base_interpreter(runtime: &Path) -> PathBuf {
    let python = runtime.join("interpreter").join("python");
    if cfg!(windows) {
        python.join("python.exe")
    } else {
        python.join("bin").join("python3")
    }
}

/// This pack's environment, built by `uv sync` from the committed lock. This is
/// the interpreter the sidecar runs under.
pub fn interpreter(runtime: &Path) -> PathBuf {
    let venv = runtime.join(pack().manifest).join(".venv");
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python3")
    }
}

/// The `uv` that installs packages: an override for tests, the copy inside the
/// bundle, then whatever is on PATH so a checkout works without one.
fn uv_binary() -> PathBuf {
    if let Ok(explicit) = std::env::var("YARNGO_UV") {
        return PathBuf::from(explicit);
    }
    paths::resource("uv").unwrap_or_else(|| PathBuf::from("uv"))
}

/// A developer's interpreter, named explicitly. Nothing else.
///
/// This used to fall back to whatever `python3` resolved to on PATH, and use it
/// if it happened to import the speech package. That saved a download and cost
/// determinism: two people would be running different versions of the engine
/// and its whole dependency tree, and the lock committed alongside this file
/// exists precisely to stop that. Supporting it means debugging other people's
/// Python installations.
///
/// It also removed a hazard. On a Mac without the Xcode command line tools,
/// `/usr/bin/python3` is a stub that opens Apple's installer dialog when run —
/// which would have appeared over our own setup screen, during first launch,
/// looking like something yarngo was asking for.
///
/// yarngo owns its runtime. `YARNGO_PYTHON` stays because a checkout has to be
/// able to run against a working environment without installing one.
pub fn existing_interpreter() -> Option<PathBuf> {
    let explicit = PathBuf::from(std::env::var("YARNGO_PYTHON").ok()?);
    can_speak(&explicit).then_some(explicit)
}

/// Whether this interpreter has the speech package this host's pack needs.
fn can_speak(python: &Path) -> bool {
    Command::new(python)
        .args(["-c", pack().probe])
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

/// Remove a runtime left by the layout that predates packs.
///
/// Before packs, the interpreter was unpacked to `<runtime>/python` and the
/// speech package was installed straight into it. The pack layout puts the
/// interpreter under `<runtime>/interpreter` and builds a virtual environment
/// beside its lock, so the old directory is unreachable: `is_installed` says
/// no, setup runs again, and roughly 350 MB sits there for good — counted by
/// the Storage pane, usable by nothing.
///
/// Deleted rather than migrated. The old interpreter carries packages resolved
/// by pip rather than the lock, which is exactly the reproducibility the packs
/// exist to fix, and uv's cache makes the reinstall cheap.
fn remove_pre_pack_runtime(runtime: &Path) {
    let legacy = runtime.join("python");
    let legacy_interpreter = if cfg!(windows) {
        legacy.join("python.exe")
    } else {
        legacy.join("bin").join("python3")
    };
    // Both conditions, so this can only ever match the layout it describes.
    if !legacy_interpreter.exists() || runtime.join("interpreter").exists() {
        return;
    }
    match std::fs::remove_dir_all(&legacy) {
        Ok(()) => eprintln!("removed the pre-pack runtime at {}", legacy.display()),
        Err(err) => eprintln!("could not remove {}: {err}", legacy.display()),
    }
}

/// Put this pack's manifest and lock where `uv sync` can build beside them.
///
/// Both files, always: a `pyproject.toml` without its lock would make uv
/// resolve from scratch, which is the behaviour the lock exists to replace.
fn stage_manifest(project: &Path) -> Result<(), String> {
    std::fs::create_dir_all(project).map_err(|e| format!("cannot create {}: {e}", project.display()))?;
    for file in ["pyproject.toml", "uv.lock"] {
        let source = paths::resource(&format!("packs/{}/{file}", pack().manifest))
            .ok_or_else(|| format!("{file} for the {} pack is missing", pack().id))?;
        std::fs::copy(&source, project.join(file))
            .map_err(|e| format!("cannot stage {file}: {e}"))?;
    }
    Ok(())
}

/// Install the runtime, reporting progress. Safe to re-run: an existing
/// interpreter is reused and `uv sync` converges on the lock.
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
    remove_pre_pack_runtime(&runtime);
    let base = runtime.join("interpreter");

    if let Err(err) = std::fs::create_dir_all(&runtime) {
        report(Progress::Failed(format!("cannot create {}: {err}", runtime.display())));
        return;
    }

    // --- 1. interpreter ---
    if !base_interpreter(&runtime).exists() {
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
                let _ = std::fs::create_dir_all(&base);
                let into = base.join("python.tar.gz");
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
        let _ = std::fs::create_dir_all(&base);
        let mut tar = Command::new("tar");
        // The archive holds a top-level `python/` directory already.
        tar.arg("-xzf").arg(&source).arg("-C").arg(&base);
        if let Err(err) = run_streaming(tar, &mut |_| {}) {
            report(Progress::Failed(format!("unpacking Python failed: {err}")));
            return;
        }
        if supplied.is_none() {
            let _ = std::fs::remove_file(&source);
        }
    }

    let base = base_interpreter(&runtime);
    if !base.exists() {
        report(Progress::Failed(format!("interpreter missing after unpack: {}", base.display())));
        return;
    }

    // --- 2. speech packages ---
    report(Progress::Step("Installing the speech engine…".into()));
    report(Progress::Fraction(0.35));

    // The manifest is copied out of the bundle rather than synced in place:
    // `uv sync` writes a `.venv` beside it, and writing inside a signed bundle
    // would break its signature.
    let project = runtime.join(pack().manifest);
    if let Err(err) = stage_manifest(&project) {
        report(Progress::Failed(err));
        return;
    }

    let mut sync = Command::new(uv_binary());
    sync.arg("sync")
        // The lock is the whole point: without --frozen, uv re-locks before
        // syncing, and what shipped stops being what was tested.
        .arg("--frozen")
        // A runtime has no development dependencies. Nothing declares any
        // today; this keeps that true when someone adds one.
        .arg("--no-dev")
        .arg("--project")
        .arg(&project)
        .arg("--python")
        .arg(&base)
        // uv must not reach for an interpreter of its own: the pin is ours, and
        // uv's downloadable versions are whatever its binary was built with.
        .env("UV_PYTHON_DOWNLOADS", "never");
    let mut seen = 0usize;
    let result = run_streaming(sync, &mut |line| {
        // uv prints one ` + name==version` line per package installed.
        if line.trim_start().starts_with('+') {
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
