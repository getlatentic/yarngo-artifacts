//! What the version 2 wire has to survive.
//!
//! The point of v2 is that a reply is not simply the next line: it is the line
//! carrying this request's id, and other things may arrive first or instead.
//! These drive a stand-in sidecar that dispatches each request on its own
//! thread, so it can answer out of order and speak unprompted — the behaviour
//! the real engine gains once its reader stops blocking on inference.
//!
//! No model is involved. These test the protocol.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use speech_engine::protocol::{Connection, Events, PROTOCOL_VERSION};
use speech_engine::EngineError;

const PATIENCE: Duration = Duration::from_secs(10);

/// A sidecar that keeps reading while it works. `body` is the per-request
/// behaviour, with `req` and `send` in scope; it runs on its own thread, which
/// is what lets a slow request sit outstanding while others are answered.
fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    let script = dir.join(format!("{name}.py"));
    let source = format!(
        r#"
import json, sys, threading, time

lock = threading.Lock()

def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()

def handle(req):
{body}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req.get("method") == "initialize":
        send({{"id": req["id"], "result": {{"protocol_version": {PROTOCOL_VERSION}}}}})
        continue
    threading.Thread(target=handle, args=(req,), daemon=True).start()
"#
    );
    std::fs::write(&script, source).expect("write stand-in");
    script
}

fn connect(dir: &tempfile::TempDir, name: &str, body: &str) -> (Connection, Events) {
    let script = stand_in(dir.path(), name, body);
    let mut child = Command::new("/usr/bin/python3")
        .arg(&script)
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stand-in");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    Connection::attach(child, stdin, stdout, stderr)
}

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Answers `echo` immediately, reflecting whatever it was given.
const ECHO: &str = r#"    send({"id": req["id"], "result": {"method": req["method"], "params": req["params"]}})"#;

#[test]
fn both_sides_agree_on_a_version_before_anything_else() {
    let dir = dir();
    let (engine, _events) = connect(&dir, "handshake", ECHO);
    let reply = engine.initialize(PATIENCE).expect("initialize");
    assert_eq!(reply["protocol_version"], PROTOCOL_VERSION);
}

#[test]
fn a_version_mismatch_stops_the_connection() {
    let dir = dir();
    let script = dir.path().join("old.py");
    std::fs::write(
        &script,
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    sys.stdout.write(json.dumps({"id": req["id"], "result": {"protocol_version": 1}}) + "\n")
    sys.stdout.flush()
"#,
    )
    .expect("write");
    let mut child = Command::new("/usr/bin/python3")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let (stdin, stdout, stderr) = (
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        child.stderr.take(),
    );
    let (engine, _events) = Connection::attach(child, stdin, stdout, stderr);
    let error = engine.initialize(PATIENCE).expect_err("should refuse");
    assert!(
        matches!(&error, EngineError::Rejected(m) if m.contains("protocol 1")),
        "{error:?}"
    );
}

/// The whole reason for the change: a cheap call is answered while an
/// expensive one is still running, instead of queueing behind it.
#[test]
fn a_ping_is_answered_while_a_long_request_runs() {
    let dir = dir();
    let (engine, _events) = connect(
        &dir,
        "busy",
        r#"    if req["method"] == "slow":
        time.sleep(2.0)
    send({"id": req["id"], "result": {"method": req["method"]}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");

    let started = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            engine.request("slow", json!({}), PATIENCE).expect("slow");
        });
        // Long enough for the slow request to be in flight.
        std::thread::sleep(Duration::from_millis(300));
        let at = Instant::now();
        engine.request("ping", json!({}), PATIENCE).expect("ping");
        assert!(
            at.elapsed() < Duration::from_millis(500),
            "ping waited {:?} behind the slow request",
            at.elapsed()
        );
    });
    assert!(started.elapsed() >= Duration::from_secs(2), "slow request did not run");
}

/// Replies are matched by id, so one arriving first does not answer the other.
#[test]
fn replies_reach_the_caller_that_asked_for_them() {
    let dir = dir();
    let (engine, _events) = connect(
        &dir,
        "out_of_order",
        r#"    time.sleep(float(req["params"].get("delay", 0)))
    send({"id": req["id"], "result": {"tag": req["params"]["tag"]}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");

    std::thread::scope(|scope| {
        let slow = scope.spawn(|| {
            engine
                .request("work", json!({ "tag": "slow", "delay": 1.0 }), PATIENCE)
                .expect("slow")
        });
        std::thread::sleep(Duration::from_millis(100));
        let quick = engine
            .request("work", json!({ "tag": "quick", "delay": 0 }), PATIENCE)
            .expect("quick");
        assert_eq!(quick["tag"], "quick");
        assert_eq!(slow.join().unwrap()["tag"], "slow");
    });
}

#[test]
fn an_event_arrives_while_a_request_is_outstanding() {
    let dir = dir();
    let (engine, events) = connect(
        &dir,
        "events",
        r#"    send({"method": "job.progress", "params": {"job_id": "j1", "completed": 1}})
    time.sleep(0.3)
    send({"method": "job.completed", "params": {"job_id": "j1"}})
    send({"id": req["id"], "result": {}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");
    engine.request("work", json!({}), PATIENCE).expect("work");

    let first = events.recv_timeout(PATIENCE).expect("progress");
    assert_eq!(first.method, "job.progress");
    assert_eq!(first.params["job_id"], "j1");
    let second = events.recv_timeout(PATIENCE).expect("completed");
    assert_eq!(second.method, "job.completed");
}

/// Progress is the same fact at different values, so it may be dropped when
/// nothing is reading. A job finishing is said once and must not be.
#[test]
fn progress_may_be_dropped_but_a_terminal_event_is_not() {
    let dir = dir();
    let (engine, events) = connect(
        &dir,
        "flood",
        r#"    for i in range(400):
        send({"method": "job.progress", "params": {"job_id": "j1", "completed": i}})
    send({"method": "job.completed", "params": {"job_id": "j1"}})
    send({"id": req["id"], "result": {}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");
    engine.request("work", json!({}), PATIENCE).expect("work");

    let deadline = Instant::now() + PATIENCE;
    let mut terminal = false;
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(event) if event.method == "job.completed" => {
                terminal = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(terminal, "the terminal event was lost");
    assert!(
        engine.dropped_events() > 0,
        "expected some progress to be dropped when the queue filled"
    );
}

#[test]
fn the_child_ending_fails_everyone_still_waiting() {
    let dir = dir();
    let (engine, _events) = connect(
        &dir,
        "exits",
        r#"    if req["method"] == "die":
        import os
        os._exit(0)
    time.sleep(30)"#,
    );
    engine.initialize(PATIENCE).expect("initialize");

    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| engine.request("never", json!({}), PATIENCE));
        std::thread::sleep(Duration::from_millis(200));
        let _ = engine.request("die", json!({}), Duration::from_millis(500));
        assert!(
            matches!(waiting.join().unwrap(), Err(EngineError::NotRunning)),
            "a pending request should fail when the child goes"
        );
    });
    assert!(engine.has_ended());
    assert!(matches!(
        engine.request("after", json!({}), PATIENCE),
        Err(EngineError::NotRunning)
    ));
}

#[test]
fn a_request_that_is_never_answered_times_out() {
    let dir = dir();
    let (engine, _events) = connect(&dir, "silent", r#"    time.sleep(30)"#);
    engine.initialize(PATIENCE).expect("initialize");
    let error = engine
        .request("quiet", json!({}), Duration::from_millis(300))
        .expect_err("should time out");
    assert!(matches!(&error, EngineError::Transport(m) if m.contains("quiet")), "{error:?}");
    // And the connection is still usable afterwards.
    assert!(!engine.has_ended());
}

#[test]
fn a_line_that_is_not_a_frame_does_not_break_the_connection() {
    let dir = dir();
    let (engine, _events) = connect(
        &dir,
        "noise",
        r#"    with lock:
        sys.stdout.write("this is not json\n")
        sys.stdout.flush()
    send({"id": req["id"], "result": {"survived": True}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");
    let reply = engine.request("work", json!({}), PATIENCE).expect("work");
    assert_eq!(reply["survived"], true);
}

#[test]
fn an_error_reply_reaches_the_caller_as_a_rejection() {
    let dir = dir();
    let (engine, _events) = connect(
        &dir,
        "refuses",
        r#"    send({"id": req["id"], "error": {"code": 41, "message": "unknown voice"}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");
    let error = engine.request("work", json!({}), PATIENCE).expect_err("should reject");
    assert!(matches!(&error, EngineError::Rejected(m) if m.contains("unknown voice")), "{error:?}");
}

/// A second reply for the same id finds nothing waiting, and must not be able
/// to answer a later request that happens to reuse the slot.
#[test]
fn a_duplicate_reply_is_ignored() {
    let dir = dir();
    let (engine, _events) = connect(
        &dir,
        "twice",
        r#"    send({"id": req["id"], "result": {"n": 1}})
    send({"id": req["id"], "result": {"n": 2}})"#,
    );
    engine.initialize(PATIENCE).expect("initialize");
    assert_eq!(engine.request("work", json!({}), PATIENCE).expect("first")["n"], 1);
    assert_eq!(engine.request("work", json!({}), PATIENCE).expect("second")["n"], 1);
}
