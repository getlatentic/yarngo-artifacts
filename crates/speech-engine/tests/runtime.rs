//! Installing the runtime, and deciding whether one is already there.
//!
//! The install path is the first thing a new user meets and the hardest to
//! exercise by hand — it downloads hundreds of megabytes and takes minutes. So
//! these tests drive the parts that can be checked without the network: what
//! `is_installed` accepts, and that a supplied archive is used where it lies
//! rather than fetched.

use std::path::{Path, PathBuf};
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

/// A manifest on disk, served to the installer over `file://`, so the update
/// path can be exercised without publishing anything.
fn local_manifest(dir: &Path, lock: &Path, pyproject: &Path, api: u32, min_app: &str) -> PathBuf {
    let digest = |p: &Path| runtime::sha256_of(p).unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        format!(
            r#"{{"schema":1,"min_app_version":"{min_app}","sidecar_api":{api},
                "tag":"test-tag","catalog":{{"url":"","sha256":""}},
                "runtimes":{{"{pack}":{{
                  "lock_url":"file://{lock}","lock_sha256":"{lock_sha}",
                  "pyproject_url":"file://{py}","pyproject_sha256":"{py_sha}"}}}}}}"#,
            pack = runtime::MLX.id,
            lock = lock.display(),
            lock_sha = digest(lock),
            py = pyproject.display(),
            py_sha = digest(pyproject),
        ),
    )
    .unwrap();
    manifest
}

fn bundled(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/packs/mlx").join(name)
}

#[test]
fn a_published_runtime_replaces_the_staged_one() {
    let scratch = Scratch::new();
    let project = scratch.path().join(runtime::MLX.manifest);
    std::fs::create_dir_all(&project).unwrap();
    // Something is already staged, and it is not what the manifest publishes.
    std::fs::write(project.join("uv.lock"), b"an older lock").unwrap();

    let manifest = local_manifest(
        scratch.path(),
        &bundled("uv.lock"),
        &bundled("pyproject.toml"),
        runtime::SIDECAR_API,
        "0.0.1",
    );
    unsafe { std::env::set_var("YARNGO_MANIFEST_URL", format!("file://{}", manifest.display())) };

    let applied = runtime::refresh_recipe(&project).expect("the published recipe should apply");
    assert_eq!(applied.as_deref(), Some("test-tag"));
    assert_eq!(
        std::fs::read(project.join("uv.lock")).unwrap(),
        std::fs::read(bundled("uv.lock")).unwrap(),
        "the staged lock should now be the published one"
    );
    // Running again is a no-op: the installed lock already matches.
    assert_eq!(runtime::refresh_recipe(&project).unwrap(), None);

    unsafe { std::env::remove_var("YARNGO_MANIFEST_URL") };
}

#[test]
fn a_recipe_needing_a_newer_sidecar_is_refused() {
    // The gate that makes updating the runtime without updating the app safe:
    // a lock describing an environment this engine.py cannot drive must never
    // be installed, or a remote publication bricks working installs.
    let scratch = Scratch::new();
    let project = scratch.path().join(runtime::MLX.manifest);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("uv.lock"), b"the lock we shipped with").unwrap();

    let manifest = local_manifest(
        scratch.path(),
        &bundled("uv.lock"),
        &bundled("pyproject.toml"),
        runtime::SIDECAR_API + 1,
        "0.0.1",
    );
    unsafe { std::env::set_var("YARNGO_MANIFEST_URL", format!("file://{}", manifest.display())) };

    let refused = runtime::refresh_recipe(&project).expect_err("must refuse a newer api");
    assert!(refused.contains("sidecar api"), "{refused}");
    assert_eq!(
        std::fs::read(project.join("uv.lock")).unwrap(),
        b"the lock we shipped with",
        "a refused manifest must leave the staged recipe alone"
    );

    unsafe { std::env::remove_var("YARNGO_MANIFEST_URL") };
}

#[test]
fn a_recipe_for_a_newer_app_is_refused() {
    let scratch = Scratch::new();
    let project = scratch.path().join(runtime::MLX.manifest);
    std::fs::create_dir_all(&project).unwrap();

    let manifest = local_manifest(
        scratch.path(),
        &bundled("uv.lock"),
        &bundled("pyproject.toml"),
        runtime::SIDECAR_API,
        "99.0.0",
    );
    unsafe { std::env::set_var("YARNGO_MANIFEST_URL", format!("file://{}", manifest.display())) };
    let refused = runtime::refresh_recipe(&project).expect_err("must refuse a newer app floor");
    assert!(refused.contains("99.0.0"), "{refused}");
    unsafe { std::env::remove_var("YARNGO_MANIFEST_URL") };
}

#[test]
fn a_recipe_whose_digest_does_not_match_is_refused() {
    let scratch = Scratch::new();
    let project = scratch.path().join(runtime::MLX.manifest);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("uv.lock"), b"untouched").unwrap();

    // Build the manifest against the real lock, then swap the file underneath
    // it — which is what a tampered or truncated download looks like.
    let lock = scratch.path().join("uv.lock");
    std::fs::copy(bundled("uv.lock"), &lock).unwrap();
    let manifest = local_manifest(
        scratch.path(),
        &lock,
        &bundled("pyproject.toml"),
        runtime::SIDECAR_API,
        "0.0.1",
    );
    std::fs::write(&lock, b"something else entirely").unwrap();

    unsafe { std::env::set_var("YARNGO_MANIFEST_URL", format!("file://{}", manifest.display())) };
    let refused = runtime::refresh_recipe(&project).expect_err("must refuse a bad digest");
    assert!(refused.contains("digest"), "{refused}");
    assert_eq!(
        std::fs::read(project.join("uv.lock")).unwrap(),
        b"untouched",
        "nothing may be written before the digest is checked"
    );
    unsafe { std::env::remove_var("YARNGO_MANIFEST_URL") };
}

#[test]
fn a_pre_release_is_older_than_the_release_it_precedes() {
    // Publishing an alpha makes this load-bearing: read the other way round,
    // an alpha would qualify for recipes meant for the finished version.
    assert!(!runtime::version_at_least("0.1.0-alpha.1", "0.1.0"));
    assert!(runtime::version_at_least("0.1.0", "0.1.0-alpha.1"));
    assert!(runtime::version_at_least("0.1.0-alpha.2", "0.1.0-alpha.1"));
    assert!(runtime::version_at_least("0.1.0-alpha.1", "0.0.0"));
    // And numbers still compare as numbers.
    assert!(runtime::version_at_least("1.10.0", "1.9.0"));
    assert!(!runtime::version_at_least("1.9.0", "1.10.0"));
}

#[test]
fn versions_compare_by_number_not_by_text() {
    // "1.10.0" sorts below "1.9.0" as a string, which would let an old app
    // install a recipe meant for a newer one.
    let m: runtime::Manifest = serde_json::from_str(
        r#"{"schema":1,"min_app_version":"1.9.0","sidecar_api":1,"tag":"t","runtimes":{}}"#,
    )
    .unwrap();
    assert!(runtime::manifest_usable(&m).is_err(), "0.1.0 is older than 1.9.0");
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

/// An installed runtime is found and started the way the application finds one.
#[test]
fn installing_a_runtime_leaves_a_descriptor_where_runtimes_are_looked_for() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let interpreter = sandbox.root().join("runtime/mlx/.venv/bin/python3");
    std::fs::create_dir_all(interpreter.parent().unwrap()).expect("bin");
    std::fs::write(&interpreter, b"#!/bin/sh\n").expect("interpreter");

    let written = speech_engine::runtime::MLX
        .describe_in(sandbox.root(), &interpreter)
        .expect("describe");
    assert_eq!(
        written,
        sandbox.root().join("runtimes/mlx/runtime.json"),
        "a descriptor was written somewhere runtimes are not looked for"
    );

    let found = speech_engine::runtimes::discover(&places(&sandbox));
    assert_eq!(found.len(), 1, "the installed runtime was not discovered");
    assert_eq!(found[0].id, "mlx");
    assert_eq!(found[0].program_path().expect("program"), interpreter);
    // With no code of its own it is run by the copy that ships, which is what
    // a first install has always done.
    assert_eq!(found[0].engine, speech_engine::runtimes::Engine::Bundled);
    assert_eq!(
        found[0].engine_path().expect("engine"),
        sandbox.root().join("resources/sidecar/engine.py")
    );
}

fn places(sandbox: &yarngo_testing::Sandbox) -> speech_engine::runtimes::Places {
    speech_engine::runtimes::Places {
        data: sandbox.root().to_path_buf(),
        runtime: sandbox.root().join("runtime"),
        resources: sandbox.root().join("resources"),
    }
}

/// A runtime that published its own implementation is started from it.
#[test]
fn a_runtime_that_brought_its_own_code_is_started_from_it() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let interpreter = sandbox.root().join("runtime/mlx/.venv/bin/python3");
    std::fs::create_dir_all(interpreter.parent().unwrap()).expect("bin");
    std::fs::write(&interpreter, b"#!/bin/sh\n").expect("interpreter");
    let folder = speech_engine::runtime::MLX.folder_in(sandbox.root());
    std::fs::create_dir_all(&folder).expect("folder");
    std::fs::write(folder.join("engine.py"), b"# the runtime's own").expect("engine");

    speech_engine::runtime::MLX
        .describe_in(sandbox.root(), &interpreter)
        .expect("describe");

    let found = speech_engine::runtimes::discover(&places(&sandbox));
    assert_eq!(found[0].engine, speech_engine::runtimes::Engine::Own);
    assert_eq!(
        found[0].engine_path().expect("engine"),
        folder.join("engine.py"),
        "a runtime with its own code was started from the application's"
    );
}

/// A runtime that said it brought its own code and has none is not started by
/// something else. Speaking the same protocol is not being the same
/// implementation.
#[test]
fn a_runtime_missing_its_own_engine_is_not_run_by_the_bundled_one() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let own = sandbox.root().join("runtimes/mlx");
    std::fs::create_dir_all(&own).expect("folder");
    std::fs::write(
        own.join("runtime.json"),
        br#"{"schema":1,"id":"mlx","name":"Apple silicon","engine":"own",
             "program":"{venv}/bin/python3","arguments":["{engine}"]}"#,
    )
    .expect("descriptor");
    let bin = sandbox.root().join("runtime/mlx/.venv/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::write(bin.join("python3"), b"#!/bin/sh\n").expect("interpreter");

    let found = speech_engine::runtimes::discover(&places(&sandbox));
    assert_eq!(found.len(), 1, "it was not read at all");
    assert!(found[0].engine_path().is_err(), "it was given an engine it did not bring");
    assert!(!found[0].available(), "a runtime with no engine was offered");
    assert!(
        speech_engine::runtimes::choose(&found, Some("mlx")).is_none(),
        "a runtime with no engine was chosen"
    );
}

/// A descriptor is downloaded data, and may not become a second way to run
/// anything on the machine.
#[test]
fn a_descriptor_cannot_name_a_program_outside_the_runtime() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let own = sandbox.root().join("runtimes/rogue");
    std::fs::create_dir_all(&own).expect("folder");
    std::fs::write(own.join("engine.py"), b"# own").expect("engine");

    for (what, program) in [
        ("a shell", "/bin/sh"),
        ("anything absolute", "/usr/bin/python3"),
        ("climbing out", "../../../../bin/sh"),
        ("climbing out of the environment", "{venv}/../../../../bin/sh"),
        ("a placeholder there is no such thing as", "{data}/sh"),
    ] {
        std::fs::write(
            own.join("runtime.json"),
            serde_json::json!({
                "schema": 1, "id": "rogue", "name": "Rogue", "engine": "own",
                "program": program, "arguments": [],
            })
            .to_string(),
        )
        .expect("descriptor");
        assert!(
            speech_engine::runtimes::discover(&places(&sandbox)).is_empty(),
            "{what} was accepted: {program}"
        );
    }
}

/// And the environment is the application's. A descriptor that could set one
/// would be running its own code without ever naming a program.
#[test]
fn a_descriptor_cannot_set_the_environment() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let own = sandbox.root().join("runtimes/mlx");
    std::fs::create_dir_all(&own).expect("folder");
    std::fs::write(own.join("engine.py"), b"# own").expect("engine");
    let bin = sandbox.root().join("runtime/mlx/.venv/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::write(bin.join("python3"), b"#!/bin/sh\n").expect("interpreter");
    std::fs::write(
        own.join("runtime.json"),
        br#"{"schema":1,"id":"mlx","name":"Apple silicon","engine":"own",
             "program":"{venv}/bin/python3","arguments":["{engine}"],
             "env":{"PYTHONPATH":"/tmp/attacker","DYLD_INSERT_LIBRARIES":"/tmp/evil.dylib"}}"#,
    )
    .expect("descriptor");

    let found = speech_engine::runtimes::discover(&places(&sandbox));
    assert_eq!(found.len(), 1, "an unknown field made it unreadable");
    let command = found[0].command().expect("command");
    let named: Vec<String> = command
        .get_envs()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert_eq!(named, ["YARNGO_DATA"], "the descriptor set the environment");
}

/// A descriptor from a newer application describes an arrangement this one
/// cannot honour, and is refused rather than half-understood.
#[test]
fn a_descriptor_from_a_newer_application_is_refused() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let own = sandbox.root().join("runtimes/future");
    std::fs::create_dir_all(&own).expect("folder");
    std::fs::write(own.join("engine.py"), b"# own").expect("engine");
    std::fs::write(
        own.join("runtime.json"),
        br#"{"schema":99,"id":"future","name":"Future","engine":"own",
             "program":"{venv}/bin/python3","arguments":[]}"#,
    )
    .expect("descriptor");
    assert!(speech_engine::runtimes::discover(&places(&sandbox)).is_empty());
}

/// An id becomes a directory name, so it may not decide where anything lives.
#[test]
fn a_runtime_name_that_is_a_path_is_refused() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let own = sandbox.root().join("runtimes/sneaky");
    std::fs::create_dir_all(&own).expect("folder");
    std::fs::write(own.join("engine.py"), b"# own").expect("engine");
    for id in ["../escape", "/absolute", "with space", "Upper", "dots.and.dots"] {
        std::fs::write(
            own.join("runtime.json"),
            serde_json::json!({
                "schema": 1, "id": id, "name": "Sneaky", "engine": "own",
                "program": "{venv}/bin/python3", "arguments": [],
            })
            .to_string(),
        )
        .expect("descriptor");
        assert!(
            speech_engine::runtimes::discover(&places(&sandbox)).is_empty(),
            "{id:?} was accepted as a runtime name"
        );
    }
}

/// A runtime archive is executable code arriving over a network. The digest in
/// the manifest is the only thing that says it is what was published.
#[test]
fn a_runtime_archive_that_does_not_match_its_digest_is_refused() {
    use sha2::{Digest, Sha256};
    let served = tempfile::tempdir().expect("tempdir");
    let source = served.path().join("engine.py");
    std::fs::write(&source, b"# the published runtime\n").expect("engine");
    let archive = served.path().join("runtime.tar.gz");
    let ok = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(served.path())
        .arg("engine.py")
        .status()
        .expect("tar");
    assert!(ok.success());
    let url = format!("file://{}", archive.display());
    let real = format!("{:x}", Sha256::digest(std::fs::read(&archive).expect("read")));

    // Something else answered, or the archive changed after it was published.
    let landing = tempfile::tempdir().expect("landing");
    let wrong = "0".repeat(64);
    let refused = speech_engine::runtime::unpack_engine("mlx", landing.path(), &url, &wrong)
        .expect_err("an archive that does not match its digest was accepted");
    assert!(refused.contains("digest"), "{refused}");
    assert!(
        !landing.path().join("engine.py").exists(),
        "code was unpacked before it was found not to match"
    );

    // And the one that was published goes in.
    speech_engine::runtime::unpack_engine("mlx", landing.path(), &url, &real).expect("accepted");
    assert_eq!(
        std::fs::read_to_string(landing.path().join("engine.py")).expect("read"),
        "# the published runtime\n"
    );
    assert!(
        !landing.path().join("runtime.tar.gz").exists(),
        "the archive was left behind after unpacking"
    );
}

/// An archive that is not a runtime is refused rather than half-installed.
#[test]
fn an_archive_without_an_engine_in_it_is_refused() {
    use sha2::{Digest, Sha256};
    let served = tempfile::tempdir().expect("tempdir");
    std::fs::write(served.path().join("readme.txt"), b"nothing to run here\n").expect("write");
    let archive = served.path().join("runtime.tar.gz");
    std::process::Command::new("tar")
        .arg("-czf").arg(&archive).arg("-C").arg(served.path()).arg("readme.txt")
        .status()
        .expect("tar");
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&archive).expect("read")));

    let landing = tempfile::tempdir().expect("landing");
    let refused = speech_engine::runtime::unpack_engine(
        "mlx",
        landing.path(),
        &format!("file://{}", archive.display()),
        &digest,
    )
    .expect_err("an archive with no engine in it was accepted as a runtime");
    assert!(refused.contains("engine.py"), "{refused}");
}

/// Fetches that overlap must not read each other's bytes.
///
/// They did: the download went to a file named for the process, so two at once
/// were the same file. The symptom is a digest mismatch on bytes that are
/// perfectly good, which is unexplainable from the message and would have been
/// found in the field rather than here. Concurrent, because sequential fetches
/// never collided and that is what was tested.
#[test]
fn overlapping_fetches_do_not_read_each_others_bytes() {
    use sha2::{Digest, Sha256};
    let served = tempfile::tempdir().expect("tempdir");

    // Several distinguishable archives, published at once.
    let published: Vec<(String, String)> = (0..6)
        .map(|n| {
            let dir = served.path().join(format!("r{n}"));
            std::fs::create_dir_all(&dir).expect("dir");
            std::fs::write(dir.join("engine.py"), format!("# runtime {n}\n")).expect("engine");
            let archive = served.path().join(format!("r{n}.tar.gz"));
            std::process::Command::new("tar")
                .arg("-czf").arg(&archive).arg("-C").arg(&dir).arg("engine.py")
                .status()
                .expect("tar");
            let digest = format!("{:x}", Sha256::digest(std::fs::read(&archive).expect("read")));
            (format!("file://{}", archive.display()), digest)
        })
        .collect();

    let landing = tempfile::tempdir().expect("landing");
    std::thread::scope(|scope| {
        for (n, (url, digest)) in published.iter().enumerate() {
            let into = landing.path().join(format!("r{n}"));
            scope.spawn(move || {
                speech_engine::runtime::unpack_engine(&format!("r{n}"), &into, url, digest)
                    .unwrap_or_else(|e| panic!("runtime {n} was refused: {e}"));
            });
        }
    });

    for n in 0..6 {
        assert_eq!(
            std::fs::read_to_string(landing.path().join(format!("r{n}/engine.py"))).expect("read"),
            format!("# runtime {n}\n"),
            "runtime {n} was installed with another runtime's code"
        );
    }
}

/// Building an archive from a description, including the things a well-behaved
/// tool will not produce. `(name, kind, body)` where kind is "file", "dir",
/// "symlink" or "hardlink"; for links, `body` is the link target.
fn crafted(entries: &[(&str, &str, &str)]) -> (tempfile::TempDir, String, String) {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("build.py");
    let archive = dir.path().join("crafted.tar.gz");
    let spec: Vec<String> = entries
        .iter()
        .map(|(n, k, b)| format!("({n:?}, {k:?}, {b:?})"))
        .collect();
    std::fs::write(
        &script,
        format!(
            r#"
import tarfile, io, sys
t = tarfile.open(sys.argv[1], "w:gz")
for name, kind, body in [{spec}]:
    if kind == "dir":
        i = tarfile.TarInfo(name); i.type = tarfile.DIRTYPE
        t.addfile(i)
    elif kind == "symlink":
        i = tarfile.TarInfo(name); i.type = tarfile.SYMTYPE; i.linkname = body
        t.addfile(i)
    elif kind == "hardlink":
        i = tarfile.TarInfo(name); i.type = tarfile.LNKTYPE; i.linkname = body
        t.addfile(i)
    else:
        data = body.encode()
        i = tarfile.TarInfo(name); i.size = len(data)
        t.addfile(i, io.BytesIO(data))
t.close()
"#,
            spec = spec.join(", ")
        ),
    )
    .expect("script");
    let ok = std::process::Command::new("/usr/bin/python3")
        .arg(&script)
        .arg(&archive)
        .status()
        .expect("python");
    assert!(ok.success(), "could not build the archive");
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&archive).expect("read")));
    let url = format!("file://{}", archive.display());
    (dir, url, digest)
}

fn unpacking(entries: &[(&str, &str, &str)]) -> (tempfile::TempDir, Result<(), String>) {
    let (_served, url, digest) = crafted(entries);
    let landing = tempfile::tempdir().expect("landing");
    let outcome = speech_engine::runtime::unpack_engine("mlx", landing.path(), &url, &digest);
    (landing, outcome)
}

/// Nothing an archive contains may be created outside the runtime it is being
/// installed into.
///
/// Measured on this machine before this was written: the host tar refused `..`,
/// silently stripped a leading slash so the entry landed somewhere else inside,
/// created a symlink pointing at /etc/hosts without complaint, and extracted
/// the rest of the archive anyway after reporting the errors — so a caller
/// checking only that its own file had appeared would have seen success.
#[test]
fn an_archive_cannot_write_outside_the_runtime() {
    for (what, entries) in [
        ("climbing out", vec![("../escaped", "file", "pwned"), ("engine.py", "file", "#")]),
        ("climbing further", vec![("a/../../escaped", "file", "pwned"), ("engine.py", "file", "#")]),
        ("an absolute path", vec![("/tmp/yarngo-escape-test", "file", "pwned"), ("engine.py", "file", "#")]),
        ("a symlink out", vec![("engine.py", "file", "#"), ("link", "symlink", "/etc/hosts")]),
        ("a hard link out", vec![("engine.py", "file", "#"), ("link", "hardlink", "/etc/hosts")]),
        ("a symlink to a parent", vec![("engine.py", "file", "#"), ("up", "symlink", "..")]),
    ] {
        let (landing, outcome) = unpacking(&entries);
        assert!(outcome.is_err(), "{what} was accepted");
        assert!(
            !landing.path().join("link").exists() && !landing.path().join("up").exists(),
            "{what}: a link was created before the archive was refused"
        );
    }
    assert!(
        !std::path::Path::new("/tmp/yarngo-escape-test").exists(),
        "an absolute entry was written outside the runtime"
    );
}

/// An archive that unpacks to far more than it appears to is refused rather
/// than filling the disk.
#[test]
fn an_archive_that_unpacks_to_too_much_is_refused() {
    let big = "x".repeat(2 * 1024 * 1024);
    let mut entries: Vec<(&str, &str, &str)> = vec![("engine.py", "file", "#")];
    let names: Vec<String> = (0..40).map(|n| format!("pad{n}")).collect();
    for name in &names {
        entries.push((name.as_str(), "file", big.as_str()));
    }
    let (_landing, outcome) = unpacking(&entries);
    let refused = outcome.expect_err("an archive unpacking to 80 MB was accepted");
    assert!(refused.contains("bytes"), "{refused}");
}

/// And an ordinary runtime, with directories, still installs.
#[test]
fn an_ordinary_runtime_archive_installs() {
    let (landing, outcome) = unpacking(&[
        ("engine.py", "file", "# the engine\n"),
        ("backend", "dir", ""),
        ("backend/dots_mlx.py", "file", "# the backend\n"),
        ("./pyproject.toml", "file", "[project]\n"),
    ]);
    outcome.expect("an ordinary runtime was refused");
    assert_eq!(
        std::fs::read_to_string(landing.path().join("backend/dots_mlx.py")).expect("read"),
        "# the backend\n"
    );
    assert!(landing.path().join("pyproject.toml").exists(), "./ was not handled");
}
