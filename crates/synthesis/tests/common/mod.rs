//! What the deletion and lifecycle tests both need: a store with a voice in it,
//! and the application's engine over a stand-in that can be made to misbehave.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use speech_engine::{EngineError, EngineHandle, Synthesis, SynthesisRequest};
use yarngo_store::import::{Legacy, LegacyClip, LegacyConsent, LegacyVoice};
use yarngo_store::Store;
use yarngo_synthesis::engine::{DurableEngine, Spawn};
use yarngo_testing::{standin, Sandbox};

pub const PATIENCE: Duration = Duration::from_secs(30);

/// A sandbox holding one voice, one clip made with it, and the recording.
pub fn seeded() -> (Sandbox, PathBuf) {
    let sandbox = Sandbox::empty();
    let voices = sandbox.root().join("voices");
    std::fs::create_dir_all(&voices).expect("voices");
    let recording = voices.join("alice.wav");
    std::fs::write(&recording, vec![0u8; 4096]).expect("recording");

    // The JSON store the application would have found, written as well as
    // imported: a sandbox that has a database but no store it came from cannot
    // exercise anything that reads back from one.
    std::fs::write(
        voices.join("voices.json"),
        format!(
            r#"{{"alice":{{"label":"Alice","reference_audio":"{}","reference_text":"A sentence Alice read.","seconds":12.0,"created":"t0"}}}}"#,
            recording.to_string_lossy()
        ),
    )
    .expect("voices.json");
    let clips_dir = sandbox.root().join("clips");
    std::fs::create_dir_all(&clips_dir).expect("clips");
    std::fs::write(clips_dir.join("clips.json"), b"[]").expect("clips.json");

    let mut store = Store::open(&sandbox.database()).expect("open");
    store
        .import_legacy(&Legacy {
            voices: [(
                "alice".to_string(),
                LegacyVoice {
                    label: "Alice".into(),
                    reference_audio: recording.to_string_lossy().into(),
                    reference_text: Some("A sentence Alice read.".into()),
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
pub fn engine(sandbox: &Sandbox, behaviour: &str, grace: Duration) -> Arc<EngineHandle> {
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

pub fn asked_for() -> SynthesisRequest {
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

pub fn ask(sandbox: &Sandbox, query: &str) -> String {
    let store = Store::open(&sandbox.database()).expect("read");
    store
        .raw()
        .query_row(query, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|_| "<none>".into())
}

pub fn count(sandbox: &Sandbox, query: &str) -> i64 {
    let store = Store::open(&sandbox.database()).expect("read");
    store.raw().query_row(query, [], |row| row.get(0)).expect("count")
}

/// Generate on another thread and collect the answer with a deadline.
///
/// Not a join: a stand-in told to sleep for ten minutes will do exactly that,
/// so waiting for the thread would turn "the cancellation never landed" into a
/// test that hangs instead of one that fails.
pub fn generating(handle: &Arc<EngineHandle>) -> Receiver<Result<Synthesis, EngineError>> {
    let (answered, answer) = std::sync::mpsc::channel();
    let handle = handle.clone();
    std::thread::spawn(move || {
        let _ = answered.send(handle.synthesize(asked_for()));
    });
    answer
}

pub fn answer(from: &Receiver<Result<Synthesis, EngineError>>) -> Result<Synthesis, EngineError> {
    from.recv_timeout(PATIENCE)
        .expect("the generation never ended")
}

pub fn until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let stop = Instant::now() + deadline;
    while Instant::now() < stop {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

pub fn staged(sandbox: &Sandbox) -> Vec<PathBuf> {
    std::fs::read_dir(sandbox.root().join("staging"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

