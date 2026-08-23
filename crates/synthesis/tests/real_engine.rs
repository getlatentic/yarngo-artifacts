//! The durable path with a real engine and a real model behind it.
//!
//! `durable.rs` covers what can go wrong, using a stand-in that can be made to
//! fail on request. This covers what has to go right, with the thing that will
//! actually run: MLX loading a 3.4 GB model, conditioning someone's voice, and
//! writing a wave file where Rust told it to — and nowhere else.
//!
//! Skipped unless asked for:
//!
//!     YARNGO_TEST_ENGINE=1 cargo test -p yarngo-synthesis --test real_engine

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use speech_engine::protocol::{Connection, Events};
use yarngo_core::{JobStatus, Rejection};
use yarngo_store::import::Legacy;
use yarngo_store::Store;
use yarngo_testing::Sandbox;
use yarngo_synthesis::{Layout, Outcome, Reference, Request, Synthesis};

/// Loading the model and conditioning a voice is a minute or two on a quiet
/// machine and longer under load.
const PATIENCE: Duration = Duration::from_secs(600);

fn wanted() -> bool {
    std::env::var_os("YARNGO_TEST_ENGINE").is_some()
}

/// One copy of the installed store per test binary, shared because nothing
/// here is supposed to change it — and made a copy because the one time this
/// pointed at the real directory, a generation landed in it.
fn store() -> Option<&'static Sandbox> {
    static STORE: OnceLock<Option<Sandbox>> = OnceLock::new();
    STORE
        .get_or_init(|| Sandbox::copying(&speech_engine::paths::installed_data_dir()))
        .as_ref()
}

fn data_dir() -> PathBuf {
    store().map(|s| s.root().to_path_buf()).unwrap_or_default()
}

fn python() -> PathBuf {
    std::env::var_os("YARNGO_PYTHON").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from("/Users/dev/workspace/voice-clone-bench/mlx-speech/.venv/bin/python")
    })
}

/// The real engine, laid out and described the way an install leaves it — the
/// interpreter inside the environment, the engine inside the runtime, and a
/// descriptor that has to satisfy the same rules any downloaded one does.
fn mlx_runtime(data: &Path) -> speech_engine::runtimes::Descriptor {
    let home = data.join("runtimes/mlx/test");
    std::fs::create_dir_all(&home).expect("runtime directory");
    std::fs::copy(repo().join("sidecar/engine.py"), home.join("engine.py")).expect("engine");
    std::fs::copy(repo().join("sidecar/protocol.py"), home.join("protocol.py")).expect("protocol");
    let bin = home.join(".venv/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let linked = bin.join("python3");
    let _ = std::fs::remove_file(&linked);
    std::os::unix::fs::symlink(python(), &linked).expect("interpreter");
    std::fs::write(
        home.join("runtime.json"),
        serde_json::json!({
            "schema": 1, "id": "mlx", "name": "Apple silicon", "engine": "own",
            "program": "{venv}/bin/python3", "arguments": ["{engine}"],
        })
        .to_string(),
    )
    .expect("descriptor");

    let places = speech_engine::runtimes::Places {
        data: data.to_path_buf(),
        resources: repo().join("packaging"),
    };
    speech_engine::runtimes::Descriptor::read(&home.join("runtime.json"), &places)
        .filter(speech_engine::runtimes::Descriptor::available)
        .expect("the mlx runtime was not usable")
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn engine() -> (Connection, Events) {
    let mut command = Command::new(python());
    command
        .arg(repo().join("sidecar/engine.py"))
        .current_dir(repo())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    store().expect("a store to copy").apply(&mut command);
    let mut child = command.spawn().expect("spawn the engine");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take();
    let (connection, events) = Connection::attach(child, stdin, stdout, stderr);
    connection.initialize(PATIENCE).expect("initialize");
    (connection, events)
}

/// A copy of this machine's own store, in a temporary database, so a test can
/// generate against a real enrolled voice without touching anything.
fn shadowed(dir: &Path) -> Option<(Store, Legacy)> {
    let legacy = Legacy::read(store()?.root())?;
    if legacy.voices.is_empty() || legacy.clips.is_empty() {
        return None;
    }
    let mut store = Store::open(&dir.join("shadow.db")).expect("open");
    store.import_legacy(&legacy).expect("import");
    Some((store, legacy))
}

/// A clip in the shadow store made with a voice that is still enrolled, and the
/// recording behind it.
fn a_custom_clip(legacy: &Legacy) -> Option<(String, Reference)> {
    let clip = legacy
        .clips
        .iter()
        .find(|c| c.voice_id.as_ref().is_some_and(|v| legacy.voices.contains_key(v)))?;
    let voice = legacy.voices.get(clip.voice_id.as_ref()?)?;
    Some((
        clip.id.clone(),
        Reference { audio: voice.reference_audio.clone(), text: None },
    ))
}

fn asked_for(clip_id: &str, reference: Reference) -> Request {
    spoken(clip_id, reference, "Hello from the durable path.")
}

fn spoken(clip_id: &str, reference: Reference, text: &str) -> Request {
    Request {
        clip_id: clip_id.into(),
        text: text.into(),
        reference: Some(reference),
        model: None,
        seed: Some(4242),
    }
}

/// Long enough that the engine splits it and reports finishing the first part
/// while it is still working on the second. That report is what a test can wait
/// for when it needs to do something while inference is genuinely running — a
/// sleep would be racing a model whose speed depends on whether it is warm.
fn two_chunks() -> String {
    let sentence = "The quick brown fox jumps over the lazy dog while the sun sets slowly \
                    behind the distant blue hills and everyone watches in silence. ";
    sentence.repeat(12)
}

/// Everything the milestone is: the person's intent recorded first, real
/// inference, a file where Rust said, and a take in SQLite.
#[test]
fn a_real_generation_becomes_a_take_in_the_database() {
    if !wanted() {
        eprintln!("set YARNGO_TEST_ENGINE=1 to run against the real engine");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let Some((mut store, legacy)) = shadowed(dir.path()) else {
        eprintln!("this machine has no enrolled voice with a clip");
        return;
    };
    let Some((clip_id, reference)) = a_custom_clip(&legacy) else { return };
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "mlx", "t1", None).expect("session");
    let (connection, _events) = engine();

    let request = asked_for(&clip_id, reference);
    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &request, "t1").expect("begin");
    let staged = pending.staged.clone();
    let outcome = synthesis.generate(pending, &connection, PATIENCE, "t2").expect("generate");

    let Outcome::Published { take_id, path } = outcome else { panic!("{outcome:?}") };
    assert!(path.exists(), "the take's audio is not where the take says it is");
    assert!(!staged.exists(), "the staged file was left behind");
    assert_eq!(
        store.load_job("job-1").expect("load").expect("job").state(),
        JobStatus::Completed
    );

    // The take says what the engine actually produced, read back from SQLite.
    let (audio_s, seed): (Option<f64>, Option<i64>) = store
        .raw()
        .query_row(
            "SELECT audio_seconds, seed FROM clip_takes WHERE id = ?1",
            [&take_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("take");
    assert!(audio_s.unwrap_or(0.0) > 0.5, "the take claims {audio_s:?} seconds of audio");
    assert_eq!(seed, Some(4242), "the seed that made it was not recorded");

    // And the file holds roughly what the take claims.
    let written = std::fs::metadata(&path).expect("audio").len();
    assert!(written > 40_000, "{written} bytes is not a second of speech");
}

/// The claim the whole cutover rests on: generating through this path leaves
/// the application's own store exactly as it was, byte for byte.
#[test]
fn a_real_generation_does_not_touch_the_legacy_store() {
    if !wanted() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let Some((mut store, legacy)) = shadowed(dir.path()) else { return };
    let Some((clip_id, reference)) = a_custom_clip(&legacy) else { return };
    let layout = Layout::under(dir.path());
    store.open_session("session-1", "mlx", "t1", None).expect("session");

    let watched = [
        data_dir().join("clips/clips.json"),
        data_dir().join("voices/voices.json"),
        data_dir().join("consent.log"),
    ];
    let before: Vec<Option<Vec<u8>>> = watched.iter().map(|p| std::fs::read(p).ok()).collect();
    // Every audio file the store already has, so a generation cannot quietly
    // overwrite one.
    let audio_before: Vec<(PathBuf, Option<u64>)> = legacy
        .clips
        .iter()
        .flat_map(|c| c.takes.iter().map(|t| PathBuf::from(&t.path)))
        .chain(legacy.voices.values().map(|v| PathBuf::from(&v.reference_audio)))
        .map(|p| {
            let size = std::fs::metadata(&p).ok().map(|m| m.len());
            (p, size)
        })
        .collect();

    let (connection, _events) = engine();
    let request = asked_for(&clip_id, reference);
    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &request, "t1").expect("begin");
    let outcome = synthesis.generate(pending, &connection, PATIENCE, "t2").expect("generate");
    assert!(matches!(outcome, Outcome::Published { .. }), "{outcome:?}");

    for (path, was) in watched.iter().zip(before) {
        assert_eq!(std::fs::read(path).ok(), was, "the engine wrote to {path:?}");
    }
    for (path, size) in audio_before {
        assert_eq!(
            std::fs::metadata(&path).ok().map(|m| m.len()),
            size,
            "an existing recording changed: {path:?}"
        );
    }
}

/// The race, against the thing that actually takes forty seconds to condition a
/// voice. The stand-in version proves the ordering; this proves it holds when
/// the window is real.
#[test]
fn a_voice_deleted_during_real_inference_refuses_the_take() {
    if !wanted() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let Some((mut store, legacy)) = shadowed(dir.path()) else { return };
    let Some((clip_id, reference)) = a_custom_clip(&legacy) else { return };
    let voice_id = legacy
        .clips
        .iter()
        .find(|c| c.id == clip_id)
        .and_then(|c| c.voice_id.clone())
        .expect("voice");
    let layout = Layout::under(dir.path());
    let db = dir.path().join("shadow.db");
    store.open_session("session-1", "mlx", "t1", None).expect("session");
    let (connection, events) = engine();

    // From another connection, the moment the engine says it has finished part
    // of the work and is still going. Not a sleep: how long inference takes
    // depends on whether the model is already resident, and a test that raced
    // that would pass on a cold machine and prove nothing on a warm one.
    let deleting = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + PATIENCE;
        while std::time::Instant::now() < deadline {
            match events.recv_timeout(Duration::from_secs(30)) {
                Ok(event) if event.method == "job.progress" => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let mut store = Store::open(&db).expect("second connection");
        store.begin_voice_deletion(&voice_id, "job-delete", "t2").expect("deletion");
    });

    let request = spoken(&clip_id, reference, &two_chunks());
    let mut synthesis = Synthesis { store: &mut store, layout: &layout, session_id: "session-1" };
    let pending = synthesis.begin("job-1", "exec-1", &request, "t1").expect("begin");
    let staged = pending.staged.clone();
    let outcome = synthesis.generate(pending, &connection, PATIENCE, "t3").expect("generate");
    deleting.join().expect("deletion");

    assert!(
        matches!(outcome, Outcome::Rejected { reason: Rejection::VoiceDeleted, .. }),
        "{outcome:?}"
    );
    // The engine finished. The job did not, and the audio is gone.
    assert_eq!(store.execution_state("exec-1").expect("state").as_deref(), Some("completed"));
    assert_eq!(
        store.load_job("job-1").expect("load").expect("job").state(),
        JobStatus::Cancelled
    );
    assert!(!staged.exists(), "audio made from a deleted voice was left on the disk");
    let takes: i64 = store
        .raw()
        .query_row("SELECT count(*) FROM clip_takes WHERE id = 'take-exec-1'", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(takes, 0, "a take was committed for a voice being deleted");
}

/// The application's own engine, opened the way the application opens it.
///
/// Everything except inference: adopting the existing store, listing what the
/// person has from the database, and asking the sidecar only about models and
/// the machine. The database is temporary; the store it reads is the real one
/// and is only read.
#[test]
fn the_durable_engine_answers_for_the_library_and_the_machine() {
    if !wanted() {
        return;
    }
    use speech_engine::SpeechEngine;
    use yarngo_synthesis::engine::{DurableEngine, Spawn};

    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = DurableEngine::open(
        &dir.path().join("app.db"),
        &data_dir(),
        Spawn {
            runtime: mlx_runtime(&data_dir()),
            data_dir: data_dir(),
            version: Some("test".into()),
        },
    )
    .expect("open");

    // From the database, which had nothing in it until it adopted the store.
    let clips = engine.clips().expect("clips");
    let voices = engine.voices().expect("voices");
    assert!(!clips.is_empty(), "no clips came across");
    assert!(!voices.is_empty(), "no voices came across");
    assert!(
        clips.iter().all(|c| !c.takes.is_empty()),
        "a clip with no audio is listed"
    );
    assert!(
        clips.iter().flat_map(|c| &c.takes).all(|t| t.path.exists()),
        "a take points at audio that is not there"
    );
    assert!(
        voices.iter().all(|v| v.reference_audio.exists() && !v.label.is_empty()),
        "a voice has no recording or no name"
    );
    assert!(
        voices.iter().any(|v| v.seconds > 0.0),
        "no voice knows how long its recording is"
    );

    // From the sidecar, which is asked only what it is for.
    let models = engine.models(false).expect("models");
    assert!(!models.is_empty());
    let machine = engine.system_info().expect("system info");
    assert!(machine.memory_bytes > 0 && !machine.chip.is_empty());
    let disk = engine.disk_space().expect("disk");
    assert!(disk.total_bytes > disk.free_bytes);
}

/// The engine thread submits work and does not wait for it.
///
/// The connection underneath has always been able to answer a ping while a
/// synthesis runs — it carries request ids and Python answers cancellation on
/// its reader. What this proves is that the layer above no longer throws that
/// away by sitting on the reply: everything here goes through the one
/// `EngineHandle` the application uses, while a real model is speaking.
#[test]
fn the_engine_answers_through_the_handle_while_it_is_generating() {
    if !wanted() {
        eprintln!("set YARNGO_TEST_ENGINE=1 to run against the real engine");
        return;
    }
    use speech_engine::{EngineHandle, SynthesisRequest};
    use std::sync::Arc;
    use yarngo_synthesis::engine::{DurableEngine, Spawn};

    let dir = tempfile::tempdir().expect("tempdir");
    let Some((_, legacy)) = shadowed(dir.path()) else { return };
    let Some((clip_id, _)) = a_custom_clip(&legacy) else { return };
    let database = dir.path().join("app.db");
    let data = data_dir();
    let spawn = Spawn {
            runtime: mlx_runtime(&data),
            data_dir: data.clone(),
            version: Some("test".into()),
        };
    let handle = Arc::new(
        EngineHandle::spawn_backend(move || {
            Ok(Box::new(DurableEngine::open(&database, &data, spawn)?))
        })
        .expect("engine"),
    );

    // Long enough to be split, so it is still going when we interrupt it.
    let generating = {
        let handle = handle.clone();
        let clip_id = clip_id.clone();
        std::thread::spawn(move || {
            handle.synthesize(SynthesisRequest {
                text: two_chunks(),
                output: PathBuf::new(),
                model: None,
                clip_id: Some(clip_id),
                voice_id: None,
                seed: Some(4242),
                name: None,
            })
        })
    };

    // First, that anything is said at all while a chunk is still running. A
    // chunk is one blocking call into the model, so an engine that only spoke
    // at chunk boundaries would leave a short clip silent for its whole length
    // — which is exactly what the interface then shows.
    let deadline = Instant::now() + PATIENCE;
    let mut early = None;
    while Instant::now() < deadline && early.is_none() {
        early = handle.progress();
        std::thread::sleep(Duration::from_millis(100));
    }
    let early = early.expect("nothing was reported while the first chunk ran");
    assert_eq!(
        early.chunks_done, 0,
        "the first thing reported was a finished chunk, so nothing was said during it"
    );

    // Then real progress: the engine saying it has finished part of the work.
    // Not a sleep — a warm model would beat any sleep worth writing.
    let deadline = Instant::now() + PATIENCE;
    let mut seen = None;
    while Instant::now() < deadline && seen.is_none() {
        seen = handle.progress().filter(|p| p.chunks_done >= 1);
        std::thread::sleep(Duration::from_millis(200));
    }
    let seen = seen.expect("the engine never reported finishing a chunk");
    assert!(seen.chunks >= 2, "the text was not split, so nothing was still running");
    assert!(!generating.is_finished(), "the synthesis finished before we could interrupt it");

    // Through the same handle, while that is still outstanding.
    let at = Instant::now();
    handle.ping().expect("ping");
    let pinged = at.elapsed();

    let at = Instant::now();
    handle.cancel_generation().expect("cancel");
    let cancelled = at.elapsed();

    assert!(
        !generating.is_finished(),
        "both answers arrived only because the synthesis had already ended"
    );
    assert!(
        pinged < Duration::from_secs(5) && cancelled < Duration::from_secs(5),
        "ping took {pinged:?} and cancel took {cancelled:?} while generating"
    );

    // And the cancellation was not merely acknowledged: it stopped the work.
    let outcome = generating.join().expect("the synthesis thread");
    assert!(outcome.is_err(), "a cancelled generation produced a take: {outcome:?}");
}

/// B. The person deletes the voice while a real model is speaking in it.
///
/// Its own copy of the store, not the one the other tests share: this one
/// removes a recording, and a test that took somebody else's fixtures with it
/// would be the same mistake as taking the real store's.
#[test]
fn deleting_a_voice_during_real_inference_stops_it_and_removes_the_recording() {
    if !wanted() {
        eprintln!("set YARNGO_TEST_ENGINE=1 to run against the real engine");
        return;
    }
    use speech_engine::{EngineHandle, SynthesisRequest};
    use std::sync::Arc;
    use yarngo_synthesis::engine::{DurableEngine, Spawn};

    let Some(sandbox) = Sandbox::copying(&speech_engine::paths::installed_data_dir()) else {
        eprintln!("this machine has not run the application");
        return;
    };
    let Some(legacy) = Legacy::read(sandbox.root()) else { return };
    let Some((clip_id, reference)) = a_custom_clip(&legacy) else { return };
    let voice_id = legacy
        .clips
        .iter()
        .find(|c| c.id == clip_id)
        .and_then(|c| c.voice_id.clone())
        .expect("the clip's voice");
    let recording = PathBuf::from(&reference.audio);
    assert!(recording.exists(), "the copied recording is not there");

    let database = sandbox.database();
    let data = sandbox.root().to_path_buf();
    {
        let mut store = Store::open(&database).expect("open");
        store.import_legacy(&legacy).expect("import");
    }
    let takes_before = takes_in(&database);
    assert!(takes_before > 0, "no existing takes, so nothing to prove survives");

    let spawn = Spawn {
            runtime: mlx_runtime(&data),
            data_dir: data.clone(),
            version: Some("test".into()),
        };
    let opened = database.clone();
    let handle = Arc::new(
        EngineHandle::spawn_backend(move || {
            Ok(Box::new(DurableEngine::open(&opened, &data, spawn)?))
        })
        .expect("engine"),
    );

    let (answered, answer) = std::sync::mpsc::channel();
    {
        let handle = handle.clone();
        let clip_id = clip_id.clone();
        std::thread::spawn(move || {
            let _ = answered.send(handle.synthesize(SynthesisRequest {
                text: two_chunks(),
                output: PathBuf::new(),
                model: None,
                clip_id: Some(clip_id),
                voice_id: None,
                seed: Some(4242),
                name: None,
            }));
        });
    }

    // The engine reporting it has finished part of the work, so the deletion
    // lands during inference on a warm machine and a cold one alike.
    let deadline = Instant::now() + PATIENCE;
    let mut running = false;
    while Instant::now() < deadline && !running {
        running = handle.progress().is_some_and(|p| p.chunks_done >= 1);
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(running, "the engine never reported finishing a chunk");

    let at = Instant::now();
    handle.delete_voice(&voice_id).expect("delete");
    let refused = at.elapsed();
    assert!(
        refused < Duration::from_secs(10),
        "the deletion waited {refused:?} for the generation it was cancelling"
    );
    assert_eq!(
        voice_status(&database, &voice_id),
        "deletion_pending",
        "the voice was not refused the moment the deletion returned"
    );

    // The cancellation reached the engine: the generation ended rather than
    // running to completion.
    let outcome = answer
        .recv_timeout(PATIENCE)
        .expect("the generation never ended");
    assert!(outcome.is_err(), "a take was published for a deleted voice: {outcome:?}");

    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && voice_status(&database, &voice_id) != "deleted" {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(voice_status(&database, &voice_id), "deleted", "the deletion never finished");

    // Which way it stopped depends on where in a chunk the model was when the
    // deletion landed, and both are correct: it stopped when asked, or it was
    // ended for not stopping. Said out loud rather than assumed, because a test
    // that claimed cooperative cancellation while forcing every time would be
    // reporting something it never exercised.
    let attempt = ended_attempt(&database);
    assert!(
        attempt == "cancelled" || attempt == "interrupted",
        "the attempt ended as {attempt:?}, which is neither stopping nor being stopped"
    );
    eprintln!("the generation {}", match attempt.as_str() {
        "cancelled" => "stopped when it was asked to",
        _ => "did not stop and the engine was ended",
    });
    assert!(!recording.exists(), "the recording is still on the disk");
    assert_eq!(
        takes_in(&database),
        takes_before,
        "deleting a voice took the clips already made with it"
    );
    assert!(
        std::fs::read_dir(sandbox.root().join("staging"))
            .map(|entries| entries.flatten().count() == 0)
            .unwrap_or(true),
        "audio made from the deleted voice was left staged"
    );

    // The installed store is untouched, which is the point of the copy.
    assert!(
        speech_engine::paths::installed_data_dir()
            .join("voices/voices.json")
            .exists(),
        "the real store was disturbed"
    );
}

fn voice_status(database: &Path, voice_id: &str) -> String {
    let store = Store::open(database).expect("read");
    store
        .raw()
        .query_row(
            "SELECT status FROM voice_profiles WHERE id = ?1",
            [voice_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "<none>".into())
}

fn takes_in(database: &Path) -> i64 {
    let store = Store::open(database).expect("read");
    store
        .raw()
        .query_row("SELECT count(*) FROM clip_takes", [], |row| row.get(0))
        .expect("count")
}

fn ended_attempt(database: &Path) -> String {
    let store = Store::open(database).expect("read");
    store
        .raw()
        .query_row(
            "SELECT e.state FROM job_executions e
               JOIN jobs j ON j.id = e.job_id
              WHERE j.kind = 'synthesis' ORDER BY e.started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "<none>".into())
}
