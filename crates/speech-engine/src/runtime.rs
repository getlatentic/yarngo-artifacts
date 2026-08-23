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

    /// Where it installs to, and where its descriptor goes.
    pub fn root(&self) -> PathBuf {
        paths::runtime_dir().join(self.manifest)
    }

    pub fn interpreter(&self) -> PathBuf {
        let venv = self.root().join(".venv");
        if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python3")
        }
    }

    /// Whether it is here and can actually speak. An interpreter alone is a
    /// half-finished install, which looks the same from the outside.
    pub fn installed(&self) -> bool {
        let python = self.interpreter();
        python.exists() && can_speak(&python)
    }

    /// Where this runtime's own files live, if it brought any.
    pub fn folder(&self) -> PathBuf {
        self.folder_in(&paths::data_dir())
    }

    /// The same, against a stated directory rather than the one this process
    /// happens to be pointed at. What makes any of this testable without
    /// setting an environment variable the whole process shares.
    pub fn folder_in(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("runtimes").join(self.id)
    }

    /// How to start it, written where a runtime is looked for.
    ///
    /// Left alone if the runtime published a descriptor of its own: a runtime
    /// that brought its own code is the authority on how to run it, and this
    /// would only be guessing over the top of it.
    ///
    /// Otherwise written here, pointing at the runtime's own implementation
    /// when it has one and at the copy that ships with the application when it
    /// does not.
    pub fn describe(&self) -> Result<PathBuf, String> {
        self.describe_in(&paths::data_dir(), &self.interpreter())
    }

    /// The same, told where things are rather than reading it from the process.
    pub fn describe_in(&self, data_dir: &Path, interpreter: &Path) -> Result<PathBuf, String> {
        let folder = self.folder_in(data_dir);
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let path = folder.join("runtime.json");
        if path.exists() && folder.join("engine.py").exists() {
            return Ok(path);
        }
        let _ = interpreter;
        let descriptor = serde_json::json!({
            "schema": crate::runtimes::DESCRIPTOR_SCHEMA,
            "id": self.id,
            "name": self.name,
            // What it is run by, stated rather than inferred from what happens
            // to be on disk: a runtime that said it brought its own code and
            // has none must not be started by a different implementation.
            "engine": if folder.join("engine.py").exists() { "own" } else { "bundled" },
            "program": "{venv}/bin/python3",
            "arguments": ["{engine}"],
        });
        std::fs::write(&path, serde_json::to_string_pretty(&descriptor).unwrap_or_default())
            .map_err(|e| e.to_string())?;
        Ok(path)
    }
}

/// The runtimes this machine could install, whether or not it has.
pub fn offered() -> Vec<&'static Pack> {
    PACKS.iter().copied().filter(|pack| pack.runs_here()).collect()
}

/// Write descriptors for packs that are installed and undescribed.
///
/// A runtime installed before descriptors existed is still a runtime. Called at
/// start so it becomes visible where every other one is, rather than needing to
/// be installed again to be found.
pub fn describe_installed() -> usize {
    offered()
        .into_iter()
        .filter(|pack| pack.installed())
        .filter(|pack| !paths::data_dir().join("runtimes").join(pack.id).join("runtime.json").exists())
        .filter(|pack| pack.describe().is_ok())
        .count()
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
    extract_under(archive, folder).map_err(|e| format!("the {id} runtime {e}"))?;

    if !folder.join("engine.py").exists() {
        return Err(format!("the {id} runtime archive has no engine.py in it"));
    }
    Ok(())
}

/// What one runtime archive may contain.
///
/// Generous for code and a manifest, and nowhere near enough to be a way to
/// fill a disk. The dependencies are not in here — those are installed by uv
/// from a lock, and are the hundreds of megabytes.
const MAX_ENTRIES: usize = 4_000;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Unpack an archive, materialising nothing outside the directory given.
///
/// The rules are here rather than in whichever `tar` the host ships, because
/// they differ and the differences are the whole question. Measured on this
/// machine: `..` was refused, a leading `/` was silently stripped so the entry
/// landed somewhere else inside, a symlink pointing at `/etc/hosts` was created
/// without complaint, and the remaining entries were extracted anyway after the
/// errors — so a caller checking only that its file appeared would have seen
/// success.
///
/// Only regular files and directories are created. A symlink, a hard link, a
/// device or a fifo is refused rather than skipped: an archive that contains
/// one is not a runtime that lost a file, it is an archive doing something
/// else.
fn extract_under(archive: &[u8], root: &Path) -> Result<(), String> {
    use std::io::Read;

    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar
        .entries()
        .map_err(|e| format!("archive could not be read: {e}"))?;

    let mut count = 0usize;
    let mut total = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("archive could not be read: {e}"))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(format!("archive has more than {MAX_ENTRIES} entries in it"));
        }

        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(format!(
                "archive contains a {kind:?}, and a runtime is files and directories"
            ));
        }
        let path = entry
            .path()
            .map_err(|e| format!("archive has an unreadable path in it: {e}"))?
            .into_owned();
        let destination = under(&root, &path)?;

        let size = entry.header().size().unwrap_or(0);
        if size > MAX_ENTRY_BYTES {
            return Err(format!("archive entry {} is larger than {MAX_ENTRY_BYTES} bytes", path.display()));
        }
        total += size;
        if total > MAX_TOTAL_BYTES {
            return Err(format!("archive unpacks to more than {MAX_TOTAL_BYTES} bytes"));
        }

        if kind.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Read no more than the header claimed, whatever the stream offers.
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(size)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("archive entry {} could not be read: {e}", path.display()))?;
        std::fs::write(&destination, &bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Where one archived path lands, or why it may not land anywhere.
///
/// Refused rather than repaired. An entry naming somewhere else is not a
/// runtime with an odd layout, and quietly relocating it — which is what
/// stripping a leading slash does — turns a refusal into a surprise.
fn under(root: &Path, path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    if path.is_absolute() {
        return Err(format!("entry {} names an absolute path", path.display()));
    }
    let mut destination = root.to_path_buf();
    for part in path.components() {
        match part {
            Component::Normal(part) => destination.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("entry {} climbs out of the runtime", path.display()))
            }
            other => {
                return Err(format!("entry {} contains {other:?}", path.display()))
            }
        }
    }
    // Belt and braces: whatever the components said, the result is underneath.
    if !destination.starts_with(root) {
        return Err(format!("entry {} resolves outside the runtime", path.display()));
    }
    Ok(destination)
}

/// Replace this pack's staged recipe with the published one, if the repository
/// offers a release this build can use.
///
/// Returns the version when something changed. Never fatal: every caller has
/// the recipe that shipped, which is what a machine with no network installs
/// from.
pub fn refresh_recipe(project: &Path) -> Result<Option<String>, String> {
    let published = crate::published::Published::newest(pack().id)?;
    Ok(published
        .stage_recipe(project)?
        .then(|| published.version().to_string()))
}

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
pub fn install_from(archive: Option<PathBuf>, report: impl FnMut(Progress)) {
    install_pack(pack(), archive, report)
}

/// Install one named runtime. Nothing is bundled, so this is how a new
/// installation gets anything at all.
pub fn install_pack(
    pack: &'static Pack,
    archive: Option<PathBuf>,
    mut report: impl FnMut(Progress),
) {
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
    let project = runtime.join(pack.manifest);
    // The bundled recipe is staged first and unconditionally: it is the floor,
    // and it is what a machine with no network installs from.
    if let Err(err) = stage_manifest(&project) {
        report(Progress::Failed(err));
        return;
    }
    // Then the published one, if the repository is reachable, vouched for, and
    // offering a release this build can use. Any failure here is reported and
    // stepped over — an unreachable repository must not stop an install that
    // the recipe that shipped can complete on its own.
    match refresh_recipe(&project) {
        Ok(Some(version)) => {
            report(Progress::Step(format!("Using the published runtime {version}…")))
        }
        Ok(None) => {}
        Err(err) => eprintln!("keeping the runtime recipe that shipped: {err}"),
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

    if !pack.installed() {
        report(Progress::Failed("install finished but the engine did not import".into()));
        return;
    }

    // Its own code, if it publishes any. Failing here is failing the install:
    // a runtime that names an implementation and cannot produce the one that
    // was published must not quietly fall back to a different one.
    if let Ok(published) = crate::published::Published::newest(pack.id) {
        if let Err(reason) = published.stage_engine(pack.id, &pack.folder()) {
            report(Progress::Failed(reason));
            return;
        }
    }

    // The last step, and the one that makes it a runtime rather than an
    // interpreter in a folder: a descriptor where runtimes are looked for.
    if let Err(reason) = pack.describe() {
        report(Progress::Failed(format!("installed, but could not be described: {reason}")));
        return;
    }

    report(Progress::Fraction(1.0));
    report(Progress::Done);
}
