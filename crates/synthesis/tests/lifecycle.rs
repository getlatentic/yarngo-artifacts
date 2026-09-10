//! Opening the application more than once.
//!
//! Everything durable is named, and a name that repeats between runs is a run
//! that cannot start. Covered here because it is invisible from a single run:
//! the first one works, and every test that builds its own database is a first
//! run.

mod common;

use yarngo_testing::standin;

use std::path::PathBuf;
use std::time::Duration;

use common::{answer, ask, asked_for, count, engine, generating, seeded, until, PATIENCE};

/// The second start is the one that finds a database with the first start's
/// names already in it.
#[test]
fn the_application_can_be_opened_again() {
    let (sandbox, _) = seeded();
    let grace = Duration::from_secs(30);

    let first = engine(&sandbox, "", grace);
    first.synthesize(asked_for()).expect("the first run generates");
    assert_eq!(count(&sandbox, "SELECT count(*) FROM clip_takes"), 1);
    drop(first);

    let second = engine(&sandbox, "", grace);
    second.synthesize(asked_for()).expect("the second run generates");
    assert_eq!(
        count(&sandbox, "SELECT count(*) FROM clip_takes"),
        2,
        "the second run did not produce a take of its own"
    );
    assert_eq!(
        count(&sandbox, "SELECT count(*) FROM engine_sessions"),
        2,
        "two runs did not leave two sessions"
    );
    assert_eq!(
        count(&sandbox, "SELECT count(*) FROM jobs WHERE state = 'completed'"),
        2
    );

    // And the first run's session is recorded as over, so nothing is left
    // looking like it is still running.
    assert_eq!(
        count(&sandbox, "SELECT count(*) FROM engine_sessions WHERE ended_at IS NULL"),
        1,
        "an earlier run's session is still open"
    );
    drop(second);
}

/// A store the application has already adopted is not adopted again.
#[test]
fn the_existing_store_is_brought_across_once() {
    let (sandbox, _) = seeded();
    let grace = Duration::from_secs(30);
    let first = engine(&sandbox, "", grace);
    let clips = count(&sandbox, "SELECT count(*) FROM clips");
    drop(first);

    let second = engine(&sandbox, "", grace);
    assert_eq!(count(&sandbox, "SELECT count(*) FROM clips"), clips);
    assert_eq!(ask(&sandbox, "SELECT status FROM voice_profiles WHERE id = 'alice'"), "active");
    drop(second);
}

/// Progress describes a generation that is running. When none is, there is
/// nothing to describe.
///
/// Held as "the latest thing the engine said", which is right while it is
/// speaking and wrong the moment it stops: the interface polls this to draw a
/// bar, and a value left behind draws a bar for work that finished.
#[test]
fn progress_stops_being_reported_when_the_generation_ends() {
    let (sandbox, _) = seeded();
    let handle = engine(&sandbox, "    time.sleep(1.0)", Duration::from_secs(30));

    assert!(handle.progress().is_none(), "something was reported before anything ran");

    let generating = generating(&handle);
    // Reported while it runs, or the other half of this proves nothing: a
    // report that never arrives is also a report that is never left behind.
    assert!(
        until(PATIENCE, || handle.progress().is_some()),
        "nothing was reported while the generation was running"
    );
    let running = handle.progress().expect("a report");
    assert!(running.chunks >= 1, "the report says nothing about the work: {running:?}");

    answer(&generating).expect("generate");
    assert!(
        handle.progress().is_none(),
        "the finished generation is still being reported as running: {:?}",
        handle.progress()
    );
}

/// What the reference was recorded saying travels with it.
///
/// The model conditions better when it is told what the reference says, and the
/// enrolment script is that transcript exactly. It was being written by the
/// application, dropped on the way into the database, and never sent — which
/// nothing would have noticed, because generation still works without it and
/// only sounds a little less like the person.
#[test]
fn the_reference_transcript_reaches_the_engine() {
    let (sandbox, recording) = seeded();
    let handle = engine(&sandbox, "", Duration::from_secs(30));

    let script = "My name is spoken here, and this is how I sound when I speak.";
    handle
        .register_voice(speech_engine::Voice {
            voice_id: "bea".into(),
            label: "Bea".into(),
            reference_audio: recording.clone(),
            reference_text: script.into(),
            seconds: 0.0,
            consent: speech_engine::Consent {
                statement: "I agree".into(),
                app_version: "test".into(),
                source: "recording".into(),
            },
        snr_db: None,
        sample_rate_hz: None,
    })
        .expect("register");

    // Read back from the database, not from what was handed in.
    let enrolled = handle
        .voices()
        .expect("voices")
        .into_iter()
        .find(|v| v.voice_id == "bea")
        .expect("the voice was not saved");
    assert_eq!(enrolled.reference_text, script, "the transcript was dropped on the way in");
    assert!(
        enrolled.seconds > 0.0,
        "the recording was stored without a measured length"
    );
    assert!(
        enrolled.reference_audio.starts_with(sandbox.root()),
        "the voice points outside its own store: {:?}",
        enrolled.reference_audio
    );

    handle.prepare_voice("bea", None).expect("prepare");
    let conditioning: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(standin::beside(sandbox.root(), "stand-in", "conditioning.asked.json"))
            .expect("the engine was never asked to condition"),
    )
    .expect("json");
    assert_eq!(
        conditioning["reference_text"], script,
        "conditioning was asked for without the transcript"
    );

    // And again where it matters most: the generation itself.
    let generated = handle
        .synthesize(speech_engine::SynthesisRequest {
            text: "Hello there.".into(),
            output: PathBuf::new(),
            model: Some("dots-tts-mf".into()),
            clip_id: None,
            voice_id: Some("bea".into()),
            seed: Some(2000),
            name: None,
        })
        .expect("generate");
    let asked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(standin::beside(sandbox.root(), "stand-in", "generate.asked.json"))
            .expect("the engine recorded nothing"),
    )
    .expect("json");
    assert_eq!(asked["reference_text"], script, "the generation lost the transcript");
    assert_eq!(
        asked["reference_audio"].as_str(),
        enrolled.reference_audio.to_str(),
        "the generation was pointed at a different recording"
    );
    assert!(generated.clip.is_some());
}

/// A store brought across before a column existed does not stay empty.
///
/// Adoption happens once, so anything learned afterwards is missing for whoever
/// had already brought theirs over — and re-importing wholesale is not the
/// answer, because by then they may have renamed things and the import would
/// refuse. Absences are filled; anything present is left as it is.
#[test]
fn what_a_voice_was_recorded_reading_is_filled_in_later() {
    let (sandbox, _) = seeded();
    let grace = Duration::from_secs(30);

    // A database from before the column: the value simply is not there.
    {
        let store = yarngo_store::Store::open(&sandbox.database()).expect("open");
        store
            .raw()
            .execute("UPDATE voice_revisions SET reference_text = NULL", [])
            .expect("clear");
    }
    let opened = engine(&sandbox, "", grace);
    let voice = opened
        .voices()
        .expect("voices")
        .into_iter()
        .find(|v| v.voice_id == "alice")
        .expect("alice");
    assert_eq!(
        voice.reference_text, "A sentence Alice read.",
        "the transcript was not filled in from the store it came from"
    );
    drop(opened);

    // What the person has since changed is theirs, and stays.
    {
        let store = yarngo_store::Store::open(&sandbox.database()).expect("open");
        store
            .raw()
            .execute("UPDATE voice_revisions SET reference_text = 'something else'", [])
            .expect("change");
    }
    let reopened = engine(&sandbox, "", grace);
    assert_eq!(
        reopened.voices().expect("voices")[0].reference_text,
        "something else",
        "a value that was already there was overwritten"
    );
}

/// Permission is written where the application says it is written.
///
/// The database is the record; the log is the copy a person can read, and the
/// interface points at it. It has to hold everything — including a permission
/// whose voice is gone, and one this build has never heard of.
#[test]
fn every_permission_reaches_the_log() {
    let (sandbox, recording) = seeded();
    let log = sandbox.root().join("consent.log");

    // A line from somewhere else: an older build, or a store imported before
    // this one existed. It must survive whatever is written next.
    std::fs::write(
        &log,
        b"{\"voice_id\": \"someone-else\", \"granted_at\": \"t-old\", \"statement\": \"I agree\"}\n",
    )
    .expect("seed the log");

    let handle = engine(&sandbox, "", Duration::from_secs(30));
    handle
        .register_voice(speech_engine::Voice {
            voice_id: "bea".into(),
            label: "Bea".into(),
            reference_audio: recording,
            reference_text: "A sentence.".into(),
            seconds: 0.0,
            consent: speech_engine::Consent {
                statement: "I have permission to use this voice.".into(),
                app_version: "test".into(),
                source: "recording".into(),
            },
        snr_db: None,
        sample_rate_hz: None,
    })
        .expect("register");

    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
        .expect("the log was not written")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is a record"))
        .collect();

    let bea = lines
        .iter()
        .find(|l| l["voice_id"] == "bea")
        .expect("the new permission is not in the log");
    assert_eq!(bea["statement"], "I have permission to use this voice.");
    assert_eq!(bea["source"], "recording");
    assert_eq!(
        bea["reference_sha256"].as_str().unwrap_or_default().len(),
        64,
        "no fingerprint of the recording it was given about"
    );
    assert!(
        lines.iter().any(|l| l["voice_id"] == "alice"),
        "the permission that came across with the store is missing"
    );
    assert!(
        lines.iter().any(|l| l["voice_id"] == "someone-else"),
        "a permission this build did not write was lost"
    );

    // Deleting the voice leaves its permission, which is the point of keeping it.
    handle.delete_voice("bea").expect("delete");
    let after = std::fs::read_to_string(&log).expect("log");
    assert!(after.contains("\"bea\""), "deleting a voice took its permission with it");
}

/// A runtime says what it can do, and the application believes it.
///
/// The point of a handshake that lists methods: a runtime that cannot learn a
/// voice from a recording is not asked to, and the offer is withdrawn rather
/// than taken and then refused after somebody has spoken into a microphone.
#[test]
fn a_runtime_that_cannot_learn_a_voice_is_not_asked_to() {
    let (sandbox, recording) = seeded();
    let handle = common::engine_without(
        &sandbox,
        "",
        Duration::from_secs(30),
        &["audio.prepare_reference"],
    );

    let can = handle.capabilities();
    assert!(
        can.methods().count() > 0,
        "the runtime listed nothing it can do, so nothing here is negotiated"
    );
    assert!(!can.can("audio.prepare_reference"));
    assert!(!can.enrolment(), "enrolment was offered by a runtime that cannot do it");
    // It can still speak in a voice it is handed one for — the two are
    // different capabilities and only one of them is missing.
    assert!(can.cloning());
    assert!(can.cancellation());

    // And asking anyway is refused rather than half-done: no voice is left
    // behind by a registration that could not finish.
    let refused = handle.register_voice(speech_engine::Voice {
        voice_id: "bea".into(),
        label: "Bea".into(),
        reference_audio: recording,
        reference_text: "A sentence.".into(),
        seconds: 0.0,
        consent: speech_engine::Consent {
            statement: "I agree".into(),
            app_version: "test".into(),
            source: "recording".into(),
        },
    snr_db: None,
        sample_rate_hz: None,
    });
    assert!(refused.is_err(), "a voice was enrolled by a runtime that cannot prepare one");
    assert!(
        !handle.voices().expect("voices").iter().any(|v| v.voice_id == "bea"),
        "a half-registered voice was left behind"
    );
}

/// The runtime that does answer everything says so, and everything is offered.
#[test]
fn a_complete_runtime_offers_everything() {
    let (sandbox, _) = seeded();
    let can = engine(&sandbox, "", Duration::from_secs(30)).capabilities();
    assert_eq!(can.protocol, "yarngo-engine");
    assert_eq!(can.version, 1);
    assert!(can.enrolment() && can.cloning() && can.cancellation());
    assert!(can.conditioning_invalidation(), "a deletion would have to kill the process");
}

/// Which runtime to start is remembered between runs.
#[test]
fn the_chosen_runtime_is_remembered() {
    use yarngo_store::preferences::RUNTIME;
    let (sandbox, _) = seeded();
    let store = yarngo_store::Store::open(&sandbox.database()).expect("open");

    assert_eq!(store.preference(RUNTIME).expect("read"), None, "something was chosen already");
    store.set_preference(RUNTIME, Some("piper"), "t1").expect("set");
    drop(store);

    let store = yarngo_store::Store::open(&sandbox.database()).expect("reopen");
    assert_eq!(store.preference(RUNTIME).expect("read").as_deref(), Some("piper"));

    // Clearing is not choosing nothing: it is going back to whatever the
    // application would pick, which is a different thing to want.
    store.set_preference(RUNTIME, None, "t2").expect("clear");
    assert_eq!(store.preference(RUNTIME).expect("read"), None);
}
