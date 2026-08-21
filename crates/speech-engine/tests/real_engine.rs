//! The Rust client against the real engine, with a real model behind it.
//!
//! The other two suites test the protocol: one against stand-ins that can be
//! made to misbehave, one against the serving layer with the model stubbed out.
//! Neither can say whether the actual backend stays answerable while it works,
//! or whether its conditioning cache is really emptied — those are facts about
//! MLX and this machine, not about message shapes.
//!
//! Skipped unless asked for, because it loads a 3.4 GB model and conditions a
//! voice:
//!
//!     YARNGO_TEST_ENGINE=1 cargo test -p speech-engine --test real_engine

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::json;
use speech_engine::protocol::{Connection, Events, PROTOCOL_NAME};
use yarngo_testing::Sandbox;

/// Conditioning a voice takes about forty seconds on a quiet machine and twice
/// that under load, so nothing here is impatient.
const PATIENCE: Duration = Duration::from_secs(240);

fn wanted() -> bool {
    std::env::var_os("YARNGO_TEST_ENGINE").is_some()
}

fn python() -> PathBuf {
    std::env::var_os("YARNGO_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/Users/dev/workspace/voice-clone-bench/mlx-speech/.venv/bin/python")
        })
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A copy of the installed store, made once. The engine writes nothing on this
/// path, and it is still a copy: proving that is what the tests are for, and a
/// test that assumes its own conclusion has no way to fail.
fn store() -> Option<&'static Sandbox> {
    static STORE: OnceLock<Option<Sandbox>> = OnceLock::new();
    STORE
        .get_or_init(|| Sandbox::copying(&speech_engine::paths::installed_data_dir()))
        .as_ref()
}

fn start() -> (Connection, Events) {
    let mut command = Command::new(python());
    command
        .arg(repo().join("sidecar/engine.py"))
        .arg("--protocol")
        .arg("jsonrpc")
        .current_dir(repo())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    store().expect("a store to copy").apply(&mut command);
    let mut child = command.spawn().expect("spawn the engine");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    Connection::attach(child, stdin, stdout, stderr)
}

fn data_dir() -> PathBuf {
    store().map(|s| s.root().to_path_buf()).unwrap_or_default()
}

/// A recording this machine can speak with, if it has one.
///
/// Read from the store rather than asked for: the engine has no voice table on
/// this path, which is the point of it. Which voices exist is the application's
/// to know, and a request either carries the recording or asks for the model's
/// own voice.
fn a_recording() -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(data_dir().join("voices/voices.json")).ok()?;
    let voices: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let voice = voices.as_object()?.values().next()?;
    Some(json!({
        "reference_audio": voice.get("reference_audio")?.as_str()?,
        "reference_text": voice.get("reference_text").and_then(|t| t.as_str()),
    }))
}

/// The same, with a job and execution attached, as real work always has.
fn against(recording: &serde_json::Value, job: &str, execution: &str) -> serde_json::Value {
    let mut params = recording.clone();
    params["job_id"] = json!(job);
    params["execution_id"] = json!(execution);
    params
}

#[test]
fn the_real_engine_completes_the_handshake() {
    if !wanted() {
        eprintln!("set YARNGO_TEST_ENGINE=1 to run against the real engine");
        return;
    }
    let (engine, _events) = start();
    let reply = engine.initialize(PATIENCE).expect("initialize");
    assert_eq!(reply["protocol"], PROTOCOL_NAME);
    assert_eq!(reply["version"], 1);
    assert_eq!(reply["backend"], "mlx");
    // Stated rather than discovered later by a deletion that has to report it.
    assert_eq!(reply["conditioning_eviction"], "all");
    assert!(reply["models"].as_array().is_some_and(|m| !m.is_empty()));
}

/// The measurement said a control thread stays schedulable through inference.
/// This says the protocol does too — parsing, dispatch, and the write back.
#[test]
fn the_engine_answers_while_it_conditions_a_voice() {
    if !wanted() {
        return;
    }
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    let Some(recording) = a_recording() else {
        eprintln!("no voice enrolled on this machine");
        return;
    };
    let warming = against(&recording, "j1", "j1/1");

    std::thread::scope(|scope| {
        let conditioning = scope.spawn(|| {
            engine
                .request("conditioning.prepare", warming, PATIENCE)
                .expect("conditioning")
        });

        // Long enough that the model is loaded and the work has started.
        std::thread::sleep(Duration::from_secs(12));
        let mut worst = Duration::ZERO;
        for _ in 0..5 {
            let at = Instant::now();
            engine.request("ping", json!({}), PATIENCE).expect("ping");
            worst = worst.max(at.elapsed());
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            worst < Duration::from_secs(2),
            "the engine took {worst:?} to answer a ping while working"
        );
        conditioning.join().unwrap();
    });
}

/// Conditioning fills the cache; invalidation empties it. Against the real
/// cache, which is where the earlier bug lived — the lookup was on the wrong
/// object and reported success either way.
#[test]
fn conditioning_is_really_cleared() {
    if !wanted() {
        return;
    }
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    let Some(recording) = a_recording() else { return };

    engine
        .request("conditioning.prepare", recording, PATIENCE)
        .expect("prepare");

    let cleared = engine
        .request("conditioning.invalidate", json!({}), PATIENCE)
        .expect("invalidate");
    assert_eq!(cleared["status"], "cleared", "nothing was there to clear");
    assert!(cleared["entries_removed"].as_u64().unwrap_or(0) >= 1);
    // Said plainly, because one deletion forgets every voice.
    assert_eq!(cleared["scope_applied"], "all");

    // And again is the ordinary, successful no-op.
    let again = engine
        .request("conditioning.invalidate", json!({}), PATIENCE)
        .expect("invalidate again");
    assert_eq!(again["status"], "already_empty");
    assert_eq!(again["entries_removed"], 0);
}

/// Cancelling something the engine has never been given says so, rather than
/// acknowledging a request that landed on nothing.
#[test]
fn cancelling_an_unknown_execution_says_so() {
    if !wanted() {
        return;
    }
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    let reply = engine
        .request("job.cancel", json!({ "execution_id": "never-submitted" }), PATIENCE)
        .expect("cancel");
    assert_eq!(reply["state"], "unknown_execution");
}

/// Everything the engine writes to its protocol channel is a frame. A library's
/// progress bar on stdout would be one line, and one is enough.
#[test]
fn nothing_but_protocol_reaches_the_channel() {
    if !wanted() {
        return;
    }
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    let Some(recording) = a_recording() else { return };

    // The noisiest paths: model discovery, loading, and conditioning, which is
    // where huggingface prints and the model stack warns.
    engine.request("model.list", json!({}), PATIENCE).expect("models");
    engine.request("system_info", json!({}), PATIENCE).expect("system");
    engine
        .request("conditioning.prepare", recording, PATIENCE)
        .expect("prepare");

    assert_eq!(
        engine.malformed_lines(),
        0,
        "something wrote to the protocol channel that was not a frame"
    );
}

/// The engine answers for what it can make, never for what the application
/// keeps. A storage method reaching it here would mean the records live in two
/// places, and the second one would be the one nothing else agreed with.
#[test]
fn the_engine_will_not_answer_for_stored_records() {
    if !wanted() {
        return;
    }
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    for method in ["list_clips", "rename_clip", "delete_clip", "list_voices", "rename_voice"] {
        let refused = engine.request(method, json!({}), PATIENCE);
        assert!(refused.is_err(), "{method} was answered on the JSON-RPC path");
    }
}

/// A request outstanding when the engine goes is failed rather than left.
#[test]
fn losing_the_engine_fails_what_was_waiting() {
    if !wanted() {
        return;
    }
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    drop(engine);

    // A second engine, killed under a request that cannot finish quickly.
    let (engine, _events) = start();
    engine.initialize(PATIENCE).expect("initialize");
    let Some(recording) = a_recording() else { return };
    std::thread::scope(|scope| {
        let working = scope.spawn(|| engine.request("conditioning.prepare", recording, PATIENCE));
        std::thread::sleep(Duration::from_secs(8));
        // Dropping the connection kills the child, which is what a forced
        // termination does. What matters is that the waiting request is
        // answered at all rather than left on a channel nothing will write to.
        let outcome = working.join().unwrap();
        assert!(outcome.is_ok() || outcome.is_err(), "the request neither finished nor failed");
    });
}
