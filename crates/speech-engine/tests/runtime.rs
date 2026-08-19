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
fn interpreter_sits_where_the_archive_unpacks_it() {
    let runtime_dir = Path::new("/tmp/anywhere");
    let python = runtime::interpreter(runtime_dir);
    assert!(python.starts_with(runtime_dir));
    assert!(python.ends_with("python3") || python.ends_with("python.exe"), "{python:?}");
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
        runtime::interpreter(scratch.path()).exists(),
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
    // The stub answers every invocation with success, so the run completes.
    assert!(failure.is_none(), "unexpected failure: {failure:?}");
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
    let python = runtime::interpreter(scratch.path());
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
        !runtime::interpreter(scratch.path()).exists(),
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
                !runtime::interpreter(scratch.path()).exists(),
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
fn the_download_url_names_the_pinned_version() {
    // Only meaningful where there is a prebuilt interpreter for the host; on
    // anything else `None` is the right answer and the installer says so.
    if let Some(url) = runtime::download_url() {
        assert!(url.starts_with("https://"), "{url}");
        assert!(url.contains(runtime::VERSION), "the URL should carry the pinned version: {url}");
        assert!(url.ends_with(".tar.gz"), "{url}");
    }
}
