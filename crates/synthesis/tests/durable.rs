//! Generation crossing the boundary between the engine and what the person has.
//!
//! Not protocol tests. The engine here is `sidecar/protocol.py` with a stand-in
//! where the model would be, so that a generation can be made to fail, to be
//! slow, or to die at a chosen moment — none of which a real model will do on
//! request. What is under test is everything on the Rust side of the file the
//! engine writes: what exists before it is asked, what the file has to survive
//! to become a take, and what a restart makes of each way it can be left.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use speech_engine::protocol::{Connection, Events};
use yarngo_core::{JobStatus, Rejection};
use yarngo_store::import::{Legacy, LegacyClip, LegacyConsent, LegacyVoice};
use yarngo_store::Store;
use yarngo_synthesis::{Finished, Layout, Outcome, Request, Synthesis};

const PATIENCE: Duration = Duration::from_secs(20);

fn sidecar_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar")
}

/// A stand-in engine whose `synthesis.generate` writes a real wave file at the
/// path it is given, and can be told to misbehave instead.
///
/// `behaviour` is Python run inside the handler before it writes, so a test can
/// raise, sleep, exit, or truncate.
fn engine(behaviour: &str) -> (Connection, Events) {
    let program = format!(
        r#"
import sys, os, time, struct, threading
sys.path.insert(0, {dir:?})
import protocol

def wave(seconds, rate=24000):
    frames = int(seconds * rate)
    data = b"\0\0" * frames
    return (b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " +
            struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16) +
            b"data" + struct.pack("<I", len(data)) + data)

def generate(params, ctx):
    seconds = float(params.get("seed") or 2000) / 1000.0
{behaviour}
    ctx.emit("job.progress", {{"chunks_done": 1, "chunks": 1}})
    path = params["output_path"]
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(wave(seconds))
    return {{"output_path": path, "audio_s": round(seconds, 2), "gen_s": 0.1,
             "seed": params.get("seed"), "sample_rate": 24000, "chunks": 1}}

BROKER = {{"ping": lambda params: {{"pong": True}}}}
MODEL = {{"synthesis.generate": generate}}
protocol.serve(broker=BROKER, model=MODEL, capabilities={{"backend": "stand-in"}})
"#,
        dir = sidecar_dir().to_string_lossy(),
        behaviour = behaviour,
    );
    let mut child = Command::new("/usr/bin/python3")
        .arg("-c")
        .arg(&program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the stand-in engine");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    let (connection, events) = Connection::attach(child, stdin, stdout, stderr);
    connection.initialize(PATIENCE).expect("initialize");
    (connection, events)
}

/// A store with one voice and one clip made with it, plus a clip made with the
/// model's own voice.
fn seeded(dir: &Path) -> (Store, PathBuf) {
    let db = dir.join("app.db");
    let mut store = Store::open(&db).expect("open");
    let recording = dir.join("alice.wav");
    std::fs::write(&recording, b"RIFF").expect("recording");
    store
        .import_legacy(&Legacy {
            voices: [(
                "alice".to_string(),
                LegacyVoice {
                    label: "Alice".into(),
                    reference_audio: recording.to_string_lossy().into(),
                     reference_text: Some("A sentence read at enrolment.".into()),
                    seconds: Some(12.0),
                    created: "t0".into(),
                },
            )]
            .into_iter()
            .collect(),
            clips: vec![LegacyClip {
                id: "clip-alice".into(),
                name: "A clip".into(),
                text: "Hello.".into(),
                voice_id: Some("alice".into()),
                model: Some("dots-tts-mf".into()),
                created: "t0".into(),
                takes: vec![],
            }],
            consent: vec![LegacyConsent {
                voice_id: "alice".into(),
                statement: Some("I agree".into()),
                app_version: None,
                source: None,
                granted_at: "t0".into(),
            }],
        })
        .expect("import");
    (store, db)
}

fn asked_for() -> Request {
    Request {
        clip_id: "clip-alice".into(),
        text: "Hello there.".into(),
        reference: None,
        model: Some("dots-tts-mf".into()),
        seed: Some(2000),
    }
}

fn takes(store: &Store, clip: &str) -> i64 {
    store
        .raw()
        .query_row(
            "SELECT count(*) FROM clip_takes WHERE clip_id = ?1",
            [clip],
            |row| row.get(0),
        )
        .expect("count")
}

fn staged_files(layout: &Layout) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(layout.staging_dir()) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// 1. The ordinary path, and the order it happens in.
#[test]
fn a_generation_becomes_a_take() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = seeded(dir.path());
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "stand-in", "t1").expect("session");
    let (connection, _events) = engine("");

    let pending = {
        let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
        synthesis.begin("job-1", "exec-1", &asked_for(), "t1").expect("begin")
    };

    // The durable intent exists, and says nothing is running yet.
    let queued = store.load_job("job-1").expect("load").expect("job");
    assert_eq!(queued.state(), JobStatus::Queued, "a job was running before it was dispatched");
    assert!(store.output_of("exec-1").expect("intent").is_some(), "no record of what to produce");
    assert_eq!(takes(&store, "clip-alice"), 0);

    let outcome = {
        let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
        synthesis.generate(pending, &connection, PATIENCE, "t2").expect("generate")
    };

    let Outcome::Published { take_id, path } = outcome else {
        panic!("not published: {outcome:?}");
    };
    assert!(path.exists(), "the take's audio is not where the take says it is");
    assert_eq!(takes(&store, "clip-alice"), 1);
    assert_eq!(
        store.load_job("job-1").expect("load").expect("job").state(),
        JobStatus::Completed
    );
    assert_eq!(
        store.execution_state("exec-1").expect("state").as_deref(),
        Some("completed")
    );
    assert!(staged_files(&layout).is_empty(), "the staged file was left behind");

    // What the engine reported is what the take says.
    let audio: Option<f64> = store
        .raw()
        .query_row(
            "SELECT audio_seconds FROM clip_takes WHERE id = ?1",
            [&take_id],
            |row| row.get(0),
        )
        .expect("take");
    assert_eq!(audio, Some(2.0));
}

/// 2. The engine could not do it.
#[test]
fn a_failed_generation_leaves_nothing_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = seeded(dir.path());
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "stand-in", "t1").expect("session");
    let (connection, _events) = engine("    raise RuntimeError('the model fell over')");

    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &asked_for(), "t1").expect("begin");
    let outcome = synthesis.generate(pending, &connection, PATIENCE, "t2").expect("generate");

    assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
    assert_eq!(store.load_job("job-1").expect("load").expect("job").state(), JobStatus::Failed);
    assert_eq!(store.execution_state("exec-1").expect("state").as_deref(), Some("failed"));
    assert_eq!(takes(&store, "clip-alice"), 0);
    assert!(staged_files(&layout).is_empty(), "a partial file was left behind");
}

/// 3. The engine goes. The job is not the person's problem to ask for again.
#[test]
fn losing_the_engine_returns_the_job_and_a_second_attempt_finishes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = seeded(dir.path());
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "stand-in", "t1").expect("session");
    let (dying, _events) = engine("    os._exit(1)");

    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &asked_for(), "t1").expect("begin");
    let outcome = synthesis.generate(pending, &dying, PATIENCE, "t2").expect("generate");
    assert_eq!(outcome, Outcome::Interrupted, "{outcome:?}");

    let job = store.load_job("job-1").expect("load").expect("job");
    assert_eq!(job.state(), JobStatus::Queued, "the job did not come back");
    assert_eq!(job.current_execution(), None);
    assert_eq!(store.execution_state("exec-1").expect("state").as_deref(), Some("interrupted"));

    // A second engine, and a second attempt at the same job.
    store.open_session("session-2", "stand-in", "t3").expect("session");
    let (living, _events) = engine("");
    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-2" };
    // The first attempt's staged audio, had it written any, is not the second's
    // to publish: each attempt has its own name.
    let second = synthesis
        .begin_again(&job, "exec-2", &asked_for(), "t3")
        .expect("second attempt");
    let outcome = synthesis.generate(second, &living, PATIENCE, "t4").expect("generate");

    assert!(matches!(outcome, Outcome::Published { .. }), "{outcome:?}");
    assert_eq!(takes(&store, "clip-alice"), 1, "the same job produced two takes");
    assert_eq!(
        store.load_job("job-1").expect("load").expect("job").state(),
        JobStatus::Completed
    );
    // The abandoned attempt keeps its own outcome.
    assert_eq!(store.execution_state("exec-1").expect("state").as_deref(), Some("interrupted"));
}

/// 4. The race the whole barrier exists for: the person deletes the voice while
///    it is generating, and the engine finishes anyway.
#[test]
fn a_voice_deleted_while_generating_refuses_the_take() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, db) = seeded(dir.path());
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "stand-in", "t1").expect("session");
    // Long enough that the deletion lands while the engine is working.
    let (connection, _events) = engine("    time.sleep(0.8)");

    let deleting = std::thread::spawn({
        let db = db.clone();
        move || {
            std::thread::sleep(Duration::from_millis(250));
            let mut store = Store::open(&db).expect("second connection");
            store.begin_voice_deletion("alice", "job-delete", "t2").expect("begin deletion");
        }
    });

    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &asked_for(), "t1").expect("begin");
    let outcome = synthesis.generate(pending, &connection, PATIENCE, "t3").expect("generate");
    deleting.join().expect("deletion");

    assert_eq!(
        outcome,
        Outcome::Rejected {
            reason: Rejection::VoiceDeleted,
            detail: "the voice was deleted while this was generating".into()
        },
        "{outcome:?}"
    );
    // The attempt finished. The job did not.
    assert_eq!(store.execution_state("exec-1").expect("state").as_deref(), Some("completed"));
    assert_eq!(
        store.load_job("job-1").expect("load").expect("job").state(),
        JobStatus::Cancelled,
        "a refused publication is not a failure"
    );
    assert_eq!(takes(&store, "clip-alice"), 0);
    assert!(
        staged_files(&layout).is_empty(),
        "audio made from a deleted voice was left on the disk"
    );
    assert!(!layout.take("clip-alice", "take-exec-1").exists());
}

/// 5. The application dies between the engine finishing and the take existing.
#[test]
fn a_crash_before_the_take_is_committed_is_finished_on_the_next_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, db) = seeded(dir.path());
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "stand-in", "t1").expect("session");
    let (connection, _events) = engine("");

    // Everything up to the decision, and then nothing: the process is gone.
    let staged = {
        let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
        let pending = synthesis.begin("job-1", "exec-1", &asked_for(), "t1").expect("begin");
        let finished = synthesis.run(pending, &connection, PATIENCE, "t2").expect("run");
        let Finished::Produced { output, .. } = finished else {
            panic!("the engine produced nothing");
        };
        PathBuf::from(output.staged_path)
    };
    assert!(staged.exists(), "the engine wrote nothing");
    assert_eq!(takes(&store, "clip-alice"), 0);
    drop(store);
    drop(connection);

    // The next start.
    let mut store = Store::open(&db).expect("reopen");
    store.end_session("session-1", "t3", "process_exited").expect("end");
    store.reconcile_ended_sessions("t3").expect("sessions");
    let settled = {
        let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-2" };
        synthesis.reconcile("t4").expect("reconcile")
    };

    assert_eq!(settled.len(), 1, "{settled:?}");
    assert!(matches!(settled[0].1, Outcome::Published { .. }), "{settled:?}");
    assert_eq!(takes(&store, "clip-alice"), 1);
    assert_eq!(
        store.load_job("job-1").expect("load").expect("job").state(),
        JobStatus::Completed
    );

    // And again, because a restart can be interrupted too.
    let again = {
        let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-2" };
        synthesis.reconcile("t5").expect("reconcile")
    };
    assert!(again.is_empty(), "there was still something to finish");
    assert_eq!(takes(&store, "clip-alice"), 1, "reconciliation made a second take");
}

/// 6. Nothing but the take is written. The byte-for-byte proof that the real
///    engine leaves the legacy store alone belongs with the real engine, which
///    is the only thing that has that code; what this says is that the Rust
///    side of the path invents no store of its own.
#[test]
fn generating_writes_the_take_and_the_database_and_nothing_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("data");
    let (mut store, _) = seeded(&data);
    let layout = Layout::under(&data);
    store.open_session("session-1", "stand-in", "t1").expect("session");
    let (connection, _events) = engine("");

    let before = tree(&data);
    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &asked_for(), "t1").expect("begin");
    let outcome = synthesis.generate(pending, &connection, PATIENCE, "t2").expect("generate");
    let Outcome::Published { path, .. } = outcome else { panic!("{outcome:?}") };

    let appeared: Vec<PathBuf> = tree(&data).into_iter().filter(|p| !before.contains(p)).collect();
    let expected = path.canonicalize().expect("take");
    for file in &appeared {
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        assert!(
            file == &expected
                // SQLite's own journalling, which is the database writing
                // itself and not a second store of anything.
                || name.starts_with("app.db"),
            "generating created {file:?}"
        );
    }
    assert!(appeared.contains(&expected), "the take was not written");
    for name in ["clips.json", "voices.json"] {
        assert!(
            !tree(&data).iter().any(|p| p.ends_with(name)),
            "the Rust path created {name}"
        );
    }
}

/// Every file under a directory, resolved, so a comparison is about files and
/// not about how they were named.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(path) = path.canonicalize() {
                found.push(path);
            }
        }
    }
    found
}
