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
    /// What it is called where somebody has to choose one.
    pub name: &'static str,
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
    /// SHA-256 of that archive. The interpreter executes everything else, so
    /// it is pinned the way uv is: a build knows exactly which bytes it will
    /// run, and a swapped or truncated download fails here instead of being
    /// installed. `None` refuses to install rather than installs unpinned —
    /// an unproven pack earns its pin when it is proven.
    pub archive_sha256: Option<&'static str>,
}

/// Apple silicon. Measured: interpreter plus MLX and its dependencies.
pub const MLX: Pack = Pack {
    id: "mlx",
    name: "Apple silicon",
    python: "3.13.15",
    release: "20260814",
    manifest: "mlx",
    packages: &["mlx-speech"],
    probe: "import mlx_speech",
    approx_bytes: 350_000_000,
    archive_bytes: 25_304_407,
    archive_sha256: Some("7d50bb42813a5644db7c40d3ad79361d0b724bb29d25a91fab1048c2c5c6a8c5"),
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
    name: "Windows and Linux",
    python: "3.12.14",
    release: "20260814",
    manifest: "torch",
    packages: &["torch", "torchaudio", "dots.tts"],
    probe: "import dots_tts",
    approx_bytes: 3_000_000_000,
    // Unmeasured: nothing has installed this pack from an archive yet.
    archive_bytes: 0,
    archive_sha256: None,
};

/// Every runtime the application knows how to fetch.
///
/// A catalogue rather than a constant, because nothing here is bundled: a new
/// installation has no runtime at all and has to be shown what it can get.
pub const PACKS: &[&Pack] = &[&MLX, &TORCH];

impl Pack {
    /// Whether this machine can run it. A pack offered on a host it cannot
    /// serve is a download that ends in an error after several hundred
    /// megabytes.
    pub fn runs_here(&self) -> bool {
        self.runs_on(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// The rule, stated for any host rather than only this one.
    ///
    /// Taking the target as arguments is what makes it checkable: a machine can
    /// only ever tell you about itself, and "does this pack run on Windows" is
    /// not a question a Mac can answer by running the code.
    pub fn runs_on(&self, os: &str, arch: &str) -> bool {
        let apple_silicon = os == "macos" && arch == "aarch64";
        match self.id {
            "mlx" => apple_silicon,
            // Proven on macOS CPU, which is how parity is tested without CUDA
            // hardware, but nothing selects it on a Mac that has MLX.
            "torch" => !apple_silicon,
            _ => false,
        }
    }

    /// Where installs before versioning left this pack's environment. Not
    /// where anything installs now — an existing one is adopted in place,
    /// because a virtual environment remembers where it was built and cannot
    /// be moved.
    pub fn legacy_home(&self, data: &Path) -> PathBuf {
        data.join("runtime").join(self.manifest)
    }

    /// Whether the environment at `home` can actually speak. An interpreter
    /// alone is a half-finished install, which looks the same from outside.
    pub fn proven_in(&self, home: &Path) -> bool {
        let python = venv_python(home);
        python.exists() && can_speak_probe(&python, self.probe)
    }
}

/// The runtimes this machine could install, whether or not it has.
pub fn offered() -> Vec<&'static Pack> {
    PACKS.iter().copied().filter(|pack| pack.runs_here()).collect()
}

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

/// Where the CPython we fetched is unpacked: shared between every version of a
/// runtime that pins the same interpreter, keyed by that pin so two pins never
/// collide. It never moves once unpacked, which is what lets each version's
/// environment point at it by absolute path.
///
/// uv cannot fetch this itself: its downloadable versions are compiled into the
/// uv binary, so pinning through uv would tie our Python to uv's release
/// cadence. The pin stays ours; uv installs the packages.
pub fn interpreter_base(data: &Path, pack: &Pack) -> PathBuf {
    let python = data.join("interpreters").join(pack.python).join("python");
    if cfg!(windows) {
        python.join("python.exe")
    } else {
        python.join("bin").join("python3")
    }
}

/// The interpreter a runtime at `home` runs under: the environment `uv sync`
/// built beside its descriptor, at the path it will live at forever. Built in
/// place rather than built and moved, because an environment remembers where
/// it was built.
pub fn venv_python(home: &Path) -> PathBuf {
    let venv = home.join(".venv");
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
fn uv_path(data: &Path) -> PathBuf {
    data.join("tools").join(if cfg!(windows) { "uv.exe" } else { "uv" })
}

/// The uv to use, fetching it if this machine has none.
///
/// An explicit override first, then a copy already fetched, then whatever is on
/// PATH — which is what lets a development checkout run without downloading
/// anything. Only then does it reach for the network.
fn ensure_uv(data: &Path, report: &mut dyn FnMut(Progress)) -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var("YARNGO_UV") {
        return Ok(PathBuf::from(explicit));
    }
    let fetched = uv_path(data);
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

    let tools = data.join("tools");
    std::fs::create_dir_all(&tools).map_err(|e| format!("cannot create {}: {e}", tools.display()))?;
    let archive = tools.join(&asset);
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

    let unpacked = tools.join("uv-unpack");
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
    let destination = uv_path(data);
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
/// Whether this interpreter has the speech package the pack needs.
fn can_speak_probe(python: &Path, probe: &str) -> bool {
    Command::new(python)
        .args(["-c", probe])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
pub fn remove_pre_pack_runtime(runtime: &Path) {
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

/// Unpack one runtime's archive into its folder.
///
/// Whether these are the bytes that were published is settled before this is
/// called, by the repository that vouched for them. What is left is the part
/// that is ours: an archive decides what it materialises and where, and this is
/// code that will be executed.
///
/// Told its folder and its bytes rather than reaching for them, so what it
/// refuses can be tested.
pub fn unpack_engine(id: &str, folder: &Path, archive: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    crate::unpack::extract_under(archive, folder).map_err(|e| format!("the {id} runtime {e}"))?;

    if !folder.join("engine.py").exists() {
        return Err(format!("the {id} runtime archive has no engine.py in it"));
    }
    Ok(())
}


/// Put the recipe that ships with the application into `home`. The floor:
/// what a machine with no network installs from.
pub fn stage_bundled(pack: &Pack, home: &Path) -> Result<(), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    for file in ["pyproject.toml", "uv.lock"] {
        let source = paths::resource(&format!("packs/{}/{file}", pack.manifest))
            .ok_or_else(|| format!("{file} for the {} pack is missing", pack.id))?;
        std::fs::copy(&source, home.join(file))
            .map_err(|e| format!("cannot stage {file}: {e}"))?;
    }
    Ok(())
}

/// Write a descriptor into `home`, unless the runtime's own archive brought
/// one — a runtime that published its descriptor is the authority on which
/// engine runs it.
pub fn describe_home(pack: &Pack, home: &Path) -> Result<(), String> {
    let path = home.join("runtime.json");
    if path.exists() {
        return Ok(());
    }
    let descriptor = serde_json::json!({
        "schema": crate::runtimes::DESCRIPTOR_SCHEMA,
        "id": pack.id,
        "name": pack.name,
        // Stated rather than inferred later: a runtime that says it brought
        // its own code and has none must not be started by a different
        // implementation.
        "engine": if home.join("engine.py").exists() { "own" } else { "bundled" },
        "program": if cfg!(windows) { "{venv}/Scripts/python.exe" } else { "{venv}/bin/python3" },
        "arguments": ["{engine}"],
    });
    std::fs::write(&path, serde_json::to_string_pretty(&descriptor).unwrap_or_default())
        .map_err(|e| e.to_string())
}

/// Build the environment at `home`, at the path it will live at forever.
///
/// The recipe — `pyproject.toml` and `uv.lock` — is already in `home`; who put
/// it there and whether to believe it was settled before this is called. What
/// happens here is mechanics: the pinned interpreter, uv, `uv sync` against
/// the lock, and proof that the result can import the speech package. Nothing
/// outside `home` is written except the shared interpreter and uv, both
/// digest-pinned and both immutable once down.
pub fn build_env(
    data: &Path,
    pack: &'static Pack,
    home: &Path,
    archive: Option<PathBuf>,
    report: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    host_supported()?;
    let base = ensure_interpreter(data, pack, archive, report)?;
    let uv = ensure_uv(data, report)?;

    report(Progress::Step("Installing the speech engine…".into()));
    report(Progress::Fraction(0.35));
    let mut sync = Command::new(uv);
    sync.arg("sync")
        // The lock is the whole point: without --frozen, uv re-locks before
        // syncing, and what shipped stops being what was tested.
        .arg("--frozen")
        // A runtime has no development dependencies. Nothing declares any
        // today; this keeps that true when someone adds one.
        .arg("--no-dev")
        .arg("--project")
        .arg(home)
        .arg("--python")
        .arg(&base)
        // uv must not reach for an interpreter of its own: the pin is ours,
        // and uv's downloadable versions are whatever its binary was built
        // with.
        .env("UV_PYTHON_DOWNLOADS", "never");
    let mut seen = 0usize;
    run_streaming(sync, &mut |line| {
        // uv prints one ` + name==version` line per package installed.
        if line.trim_start().starts_with('+') {
            seen += 1;
            report(Progress::Fraction((0.35 + seen as f32 * 0.01).min(0.90)));
        }
    })
    .map_err(|err| format!("installing the speech engine failed: {err}"))?;

    if !pack.proven_in(home) {
        return Err("install finished but the engine did not import".into());
    }
    Ok(())
}

/// The pinned interpreter, fetched and unpacked if it is not already here.
///
/// `archive` is a `cpython-…install_only.tar.gz` somebody downloaded by hand,
/// for a machine that cannot reach the release host — checked against the same
/// pin, because it claims to be the same bytes.
fn ensure_interpreter(
    data: &Path,
    pack: &'static Pack,
    archive: Option<PathBuf>,
    report: &mut dyn FnMut(Progress),
) -> Result<PathBuf, String> {
    let base = interpreter_base(data, pack);
    if base.exists() {
        return Ok(base);
    }
    let Some(expected) = pack.archive_sha256 else {
        // Refused rather than installed unpinned: the interpreter executes
        // everything else this install produces.
        return Err(format!(
            "the {} pack has no pinned interpreter digest for this host yet",
            pack.id
        ));
    };
    let Some(url) = python_url() else {
        return Err(format!(
            "no prebuilt Python for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };

    let root = data.join("interpreters").join(pack.python);
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;

    let supplied = archive.clone();
    let source = match &supplied {
        Some(path) => path.clone(),
        None => {
            report(Progress::Step("Downloading Python…".into()));
            report(Progress::Fraction(0.05));
            let into = root.join("python.tar.gz");
            let mut curl = Command::new("curl");
            curl.args(["-fL", "-sS", "--retry", "3", "-o"]).arg(&into).arg(&url);
            run_streaming(curl, &mut |_| {})
                .map_err(|e| format!("downloading Python failed: {e}"))?;
            into
        }
    };

    let actual = sha256_of(&source)?;
    if actual != expected {
        if supplied.is_none() {
            let _ = std::fs::remove_file(&source);
        }
        return Err(format!(
            "the Python archive does not match the digest this build expects \
             (wanted {expected}, got {actual})"
        ));
    }

    report(Progress::Step("Unpacking Python…".into()));
    report(Progress::Fraction(0.25));
    // The host tar, on bytes that just matched a compiled-in digest. The
    // archive holds a top-level `python/` directory already.
    let mut tar = Command::new("tar");
    tar.arg("-xzf").arg(&source).arg("-C").arg(&root);
    run_streaming(tar, &mut |_| {}).map_err(|e| format!("unpacking Python failed: {e}"))?;
    if supplied.is_none() {
        let _ = std::fs::remove_file(&source);
    }

    if !base.exists() {
        return Err(format!("interpreter missing after unpack: {}", base.display()));
    }
    Ok(base)
}
