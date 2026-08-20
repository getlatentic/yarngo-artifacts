//! Installing the runtime, and deciding whether one is already there.
//!
//! The install path is the first thing a new user meets and the hardest to
//! exercise by hand — it downloads hundreds of megabytes and takes minutes. So
//! these tests drive the parts that can be checked without the network: what
//! `is_installed` accepts, and that a supplied archive is used where it lies
//! rather than fetched.

use std::path::Path;
use std::process::Command;

use speech_engine::runtime::{self, Progress};

/// Point the runtime at a scratch directory for the length of a test.
///
/// `YARNGO_RUNTIME_DIR` is process-global and the test harness runs tests in
/// parallel, so the guard is held for the whole test — without it two of these
/// clobber each other's directory and the failure looks like a bug in the
/// installer rather than in the test.
struct Scratch {
    dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Scratch {
    fn new() -> Self {
        let guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("YARNGO_RUNTIME_DIR", dir.path()) };
        Self { dir, _guard: guard }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// A tarball holding `python/bin/python3`, which is the shape the real archive
/// unpacks to. `executable` decides whether the file can actually run.
fn fake_archive(dir: &Path, executable: bool) -> std::path::PathBuf {
    let staging = dir.join("staging");
    let bin = staging.join("python").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let interpreter = bin.join("python3");
    std::fs::write(
        &interpreter,
        if executable { "#!/bin/sh\nexit 0\n" } else { "not a program" },
    )
    .unwrap();
    if executable {
        Command::new("chmod").arg("+x").arg(&interpreter).status().unwrap();
    }

    let archive = dir.join("python.tar.gz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .arg("python")
        .status()
        .unwrap();
    assert!(status.success(), "could not build the fixture archive");
    archive
}

#[test]
fn the_interpreter_is_the_pack_environment_not_the_raw_unpack() {
    let runtime_dir = Path::new("/tmp/anywhere");
    let python = runtime::interpreter(runtime_dir);
    assert!(python.starts_with(runtime_dir));
    assert!(python.ends_with("python3") || python.ends_with("python.exe"), "{python:?}");
    // The sidecar runs inside the pack's environment, built from the lock —
    // not the interpreter the tarball unpacked, which has no packages in it.
    assert!(python.to_string_lossy().contains(".venv"), "{python:?}");
    assert!(python.to_string_lossy().contains(runtime::MLX.manifest), "{python:?}");
    assert_ne!(python, runtime::base_interpreter(runtime_dir));
}

#[test]
fn every_pack_ships_a_manifest_and_a_lock() {
    // A pyproject without its lock would send uv back to resolving from
    // scratch, which is the behaviour the lock exists to replace.
    for pack in [&runtime::MLX, &runtime::TORCH] {
        for file in ["pyproject.toml", "uv.lock"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/packs")
                .join(pack.manifest)
                .join(file);
            assert!(path.exists(), "the {} pack has no {file}", pack.id);
        }
    }
}

#[test]
fn the_torch_lock_keeps_pynini_out() {
    let lock = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/packs/torch/uv.lock"),
    )
    .unwrap();
    // The exclusion is recorded, so the decision is legible to whoever reads
    // the lock next...
    assert!(
        lock.contains("excludes = ") && lock.contains("wetextprocessing"),
        "the exclusion should be stated in the lock, not just implied by absence"
    );
    // ...and scoped to dots-tts, so it says "yarngo packages dots.tts without
    // its normaliser" rather than "nothing may ever use this".
    assert!(lock.contains(r#"name = "dots-tts""#), "the exclusion should name what it applies to");
    // But pynini itself resolves nowhere: no package entry, on any platform.
    assert!(
        !lock.contains(r#"name = "pynini""#),
        "pynini has no Windows wheels and would drag conda into the runtime"
    );
}

#[test]
fn the_torch_lock_actually_covers_windows() {
    // The default resolution took torch 2.13.0, whose cu129 build publishes no
    // Windows wheel — the lock looked complete and a Windows sync would have
    // failed at install. The pack pins the newest torch/torchaudio pair that
    // ships win_amd64 on the CUDA index.
    let lock = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/packs/torch/uv.lock"),
    )
    .unwrap();
    assert!(
        lock.contains("cu129-cp312-cp312-win_amd64.whl"),
        "the CUDA torch in the lock must have a Windows wheel"
    );
    assert!(
        lock.contains("download.pytorch.org/whl/cu129"),
        "non-darwin torch must come from the CUDA index, not PyPI"
    );
    assert!(
        lock.contains("macosx_11_0_arm64") || lock.contains("macosx_14_0_arm64"),
        "darwin torch is the parity-test path and must stay resolvable"
    );
}

#[test]
fn a_runtime_from_the_old_layout_is_cleared_out() {
    // Anyone who installed before packs existed has ~350 MB at <runtime>/python
    // that the pack layout can never reach. Left alone it is counted by the
    // Storage pane and used by nothing.
    let scratch = Scratch::new();
    let legacy_bin = scratch.path().join("python").join("bin");
    std::fs::create_dir_all(&legacy_bin).unwrap();
    std::fs::write(legacy_bin.join("python3"), "#!/bin/sh\nexit 0\n").unwrap();
    Command::new("chmod").arg("+x").arg(legacy_bin.join("python3")).status().unwrap();
    std::fs::create_dir_all(scratch.path().join("python").join("lib")).unwrap();

    // An unsupported host refuses before this runs, so only assert the removal
    // where the installer actually gets that far.
    let source = tempfile::tempdir().unwrap();
    let archive = fake_archive(source.path(), true);
    runtime::install_from(Some(archive), |_| {});

    if runtime::host_supported().is_ok() {
        assert!(
            !scratch.path().join("python").exists(),
            "the pre-pack runtime should have been removed"
        );
    }
}

#[test]
fn a_pack_runtime_is_left_alone() {
    // The guard is two-part so it can only match the old layout. With the pack
    // layout present, nothing is touched.
    let scratch = Scratch::new();
    let legacy_bin = scratch.path().join("python").join("bin");
    std::fs::create_dir_all(&legacy_bin).unwrap();
    std::fs::write(legacy_bin.join("python3"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::create_dir_all(scratch.path().join("interpreter")).unwrap();

    let source = tempfile::tempdir().unwrap();
    let archive = fake_archive(source.path(), true);
    runtime::install_from(Some(archive), |_| {});

    assert!(
        scratch.path().join("python").exists(),
        "a directory beside a pack layout is not ours to delete"
    );
}

#[test]
fn uv_is_pinned_and_its_digest_is_compiled_in() {
    // The digest lives in the binary rather than being fetched beside the
    // archive: a .sha256 published next to the file it describes proves the
    // download survived the wire, not that it is the build this app was tested
    // against. If the pin moves, this fails and the digest must move with it.
    let (asset, digest) = runtime::uv_asset().expect("this host has a uv build");
    assert!(asset.starts_with("uv-"), "{asset}");
    assert_eq!(digest.len(), 64, "a sha256 is 64 hex characters: {digest}");
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{digest}");
    assert!(
        asset.ends_with(".tar.gz") || asset.ends_with(".zip"),
        "the installer only unpacks these: {asset}"
    );
}

#[test]
fn a_tampered_uv_download_is_refused() {
    // The point of the digest. A file that is not what the build expects must
    // never be unpacked, let alone run — it is the thing that installs
    // everything else.
    let scratch = Scratch::new();
    let (asset, _) = runtime::uv_asset().expect("this host has a uv build");
    let planted = scratch.path().join(&asset);
    std::fs::create_dir_all(scratch.path()).unwrap();
    std::fs::write(&planted, b"not uv at all").unwrap();

    let checked = runtime::sha256_of(&planted).unwrap();
    let (_, expected) = runtime::uv_asset().unwrap();
    assert_ne!(checked, expected, "a planted file must not match the pinned digest");
    assert_eq!(checked.len(), 64);
}

#[test]
fn production_never_borrows_the_machines_python() {
    // The PATH probe is gone deliberately. Using whatever `python3` resolved to
    // meant two people ran different versions of the engine and its whole
    // dependency tree — which is what the committed lock exists to prevent.
    // It also risked Apple's command-line-tools dialog appearing over our own
    // setup screen, since a bare /usr/bin/python3 is a stub on a Mac without
    // them.
    let _scratch = Scratch::new();
    unsafe { std::env::remove_var("YARNGO_PYTHON") };
    assert!(
        runtime::existing_interpreter().is_none(),
        "with no explicit interpreter named, nothing on this machine counts"
    );
}

#[test]
fn nothing_installed_is_not_installed() {
    let scratch = Scratch::new();
    assert!(!runtime::is_installed(), "an empty {:?} is not an install", scratch.path());
}

#[test]
fn an_interpreter_alone_is_not_installed() {
    // The check is deliberately two-part: a half-finished install leaves the
    // interpreter without the speech package, and calling that "installed"
    // would fail later, at the first synthesis, with a worse message.
    let scratch = Scratch::new();
    let bin = scratch.path().join("python").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("python3"), "#!/bin/sh\nexit 1\n").unwrap();
    Command::new("chmod").arg("+x").arg(bin.join("python3")).status().unwrap();

    assert!(!runtime::is_installed(), "an interpreter without mlx_speech is not an install");
}

#[test]
fn a_supplied_archive_is_unpacked_and_left_alone() {
    let scratch = Scratch::new();
    let source = tempfile::tempdir().unwrap();
    let archive = fake_archive(source.path(), true);

    let mut steps = Vec::new();
    let mut failure = None;
    runtime::install_from(Some(archive.clone()), |p| match p {
        Progress::Step(step) => steps.push(step),
        Progress::Failed(err) => failure = Some(err),
        _ => {}
    });

    assert!(
        runtime::base_interpreter(scratch.path()).exists(),
        "the archive was not unpacked into the runtime directory"
    );
    assert!(archive.exists(), "a file the user supplied must not be deleted");
    assert!(
        !steps.iter().any(|s| s.contains("Downloading")),
        "a supplied archive must not be downloaded again: {steps:?}"
    );
    assert!(
        steps.iter().any(|s| s.contains("Unpacking")),
        "expected an unpacking step, got {steps:?}"
    );
    // The package step then fails, and that is the correct outcome: the stub is
    // a shell script, not an interpreter. pip used to accept it, because
    // `-m pip install` on a script that exits 0 looks like success; uv queries
    // the interpreter and refuses. The stricter behaviour is worth asserting —
    // a fake runtime should never reach the point of being called installed.
    let failure = failure.expect("a stub interpreter must not pass for a real one");
    assert!(
        failure.contains("Python interpreter") || failure.contains("interpreter"),
        "the reason should name what was wrong with it: {failure}"
    );
}

#[test]
fn a_failing_package_step_is_reported_with_its_output() {
    let scratch = Scratch::new();
    let source = tempfile::tempdir().unwrap();
    // An interpreter that refuses everything, which is what a partial or
    // mismatched runtime looks like in practice.
    let archive = fake_archive(source.path(), true);
    let mut steps = Vec::new();
    let mut failure = None;
    // Unpack first, then break the interpreter before the package step runs.
    runtime::install_from(Some(archive.clone()), |p| match p {
        Progress::Step(step) => steps.push(step),
        Progress::Failed(err) => failure = Some(err),
        _ => {}
    });
    let python = runtime::base_interpreter(scratch.path());
    std::fs::write(&python, "#!/bin/sh\necho 'no module named pip' >&2\nexit 1\n").unwrap();
    Command::new("chmod").arg("+x").arg(&python).status().unwrap();

    failure = None;
    runtime::install_from(Some(archive), |p| {
        if let Progress::Failed(err) = p {
            failure = Some(err);
        }
    });

    let failure = failure.expect("a refusing interpreter must fail the install");
    assert!(
        failure.contains("speech engine") || failure.contains("pip"),
        "the reason should name the step that failed: {failure}"
    );
}

#[test]
fn an_unusable_archive_fails_with_a_reason() {
    let scratch = Scratch::new();
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("python.tar.gz");
    std::fs::write(&archive, b"this is not a tarball").unwrap();

    let mut failure = None;
    runtime::install_from(Some(archive), |p| {
        if let Progress::Failed(err) = p {
            failure = Some(err);
        }
    });

    let failure = failure.expect("a corrupt archive must fail rather than continue");
    assert!(failure.contains("unpacking"), "the reason should name the step: {failure}");
    assert!(
        !runtime::base_interpreter(scratch.path()).exists(),
        "nothing should be left behind by a failed unpack"
    );
}

#[test]
fn an_unsupported_host_is_refused_before_anything_is_downloaded() {
    let scratch = Scratch::new();
    let source = tempfile::tempdir().unwrap();
    let archive = fake_archive(source.path(), true);

    let mut steps = Vec::new();
    let mut failure = None;
    runtime::install_from(Some(archive), |p| match p {
        Progress::Step(step) => steps.push(step),
        Progress::Failed(err) => failure = Some(err),
        _ => {}
    });

    match runtime::host_supported() {
        // On a machine that can run it, the install proceeds as usual.
        Ok(()) => assert!(!steps.is_empty(), "a supported host should get on with it"),
        Err(reason) => {
            assert_eq!(failure.as_deref(), Some(reason.as_str()));
            assert!(steps.is_empty(), "nothing should happen first: {steps:?}");
            assert!(
                !runtime::base_interpreter(scratch.path()).exists(),
                "nothing should be installed on a host that cannot use it"
            );
        }
    }
}

#[test]
fn the_refusal_names_the_reason_rather_than_the_symptom() {
    // Whatever this host is, the message has to say what is wrong in words the
    // person reading it can act on — never a bare "unsupported".
    if let Err(reason) = runtime::host_supported() {
        assert!(reason.len() > 40, "too terse to act on: {reason}");
        assert!(
            reason.contains("Apple silicon") || reason.contains("Apple's MLX"),
            "the reason should name what is actually missing: {reason}"
        );
    }
}

#[test]
fn an_interpreter_the_machine_already_has_is_found_and_used() {
    let scratch = Scratch::new();
    let stub = scratch.path().join("already-here");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    unsafe { std::env::set_var("YARNGO_PYTHON", &stub) };

    // Nothing was installed here, but the machine can already speak, so there
    // is nothing to download and nothing for setup to ask.
    assert_eq!(runtime::existing_interpreter().as_deref(), Some(stub.as_path()));
    assert!(runtime::is_installed(), "an interpreter that imports the package is an install");

    let paths = speech_engine::EnginePaths::resolve(Path::new("/nonexistent"));
    assert_eq!(paths.python, stub, "the found interpreter should be the one used");

    unsafe { std::env::remove_var("YARNGO_PYTHON") };
}

#[test]
fn an_interpreter_that_cannot_import_the_package_is_not_used() {
    let scratch = Scratch::new();
    let stub = scratch.path().join("no-package");
    std::fs::write(&stub, "#!/bin/sh\nexit 1\n").unwrap();
    Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    unsafe { std::env::set_var("YARNGO_PYTHON", &stub) };

    assert_ne!(
        runtime::existing_interpreter().as_deref(),
        Some(stub.as_path()),
        "an interpreter without the speech package is not a runtime"
    );

    unsafe { std::env::remove_var("YARNGO_PYTHON") };
}

#[test]
fn the_two_packs_cannot_share_an_interpreter() {
    // This is the whole reason the Python version belongs to the pack. mlx-speech
    // declares >=3.13; upstream dots.tts declares >=3.10,<3.13. A single pin
    // would quietly break whichever backend it was not chosen for, and the
    // breakage would appear as a failed install on someone else's machine.
    let (mlx, torch) = (&runtime::MLX, &runtime::TORCH);
    assert_ne!(mlx.python, torch.python, "one pin cannot satisfy both backends");
    assert!(mlx.python.starts_with("3.13"), "mlx-speech needs 3.13: {}", mlx.python);
    assert!(torch.python.starts_with("3.12"), "dots.tts refuses 3.13: {}", torch.python);
}

#[test]
fn both_packs_carry_the_same_checkpoints() {
    // The product promises dots.tts MF and SOAR everywhere. A second backend
    // that installed a different model family would be a different product
    // wearing the same two labels.
    assert!(runtime::TORCH.packages.iter().any(|p| p.contains("dots")));
    // And it must not drag in the normaliser: that pulls pynini, which has no
    // Windows wheels, for a feature the upstream runtime defaults to off.
    assert!(
        !runtime::TORCH.packages.iter().any(|p| p.eq_ignore_ascii_case("WeTextProcessing")),
        "the normaliser is what forces conda onto Windows"
    );
}

#[test]
fn only_the_proven_pack_is_selected() {
    // The torch pack is declared but has never run anywhere. Until the spike
    // passes, nothing may choose it.
    assert_eq!(runtime::pack().id, runtime::MLX.id);
    assert_eq!(runtime::VERSION, runtime::MLX.python);
}

#[test]
fn the_download_url_names_the_pinned_version() {
    // Only meaningful where there is a prebuilt interpreter for the host; on
    // anything else `None` is the right answer and the installer says so.
    if let Some(url) = runtime::download_url() {
        assert!(url.starts_with("https://"), "{url}");
        assert!(url.contains(runtime::VERSION), "the URL should carry the pinned version: {url}");
        assert!(url.ends_with(".tar.gz"), "{url}");
    }
}
