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
use std::time::Duration;

use speech_engine::protocol::{Connection, Events};
use yarngo_core::{JobStatus, Rejection};
use yarngo_store::import::Legacy;
use yarngo_store::Store;
use yarngo_synthesis::{Layout, Outcome, Reference, Request, Synthesis};

/// Loading the model and conditioning a voice is a minute or two on a quiet
/// machine and longer under load.
const PATIENCE: Duration = Duration::from_secs(600);

fn wanted() -> bool {
    std::env::var_os("YARNGO_TEST_ENGINE").is_some()
}

fn data_dir() -> PathBuf {
    PathBuf::from("/Users/dev/Library/Application Support/Yarngo Studio")
}

fn python() -> PathBuf {
    std::env::var_os("YARNGO_PYTHON").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from("/Users/dev/workspace/voice-clone-bench/mlx-speech/.venv/bin/python")
    })
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn engine() -> (Connection, Events) {
    let mut child = Command::new(python())
        .arg(repo().join("sidecar/engine.py"))
        .arg("--protocol")
        .arg("jsonrpc")
        .current_dir(repo())
        .env("YARNGO_DATA", data_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the engine");
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
    let legacy = Legacy::read(&data_dir())?;
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
    store.open_session("session-1", "mlx", "t1").expect("session");
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
    store.open_session("session-1", "mlx", "t1").expect("session");

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
    store.open_session("session-1", "mlx", "t1").expect("session");
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
