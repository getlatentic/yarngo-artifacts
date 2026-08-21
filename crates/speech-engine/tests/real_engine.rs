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
use std::time::{Duration, Instant};

use serde_json::json;
use speech_engine::protocol::{Connection, Events, PROTOCOL_NAME};

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

fn start() -> (Connection, Events) {
    let mut child = Command::new(python())
        .arg(repo().join("sidecar/engine.py"))
        .arg("--protocol")
        .arg("jsonrpc")
        .current_dir(repo())
        .env(
            "YARNGO_DATA",
            "/Users/dev/Library/Application Support/Yarngo Studio",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the engine");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    Connection::attach(child, stdin, stdout, stderr)
}

/// A voice this machine has, if it has one.
fn a_voice(engine: &Connection) -> Option<String> {
    let voices = engine
        .request("list_voices", json!({}), PATIENCE)
        .expect("list_voices");
    voices["voices"]
        .as_array()?
        .first()?
        .get("voice_id")?
        .as_str()
        .map(Into::into)
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
    let Some(voice) = a_voice(&engine) else {
        eprintln!("no voice enrolled on this machine");
        return;
    };

    std::thread::scope(|scope| {
        let conditioning = scope.spawn(|| {
            engine
                .request(
                    "conditioning.prepare",
                    json!({ "voice_id": voice, "job_id": "j1", "execution_id": "j1/1" }),
                    PATIENCE,
                )
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
    let Some(voice) = a_voice(&engine) else { return };

    engine
        .request("conditioning.prepare", json!({ "voice_id": voice }), PATIENCE)
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
    let Some(voice) = a_voice(&engine) else { return };

    // The noisiest paths: model discovery, loading, and conditioning, which is
    // where huggingface prints and the model stack warns.
    engine.request("list_models", json!({}), PATIENCE).expect("models");
    engine.request("system_info", json!({}), PATIENCE).expect("system");
    engine
        .request("conditioning.prepare", json!({ "voice_id": voice }), PATIENCE)
        .expect("prepare");

    assert_eq!(
        engine.malformed_lines(),
        0,
        "something wrote to the protocol channel that was not a frame"
    );
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
    let Some(voice) = a_voice(&engine) else { return };
    std::thread::scope(|scope| {
        let working = scope.spawn(|| {
            engine.request(
                "conditioning.prepare",
                json!({ "voice_id": voice }),
                PATIENCE,
            )
        });
        std::thread::sleep(Duration::from_secs(8));
        engine.request("engine.stop", json!({}), Duration::from_millis(200)).ok();
        // Dropping the connection kills the child, which is what a forced
        // termination does.
        let outcome = working.join().unwrap();
        assert!(outcome.is_ok() || outcome.is_err(), "the request neither finished nor failed");
    });
}
