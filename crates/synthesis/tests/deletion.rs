//! Deleting a voice while it is being used.
//!
//! Everything here goes through the one `EngineHandle` the application has, so
//! the cancellation really does have to reach a busy engine rather than wait
//! for it. The model is a stand-in, because the cases are ones a real model
//! cannot be asked for: finish anyway, never stop, refuse to come back.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::sync::mpsc::Receiver;

use speech_engine::{EngineError, EngineHandle, Synthesis, SynthesisRequest};
use yarngo_store::import::{Legacy, LegacyClip, LegacyConsent, LegacyVoice};
use yarngo_store::Store;
use yarngo_synthesis::engine::{DurableEngine, Spawn};
use yarngo_testing::{standin, Sandbox};

const PATIENCE: Duration = Duration::from_secs(30);

/// A sandbox holding one voice, one clip made with it, and the recording.
fn seeded() -> (Sandbox, PathBuf) {
    let sandbox = Sandbox::empty();
    let voices = sandbox.root().join("voices");
    std::fs::create_dir_all(&voices).expect("voices");
    let recording = voices.join("alice.wav");
    std::fs::write(&recording, vec![0u8; 4096]).expect("recording");

    let mut store = Store::open(&sandbox.database()).expect("open");
    store
        .import_legacy(&Legacy {
            voices: [(
                "alice".to_string(),
                LegacyVoice {
                    label: "Alice".into(),
                    reference_audio: recording.to_string_lossy().into(),
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
    drop(store);
    (sandbox, recording)
}

/// The application's engine, over a stand-in that behaves as the test needs.
fn engine(sandbox: &Sandbox, behaviour: &str, grace: Duration) -> Arc<EngineHandle> {
    let script = standin::script(sandbox.root(), behaviour);
    let spawn = Spawn {
        python: standin::python(),
        script,
        work_dir: sandbox.root().to_path_buf(),
        data_dir: sandbox.root().to_path_buf(),
    };
    let database = sandbox.database();
    let data = sandbox.root().to_path_buf();
    Arc::new(
        EngineHandle::spawn_backend(move || {
            Ok(Box::new(
                DurableEngine::open(&database, &data, spawn)?.with_grace(grace),
            ))
        })
        .expect("engine"),
    )
}

fn asked_for() -> SynthesisRequest {
    SynthesisRequest {
        text: "Hello there.".into(),
        output: PathBuf::new(),
        model: Some("dots-tts-mf".into()),
        clip_id: Some("clip-alice".into()),
        voice_id: None,
        seed: Some(2000),
        name: None,
    }
}

fn ask(sandbox: &Sandbox, query: &str) -> String {
    let store = Store::open(&sandbox.database()).expect("read");
    store
        .raw()
        .query_row(query, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|_| "<none>".into())
}

fn count(sandbox: &Sandbox, query: &str) -> i64 {
    let store = Store::open(&sandbox.database()).expect("read");
    store.raw().query_row(query, [], |row| row.get(0)).expect("count")
}

/// Generate on another thread and collect the answer with a deadline.
///
/// Not a join: a stand-in told to sleep for ten minutes will do exactly that,
/// so waiting for the thread would turn "the cancellation never landed" into a
/// test that hangs instead of one that fails.
fn generating(handle: &Arc<EngineHandle>) -> Receiver<Result<Synthesis, EngineError>> {
    let (answered, answer) = std::sync::mpsc::channel();
    let handle = handle.clone();
    std::thread::spawn(move || {
        let _ = answered.send(handle.synthesize(asked_for()));
    });
    answer
}

fn answer(from: &Receiver<Result<Synthesis, EngineError>>) -> Result<Synthesis, EngineError> {
    from.recv_timeout(PATIENCE)
        .expect("the generation never ended")
}

fn until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let stop = Instant::now() + deadline;
    while Instant::now() < stop {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn staged(sandbox: &Sandbox) -> Vec<PathBuf> {
    std::fs::read_dir(sandbox.root().join("staging"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// C. The engine finishes despite being asked to stop.
///
/// Not a failure and not a success: the audio exists and the application will
/// not have it, because the voice it was made from is gone.
#[test]
fn a_generation_that_finishes_anyway_is_not_published() {
    let (sandbox, recording) = seeded();
    // Long enough to still be running when the deletion lands, and deaf to the
    // cancellation when it does.
    let handle = engine(&sandbox, "    time.sleep(1.5)", Duration::from_secs(30));

    let generating = generating(&handle);
    assert!(
        until(PATIENCE, || handle.progress().is_some()
            || count(&sandbox, "SELECT count(*) FROM job_executions WHERE state = 'running'") == 1),
        "the generation never started"
    );

    handle.delete_voice("alice").expect("delete");
    // The barrier is up from the moment that returned.
    assert_eq!(ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'"), "deletion_pending");

    let outcome = answer(&generating);
    // Refused, not failed. The generation did what it was told; what changed is
    // that the person no longer wants the voice it was speaking in, and telling
    // them their own deletion broke something would be a lie.
    match outcome {
        Err(EngineError::Refused(reason)) => assert!(
            reason.contains("deleted"),
            "the refusal does not say why: {reason:?}"
        ),
        other => panic!("a deleted voice's generation ended as {other:?}"),
    }

    assert!(
        until(PATIENCE, || ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'") == "deleted"),
        "the deletion never finished"
    );
    assert_eq!(
        ask(&sandbox, "SELECT state FROM job_executions WHERE id LIKE 'job-%/1' LIMIT 1"),
        "completed",
        "the attempt did not finish, so this is not the race it claims to be"
    );
    assert_eq!(
        ask(&sandbox, "SELECT state FROM jobs WHERE kind = 'synthesis' LIMIT 1"),
        "cancelled",
        "a refused publication was recorded as something other than a cancellation"
    );
    assert_eq!(count(&sandbox, "SELECT count(*) FROM clip_takes"), 0);
    assert!(staged(&sandbox).is_empty(), "audio from a deleted voice was left staged");
    assert!(!recording.exists(), "the recording is still on the disk");
}

/// D. The engine will not stop, so it is ended.
#[test]
fn an_engine_that_will_not_stop_is_ended_and_the_deletion_finishes() {
    let (sandbox, recording) = seeded();
    // Never returns, never looks at the cancellation.
    let handle = engine(&sandbox, "    time.sleep(600)", Duration::from_secs(2));

    let generating = generating(&handle);
    assert!(
        until(PATIENCE, || count(&sandbox, "SELECT count(*) FROM job_executions WHERE state = 'running'") == 1),
        "the generation never started"
    );

    handle.delete_voice("alice").expect("delete");
    assert!(
        until(PATIENCE, || ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'") == "deleted"),
        "the deletion never finished: the voice is {}",
        ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'")
    );
    assert_eq!(
        ask(&sandbox, "SELECT state FROM job_executions WHERE id LIKE 'job-%/1' LIMIT 1"),
        "interrupted",
        "an engine that was killed is not one that cancelled"
    );
    assert_eq!(ask(&sandbox, "SELECT state FROM jobs WHERE kind = 'synthesis' LIMIT 1"), "cancelled");
    assert!(answer(&generating).is_err(), "an ended generation reported success");
    assert!(!recording.exists(), "the recording survived the engine being ended");
    assert!(staged(&sandbox).is_empty());

    // And the application still has an engine.
    assert!(handle.ping().is_ok(), "nothing replaced the engine that was ended");
}

/// E. The replacement will not come up.
///
/// The deletion is not conditional on the application still working: a
/// recording somebody asked to remove does not stay because the thing that
/// would have replaced its reader failed to start.
#[test]
fn a_replacement_that_will_not_start_does_not_keep_the_recording() {
    let (sandbox, recording) = seeded();
    let refuse = sandbox.root().join("refuse");
    let handle = engine(&sandbox, "    time.sleep(600)", Duration::from_secs(2));

    let generating = generating(&handle);
    assert!(
        until(PATIENCE, || count(&sandbox, "SELECT count(*) FROM job_executions WHERE state = 'running'") == 1),
        "the generation never started"
    );

    // From here, nothing new will start.
    std::fs::write(&refuse, b"1").expect("refuse");
    handle.delete_voice("alice").expect("delete");
    assert!(
        until(PATIENCE, || ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'") == "deleted"),
        "the recording was kept because the replacement failed"
    );
    assert!(!recording.exists());
    assert_eq!(
        ask(&sandbox, "SELECT state FROM assets WHERE kind = 'voice_reference'"),
        "deleted"
    );
    assert!(answer(&generating).is_err());
    // And the application knows it has no engine, rather than finding out at
    // the next generation.
    assert!(handle.ping().is_err(), "an engine that never started answered");
}

/// A voice with nothing running is deleted outright, with no engine call to
/// stop anything and no waiting.
#[test]
fn deleting_an_idle_voice_finishes_at_once() {
    let (sandbox, recording) = seeded();
    let handle = engine(&sandbox, "", Duration::from_secs(30));
    handle.delete_voice("alice").expect("delete");
    assert_eq!(ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'"), "deleted");
    assert!(!recording.exists());
    // And the clip made with it is still there, saying what made it.
    assert_eq!(count(&sandbox, "SELECT count(*) FROM clips WHERE deleted_at IS NULL"), 1);
}

