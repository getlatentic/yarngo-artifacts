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

/// What the generation currently running has produced so far.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Generating {
    pub written_s: f32,
    pub elapsed_s: f32,
    pub chunks_done: u32,
    pub chunks: u32,
}

pub fn read_progress() -> Option<Generating> {
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

/// uv, which installs the packages. Pinned, and its digest compiled in.
///
/// It used to ship inside the bundle, where it was 42 MB of a 37.5 MB download
/// — larger than the application itself. It is fetched once instead, into the
/// runtime directory it serves, so it is removed along with the runtime and
/// never sits in the download of someone who already has one.
const UV_VERSION: &str = "0.12.5";

/// `(asset, sha256)` for this host. The digest is here rather than fetched
/// beside the archive: a `.sha256` published next to the file it describes
/// proves the download was not corrupted in transit, not that it is the file
/// this app was built against.
pub fn uv_asset() -> Option<(String, &'static str)> {
    let (target, ext, digest) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => (
            "aarch64-apple-darwin",
            "tar.gz",
            "5bb0e5fe008a773c3dbcb97ff79cd89e1241464fe9d2f986d52ad8f1b037bd62",
        ),
        ("macos", "x86_64") => (
            "x86_64-apple-darwin",
            "tar.gz",
            "b3b2137477cf96c9686ebfb71524614cec780c673fd73e59bce099aef02e70e8",
        ),
        ("windows", "x86_64") => (
            "x86_64-pc-windows-msvc",
            "zip",
            "4c4d49d8738847d9b71ba319e49a5688c93eac0fe6204b1df24e98528dddf39a",
        ),
        _ => return None,
    };
    Some((format!("uv-{target}.{ext}"), digest))
}

pub fn sha256_of(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Where uv lives once fetched: beside the interpreter it installs into.
fn uv_path(runtime: &Path) -> PathBuf {
    runtime.join(if cfg!(windows) { "uv.exe" } else { "uv" })
}

/// The uv to use, fetching it if this machine has none.
///
/// An explicit override first, then a copy already fetched, then whatever is on
/// PATH — which is what lets a development checkout run without downloading
/// anything. Only then does it reach for the network.
fn ensure_uv(runtime: &Path, report: &mut dyn FnMut(Progress)) -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var("YARNGO_UV") {
        return Ok(PathBuf::from(explicit));
    }
    let fetched = uv_path(runtime);
    if fetched.exists() {
        return Ok(fetched);
    }
    if Command::new("uv").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Ok(PathBuf::from("uv"));
    }

    let (asset, expected) = uv_asset().ok_or_else(|| {
        format!("no uv build for {} {}", std::env::consts::OS, std::env::consts::ARCH)
    })?;
    report(Progress::Step("Fetching the installer…".into()));

    std::fs::create_dir_all(runtime).map_err(|e| format!("cannot create {}: {e}", runtime.display()))?;
    let archive = runtime.join(&asset);
    let url =
        format!("https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/{asset}");
    let mut curl = Command::new("curl");
    curl.args(["-fL", "-sS", "--retry", "3", "-o"]).arg(&archive).arg(&url);
    run_streaming(curl, &mut |_| {}).map_err(|e| format!("downloading uv failed: {e}"))?;

    let actual = sha256_of(&archive)?;
    if actual != expected {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "the uv download does not match the digest this build expects \
             (wanted {expected}, got {actual})"
        ));
    }

    let unpacked = runtime.join("uv-unpack");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    // tar reads both, and ships with Windows 10 1803 and later.
    let mut extract = Command::new("tar");
    extract
        .arg(if asset.ends_with(".zip") { "-xf" } else { "-xzf" })
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked);
    run_streaming(extract, &mut |_| {}).map_err(|e| format!("unpacking uv failed: {e}"))?;

    // The archive holds a directory named after the target; the binary is
    // inside it. Find it rather than reconstructing the name twice.
    let binary = find_uv(&unpacked).ok_or("no uv binary in the archive")?;
    let destination = uv_path(runtime);
    std::fs::rename(&binary, &destination)
        .or_else(|_| std::fs::copy(&binary, &destination).map(|_| ()))
        .map_err(|e| format!("cannot place uv: {e}"))?;
    let _ = std::fs::remove_dir_all(&unpacked);
    let _ = std::fs::remove_file(&archive);
    Ok(destination)
}

fn find_uv(dir: &Path) -> Option<PathBuf> {
    let wanted = if cfg!(windows) { "uv.exe" } else { "uv" };
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_uv(&path) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(wanted) {
            return Some(path);
        }
    }
    None
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

/// Where the published recipes live. The only URL this app knows; everything it
/// names is pinned to an immutable tag and carries a digest.
const MANIFEST_URL: &str = concat!(
    "https://github.com/getlatentic/yarngo-artifacts",
    "/releases/download/latest/manifest.json"
);

/// What this build can honour. A published recipe declaring a higher number
/// describes an environment this sidecar does not know how to drive, so it is
/// refused rather than half-understood — that refusal is what makes updating
/// the runtime without updating the app safe.
pub const SIDECAR_API: u32 = 1;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Recipe {
    pub lock_url: String,
    pub lock_sha256: String,
    pub pyproject_url: String,
    pub pyproject_sha256: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub min_app_version: String,
    pub sidecar_api: u32,
    pub tag: String,
    pub runtimes: std::collections::HashMap<String, Recipe>,
}

/// `1.2.3` against `1.10.0`, without pulling in a version crate.
///
/// Two rules that a string comparison gets wrong. Numbers compare as numbers,
/// or `1.10` sorts below `1.9`. And a pre-release is *older* than the release
/// it precedes — `0.1.0-alpha` comes before `0.1.0` — which matters the moment
/// an alpha is published, because the naive reading has it the other way round
/// and would hand alpha users recipes meant for the finished version.
pub fn version_at_least(have: &str, need: &str) -> bool {
    fn split(v: &str) -> (Vec<u32>, bool) {
        let (numbers, pre) = match v.split_once('-') {
            Some((n, _)) => (n, true),
            None => (v, false),
        };
        (numbers.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect(), pre)
    }
    let (have_n, have_pre) = split(have);
    let (need_n, need_pre) = split(need);
    for i in 0..have_n.len().max(need_n.len()) {
        let (h, n) = (have_n.get(i).copied().unwrap_or(0), need_n.get(i).copied().unwrap_or(0));
        if h != n {
            return h > n;
        }
    }
    // Same numbers: a pre-release satisfies a pre-release floor, but not a
    // finished one.
    !have_pre || need_pre
}

/// Whether this build may act on a published manifest.
pub fn manifest_usable(manifest: &Manifest) -> Result<(), String> {
    if manifest.schema != 1 {
        return Err(format!("manifest schema {} is not one this build reads", manifest.schema));
    }
    if manifest.sidecar_api > SIDECAR_API {
        return Err(format!(
            "the published runtime needs sidecar api {}, this build implements {SIDECAR_API}",
            manifest.sidecar_api
        ));
    }
    let ours = env!("CARGO_PKG_VERSION");
    if !version_at_least(ours, &manifest.min_app_version) {
        return Err(format!(
            "the published runtime needs yarngo {} or newer; this is {ours}",
            manifest.min_app_version
        ));
    }
    Ok(())
}

fn fetch_text(url: &str) -> Result<Vec<u8>, String> {
    let into = std::env::temp_dir().join(format!("yarngo-fetch-{}", std::process::id()));
    let mut curl = Command::new("curl");
    curl.args(["-fL", "-sS", "--retry", "2", "--max-time", "30", "-o"]).arg(&into).arg(url);
    run_streaming(curl, &mut |_| {})?;
    let bytes = std::fs::read(&into).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&into);
    Ok(bytes)
}

/// The published manifest, or why it cannot be used. Never fatal: every caller
/// carries on with what it shipped with.
pub fn fetch_manifest() -> Result<Manifest, String> {
    let url = std::env::var("YARNGO_MANIFEST_URL").unwrap_or_else(|_| MANIFEST_URL.into());
    let bytes = fetch_text(&url).map_err(|e| format!("could not reach the manifest: {e}"))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("manifest is not readable: {e}"))?;
    manifest_usable(&manifest)?;
    Ok(manifest)
}

/// Replace this pack's staged recipe with the published one, if there is a
/// usable newer one. Returns the tag when something changed.
///
/// The digest is checked before anything is written, so a truncated or swapped
/// download leaves the staged recipe untouched rather than half-replaced.
pub fn refresh_recipe(project: &Path) -> Result<Option<String>, String> {
    let manifest = fetch_manifest()?;
    let recipe = manifest
        .runtimes
        .get(pack().id)
        .ok_or_else(|| format!("the manifest has no runtime for the {} pack", pack().id))?;

    let lock = fetch_text(&recipe.lock_url)?;
    let pyproject = fetch_text(&recipe.pyproject_url)?;
    for (what, bytes, expected) in [
        ("uv.lock", &lock, &recipe.lock_sha256),
        ("pyproject.toml", &pyproject, &recipe.pyproject_sha256),
    ] {
        use sha2::{Digest, Sha256};
        let actual = format!("{:x}", Sha256::digest(bytes));
        if &actual != expected {
            return Err(format!("{what} does not match its digest (wanted {expected}, got {actual})"));
        }
    }

    // Nothing to do if the published recipe is the one already installed.
    let staged = project.join("uv.lock");
    if staged.exists() && sha256_of(&staged)? == recipe.lock_sha256 {
        return Ok(None);
    }

    std::fs::create_dir_all(project).map_err(|e| e.to_string())?;
    std::fs::write(project.join("uv.lock"), &lock).map_err(|e| e.to_string())?;
    std::fs::write(project.join("pyproject.toml"), &pyproject).map_err(|e| e.to_string())?;
    Ok(Some(manifest.tag))
}

/// Whether a published runtime differs from the one installed. Answers without
/// changing anything, so the app can offer rather than act.
pub fn runtime_update() -> Result<Option<String>, String> {
    let manifest = fetch_manifest()?;
    let recipe = manifest
        .runtimes
        .get(pack().id)
        .ok_or_else(|| format!("the manifest has no runtime for the {} pack", pack().id))?;
    let staged = paths::runtime_dir().join(pack().manifest).join("uv.lock");
    if !staged.exists() {
        return Ok(None);
    }
    Ok((sha256_of(&staged)? != recipe.lock_sha256).then_some(manifest.tag))
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
                curl.args(["-fL", "-sS", "--retry", "3", "-o"]).arg(&into).arg(&url);
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
    // The bundled recipe is staged first and unconditionally: it is the floor,
    // and it is what a machine with no network installs from.
    if let Err(err) = stage_manifest(&project) {
        report(Progress::Failed(err));
        return;
    }
    // Then the published one, if it is reachable, digest-matching, and does not
    // need a newer sidecar than this build implements. Any failure here is
    // reported and stepped over — an unreachable manifest must not stop an
    // install that the bundled recipe can complete on its own.
    match refresh_recipe(&project) {
        Ok(Some(tag)) => report(Progress::Step(format!("Using the published runtime {tag}…"))),
        Ok(None) => {}
        Err(err) => eprintln!("keeping the bundled runtime recipe: {err}"),
    }

    let uv = match ensure_uv(&runtime, &mut report) {
        Ok(path) => path,
        Err(err) => {
            report(Progress::Failed(err));
            return;
        }
    };

    let mut sync = Command::new(uv);
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
