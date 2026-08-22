//! Deleting a voice: what goes, what stays, and what happens when it is
//! interrupted.
//!
//! The point of the whole slice is the asymmetry. A voice is removed as
//! something usable — its recording, its conditioning, its availability — and
//! kept as something that happened, because clips made with it are the person's
//! own work and would be lying if they claimed anything else made them.

use std::path::PathBuf;

use yarngo_store::deletion::{Conditioning, Invalidation, Outcome};
use yarngo_store::import::{Legacy, LegacyClip, LegacyConsent, LegacyTake, LegacyVoice};
use yarngo_core::{DurableJobKind, Execution, Job};
use yarngo_store::takes::Produced;
use yarngo_store::{Store, VoiceProvenance};

/// An engine that answers however a test needs, and counts what was asked.
struct FakeEngine {
    answers: Vec<std::result::Result<Invalidation, String>>,
    terminate_fails: bool,
    restart_fails: bool,
    pub invalidated: usize,
    pub terminations: usize,
    pub restarts: usize,
}

impl FakeEngine {
    fn cleared() -> Self {
        Self {
            answers: vec![Ok(Invalidation::Cleared { entries_removed: 1 })],
            terminate_fails: false,
            restart_fails: false,
            invalidated: 0,
            terminations: 0,
            restarts: 0,
        }
    }

    fn answering(answers: Vec<std::result::Result<Invalidation, String>>) -> Self {
        Self {
            answers,
            terminate_fails: false,
            restart_fails: false,
            invalidated: 0,
            terminations: 0,
            restarts: 0,
        }
    }
}

impl Conditioning for FakeEngine {
    fn invalidate(&mut self) -> std::result::Result<Invalidation, String> {
        self.invalidated += 1;
        if self.answers.is_empty() {
            return Ok(Invalidation::AlreadyEmpty);
        }
        self.answers.remove(0)
    }

    fn terminate(&mut self) -> std::result::Result<(), String> {
        self.terminations += 1;
        if self.terminate_fails {
            return Err("the process will not die".into());
        }
        Ok(())
    }

    fn restart(&mut self) -> std::result::Result<(), String> {
        self.restarts += 1;
        if self.restart_fails {
            return Err("the engine will not start".into());
        }
        Ok(())
    }
}

/// A store holding two voices, a clip made with each, and a built-in clip —
/// with real files on the disk, because deleting them is half of what is being
/// tested.
fn seeded(dir: &tempfile::TempDir) -> (Store, PathBuf, PathBuf) {
    let alice_recording = dir.path().join("alice.wav");
    let bob_recording = dir.path().join("bob.wav");
    for path in [&alice_recording, &bob_recording] {
        std::fs::write(path, b"RIFF....WAVE").expect("write recording");
    }
    let alice_take = dir.path().join("alice-take.wav");
    let builtin_take = dir.path().join("builtin-take.wav");
    for path in [&alice_take, &builtin_take] {
        std::fs::write(path, b"RIFF....WAVE").expect("write take");
    }

    let mut voices = std::collections::BTreeMap::new();
    voices.insert(
        "alice".to_string(),
        LegacyVoice {
            label: "Alice".into(),
            reference_audio: alice_recording.to_string_lossy().into(),
            reference_text: Some("A sentence read at enrolment.".into()),
            seconds: Some(21.0),
            created: "2026-08-19T08:00:00".into(),
        },
    );
    voices.insert(
        "bob".to_string(),
        LegacyVoice {
            label: "Bob".into(),
            reference_audio: bob_recording.to_string_lossy().into(),
            reference_text: Some("A sentence read at enrolment.".into()),
            seconds: Some(20.0),
            created: "2026-08-19T09:00:00".into(),
        },
    );

    let legacy = Legacy {
        voices,
        consent: vec![LegacyConsent {
            voice_id: "alice".into(),
            statement: Some("I have permission to use this voice.".into()),
            app_version: Some("0.1.0".into()),
            source: Some("recording".into()),
            reference_sha256: None,
            granted_at: "2026-08-19T08:00:00".into(),
        }],
        clips: vec![
            LegacyClip {
                id: "clip-alice".into(),
                name: "Alice speaking".into(),
                text: "Made with a voice that gets deleted.".into(),
                voice_id: Some("alice".into()),
                model: Some("dots-tts-mf".into()),
                created: "2026-08-20T10:00:00".into(),
                takes: vec![LegacyTake {
                    id: "take-alice".into(),
                    path: alice_take.to_string_lossy().into(),
                    audio_s: Some(5.0),
                    gen_s: Some(9.0),
                    seed: Some(7),
                    created: "2026-08-20T10:00:00".into(),
                }],
            },
            LegacyClip {
                id: "clip-builtin".into(),
                name: "The model itself".into(),
                text: "Spoken by the model.".into(),
                voice_id: None,
                model: Some("dots-tts-mf".into()),
                created: "2026-08-20T11:00:00".into(),
                takes: vec![LegacyTake {
                    id: "take-builtin".into(),
                    path: builtin_take.to_string_lossy().into(),
                    audio_s: Some(3.0),
                    gen_s: Some(4.0),
                    seed: None,
                    created: "2026-08-20T11:00:00".into(),
                }],
            },
        ],
    };

    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).expect("open");
    store.import_legacy(&legacy).expect("import");
    (store, alice_recording, alice_take)
}

fn status(store: &Store, voice: &str) -> String {
    store
        .raw()
        .query_row(
            "SELECT status FROM voice_profiles WHERE id = ?1",
            [voice],
            |row| row.get(0),
        )
        .expect("status")
}

fn asset_state(store: &Store, id: &str) -> String {
    store
        .raw()
        .query_row("SELECT state FROM assets WHERE id = ?1", [id], |row| row.get(0))
        .expect("asset")
}

/// The whole shape of it, in one pass.
#[test]
fn the_voice_goes_and_its_clips_stay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, recording, take) = seeded(&dir);
    let mut engine = FakeEngine::cleared();

    assert!(store.voice_usable("alice").expect("usable"));
    assert!(store
        .begin_voice_deletion("alice", "job-1", "2026-08-21T12:00:00")
        .expect("begin"));

    // Refused for new work from the barrier, before anything has been removed.
    assert!(!store.voice_usable("alice").expect("usable"));
    assert!(recording.exists(), "the recording went before the barrier");

    let outcome = store
        .finish_voice_deletion("alice", "job-1", &mut engine, "2026-08-21T12:00:01")
        .expect("finish");
    assert_eq!(
        outcome,
        Outcome::Deleted {
            files_removed: 1,
            engine_terminated: false,
            engine_unavailable: false
        }
    );

    // Gone: the recording, and the conditioning derived from it.
    assert!(!recording.exists(), "the recording is still on the disk");
    assert_eq!(engine.invalidated, 1);
    assert_eq!(status(&store, "alice"), "deleted");
    assert_eq!(asset_state(&store, "voice_reference:alice"), "deleted");

    // Kept: the clip, its audio, and what it says it was made with.
    assert!(take.exists(), "a generated take was removed");
    assert_eq!(asset_state(&store, "generated_clip:take-alice"), "active");
    let (kind, revision): (String, Option<String>) = store
        .raw()
        .query_row(
            "SELECT voice_kind, voice_revision_id FROM clips WHERE id = 'clip-alice'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("clip");
    assert_eq!(kind, "custom", "the clip forgot where it came from");
    assert_eq!(revision.as_deref(), Some("alice/r1"));
    assert_eq!(
        store.clip_voice("clip-alice").expect("projection"),
        Some(VoiceProvenance::Deleted { label_at_generation: None })
    );

    // And the consent record is evidence, not a dependency of the voice.
    let consents: i64 = store
        .raw()
        .query_row("SELECT count(*) FROM consent_events", [], |row| row.get(0))
        .expect("consent");
    assert_eq!(consents, 1, "deleting the voice took its consent record");
}

/// A clip made with a deleted voice and one made with the model's own must not
/// end up saying the same thing.
#[test]
fn only_the_deleted_voices_clips_change_what_they_say() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _, _) = seeded(&dir);
    let mut engine = FakeEngine::cleared();

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");

    assert_eq!(
        store.clip_voice("clip-alice").expect("alice"),
        Some(VoiceProvenance::Deleted { label_at_generation: None })
    );
    assert_eq!(
        store.clip_voice("clip-builtin").expect("builtin"),
        Some(VoiceProvenance::BuiltIn),
        "a built-in clip was caught up in another voice's deletion"
    );
}

/// Nothing to clear is the ordinary case after a restart, and is a success.
#[test]
fn an_empty_conditioning_cache_is_not_a_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, recording, _) = seeded(&dir);
    let mut engine = FakeEngine::answering(vec![Ok(Invalidation::AlreadyEmpty)]);

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    let outcome = store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");
    assert_eq!(
        outcome,
        Outcome::Deleted {
            files_removed: 1,
            engine_terminated: false,
            engine_unavailable: false
        }
    );
    assert_eq!(engine.terminations, 0, "an empty cache killed the engine");
    assert!(!recording.exists());
    assert_eq!(status(&store, "alice"), "deleted");
}

/// The cache cannot evict one speaker, so deleting one voice clears every
/// voice's conditioning. The others stay perfectly valid records and are
/// reconditioned when next used — extra work, not lost data.
#[test]
fn clearing_every_voices_conditioning_leaves_the_other_voices_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _, _) = seeded(&dir);
    let mut engine = FakeEngine::answering(vec![Ok(Invalidation::Cleared { entries_removed: 2 })]);

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");

    assert_eq!(status(&store, "alice"), "deleted");
    assert_eq!(status(&store, "bob"), "active", "another voice was deleted too");
    assert!(store.voice_usable("bob").expect("usable"), "bob became unusable");
    assert_eq!(asset_state(&store, "voice_reference:bob"), "active");
}

/// An engine that cannot show the voice is unreachable is ended, because a
/// deletion that cannot be proved is not one.
#[test]
fn an_engine_that_cannot_forget_is_terminated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, recording, _) = seeded(&dir);
    let mut engine = FakeEngine::answering(vec![Ok(Invalidation::UnsupportedLayout)]);

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    let outcome = store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");
    assert_eq!(
        outcome,
        Outcome::Deleted {
            files_removed: 1,
            engine_terminated: true,
            engine_unavailable: false
        }
    );
    assert_eq!(engine.terminations, 1);
    assert!(!recording.exists());
    assert_eq!(status(&store, "alice"), "deleted");
}

/// Ending the old process is what makes the conditioning unreachable. Whether
/// anything takes its place is a question about having an engine, and answering
/// it badly must not turn into keeping a recording the person asked to delete.
#[test]
fn a_replacement_that_will_not_start_does_not_keep_the_recording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, recording, _) = seeded(&dir);
    let mut engine = FakeEngine::answering(vec![Ok(Invalidation::UnsupportedLayout)]);
    engine.restart_fails = true;

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    let outcome = store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");
    assert_eq!(
        outcome,
        Outcome::Deleted {
            files_removed: 1,
            engine_terminated: true,
            // Said out loud, so the application can report an engine that is
            // down rather than finding out at the next generation.
            engine_unavailable: true
        }
    );
    assert!(!recording.exists(), "a failed restart kept the recording");
    assert_eq!(status(&store, "alice"), "deleted");
}

/// But a process that will not die is different: nothing here can show it has
/// stopped being able to use the voice, so the deletion stops rather than
/// claiming to have removed something still in reach.
#[test]
fn a_deletion_that_cannot_be_proved_stops_at_the_barrier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, recording, _) = seeded(&dir);
    let mut engine = FakeEngine::answering(vec![Err("the engine is wedged".into())]);
    engine.terminate_fails = true;

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    let outcome = store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");
    assert!(matches!(outcome, Outcome::Blocked { .. }), "{outcome:?}");
    assert_eq!(engine.terminations, 1, "the engine was not asked to stop");

    assert_eq!(status(&store, "alice"), "deletion_pending");
    assert!(!store.voice_usable("alice").expect("usable"), "a blocked voice became usable");
    assert!(recording.exists(), "the recording went while the engine still held the voice");
}

/// A generation can finish after deletion became authoritative — cancellation
/// is cooperative, and the engine may reach the end before it notices. What it
/// cannot do is become a clip the person can play in a voice they deleted.
#[test]
fn work_that_finishes_after_the_barrier_is_not_committed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _, _) = seeded(&dir);

    // Before the barrier, publication is permitted and a take lands.
    assert!(store.publication_permitted("clip-alice").expect("permitted"));
    let early = dir.path().join("early.wav");
    std::fs::write(&early, b"RIFF").expect("write");
    assert!(
        publish(&mut store, "clip-alice", "exec-early", &early, "t0"),
        "an ordinary take was refused"
    );

    store.begin_voice_deletion("alice", "job-1", "t1").expect("begin");

    // The engine finishes anyway, and writes its output.
    assert!(!store.publication_permitted("clip-alice").expect("permitted"));
    let late = dir.path().join("late.wav");
    std::fs::write(&late, b"RIFF").expect("write");
    assert!(
        !publish(&mut store, "clip-alice", "exec-late", &late, "t2"),
        "a take was committed for a voice being deleted"
    );

    let takes: i64 = store
        .raw()
        .query_row(
            "SELECT count(*) FROM clip_takes WHERE clip_id = 'clip-alice'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(takes, 2, "the late take was recorded");
    // A clip made with the model's own voice is unaffected by any of this.
    assert!(store.publication_permitted("clip-builtin").expect("permitted"));
    assert!(publish(&mut store, "clip-builtin", "exec-builtin", &late, "t3"));
}

/// The whole publication, run for one attempt, so the barrier is exercised
/// where the row would actually be written rather than only where it is asked
/// about.
fn publish(
    store: &mut Store,
    clip_id: &str,
    execution_id: &str,
    path: &std::path::Path,
    at: &str,
) -> bool {
    let job_id = format!("job-{execution_id}");
    let mut job = Job::queued(&job_id, DurableJobKind::Synthesis);
    store.open_session(execution_id, "fake", at).expect("session");
    store.insert_job(&job, Some(clip_id), at).expect("insert");
    job.dispatch(execution_id);
    let mut execution = Execution::started(execution_id, &job_id, execution_id);
    store.save_progress(&job, Some(&execution), at).expect("dispatch");
    store
        .intend_output(execution_id, clip_id, &path.to_string_lossy())
        .expect("intend");
    job.execution_completed(&mut execution);
    store
        .record_output(&job, &execution, &Produced::default(), at)
        .expect("record");

    let output = store.output_of(execution_id).expect("output").expect("output");
    let published = job.take_published(&execution);
    assert!(matches!(published, yarngo_core::Applied::Moved { .. }), "{published:?}");
    store
        .publish_take(&output, &path.to_string_lossy(), &job, &execution, at)
        .expect("publish")
}

/// Crashing part-way leaves the deletion findable and finishable. From the
/// records, because a crash takes everything else.
#[test]
fn a_deletion_interrupted_part_way_is_resumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, recording, take) = seeded(&dir);
    let path = dir.path().join("store.db");

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    // Everything stops here: the barrier is up, the recording is still there.
    drop(store);
    assert!(recording.exists());

    let mut store = Store::open(&path).expect("reopen");
    let unfinished = store.unfinished_voice_deletions().expect("unfinished");
    assert_eq!(unfinished, [("alice".to_string(), "job-1".to_string())]);
    // And the voice is still refused, having never come back as usable.
    assert!(!store.voice_usable("alice").expect("usable"));

    let mut engine = FakeEngine::answering(vec![Ok(Invalidation::AlreadyEmpty)]);
    for (voice, job) in unfinished {
        store
            .finish_voice_deletion(&voice, &job, &mut engine, "t1")
            .expect("resume");
    }

    assert_eq!(status(&store, "alice"), "deleted");
    assert!(!recording.exists());
    assert!(take.exists(), "resuming took a generated take with it");
    assert!(store.unfinished_voice_deletions().expect("after").is_empty());
}

/// Deleting twice answers against the tombstone rather than failing, and
/// without writing a second record of one deletion.
#[test]
fn deleting_an_already_deleted_voice_is_a_quiet_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _, _) = seeded(&dir);
    let mut engine = FakeEngine::cleared();

    store.begin_voice_deletion("alice", "job-1", "t0").expect("begin");
    store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t1")
        .expect("finish");

    // Asking again starts nothing.
    assert!(
        !store.begin_voice_deletion("alice", "job-2", "t2").expect("second"),
        "a second deletion job was created"
    );
    let outcome = store
        .finish_voice_deletion("alice", "job-1", &mut engine, "t3")
        .expect("second finish");
    assert_eq!(outcome, Outcome::AlreadyDeleted);
    assert_eq!(engine.invalidated, 1, "the engine was asked again for nothing");

    let jobs: i64 = store
        .raw()
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'voice_delete' AND target_id = 'alice'",
            [],
            |row| row.get(0),
        )
        .expect("jobs");
    assert_eq!(jobs, 1, "one deletion left two records of itself");
}

/// Asking twice at once starts one deletion, not two.
#[test]
fn beginning_a_deletion_twice_creates_one_job() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _, _) = seeded(&dir);

    assert!(store.begin_voice_deletion("alice", "job-1", "t0").expect("first"));
    assert!(!store.begin_voice_deletion("alice", "job-2", "t1").expect("second"));
    let jobs: i64 = store
        .raw()
        .query_row("SELECT count(*) FROM jobs WHERE kind = 'voice_delete'", [], |row| {
            row.get(0)
        })
        .expect("jobs");
    assert_eq!(jobs, 1);
}
