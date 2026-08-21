//! What the application knows after it, or the engine, stops unexpectedly.
//!
//! The lifecycle rules are tested in `yarngo-core` without a file. These test
//! that they survive being written down and read back — that recovery is
//! decided from what the database says rather than from what happened to still
//! be in memory.

use std::path::PathBuf;

use yarngo_core::{DurableJobKind, Execution, ExecutionStatus, Job, JobStatus, Observation};
use yarngo_store::Store;

fn at(seconds: u32) -> String {
    format!("2026-08-21T12:00:{seconds:02}Z")
}

/// A store on disk, so it can be closed and reopened the way a restart does.
fn on_disk(dir: &tempfile::TempDir, name: &str) -> (Store, PathBuf) {
    let path = dir.path().join(name);
    (Store::open(&path).expect("open"), path)
}

#[test]
fn a_job_running_when_its_engine_died_comes_back_and_is_finished_by_another() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, path) = on_disk(&dir, "recovery.db");

    // A session, a job, and an attempt at it.
    store.open_session("session-1", "mlx", &at(0)).expect("session");
    let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
    store.insert_job(&job, Some("clip-1"), &at(0)).expect("insert");
    job.dispatch("exec-1");
    let execution = Execution::started("exec-1", "job-1", "session-1");
    store.save_progress(&job, Some(&execution), &at(1)).expect("dispatch");

    // The engine goes, and so does the application: nothing tidies up.
    store.end_session("session-1", &at(2), "process_exited").expect("end");
    drop(store);

    // Reopened, the way a restart sees it.
    let mut store = Store::open(&path).expect("reopen");
    let settled = store.reconcile_ended_sessions(&at(3)).expect("reconcile");
    assert_eq!(settled.interrupted, ["exec-1"]);
    assert_eq!(settled.requeued, ["job-1"]);
    assert!(settled.cancelled.is_empty());

    assert_eq!(
        store.execution_state("exec-1").expect("state").as_deref(),
        Some("interrupted")
    );
    let mut job = store.load_job("job-1").expect("load").expect("job-1");
    assert_eq!(job.state(), JobStatus::Queued, "the job did not come back");
    assert_eq!(job.current_execution(), None);

    // A second engine takes it. Same job, second attempt.
    store.open_session("session-2", "mlx", &at(4)).expect("session");
    job.dispatch("exec-2");
    let mut second = Execution::started("exec-2", "job-1", "session-2");
    store.save_progress(&job, Some(&second), &at(5)).expect("redispatch");

    // The abandoned attempt is still talking, and is refused for whose it is.
    let stale = job.observe("exec-1", Observation::Completed);
    assert!(matches!(stale, yarngo_core::Applied::Stale { .. }), "{stale:?}");
    assert_eq!(job.state(), JobStatus::Running);

    job.settle(&mut second, Observation::Completed);
    store.save_progress(&job, Some(&second), &at(6)).expect("complete");

    // And that is what survives a second restart.
    drop(store);
    let store = Store::open(&path).expect("reopen again");
    let job = store.load_job("job-1").expect("load").expect("job-1");
    assert_eq!(job.state(), JobStatus::Completed);
    assert_eq!(
        store.execution_state("exec-1").expect("first").as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        store.execution_state("exec-2").expect("second").as_deref(),
        Some("completed")
    );
}

/// Stopping had been asked for, so the engine going settles it rather than
/// returning the work to the queue. The attempt was interrupted, not cancelled:
/// no engine observed the cancellation, it simply stopped existing.
#[test]
fn a_job_being_cancelled_when_its_engine_died_is_cancelled_not_requeued() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, path) = on_disk(&dir, "cancel.db");

    store.open_session("session-1", "mlx", &at(0)).expect("session");
    let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
    store.insert_job(&job, Some("clip-1"), &at(0)).expect("insert");
    job.dispatch("exec-1");
    let execution = Execution::started("exec-1", "job-1", "session-1");
    store.save_progress(&job, Some(&execution), &at(1)).expect("dispatch");

    job.request_cancel();
    store.save_progress(&job, Some(&execution), &at(2)).expect("cancel");
    assert_eq!(job.state(), JobStatus::CancelRequested);

    // Killed rather than stopping, which is often how a stubborn cancellation
    // takes effect.
    store
        .end_session("session-1", &at(3), "killed_for_cancellation")
        .expect("end");
    drop(store);

    let mut store = Store::open(&path).expect("reopen");
    let settled = store.reconcile_ended_sessions(&at(4)).expect("reconcile");
    assert_eq!(settled.cancelled, ["job-1"]);
    assert!(settled.requeued.is_empty(), "cancelled work was restarted");

    let job = store.load_job("job-1").expect("load").expect("job-1");
    assert_eq!(job.state(), JobStatus::Cancelled);
    assert_eq!(
        store.execution_state("exec-1").expect("state").as_deref(),
        Some("interrupted"),
        "an engine that was killed is not one that cancelled"
    );
}

/// Reconciliation reads the session, not the clock. A job running under an
/// engine that is still alive is left alone however long it has been going.
#[test]
fn work_under_a_live_session_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = on_disk(&dir, "live.db");

    store.open_session("session-1", "mlx", &at(0)).expect("session");
    let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
    store.insert_job(&job, None, &at(0)).expect("insert");
    job.dispatch("exec-1");
    let execution = Execution::started("exec-1", "job-1", "session-1");
    store.save_progress(&job, Some(&execution), &at(1)).expect("dispatch");

    let settled = store.reconcile_ended_sessions(&at(2)).expect("reconcile");
    assert_eq!(settled, yarngo_store::jobs::Reconciled::default());
    assert_eq!(
        store.load_job("job-1").expect("load").unwrap().state(),
        JobStatus::Running
    );
}

/// Running twice changes nothing the second time: everything it would settle
/// has already been settled.
#[test]
fn reconciling_twice_settles_nothing_the_second_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = on_disk(&dir, "twice.db");

    store.open_session("session-1", "mlx", &at(0)).expect("session");
    let mut job = Job::queued("job-1", DurableJobKind::VoiceDelete);
    store.insert_job(&job, Some("voice-1"), &at(0)).expect("insert");
    job.dispatch("exec-1");
    let execution = Execution::started("exec-1", "job-1", "session-1");
    store.save_progress(&job, Some(&execution), &at(1)).expect("dispatch");
    store.end_session("session-1", &at(2), "crashed").expect("end");

    let first = store.reconcile_ended_sessions(&at(3)).expect("first");
    assert_eq!(first.requeued, ["job-1"]);
    let second = store.reconcile_ended_sessions(&at(4)).expect("second");
    assert_eq!(second, yarngo_store::jobs::Reconciled::default());
}

/// A finished job is not disturbed by the session it finished under ending.
#[test]
fn a_completed_job_survives_its_session_ending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = on_disk(&dir, "done.db");

    store.open_session("session-1", "mlx", &at(0)).expect("session");
    let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
    store.insert_job(&job, None, &at(0)).expect("insert");
    job.dispatch("exec-1");
    let mut execution = Execution::started("exec-1", "job-1", "session-1");
    job.settle(&mut execution, Observation::Completed);
    store.save_progress(&job, Some(&execution), &at(1)).expect("save");
    assert_eq!(execution.state(), ExecutionStatus::Completed);

    store.end_session("session-1", &at(2), "process_exited").expect("end");
    let settled = store.reconcile_ended_sessions(&at(3)).expect("reconcile");
    assert_eq!(settled, yarngo_store::jobs::Reconciled::default());
    assert_eq!(
        store.load_job("job-1").expect("load").unwrap().state(),
        JobStatus::Completed
    );
}

/// The references in the schema are enforced, not decorative. Off by default in
/// SQLite and set per connection, so this is worth asserting rather than
/// assuming.
#[test]
fn an_execution_cannot_name_a_job_that_does_not_exist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, _) = on_disk(&dir, "fk.db");
    store.open_session("session-1", "mlx", &at(0)).expect("session");

    let orphan = Execution::started("exec-1", "job-that-never-was", "session-1");
    let job = Job::queued("job-that-never-was", DurableJobKind::Synthesis);
    let refused = store.save_progress(&job, Some(&orphan), &at(1));
    assert!(refused.is_err(), "an orphaned attempt was accepted");
}
