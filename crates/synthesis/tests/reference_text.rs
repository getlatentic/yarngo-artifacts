//! What gets stored as a voice's reference text.
//!
//! It must describe the recording. A model told about words the audio does not
//! contain speaks them before it speaks anything it was asked for — they are in
//! its prompt and it finishes them first. Found in the field: every clip made
//! with an enrolled voice began "and the way I shape my words", the tail of the
//! script the reader had stopped just short of.

mod common;

use std::time::Duration;

use speech_engine::{Consent, Voice};

const SCRIPT: &str = "My name is spoken here, and this is how I sound when I speak naturally. \
                      The quick brown fox jumps over the lazy dog, while five wizards judge my \
                      calm voice. I am recording this so the app can learn my accent, my rhythm, \
                      and the way I shape my words.";

const GRACE: Duration = Duration::from_secs(2);

fn enrol(engine: &speech_engine::EngineHandle, recording: &std::path::Path) -> Vec<Voice> {
    engine
        .register_voice(Voice {
            voice_id: "reader".into(),
            label: "The reader".into(),
            reference_audio: recording.to_path_buf(),
            // What they were asked to read — a request, not a report.
            reference_text: SCRIPT.into(),
            seconds: 0.0,
            consent: Consent {
                statement: "I agree".into(),
                ..Default::default()
            },
            snr_db: Some(31.0),
            sample_rate_hz: Some(48_000),
        })
        .expect("enrol");
    engine.voices().expect("voices")
}

fn stored(voices: &[Voice]) -> &str {
    &voices.iter().find(|v| v.voice_id == "reader").expect("the voice").reference_text
}

/// The bug, through the path it actually happened on.
#[test]
fn a_reader_who_stopped_early_is_not_recorded_as_having_finished() {
    let (sandbox, recording) = common::seeded();
    // The recording carries the script up to "my rhythm," and no further.
    let engine = common::engine_hearing(&sandbox, GRACE, 44);
    let voices = enrol(&engine, &recording);

    // The recording is shortened with the text. They describe each other or
    // they describe nothing: text claiming words the audio lacks makes the
    // model speak them first, and audio the text does not cover costs the
    // opening words of every line the voice is later asked for. Cutting one
    // and not the other trades one defect for the other, which is exactly
    // what an earlier version of this fix did.
    let voice = voices.iter().find(|v| v.voice_id == "reader").expect("the voice");
    let whole = std::fs::metadata(&recording).expect("the take").len();
    let kept = std::fs::metadata(&voice.reference_audio).expect("the reference").len();
    assert!(
        kept < whole,
        "the text was cut and the audio was not: {kept} of {whole} bytes"
    );

    // Where it was cut, not how it was punctuated — the engine's own rule for
    // trailing marks is checked in sidecar/test_reference_text.py, and the
    // stand-in here does not share it.
    let text = stored(&voices);
    assert!(
        text.trim_end_matches([',', '.', ' ']).ends_with("my rhythm"),
        "stored something other than what the recording contains: {text:?}"
    );
    assert!(
        !text.contains("shape my words"),
        "words the recording does not contain were stored as spoken, which is \
         what the model reads out before everything else: {text:?}"
    );
}

#[test]
fn a_reader_who_finished_keeps_the_whole_script() {
    let (sandbox, recording) = common::seeded();
    let engine = common::engine_hearing(&sandbox, GRACE, -1);
    let voices = enrol(&engine, &recording);
    assert_eq!(stored(&voices), SCRIPT);
}

/// A runtime that cannot listen back — an older one — leaves the caller's
/// account standing. It is the only account there is, and refusing to enrol
/// over it would be worse than the defect it guards against.
#[test]
fn a_runtime_that_does_not_listen_back_leaves_the_script_alone() {
    let (sandbox, recording) = common::seeded();
    let engine = common::engine_hearing(&sandbox, GRACE, 0);
    let voices = enrol(&engine, &recording);
    assert_eq!(stored(&voices), SCRIPT);
}

/// The numbers a take was accepted on survive onto the voice.
///
/// A clone that sounds thin or hissy months later is diagnosed from what was
/// actually captured — and "we never measured" must read as absent, not as a
/// zero that looks like a measurement.
#[test]
fn what_the_take_measured_is_stored_with_the_voice() {
    let (sandbox, recording) = common::seeded();
    let engine = common::engine_hearing(&sandbox, GRACE, 44);
    let voices = enrol(&engine, &recording);
    let reader = voices.iter().find(|v| v.voice_id == "reader").expect("the voice");
    assert_eq!(reader.snr_db, Some(31.0), "the accepted take's noise figure");
    assert_eq!(reader.sample_rate_hz, Some(48_000), "the capture rate");
}

/// A runtime that cannot listen must not tick a voice off as looked at.
///
/// It is the difference between "this has been checked" and "nothing here can
/// check it". Marked by the second, the voice is stranded: every later runtime
/// finds it already done and the recording is never compared to its text.
#[test]
fn a_runtime_that_cannot_listen_leaves_old_voices_for_one_that_can() {
    let (sandbox, recording) = common::seeded();
    {
        let engine = common::engine_hearing(&sandbox, GRACE, 0);
        enrol(&engine, &recording);
    }
    // Started again on the same deaf runtime, with the voice already there.
    {
        let _engine = common::engine_hearing(&sandbox, GRACE, 0);
    }
    assert_eq!(
        common::count(
            &sandbox,
            "SELECT COUNT(*) FROM voice_revisions WHERE voice_id = 'reader' \
             AND reference_checked_at IS NOT NULL"
        ),
        0,
        "a runtime that cannot listen marked the voice as checked, stranding it"
    );

    // And a runtime that can still repairs it afterwards.
    let _engine = common::engine_hearing(&sandbox, GRACE, 44);
    let text = common::ask(
        &sandbox,
        "SELECT reference_text FROM voice_revisions WHERE voice_id = 'reader'",
    );
    assert!(!text.contains("shape my words"), "never repaired: {text}");
}

/// A voice enrolled before any of this existed.
///
/// Its recording is whatever was read; its stored text is the whole script,
/// because that is what the application asserted at the time. Nobody should
/// have to record again for that, so it is repaired when the engine next
/// starts.
#[test]
fn a_voice_enrolled_before_the_check_is_repaired_at_start() {
    let (sandbox, recording) = common::seeded();

    // Enrol against a runtime that cannot listen back, which is what the old
    // ones were: the whole script goes in, unverified.
    {
        let engine = common::engine_hearing(&sandbox, GRACE, 0);
        enrol(&engine, &recording);
    }
    let before = common::ask(&sandbox, "SELECT reference_text FROM voice_revisions WHERE voice_id = 'reader'");
    assert!(before.ends_with("shape my words."), "{before}");
    assert_eq!(
        common::count(&sandbox, "SELECT COUNT(*) FROM voice_revisions WHERE voice_id = 'reader' AND reference_checked_at IS NOT NULL"),
        0,
        "an unverified voice should not be recorded as checked"
    );

    // Now a runtime that can, and a recording that stops at "my rhythm,".
    let engine = common::engine_hearing(&sandbox, GRACE, 44);
    let after = common::ask(&sandbox, "SELECT reference_text FROM voice_revisions WHERE voice_id = 'reader'");
    assert!(
        !after.contains("shape my words"),
        "the old voice still claims words its recording does not contain: {after}"
    );
    assert_eq!(
        common::count(&sandbox, "SELECT COUNT(*) FROM voice_revisions WHERE voice_id = 'reader' AND reference_checked_at IS NOT NULL"),
        1,
        "the repair did not record that it had happened"
    );

    // And it is not asked again at every start.
    drop(engine);
    let _engine = common::engine_hearing(&sandbox, GRACE, 10);
    let again = common::ask(&sandbox, "SELECT reference_text FROM voice_revisions WHERE voice_id = 'reader'");
    assert_eq!(again, after, "a checked reference was checked again and changed");
}
