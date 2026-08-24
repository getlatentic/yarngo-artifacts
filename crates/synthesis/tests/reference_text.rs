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
