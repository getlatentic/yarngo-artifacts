//! Opening the application more than once.
//!
//! Everything durable is named, and a name that repeats between runs is a run
//! that cannot start. Covered here because it is invisible from a single run:
//! the first one works, and every test that builds its own database is a first
//! run.

mod common;

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
