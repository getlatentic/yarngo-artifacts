//! Deleting a voice while it is being used.
//!
//! Everything here goes through the one `EngineHandle` the application has, so
//! the cancellation really does have to reach a busy engine rather than wait
//! for it. The model is a stand-in, because the cases are ones a real model
//! cannot be asked for: finish anyway, never stop, refuse to come back.

mod common;

use std::time::Duration;

use speech_engine::EngineError;

use common::{answer, ask, count, engine, generating, seeded, staged, until, PATIENCE};

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

