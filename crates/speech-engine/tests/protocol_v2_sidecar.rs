//! The Rust client against the real Python serving layer.
//!
//! `protocol_v2.rs` tests the client against stand-ins written per case, which
//! can be made to do anything. These run `sidecar/protocol.py` itself, with
//! handlers that stand in only for the model — so what is under test is the two
//! halves meeting: the reader that keeps reading, the actor that serialises
//! model work, and the writer that owns stdout.
//!
//! Still no model. The handlers sleep where inference would be.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use speech_engine::protocol::{Connection, Events};
use speech_engine::EngineError;

const PATIENCE: Duration = Duration::from_secs(10);

fn sidecar_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar")
}

/// Start `protocol.serve` with the given handler definitions. `setup` is Python
/// run before serving, and must define `BROKER` and `MODEL` dictionaries.
fn connect(setup: &str) -> (Connection, Events) {
    let program = format!(
        r#"
import sys, time, threading
sys.path.insert(0, {dir:?})
import protocol

{setup}

protocol.serve(broker=BROKER, model=MODEL, capabilities={{"backend": "stand-in"}})
"#,
        dir = sidecar_dir().to_string_lossy(),
        setup = setup,
    );
    let mut child = Command::new("/usr/bin/python3")
        .arg("-c")
        .arg(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidecar");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    Connection::attach(child, stdin, stdout, stderr)
}

/// A broker `ping`, and model work that sleeps and reports progress.
const HANDLERS: &str = r#"
def ping(params):
    return {"pong": True}

def status(params):
    return {"state": "ready"}

def slow(params, ctx):
    total = params.get("chunks", 4)
    for i in range(total):
        if ctx.cancelled():
            return {"stopped_after": i}
        time.sleep(params.get("each", 0.25))
        ctx.emit("job.progress", {"job_id": ctx.job_id, "completed": i + 1, "total": total})
    return {"completed": total}

def touches_state(params, ctx):
    # Records the thread it ran on, so a test can show every model operation
    # shares one.
    return {"thread": threading.current_thread().name}

def explodes(params, ctx):
    raise ValueError("no such voice")

BROKER = {"ping": ping, "engine.status": status}
MODEL = {"slow": slow, "touches_state": touches_state, "explodes": explodes}
"#;

#[test]
fn the_handshake_reports_the_version_and_capabilities() {
    let (engine, _events) = connect(HANDLERS);
    let reply = engine.initialize(PATIENCE).expect("initialize");
    assert_eq!(reply["protocol_version"], 2);
    assert_eq!(reply["backend"], "stand-in");
}

/// The reader keeps reading. A broker call is answered while the actor is busy,
/// which is what the file-based signals existed to work around.
#[test]
fn a_broker_call_is_answered_while_the_actor_works() {
    let (engine, _events) = connect(HANDLERS);
    engine.initialize(PATIENCE).expect("initialize");

    std::thread::scope(|scope| {
        scope.spawn(|| {
            engine
                .request("slow", json!({ "job_id": "j1", "chunks": 4, "each": 0.4 }), PATIENCE)
                .expect("slow");
        });
        std::thread::sleep(Duration::from_millis(300));
        let at = Instant::now();
        let pong = engine.request("ping", json!({}), PATIENCE).expect("ping");
        assert_eq!(pong["pong"], true);
        assert!(
            at.elapsed() < Duration::from_millis(400),
            "ping waited {:?} for the actor",
            at.elapsed()
        );
    });
}

/// Progress arrives while the request that produces it is still outstanding.
#[test]
fn progress_arrives_before_the_result() {
    let (engine, events) = connect(HANDLERS);
    engine.initialize(PATIENCE).expect("initialize");

    std::thread::scope(|scope| {
        scope.spawn(|| {
            engine
                .request("slow", json!({ "job_id": "j2", "chunks": 3, "each": 0.2 }), PATIENCE)
                .expect("slow");
        });
        let first = events.recv_timeout(PATIENCE).expect("progress");
        assert_eq!(first.method, "job.progress");
        assert_eq!(first.params["job_id"], "j2");
        assert_eq!(first.params["completed"], 1);
    });
}

/// Cancellation is answered by the reader, not queued behind the work it is
/// meant to stop, and the work notices at its next checkpoint.
#[test]
fn a_cancellation_is_acknowledged_at_once_and_stops_the_work() {
    let (engine, _events) = connect(HANDLERS);
    engine.initialize(PATIENCE).expect("initialize");

    std::thread::scope(|scope| {
        let work = scope.spawn(|| {
            engine
                .request("slow", json!({ "job_id": "j3", "chunks": 20, "each": 0.2 }), PATIENCE)
                .expect("slow")
        });
        std::thread::sleep(Duration::from_millis(400));
        let at = Instant::now();
        let ack = engine
            .request("job.cancel", json!({ "job_id": "j3" }), PATIENCE)
            .expect("cancel");
        assert_eq!(ack["state"], "cancel_requested");
        assert!(
            at.elapsed() < Duration::from_millis(300),
            "the acknowledgement waited {:?}",
            at.elapsed()
        );

        let result = work.join().unwrap();
        let stopped = result["stopped_after"].as_u64().expect("stopped_after");
        assert!(stopped < 20, "the work ran to completion despite cancelling");
    });
}

/// Every model operation shares one thread, so nothing reads model state while
/// something else writes it.
#[test]
fn model_work_all_happens_on_one_thread() {
    let (engine, _events) = connect(HANDLERS);
    engine.initialize(PATIENCE).expect("initialize");

    let mut threads = Vec::new();
    for _ in 0..5 {
        let reply = engine
            .request("touches_state", json!({}), PATIENCE)
            .expect("touches_state");
        threads.push(reply["thread"].as_str().unwrap().to_string());
    }
    threads.dedup();
    assert_eq!(threads.len(), 1, "model work ran on {threads:?}");
}

/// A handler that raises answers the caller waiting on that id, rather than
/// printing a trace and leaving them to time out.
#[test]
fn a_failing_handler_answers_with_an_error() {
    let (engine, _events) = connect(HANDLERS);
    engine.initialize(PATIENCE).expect("initialize");
    let error = engine
        .request("explodes", json!({}), PATIENCE)
        .expect_err("should fail");
    assert!(
        matches!(&error, EngineError::Rejected(m) if m.contains("no such voice")),
        "{error:?}"
    );
    // And the engine is still answering afterwards.
    engine.request("ping", json!({}), PATIENCE).expect("ping after failure");
}

#[test]
fn an_unknown_method_is_refused_by_name() {
    let (engine, _events) = connect(HANDLERS);
    engine.initialize(PATIENCE).expect("initialize");
    let error = engine
        .request("no.such.method", json!({}), PATIENCE)
        .expect_err("should refuse");
    assert!(
        matches!(&error, EngineError::Rejected(m) if m.contains("no.such.method")),
        "{error:?}"
    );
}

/// Anything writing to stdout would put a line on the wire that is not a frame.
/// The serving layer keeps the real stdout for itself and points the rest at
/// the log, so a stray print cannot corrupt the protocol.
#[test]
fn a_handler_printing_to_stdout_does_not_corrupt_the_wire() {
    let (engine, _events) = connect(
        r#"
def noisy(params, ctx):
    print("a library progress bar")
    sys.stdout.write("more noise\n")
    return {"survived": True}

BROKER = {}
MODEL = {"noisy": noisy}
"#,
    );
    engine.initialize(PATIENCE).expect("initialize");
    let reply = engine.request("noisy", json!({}), PATIENCE).expect("noisy");
    assert_eq!(reply["survived"], true);
    let again = engine.request("noisy", json!({}), PATIENCE).expect("still speaking");
    assert_eq!(again["survived"], true);
    // The point is not that the client survived noise — it does — but that no
    // noise reached the wire to survive.
    assert_eq!(
        engine.malformed_lines(),
        0,
        "a handler's print reached the protocol channel"
    );
}

/// More queued than the engine will hold is refused with a reply, not absorbed
/// into a queue that grows until something else breaks.
#[test]
fn too_much_queued_work_is_refused_rather_than_absorbed() {
    let (engine, _events) = connect(
        r#"
def blocks(params, ctx):
    time.sleep(params.get("each", 5))
    return {}

BROKER = {}
MODEL = {"blocks": blocks}
"#,
    );
    engine.initialize(PATIENCE).expect("initialize");

    let mut refusals = 0;
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..80 {
            handles.push(scope.spawn(|| {
                engine.request("blocks", json!({ "each": 3 }), Duration::from_secs(2))
            }));
        }
        for handle in handles {
            if let Err(EngineError::Rejected(message)) = handle.join().unwrap() {
                if message.contains("too much queued") {
                    refusals += 1;
                }
            }
        }
    });
    assert!(refusals > 0, "the queue absorbed everything instead of refusing");
}
