//! The wire between the app and the sidecar.
//!
//! One JSON object per line, over a child process's stdin and stdout. It is the
//! seam most likely to break silently — a renamed field or a changed reply
//! shape shows up as a mysterious failure at generation time, hours after the
//! change that caused it. So these tests drive a stand-in sidecar written for
//! the purpose: they check the protocol, not the model.
//!
//! `sidecar.rs` fails a spawn fast by calling `ping` during construction, which
//! means every case below has to answer that first.

use std::path::{Path, PathBuf};

use speech_engine::sidecar::MlxSidecar;
use speech_engine::{EngineError, SpeechEngine, SynthesisRequest};

/// Write a Python script that speaks the protocol however the test needs, and
/// hand back its path. `body` is the per-request behaviour, with `req` in scope.
fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    let script = dir.join(format!("{name}.py"));
    let source = format!(
        r#"
import json, sys

def reply(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req["method"] == "ping":
        reply({{"id": req["id"], "ok": True, "result": {{"pong": True}}}})
        continue
{body}
"#
    );
    std::fs::write(&script, source).expect("write stand-in sidecar");
    script
}

fn python() -> PathBuf {
    PathBuf::from("/usr/bin/python3")
}

fn spawn(dir: &tempfile::TempDir, name: &str, body: &str) -> speech_engine::Result<MlxSidecar> {
    let script = stand_in(dir.path(), name, body);
    MlxSidecar::spawn(&python(), &script, dir.path())
}

fn request() -> SynthesisRequest {
    SynthesisRequest {
        text: "Good morning.".into(),
        output: PathBuf::from("/tmp/out.wav"),
        model: Some("dots-tts-mf".into()),
        voice_id: None,
        seed: Some(4417),
        name: None,
        clip_id: None,
    }
}

#[test]
fn spawn_completes_the_ping_handshake() {
    let dir = tempfile::tempdir().unwrap();
    // A sidecar that answers nothing but ping still has to spawn: the handshake
    // is the whole contract at construction time.
    assert!(spawn(&dir, "ping_only", "    pass").is_ok());
}

#[test]
fn spawn_fails_when_the_interpreter_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let script = stand_in(dir.path(), "unused", "    pass");
    match MlxSidecar::spawn(Path::new("/nonexistent/python"), &script, dir.path()) {
        Err(EngineError::Transport(_)) => {}
        Err(other) => panic!("expected Transport, got {other:?}"),
        Ok(_) => panic!("a missing interpreter cannot be spawned"),
    }
}

#[test]
fn a_result_is_parsed_into_its_type() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = spawn(
        &dir,
        "synth",
        r#"    reply({"id": req["id"], "ok": True, "result": {
        "output": req["params"]["output"], "model": "dots-tts-mf",
        "audio_s": 5.44, "gen_s": 12.0, "rtf": 2.2, "seed": 4417,
        "sample_rate": 24000, "clip": None}})"#,
    )
    .unwrap();

    let out = engine.synthesize(&request()).expect("a well-formed reply parses");
    assert_eq!(out.model, "dots-tts-mf");
    assert_eq!(out.seed, Some(4417));
    assert_eq!(out.sample_rate, 24000);
    assert!((out.audio_s - 5.44).abs() < 1e-6);
}

#[test]
fn the_request_carries_every_field_the_engine_needs() {
    let dir = tempfile::tempdir().unwrap();
    // Echo the params back through a field the reply already has, so a dropped
    // or renamed request field fails here rather than at generation time.
    let mut engine = spawn(
        &dir,
        "echo",
        r#"    p = req["params"]
    reply({"id": req["id"], "ok": True, "result": {
        "output": "/tmp/out.wav", "model": json.dumps(sorted(p.keys())),
        "audio_s": 1.0, "gen_s": 1.0, "rtf": 1.0, "seed": p.get("seed"),
        "sample_rate": 24000, "clip": None}})"#,
    )
    .unwrap();

    let out = engine.synthesize(&request()).unwrap();
    let sent: Vec<String> = serde_json::from_str(&out.model).unwrap();
    for field in ["text", "output", "model", "voice_id", "seed", "name", "clip_id"] {
        assert!(sent.contains(&field.to_string()), "{field} was not sent: {sent:?}");
    }
}

#[test]
fn a_clip_of_several_takes_is_parsed() {
    let dir = tempfile::tempdir().unwrap();
    // Generating again adds a take under the same clip, so the reply carries
    // the whole list — newest first — rather than one file.
    let mut engine = spawn(
        &dir,
        "takes",
        r#"    reply({"id": req["id"], "ok": True, "result": {
        "output": "/tmp/out.wav", "model": "dots-tts-mf",
        "audio_s": 1.0, "gen_s": 1.0, "rtf": 1.0, "seed": 2796,
        "sample_rate": 24000,
        "clip": {"id": "clip-1", "title": "t", "name": "n", "text": "t",
                 "voice_id": None, "model": "dots-tts-mf", "created": "now",
                 "takes": [
                   {"id": "take-2", "path": "/tmp/b.wav", "audio_s": 1.3,
                    "gen_s": 4.0, "seed": 2796, "created": "17:02"},
                   {"id": "take-1", "path": "/tmp/a.wav", "audio_s": 1.4,
                    "gen_s": 5.0, "seed": 9895, "created": "13:06"}]}}})"#,
    )
    .unwrap();

    let clip = engine.synthesize(&request()).unwrap().clip.expect("a clip came back");
    assert_eq!(clip.takes.len(), 2);
    assert_eq!(clip.latest().map(|t| t.id.as_str()), Some("take-2"), "newest first");
    assert_eq!(clip.take("take-1").map(|t| t.seed), Some(Some(9895)));
    assert!(clip.take("missing").is_none());
    let rtf = clip.latest().unwrap().rtf().unwrap();
    assert!((rtf - 4.0 / 1.3).abs() < 1e-5, "{rtf}");
}

#[test]
fn a_clip_written_before_takes_existed_still_parses() {
    let dir = tempfile::tempdir().unwrap();
    // The sidecar migrates on read, but the Rust side must not fall over if it
    // ever sees the old shape — `takes` defaults rather than failing.
    let mut engine = spawn(
        &dir,
        "old_clip",
        r#"    reply({"id": req["id"], "ok": True, "result": {
        "output": "/tmp/out.wav", "model": "dots-tts-mf",
        "audio_s": 1.0, "gen_s": 1.0, "rtf": 1.0, "seed": 1,
        "sample_rate": 24000,
        "clip": {"id": "clip-1", "title": "t", "name": "n", "text": "t",
                 "voice_id": None, "model": "dots-tts-mf", "created": "now"}}})"#,
    )
    .unwrap();

    let clip = engine.synthesize(&request()).unwrap().clip.unwrap();
    assert!(clip.takes.is_empty());
    assert!(clip.latest().is_none());
}

#[test]
fn an_engine_side_refusal_is_rejected_not_transport() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = spawn(
        &dir,
        "refuse",
        r#"    reply({"id": req["id"], "ok": False, "error": "nothing to say"})"#,
    )
    .unwrap();

    // The distinction matters: `Rejected` is the engine answering, and the app
    // shows it. `NotRunning` restarts the process.
    match engine.synthesize(&request()) {
        Err(EngineError::Rejected(message)) => assert_eq!(message, "nothing to say"),
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[test]
fn a_reply_of_the_wrong_shape_is_a_transport_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = spawn(
        &dir,
        "wrong_shape",
        r#"    reply({"id": req["id"], "ok": True, "result": {"audio_s": "five"}})"#,
    )
    .unwrap();

    match engine.synthesize(&request()) {
        Err(EngineError::Transport(message)) => {
            assert!(message.contains("unexpected result shape"), "{message}")
        }
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[test]
fn a_line_that_is_not_json_is_a_transport_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = spawn(
        &dir,
        "garbage",
        r#"    sys.stdout.write("this is not json\n"); sys.stdout.flush()"#,
    )
    .unwrap();

    match engine.synthesize(&request()) {
        Err(EngineError::Transport(message)) => {
            assert!(message.contains("unparseable reply"), "{message}")
        }
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[test]
fn a_sidecar_that_exits_reports_not_running() {
    let dir = tempfile::tempdir().unwrap();
    // Answers ping, then leaves. The app restarts the engine on this and only
    // on this, so it must not be reported as a transport failure.
    let mut engine = spawn(&dir, "exits", r#"    sys.exit(0)"#).unwrap();

    match engine.synthesize(&request()) {
        Err(EngineError::NotRunning) => {}
        other => panic!("expected NotRunning, got {other:?}"),
    }
}

#[test]
fn ids_increase_so_replies_cannot_be_read_out_of_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = spawn(
        &dir,
        "ids",
        r#"    reply({"id": req["id"], "ok": True, "result": {
        "output": "/tmp/out.wav", "model": str(req["id"]),
        "audio_s": 1.0, "gen_s": 1.0, "rtf": 1.0, "seed": 1,
        "sample_rate": 24000, "clip": None}})"#,
    )
    .unwrap();

    let first: u64 = engine.synthesize(&request()).unwrap().model.parse().unwrap();
    let second: u64 = engine.synthesize(&request()).unwrap().model.parse().unwrap();
    assert!(second > first, "ids went {first} then {second}");
}

#[test]
fn dropping_the_engine_stops_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("still-alive");
    // The child writes a marker as it goes down cleanly; killed, it never gets
    // to. Either way the point is that the process does not outlive the engine.
    let engine = spawn(
        &dir,
        "long_lived",
        &format!(
            "    import time\n    time.sleep(30)\n    open({:?}, 'w').write('x')",
            marker.display().to_string()
        ),
    )
    .unwrap();
    drop(engine);

    // A killed child releases its end of the pipe immediately; give the OS a
    // moment, then check nothing wrote the marker.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!marker.exists(), "the sidecar outlived the engine that owned it");
}

#[test]
fn stdout_noise_before_the_reply_is_not_tolerated_silently() {
    let dir = tempfile::tempdir().unwrap();
    // A print() left in the sidecar lands on stdout ahead of the reply. That is
    // a real mistake to make, and it must fail loudly rather than hang or
    // return the wrong thing.
    let mut engine = spawn(
        &dir,
        "chatty",
        r#"    print("loading model")
    reply({"id": req["id"], "ok": True, "result": {
        "output": "/tmp/out.wav", "model": "dots-tts-mf",
        "audio_s": 1.0, "gen_s": 1.0, "rtf": 1.0, "seed": 1,
        "sample_rate": 24000, "clip": None}})"#,
    )
    .unwrap();

    match engine.synthesize(&request()) {
        Err(EngineError::Transport(_)) => {}
        other => panic!("noise on stdout is a protocol violation, got {other:?}"),
    }
}

/// The real sidecar, against the real protocol. Ignored by default because it
/// needs the installed runtime; run it with `cargo test -- --ignored` on a
/// machine that has one.
#[test]
#[ignore = "needs the installed Python runtime"]
fn the_real_sidecar_answers_a_ping() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let paths = speech_engine::EnginePaths::resolve(&root);
    if let Some(missing) = paths.missing() {
        panic!("{missing}");
    }

    let mut engine = MlxSidecar::spawn(&paths.python, &paths.script, &paths.work_dir)
        .expect("spawn the real sidecar");
    let models = engine.models(false).expect("the real sidecar lists models");
    assert!(!models.is_empty(), "the catalogue is empty");
}
