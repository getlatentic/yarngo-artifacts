//! The sidecar as it ships, rather than as it sits in the checkout.
//!
//! A bundle carries its own copy of the engine, and a copy can be old. That was
//! not hypothetical: an installed build was found running a sidecar with no
//! knowledge of this protocol at all, which the application reported as an
//! unknown method at start-up and nothing caught before a person did.
//!
//! What it checks is the handshake — that the thing in the bundle speaks this
//! protocol, at this version, and answers. Not that it can generate: that needs
//! a model, and a packaging step should not depend on one.
//!
//!     YARNGO_TEST_BUNDLE="/Applications/Yarngo Studio.app" \
//!         cargo test -p speech-engine --test packaged_sidecar

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;
use speech_engine::protocol::{Connection, PROTOCOL_NAME, PROTOCOL_VERSION};
use yarngo_testing::Sandbox;

/// Long enough for an interpreter to start and import the protocol module, and
/// short enough that a packaging step fails rather than hangs.
const PATIENCE: Duration = Duration::from_secs(30);

fn bundle() -> Option<PathBuf> {
    std::env::var_os("YARNGO_TEST_BUNDLE").map(PathBuf::from)
}

/// The engine inside the bundle. Packagers flatten resource globs, so it may
/// sit directly in Resources rather than under a sidecar directory.
fn script(bundle: &std::path::Path) -> Option<PathBuf> {
    let resources = bundle.join("Contents/Resources");
    [resources.join("sidecar/engine.py"), resources.join("engine.py")]
        .into_iter()
        .find(|path| path.exists())
}

/// An interpreter belonging to a runtime installed on this machine.
///
/// Versions live at `runtimes/<id>/<version>/.venv`, and installs that predate
/// versioning at `runtime/<id>/.venv`; both are looked in, newest first, so
/// this keeps working either side of that change.
fn an_installed_interpreter() -> Option<std::path::PathBuf> {
    if let Ok(named) = std::env::var("YARNGO_PYTHON") {
        return Some(std::path::PathBuf::from(named));
    }
    // Read before the sandbox redirects it, and only read.
    let data = speech_engine::paths::data_dir();

    let mut homes: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(runtimes) = std::fs::read_dir(data.join("runtimes")) {
        for runtime in runtimes.flatten() {
            if let Ok(versions) = std::fs::read_dir(runtime.path()) {
                homes.extend(versions.flatten().map(|v| v.path()));
            }
        }
    }
    if let Ok(legacy) = std::fs::read_dir(data.join("runtime")) {
        homes.extend(legacy.flatten().map(|e| e.path()));
    }
    homes.sort();
    homes
        .into_iter()
        .rev()
        .map(|home| home.join(".venv/bin/python3"))
        .find(|python| python.exists())
}

#[test]
fn the_bundled_sidecar_speaks_this_protocol() {
    let Some(bundle) = bundle() else {
        eprintln!("set YARNGO_TEST_BUNDLE to a packaged .app to check its sidecar");
        return;
    };
    // Said apart, because they are different faults and the messages were not:
    // a path that resolves nowhere reads as an empty bundle, which sends you
    // looking at the packager rather than at how it was called.
    assert!(
        bundle.is_dir(),
        "no bundle at {} — YARNGO_TEST_BUNDLE must be an absolute path, since \
         cargo runs this from its own package directory",
        bundle.display()
    );
    let script = script(&bundle).unwrap_or_else(|| {
        panic!("{} has no engine.py in Contents/Resources", bundle.display())
    });
    // This starts the engine and takes it through a handshake, so it needs an
    // interpreter that can import the whole speech stack — not merely one that
    // can parse the file. Any installed runtime on this machine will do; the
    // bundle is what is under test, not the environment.
    // Skipped rather than failed when there is nothing to run it with: a
    // machine with no runtime installed cannot answer the question, and
    // failing the package build over it would say the bundle is broken when
    // what is missing is the environment. Said loudly, because a check that
    // quietly did not run must not read as one that passed.
    let Some(python) = an_installed_interpreter() else {
        eprintln!(
            "SKIPPED: the bundled sidecar was not started, because no runtime is installed \
             on this machine to start it with. Install one, or set YARNGO_PYTHON to an \
             interpreter that has the speech stack, and package again."
        );
        return;
    };

    // Its own data directory: a packaging check must not read, still less
    // write, whatever the person running it happens to have.
    let sandbox = Sandbox::empty();
    let mut command = Command::new(&python);
    command
        .arg(&script)
        // Python writes compiled bytecode beside what it imports, and what it
        // is importing here is a signed bundle. A file appearing inside one
        // breaks its seal, so this check would fail the build it just passed.
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sandbox.apply(&mut command);
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("could not start {}: {e}", script.display()));
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    let (engine, _events) = Connection::attach(child, stdin, stdout, stderr);

    let hello = engine
        .initialize(PATIENCE)
        .expect("the bundled sidecar did not complete the handshake");
    assert_eq!(
        hello["protocol"], PROTOCOL_NAME,
        "the bundled sidecar speaks something else"
    );
    assert_eq!(
        hello["version"], PROTOCOL_VERSION,
        "the bundled sidecar is a different version of this protocol"
    );

    engine
        .request("ping", json!({}), PATIENCE)
        .expect("the bundled sidecar did not answer a ping");

    assert_eq!(
        engine.malformed_lines(),
        0,
        "something in the bundle wrote to the protocol channel"
    );
}
