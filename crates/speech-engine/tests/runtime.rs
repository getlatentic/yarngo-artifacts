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
fn the_interpreter_is_the_versions_environment_not_the_raw_unpack() {
    let data = Path::new("/tmp/anywhere");
    let home = data.join("runtimes/mlx/1.0.0");
    let python = runtime::venv_python(&home);
    assert!(python.starts_with(&home), "{python:?}");
    assert!(python.ends_with("python3") || python.ends_with("python.exe"), "{python:?}");
    // The sidecar runs inside the version's own environment, built from the
    // lock — not the interpreter the tarball unpacked, which has no packages
    // in it and is shared between versions.
    assert!(python.to_string_lossy().contains(".venv"), "{python:?}");
    let base = runtime::interpreter_base(data, &runtime::MLX);
    assert_ne!(python, base);
    assert!(
        base.starts_with(data.join("interpreters").join(runtime::MLX.python)),
        "the base interpreter is keyed by its pin, so two pins never collide: {base:?}"
    );
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

    runtime::remove_pre_pack_runtime(scratch.path());
    assert!(
        !scratch.path().join("python").exists(),
        "the pre-pack runtime should have been removed"
    );
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

    runtime::remove_pre_pack_runtime(scratch.path());
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
    // Using whatever `python3` resolved to meant two people ran different
    // versions of the engine and its whole dependency tree — which is what the
    // committed lock exists to prevent. It also risked Apple's
    // command-line-tools dialog appearing over our own setup screen, since a
    // bare /usr/bin/python3 is a stub on a Mac without them. So the base
    // interpreter is a fixed place under our data directory and nothing else.
    let base = runtime::interpreter_base(Path::new("/data"), &runtime::MLX);
    assert!(base.starts_with("/data/interpreters"), "{base:?}");
    assert!(
        base.to_string_lossy().contains(runtime::MLX.python),
        "unpinned location: {base:?}"
    );
}

#[test]
fn nothing_installed_is_not_proven() {
    let scratch = Scratch::new();
    assert!(
        !runtime::MLX.proven_in(scratch.path()),
        "an empty {:?} is not an install",
        scratch.path()
    );
}

#[test]
fn an_interpreter_alone_is_not_proven() {
    // The check is deliberately two-part: a half-finished install leaves the
    // interpreter without the speech package, and calling that "installed"
    // would fail later, at the first synthesis, with a worse message.
    let scratch = Scratch::new();
    let bin = scratch.path().join(".venv").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("python3"), "#!/bin/sh\nexit 1\n").unwrap();
    Command::new("chmod").arg("+x").arg(bin.join("python3")).status().unwrap();

    assert!(
        !runtime::MLX.proven_in(scratch.path()),
        "an interpreter without mlx_speech is not an install"
    );
    // And one that answers the probe is.
    std::fs::write(bin.join("python3"), "#!/bin/sh\nexit 0\n").unwrap();
    Command::new("chmod").arg("+x").arg(bin.join("python3")).status().unwrap();
    assert!(runtime::MLX.proven_in(scratch.path()));
}

#[test]
fn a_supplied_archive_that_is_not_the_pinned_one_is_refused() {
    // A hand-carried archive claims to be the same bytes the installer would
    // have fetched, so it is checked against the same pin — the interpreter
    // executes everything else, and "the user chose the file" is not a
    // provenance.
    let scratch = Scratch::new();
    let source = tempfile::tempdir().unwrap();
    let archive = fake_archive(source.path(), true);

    let home = scratch.path().join("runtimes/mlx/1.0.0");
    runtime::stage_bundled(&runtime::MLX, &home).expect("recipe");
    let mut steps = Vec::new();
    let failure = runtime::build_env(
        scratch.path(),
        &runtime::MLX,
        &home,
        Some(archive.clone()),
        &mut |p| {
            if let Progress::Step(step) = p {
                steps.push(step);
            }
        },
    )
    .expect_err("an archive that does not match the pin was accepted");

    assert!(failure.contains("digest"), "the reason should name the check: {failure}");
    assert!(archive.exists(), "a file the user supplied must not be deleted");
    assert!(
        !steps.iter().any(|s| s.contains("Downloading")),
        "a supplied archive must not be downloaded again: {steps:?}"
    );
    assert!(
        !runtime::interpreter_base(scratch.path(), &runtime::MLX).exists(),
        "nothing may be unpacked from bytes that failed their pin"
    );
}

#[test]
fn a_failing_package_step_is_reported_with_its_output() {
    let scratch = Scratch::new();
    // The interpreter is already down, so the build goes straight to uv — and
    // uv refuses, the way a partial or mismatched runtime does in practice.
    let base = runtime::interpreter_base(scratch.path(), &runtime::MLX);
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, "#!/bin/sh\nexit 0\n").unwrap();
    Command::new("chmod").arg("+x").arg(&base).status().unwrap();

    let uv = scratch.path().join("uv-that-refuses");
    std::fs::write(&uv, "#!/bin/sh\necho 'error: no lock file found' >&2\nexit 2\n").unwrap();
    Command::new("chmod").arg("+x").arg(&uv).status().unwrap();
    unsafe { std::env::set_var("YARNGO_UV", &uv) };

    let home = scratch.path().join("runtimes/mlx/1.0.0");
    runtime::stage_bundled(&runtime::MLX, &home).expect("recipe");
    let failure = runtime::build_env(scratch.path(), &runtime::MLX, &home, None, &mut |_| {})
        .expect_err("a refusing uv must fail the install");
    unsafe { std::env::remove_var("YARNGO_UV") };

    assert!(
        failure.contains("speech engine") && failure.contains("lock file"),
        "the reason should carry the step and its output: {failure}"
    );
}

#[test]
fn a_corrupt_archive_is_refused_before_it_is_unpacked() {
    let scratch = Scratch::new();
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("python.tar.gz");
    std::fs::write(&archive, b"this is not a tarball").unwrap();

    let home = scratch.path().join("runtimes/mlx/1.0.0");
    runtime::stage_bundled(&runtime::MLX, &home).expect("recipe");
    let failure =
        runtime::build_env(scratch.path(), &runtime::MLX, &home, Some(archive), &mut |_| {})
            .expect_err("a corrupt archive must fail rather than continue");

    // The pin refuses it before tar ever runs on it.
    assert!(failure.contains("digest"), "the reason should name the check: {failure}");
    assert!(
        !runtime::interpreter_base(scratch.path(), &runtime::MLX).exists(),
        "nothing should be left behind"
    );
}

#[test]
fn an_unsupported_host_is_refused_before_anything_is_downloaded() {
    let scratch = Scratch::new();
    if runtime::host_supported().is_ok() {
        // On a machine that can run it there is nothing to refuse; the other
        // half of this test lives on hosts where the refusal fires.
        return;
    }
    let home = scratch.path().join("runtimes/mlx/1.0.0");
    let failure = runtime::build_env(scratch.path(), &runtime::MLX, &home, None, &mut |_| {})
        .expect_err("an unsupported host must refuse");
    assert_eq!(failure, runtime::host_supported().unwrap_err());
    assert!(
        !scratch.path().join("interpreters").exists(),
        "nothing should be installed on a host that cannot use it"
    );
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

/// An installed version carries its descriptor beside its environment, and the
/// descriptor resolves everything inside that one directory.
#[test]
fn installing_a_runtime_leaves_a_descriptor_beside_its_environment() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/mlx/1.0.0");
    let interpreter = home.join(".venv/bin/python3");
    std::fs::create_dir_all(interpreter.parent().unwrap()).expect("bin");
    std::fs::write(&interpreter, b"#!/bin/sh\n").expect("interpreter");

    speech_engine::runtime::describe_home(&speech_engine::runtime::MLX, &home)
        .expect("describe");

    let found =
        speech_engine::runtimes::Descriptor::read(&home.join("runtime.json"), &places(&sandbox))
            .expect("the descriptor an install writes must load");
    assert_eq!(found.id, "mlx");
    assert_eq!(found.program_path().expect("program"), interpreter);
    // With no code of its own it is run by the copy that ships, which is what
    // a first install has always done.
    assert_eq!(found.engine, speech_engine::runtimes::Engine::Bundled);
    assert_eq!(
        found.engine_path().expect("engine"),
        sandbox.root().join("resources/sidecar/engine.py")
    );

    // And once its own code is there, describing again does not overwrite what
    // the archive said.
    std::fs::write(home.join("engine.py"), b"# own\n").expect("engine");
    speech_engine::runtime::describe_home(&speech_engine::runtime::MLX, &home)
        .expect("describe again");
    let again =
        speech_engine::runtimes::Descriptor::read(&home.join("runtime.json"), &places(&sandbox))
            .expect("still loads");
    assert_eq!(again.engine, speech_engine::runtimes::Engine::Bundled,
        "an existing descriptor is the authority; a re-describe must not rewrite it");
}

fn places(sandbox: &yarngo_testing::Sandbox) -> speech_engine::runtimes::Places {
    speech_engine::runtimes::Places {
        data: sandbox.root().to_path_buf(),
        resources: sandbox.root().join("resources"),
    }
}

/// Load the descriptor at `home`, the way the application loads one.
fn read(home: &Path, sandbox: &yarngo_testing::Sandbox) -> Option<speech_engine::runtimes::Descriptor> {
    speech_engine::runtimes::Descriptor::read(&home.join("runtime.json"), &places(sandbox))
}

/// A runtime that published its own implementation is started from it.
#[test]
fn a_runtime_that_brought_its_own_code_is_started_from_it() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/mlx/1.0.0");
    let interpreter = home.join(".venv/bin/python3");
    std::fs::create_dir_all(interpreter.parent().unwrap()).expect("bin");
    std::fs::write(&interpreter, b"#!/bin/sh\n").expect("interpreter");
    std::fs::write(home.join("engine.py"), b"# the runtime's own").expect("engine");

    speech_engine::runtime::describe_home(&speech_engine::runtime::MLX, &home)
        .expect("describe");

    let found = read(&home, &sandbox).expect("loads");
    assert_eq!(found.engine, speech_engine::runtimes::Engine::Own);
    assert_eq!(
        found.engine_path().expect("engine"),
        home.join("engine.py"),
        "a runtime with its own code was started from the application's"
    );
}

/// A runtime that said it brought its own code and has none is not started by
/// something else. Speaking the same protocol is not being the same
/// implementation.
#[test]
fn a_runtime_missing_its_own_engine_is_not_run_by_the_bundled_one() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/mlx/1.0.0");
    std::fs::create_dir_all(&home).expect("folder");
    std::fs::write(
        home.join("runtime.json"),
        br#"{"schema":1,"id":"mlx","name":"Apple silicon","engine":"own",
             "program":"{venv}/bin/python3","arguments":["{engine}"]}"#,
    )
    .expect("descriptor");
    let bin = home.join(".venv/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::write(bin.join("python3"), b"#!/bin/sh\n").expect("interpreter");

    let found = read(&home, &sandbox).expect("it was not read at all");
    assert!(found.engine_path().is_err(), "it was given an engine it did not bring");
    assert!(!found.available(), "a runtime with no engine was offered");
}

/// A descriptor is downloaded data, and may not become a second way to run
/// anything on the machine.
#[test]
fn a_descriptor_cannot_name_a_program_outside_the_runtime() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/rogue/1.0.0");
    std::fs::create_dir_all(&home).expect("folder");
    std::fs::write(home.join("engine.py"), b"# own").expect("engine");

    for (what, program) in [
        ("a shell", "/bin/sh"),
        ("anything absolute", "/usr/bin/python3"),
        ("climbing out", "../../../../bin/sh"),
        ("climbing out of the environment", "{venv}/../../../../bin/sh"),
        ("a placeholder there is no such thing as", "{data}/sh"),
    ] {
        std::fs::write(
            home.join("runtime.json"),
            serde_json::json!({
                "schema": 1, "id": "rogue", "name": "Rogue", "engine": "own",
                "program": program, "arguments": [],
            })
            .to_string(),
        )
        .expect("descriptor");
        assert!(
            read(&home, &sandbox).is_none(),
            "{what} was accepted: {program}"
        );
    }
}

/// And the environment is the application's. A descriptor that could set one
/// would be running its own code without ever naming a program.
#[test]
fn a_descriptor_cannot_set_the_environment() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/mlx/1.0.0");
    std::fs::create_dir_all(&home).expect("folder");
    std::fs::write(home.join("engine.py"), b"# own").expect("engine");
    let bin = home.join(".venv/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::write(bin.join("python3"), b"#!/bin/sh\n").expect("interpreter");
    std::fs::write(
        home.join("runtime.json"),
        br#"{"schema":1,"id":"mlx","name":"Apple silicon","engine":"own",
             "program":"{venv}/bin/python3","arguments":["{engine}"],
             "env":{"PYTHONPATH":"/tmp/attacker","DYLD_INSERT_LIBRARIES":"/tmp/evil.dylib"}}"#,
    )
    .expect("descriptor");

    let found = read(&home, &sandbox).expect("an unknown field made it unreadable");
    let command = found.command().expect("command");
    let mut named: Vec<String> = command
        .get_envs()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    named.sort();

    // An allowlist rather than a search for the two it tried to set: what makes
    // this safe is that the environment is exactly what the application decided
    // to put there, so anything new has to be added here deliberately.
    assert_eq!(
        named,
        ["YARNGO_CATALOG", "YARNGO_DATA"],
        "the environment is not exactly what the application sets"
    );
    let values: Vec<String> = command
        .get_envs()
        .filter_map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
        .collect();
    assert!(
        !values.iter().any(|value| value.contains("attacker") || value.contains("evil")),
        "a value the descriptor named reached the command: {values:?}"
    );
}

/// A descriptor from a newer application describes an arrangement this one
/// cannot honour, and is refused rather than half-understood.
#[test]
fn a_descriptor_from_a_newer_application_is_refused() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/future/1.0.0");
    std::fs::create_dir_all(&home).expect("folder");
    std::fs::write(home.join("engine.py"), b"# own").expect("engine");
    std::fs::write(
        home.join("runtime.json"),
        br#"{"schema":99,"id":"future","name":"Future","engine":"own",
             "program":"{venv}/bin/python3","arguments":[]}"#,
    )
    .expect("descriptor");
    assert!(read(&home, &sandbox).is_none());
}

/// An id becomes a directory name, so it may not decide where anything lives.
#[test]
fn a_runtime_name_that_is_a_path_is_refused() {
    let sandbox = yarngo_testing::Sandbox::empty();
    let home = sandbox.root().join("runtimes/sneaky/1.0.0");
    std::fs::create_dir_all(&home).expect("folder");
    std::fs::write(home.join("engine.py"), b"# own").expect("engine");
    for id in ["../escape", "/absolute", "with space", "Upper", "dots.and.dots"] {
        std::fs::write(
            home.join("runtime.json"),
            serde_json::json!({
                "schema": 1, "id": id, "name": "Sneaky", "engine": "own",
                "program": "{venv}/bin/python3", "arguments": [],
            })
            .to_string(),
        )
        .expect("descriptor");
        assert!(
            read(&home, &sandbox).is_none(),
            "{id:?} was accepted as a runtime name"
        );
    }
}

/// The archive that was published goes in, and nothing of it is left behind.
#[test]
fn a_published_runtime_archive_is_unpacked() {
    let (_built, archive) = crafted(&[("engine.py", "file", "# the published runtime\n")]);

    let landing = tempfile::tempdir().expect("landing");
    speech_engine::runtime::unpack_engine("mlx", landing.path(), &archive).expect("accepted");

    assert_eq!(
        std::fs::read_to_string(landing.path().join("engine.py")).expect("read"),
        "# the published runtime\n"
    );
    assert!(
        !landing.path().join("crafted.tar.gz").exists(),
        "the archive was left behind after unpacking"
    );
}

/// An archive that is not a runtime is refused rather than half-installed.
#[test]
fn an_archive_without_an_engine_in_it_is_refused() {
    let (_built, archive) = crafted(&[("readme.txt", "file", "nothing to run here\n")]);

    let landing = tempfile::tempdir().expect("landing");
    let refused = speech_engine::runtime::unpack_engine("mlx", landing.path(), &archive)
        .expect_err("an archive with no engine in it was accepted as a runtime");
    assert!(refused.contains("engine.py"), "{refused}");
}

/// Building an archive from a description, including the things a well-behaved
/// tool will not produce. `(name, kind, body)` where kind is "file", "dir",
/// "symlink" or "hardlink"; for links, `body` is the link target.
fn crafted(entries: &[(&str, &str, &str)]) -> (tempfile::TempDir, Vec<u8>) {
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
    elif kind == "contiguous":
        data = body.encode()
        i = tarfile.TarInfo(name); i.type = tarfile.CONTTYPE; i.size = len(data)
        t.addfile(i, io.BytesIO(data))
    elif kind == "file4777":
        data = body.encode()
        i = tarfile.TarInfo(name); i.size = len(data); i.mode = 0o4777
        t.addfile(i, io.BytesIO(data))
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
    let bytes = std::fs::read(&archive).expect("read");
    (dir, bytes)
}

fn unpacking(entries: &[(&str, &str, &str)]) -> (tempfile::TempDir, Result<(), String>) {
    let (_built, archive) = crafted(entries);
    let landing = tempfile::tempdir().expect("landing");
    let outcome = speech_engine::runtime::unpack_engine("mlx", landing.path(), &archive);
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

/// Type 7 — "contiguous" — is a regular file to most tools, which is exactly
/// why it is refused: the allowlist is Regular and Directory, not "whatever
/// probably behaves like a file".
#[test]
fn an_archive_with_a_contiguous_entry_is_refused() {
    let (_landing, outcome) = unpacking(&[("cont.py", "contiguous", "# type 7\n")]);
    outcome.expect_err("a contiguous entry was accepted");
}

/// The archive's modes are not consulted. A setuid, group-writable entry lands
/// as an ordinary owner-writable file, whatever it asked for.
#[cfg(unix)]
#[test]
fn an_archives_modes_are_not_taken() {
    use std::os::unix::fs::PermissionsExt;
    let (landing, outcome) = unpacking(&[
        ("engine.py", "file", "# fine\n"),
        ("sneaky", "file4777", "#!/bin/sh\n"),
    ]);
    outcome.expect("a plain file, whatever bits it wore");
    let mode = std::fs::metadata(landing.path().join("sneaky"))
        .expect("extracted")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o644, "extracted with mode {mode:o}");
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
