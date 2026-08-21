//! Applying what the engine says to what the application believes.
//!
//! The engine reports; it does not decide. Everything it sends is an
//! observation about one attempt, and two of those observations are traps.
//!
//! A repeat. Terminal events are sent once, but "once" is a property of a
//! process that did not crash halfway through saying it, and a retry can say it
//! again. Applying the second one is how a job that was cancelled becomes
//! completed.
//!
//! A ghost. A job is attempted again after its engine dies, and the abandoned
//! attempt can still be talking. Its messages name a job that exists and a
//! state that is plausible. What makes them wrong is only which attempt they
//! came from, so that is what is checked first — before the transition, because
//! a stale message usually carries a move that would be legal if it were
//! current.

use crate::job::{DurableJobKind, ExecutionStatus, JobStatus};

/// Why the application would not publish what the engine produced.
///
/// Where the job lands depends on it: work stopped because the person removed
/// the voice was cancelled, and work whose output cannot be used failed. Both
/// end the job, and calling them the same thing would tell the person their
/// deletion broke something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The voice was deleted while this was generating. Refusing to publish is
    /// the deletion taking effect, not a failure.
    VoiceDeleted,
    /// The file is not there, is empty, or is not the audio it claimed to be.
    Unusable,
}

impl Rejection {
    fn job_target(self) -> JobStatus {
        match self {
            Self::VoiceDeleted => JobStatus::Cancelled,
            Self::Unusable => JobStatus::Failed,
        }
    }
}

/// What the engine has reported about an attempt.
///
/// Internal, because on its own it is not enough to settle anything: what it
/// means for the job depends on the kind of work. Callers say which event
/// happened — [`Job::execution_completed`] and the rest — and this is how that
/// is carried the short distance to the two records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observation {
    Completed,
    Failed,
    Cancelled,
}

impl Observation {
    fn job_target(&self) -> JobStatus {
        match self {
            Self::Completed => JobStatus::Completed,
            Self::Failed => JobStatus::Failed,
            Self::Cancelled => JobStatus::Cancelled,
        }
    }

    fn execution_target(&self) -> ExecutionStatus {
        match self {
            Self::Completed => ExecutionStatus::Completed,
            Self::Failed => ExecutionStatus::Failed,
            Self::Cancelled => ExecutionStatus::Cancelled,
        }
    }
}

/// What became of an observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// Taken, and the job moved.
    Moved { to: JobStatus },
    /// From an attempt that is no longer the current one. Ignored: it describes
    /// work the application already stopped believing in.
    Stale { saw: String, current: Option<String> },
    /// The job already has an outcome, and an outcome does not change. A second
    /// completion, or a completion racing a cancellation that landed first.
    AlreadyFinished { state: JobStatus },
    /// Not a move this job can make from where it is.
    NotPermitted { from: JobStatus, to: JobStatus },
    /// The attempt finished and the job did not. What the engine produced is
    /// waiting to be checked, and until something checks it nothing has been
    /// decided — reporting this as a move would be the answer to a question
    /// nobody has asked yet.
    Awaiting { job: JobStatus },
}

/// One engine's attempt at a job.
#[derive(Clone, Debug)]
pub struct Execution {
    pub id: String,
    pub job_id: String,
    /// Which engine ran it. After a restart, an attempt still marked running by
    /// a session that has ended was interrupted — no inference required.
    pub session_id: String,
    state: ExecutionStatus,
}

impl Execution {
    pub fn started(
        id: impl Into<String>,
        job_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            job_id: job_id.into(),
            session_id: session_id.into(),
            state: ExecutionStatus::Running,
        }
    }

    pub fn state(&self) -> ExecutionStatus {
        self.state
    }

    fn settle(&mut self, to: ExecutionStatus) -> bool {
        if !self.state.can_move_to(to) {
            return false;
        }
        self.state = to;
        true
    }

    /// The engine running this stopped. Terminal for the attempt, and a claim
    /// about nothing else.
    fn interrupt(&mut self) -> bool {
        self.settle(ExecutionStatus::Interrupted)
    }

    /// The engine finished the work.
    ///
    /// Settled alone, because finishing the computation and finishing the job
    /// are different events with a decision in between. The engine produced a
    /// file; whether that file becomes something the person has is for the
    /// application to say, after it has looked at the file and at what has
    /// happened since the work started.
    fn finished(&mut self, observation: Observation) -> bool {
        self.settle(observation.execution_target())
    }
}

/// What the person asked for, and the attempt currently speaking for it.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub kind: DurableJobKind,
    /// Set when this job exists because an earlier one ended badly and the
    /// person asked again. The original keeps its outcome.
    pub retry_of: Option<String>,
    state: JobStatus,
    current_execution: Option<String>,
}

impl Job {
    pub fn queued(id: impl Into<String>, kind: DurableJobKind) -> Self {
        Self {
            id: id.into(),
            kind,
            retry_of: None,
            state: JobStatus::Queued,
            current_execution: None,
        }
    }

    /// A fresh job standing in for one that ended badly. The failure was an
    /// outcome and keeps it; this is the person asking a second time, which is
    /// a different thing from an engine trying again.
    pub fn retrying(id: impl Into<String>, previous: &Job) -> Self {
        Self {
            id: id.into(),
            kind: previous.kind,
            retry_of: Some(previous.id.clone()),
            state: JobStatus::Queued,
            current_execution: None,
        }
    }

    /// Rebuild a job from what was written down. The only way to arrive at a
    /// state without passing through the moves that lead to it, and therefore
    /// only for the store: everything else must go through the transitions.
    pub fn restored(
        id: impl Into<String>,
        kind: DurableJobKind,
        state: JobStatus,
        current_execution: Option<String>,
        retry_of: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            retry_of,
            state,
            current_execution,
        }
    }

    pub fn state(&self) -> JobStatus {
        self.state
    }

    pub fn current_execution(&self) -> Option<&str> {
        self.current_execution.as_deref()
    }

    /// Hand the job to an attempt. The identifier is the application's to make:
    /// the engine cannot name an attempt it may be about to be replaced for.
    pub fn dispatch(&mut self, execution_id: impl Into<String>) -> Applied {
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        if !self.state.can_move_to(JobStatus::Running) {
            return Applied::NotPermitted {
                from: self.state,
                to: JobStatus::Running,
            };
        }
        self.current_execution = Some(execution_id.into());
        self.state = JobStatus::Running;
        Applied::Moved { to: self.state }
    }

    /// Note that stopping has been asked for. Not an outcome: the work may
    /// still finish before it reaches a point where it can stop.
    pub fn request_cancel(&mut self) -> Applied {
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        let target = match self.state {
            // Never dispatched, so there is nothing to ask to stop.
            JobStatus::Queued => JobStatus::Cancelled,
            _ => JobStatus::CancelRequested,
        };
        if !self.state.can_move_to(target) {
            return Applied::NotPermitted {
                from: self.state,
                to: target,
            };
        }
        self.state = target;
        Applied::Moved { to: target }
    }

    /// Take what the engine said, if it is still the engine we are listening to.
    /// Deliberately not public. Moving a job straight from what the engine
    /// said is right only where the engine's outcome is the job's, and a
    /// caller free to do it for synthesis could complete a job on the strength
    /// of a file nothing had read.
    fn observe(&mut self, execution_id: &str, observation: Observation) -> Applied {
        if self.current_execution.as_deref() != Some(execution_id) {
            return Applied::Stale {
                saw: execution_id.to_string(),
                current: self.current_execution.clone(),
            };
        }
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        let target = observation.job_target();
        if !self.state.can_move_to(target) {
            return Applied::NotPermitted {
                from: self.state,
                to: target,
            };
        }
        self.state = target;
        Applied::Moved { to: target }
    }

    /// The engine carrying this job is gone.
    ///
    /// Named for the event rather than exposed as a move to `Queued`. Returning
    /// running work to the queue is only ever right because the thing running it
    /// vanished, and a caller able to say so for any other reason could restart
    /// work that is still in progress.
    ///
    /// The attempt is over; the job usually is not. It returns to waiting for
    /// an engine that can take it — unless stopping had already been asked for,
    /// in which case resuming would restart work the person cancelled, and the
    /// engine going is how that cancellation finally took effect.
    pub fn execution_interrupted(&mut self, execution: &mut Execution) -> Applied {
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        execution.interrupt();
        self.current_execution = None;
        let target = match self.state {
            JobStatus::CancelRequested => JobStatus::Cancelled,
            _ => JobStatus::Queued,
        };
        if self.state == target {
            // Never dispatched. Nothing interrupted it, and it is already where
            // a later engine will find it.
            return Applied::Moved { to: target };
        }
        if !self.state.can_move_to(target) {
            return Applied::NotPermitted {
                from: self.state,
                to: target,
            };
        }
        self.state = target;
        Applied::Moved { to: target }
    }

    /// The engine finished this attempt successfully.
    ///
    /// Whether that finishes the job is the job kind's to say. For most kinds
    /// the engine's success is the outcome; for synthesis it is a file nobody
    /// has looked at, and the job waits for [`Job::take_published`] or
    /// [`Job::take_rejected`].
    pub fn execution_completed(&mut self, execution: &mut Execution) -> Applied {
        self.engine_reported(execution, Observation::Completed)
    }

    /// The engine could not do it. Terminal for both: nothing was produced, so
    /// there is nothing left to decide.
    pub fn execution_failed(&mut self, execution: &mut Execution) -> Applied {
        self.engine_reported(execution, Observation::Failed)
    }

    /// The engine stopped because it was asked to. Terminal for both, and for
    /// the same reason: what it stopped short of making is not wanted.
    pub fn execution_cancelled(&mut self, execution: &mut Execution) -> Applied {
        self.engine_reported(execution, Observation::Cancelled)
    }

    /// The application checked what the engine produced and committed it. This
    /// is what finishes a synthesis job — not the engine finishing.
    pub fn take_published(&mut self, execution: &Execution) -> Applied {
        self.publication(execution, JobStatus::Completed)
    }

    /// The application checked what the engine produced and would not have it.
    pub fn take_rejected(&mut self, execution: &Execution, reason: Rejection) -> Applied {
        self.publication(execution, reason.job_target())
    }

    /// Move both records for something the engine reported.
    ///
    /// Deliberately not public. It is the right shape only where the engine's
    /// outcome is the job's, and a caller free to use it for synthesis could
    /// complete a job on the strength of a file nothing had read.
    fn engine_reported(&mut self, execution: &mut Execution, observation: Observation) -> Applied {
        if observation == Observation::Completed && !self.kind.completes_with_execution() {
            // Staleness before anything else: an abandoned attempt's success is
            // not this job's, and must not even mark its own record finished
            // under a job that has moved on.
            if self.current_execution.as_deref() != Some(execution.id.as_str()) {
                return Applied::Stale {
                    saw: execution.id.clone(),
                    current: self.current_execution.clone(),
                };
            }
            if self.state.is_terminal() {
                return Applied::AlreadyFinished { state: self.state };
            }
            execution.finished(observation);
            return Applied::Awaiting { job: self.state };
        }
        let outcome = self.observe(&execution.id.clone(), observation);
        if matches!(outcome, Applied::Moved { .. }) {
            execution.finished(observation);
        }
        outcome
    }

    /// Settle a job on what the application decided about the engine's output.
    fn publication(&mut self, execution: &Execution, target: JobStatus) -> Applied {
        if self.current_execution.as_deref() != Some(execution.id.as_str()) {
            return Applied::Stale {
                saw: execution.id.clone(),
                current: self.current_execution.clone(),
            };
        }
        if execution.state() != ExecutionStatus::Completed {
            // Nothing was produced to publish or refuse. Publishing the output
            // of an attempt that did not finish is how a partial file becomes a
            // take.
            return Applied::NotPermitted {
                from: self.state,
                to: target,
            };
        }
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        if !self.state.can_move_to(target) {
            return Applied::NotPermitted {
                from: self.state,
                to: target,
            };
        }
        self.state = target;
        Applied::Moved { to: target }
    }
}

#[cfg(test)]
mod tests {
    use super::{Applied, Execution, Job, Rejection};
    use crate::job::{DurableJobKind, ExecutionStatus, JobStatus};

    /// A dispatched job whose engine finishing is the whole of it, which is
    /// what the lifecycle below is about. Synthesis has a decision after the
    /// engine and gets its own tests.
    fn dispatched() -> (Job, Execution) {
        started(DurableJobKind::ModelInstall)
    }

    /// A dispatched job that produces something for the application to check.
    fn synthesising() -> (Job, Execution) {
        started(DurableJobKind::Synthesis)
    }

    fn started(kind: DurableJobKind) -> (Job, Execution) {
        let mut job = Job::queued("job-1", kind);
        assert_eq!(job.dispatch("exec-1"), Applied::Moved { to: JobStatus::Running });
        (job, Execution::started("exec-1", "job-1", "session-1"))
    }

    /// Terminal events are sent once by a process that did not crash halfway
    /// through saying so. The second one must change nothing.
    #[test]
    fn a_repeated_completion_changes_nothing() {
        let (mut job, mut exec) = dispatched();
        assert_eq!(
            job.execution_completed(&mut exec),
            Applied::Moved { to: JobStatus::Completed }
        );
        assert_eq!(
            job.execution_completed(&mut exec),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
        assert_eq!(job.state(), JobStatus::Completed);
        assert_eq!(exec.state(), ExecutionStatus::Completed);
    }

    #[test]
    fn a_second_outcome_does_not_replace_the_first() {
        let (mut job, mut exec) = dispatched();
        job.execution_completed(&mut exec);
        assert_eq!(
            job.execution_failed(&mut exec),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
        assert_eq!(job.state(), JobStatus::Completed);
    }

    /// The one that matters most: a completion cannot undo a cancellation.
    #[test]
    fn a_completion_cannot_undo_a_cancellation() {
        let (mut job, mut exec) = dispatched();
        job.request_cancel();
        job.execution_cancelled(&mut exec);
        assert_eq!(job.state(), JobStatus::Cancelled);
        assert_eq!(
            job.execution_completed(&mut exec),
            Applied::AlreadyFinished { state: JobStatus::Cancelled }
        );
        assert_eq!(job.state(), JobStatus::Cancelled);
    }

    /// But work that finished before it reached a checkpoint really finished.
    #[test]
    fn a_cancellation_that_arrived_too_late_still_completes() {
        let (mut job, mut exec) = dispatched();
        job.request_cancel();
        assert_eq!(job.state(), JobStatus::CancelRequested);
        assert_eq!(
            job.execution_completed(&mut exec),
            Applied::Moved { to: JobStatus::Completed }
        );
    }

    // The recovery sequence, one step per assertion.

    /// Losing the engine ends the attempt and returns the job to waiting.
    #[test]
    fn an_interrupted_execution_returns_its_job_to_the_queue() {
        let (mut job, mut exec) = dispatched();
        assert_eq!(job.execution_interrupted(&mut exec), Applied::Moved { to: JobStatus::Queued });
        assert_eq!(exec.state(), ExecutionStatus::Interrupted);
        assert_eq!(job.state(), JobStatus::Queued);
        assert_eq!(job.current_execution(), None);
    }

    /// And a later engine can take it, as the same job.
    #[test]
    fn a_queued_job_is_dispatched_to_a_second_attempt() {
        let (mut job, mut first) = dispatched();
        job.execution_interrupted(&mut first);
        assert_eq!(job.dispatch("exec-2"), Applied::Moved { to: JobStatus::Running });
        assert_eq!(job.current_execution(), Some("exec-2"));
        assert_eq!(job.id, "job-1", "recovery changed which job this is");
    }

    /// The abandoned attempt can still be talking, and what it says is about
    /// work the application stopped believing in.
    #[test]
    fn a_late_completion_from_the_first_attempt_is_ignored() {
        let (mut job, mut first) = dispatched();
        job.execution_interrupted(&mut first);
        job.dispatch("exec-2");
        assert_eq!(
            job.execution_completed(&mut first),
            Applied::Stale {
                saw: "exec-1".into(),
                current: Some("exec-2".into())
            }
        );
        assert_eq!(job.state(), JobStatus::Running, "a ghost moved the job");
    }

    /// And the attempt that is current settles it.
    #[test]
    fn the_second_attempt_completes_the_job() {
        let (mut job, mut first) = dispatched();
        job.execution_interrupted(&mut first);
        job.dispatch("exec-2");
        let mut second = Execution::started("exec-2", "job-1", "session-2");
        assert_eq!(
            job.execution_completed(&mut second),
            Applied::Moved { to: JobStatus::Completed }
        );
        assert_eq!(job.state(), JobStatus::Completed);
        assert_eq!(first.state(), ExecutionStatus::Interrupted);
        assert_eq!(second.state(), ExecutionStatus::Completed);
    }

    /// A job whose cancellation was already asked for does not come back when
    /// the engine goes. Resuming would restart work the person stopped, and
    /// killing the engine is how that cancellation took effect.
    #[test]
    fn losing_the_engine_mid_cancellation_finishes_the_cancellation() {
        let (mut job, mut exec) = dispatched();
        job.request_cancel();
        assert_eq!(
            job.execution_interrupted(&mut exec),
            Applied::Moved { to: JobStatus::Cancelled }
        );
        assert_eq!(exec.state(), ExecutionStatus::Interrupted);
    }

    /// The two ways a cancellation ends differ in what the attempt did, and the
    /// difference is the whole diagnostic value: one engine stopped when asked,
    /// the other was killed for not stopping.
    #[test]
    fn a_cancelled_attempt_and_an_abandoned_one_are_told_apart() {
        let (mut observed, mut stopped) = dispatched();
        observed.request_cancel();
        observed.execution_cancelled(&mut stopped);
        assert_eq!(observed.state(), JobStatus::Cancelled);
        assert_eq!(stopped.state(), ExecutionStatus::Cancelled);

        let (mut abandoned, mut vanished) = dispatched();
        abandoned.request_cancel();
        abandoned.execution_interrupted(&mut vanished);
        assert_eq!(abandoned.state(), JobStatus::Cancelled);
        assert_eq!(
            vanished.state(),
            ExecutionStatus::Interrupted,
            "an engine that was killed is not one that cancelled"
        );
    }

    /// An attempt that ended is not revived by recovery; recovery makes another.
    #[test]
    fn an_interrupted_attempt_stays_interrupted() {
        let (mut job, mut exec) = dispatched();
        job.execution_interrupted(&mut exec);
        assert!(!exec.interrupt(), "a finished attempt moved again");
        assert_eq!(exec.state(), ExecutionStatus::Interrupted);
    }

    /// Retrying something that genuinely failed is the person asking again,
    /// which is a new job. The failure keeps its outcome.
    #[test]
    fn retrying_a_failure_is_a_new_job_that_remembers_the_old_one() {
        let (mut job, mut exec) = dispatched();
        job.execution_failed(&mut exec);
        assert_eq!(job.state(), JobStatus::Failed);

        let retry = Job::retrying("job-2", &job);
        assert_eq!(retry.retry_of.as_deref(), Some("job-1"));
        assert_eq!(retry.state(), JobStatus::Queued);
        assert_eq!(retry.kind, job.kind);
        assert_eq!(job.state(), JobStatus::Failed, "the retry rewrote history");
    }

    /// The case that makes them two events: the engine finishes the audio, and
    /// the application refuses to publish it. Both records are true, and they
    /// disagree.
    #[test]
    fn work_can_finish_while_the_job_it_was_for_is_cancelled() {
        let (mut job, mut exec) = synthesising();
        // Something happened that means this must not be published — a voice
        // being deleted — so stopping was asked for.
        job.request_cancel();
        // The engine got there first, which cooperative cancellation permits.
        assert_eq!(
            job.execution_completed(&mut exec),
            Applied::Awaiting { job: JobStatus::CancelRequested }
        );
        assert_eq!(exec.state(), ExecutionStatus::Completed);
        // The application looks at what it has and declines to keep it.
        assert_eq!(
            job.take_rejected(&exec, Rejection::VoiceDeleted),
            Applied::Moved { to: JobStatus::Cancelled }
        );

        assert_eq!(exec.state(), ExecutionStatus::Completed, "the attempt was rewritten");
        assert_eq!(job.state(), JobStatus::Cancelled);
    }

    /// And once the job is cancelled, the completion that produced the audio
    /// cannot be applied to it afterwards.
    #[test]
    fn a_finished_attempt_cannot_complete_a_cancelled_job() {
        let (mut job, mut exec) = synthesising();
        job.request_cancel();
        job.execution_completed(&mut exec);
        job.take_rejected(&exec, Rejection::VoiceDeleted);
        assert_eq!(
            job.take_published(&exec),
            Applied::AlreadyFinished { state: JobStatus::Cancelled }
        );
    }

    #[test]
    fn staleness_is_decided_before_the_transition_is() {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        job.dispatch("exec-2");
        let mut abandoned = Execution::started("exec-1", "job-1", "session-1");
        assert!(matches!(
            job.execution_completed(&mut abandoned),
            Applied::Stale { .. }
        ));
        assert_eq!(job.state(), JobStatus::Running);
    }

    #[test]
    fn an_event_for_an_undispatched_job_is_stale() {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        let mut never_dispatched = Execution::started("exec-1", "job-1", "session-1");
        assert_eq!(
            job.execution_completed(&mut never_dispatched),
            Applied::Stale {
                saw: "exec-1".into(),
                current: None
            }
        );
        assert_eq!(job.state(), JobStatus::Queued);
    }

    #[test]
    fn cancelling_a_queued_job_finishes_it_outright() {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        assert_eq!(job.request_cancel(), Applied::Moved { to: JobStatus::Cancelled });
    }

    // Synthesis, where the engine finishing and the job finishing are two
    // events with a decision between them.

    /// The whole point of the split: a file exists, and nothing has decided
    /// what it is yet.
    #[test]
    fn an_engine_completion_does_not_complete_a_synthesis_job() {
        let (mut job, mut exec) = synthesising();
        assert_eq!(
            job.execution_completed(&mut exec),
            Applied::Awaiting { job: JobStatus::Running }
        );
        assert_eq!(exec.state(), ExecutionStatus::Completed);
        assert_eq!(job.state(), JobStatus::Running, "a file nobody read finished the job");
    }

    #[test]
    fn publishing_the_take_is_what_completes_it() {
        let (mut job, mut exec) = synthesising();
        job.execution_completed(&mut exec);
        assert_eq!(job.take_published(&exec), Applied::Moved { to: JobStatus::Completed });
        assert_eq!(job.state(), JobStatus::Completed);
    }

    /// Where the rejection lands says why it was rejected, and the person can
    /// tell their own deletion from something going wrong.
    #[test]
    fn a_refusal_lands_where_its_reason_says() {
        let (mut deleted, mut one) = synthesising();
        deleted.execution_completed(&mut one);
        assert_eq!(
            deleted.take_rejected(&one, Rejection::VoiceDeleted),
            Applied::Moved { to: JobStatus::Cancelled }
        );

        let (mut broken, mut two) = synthesising();
        broken.execution_completed(&mut two);
        assert_eq!(
            broken.take_rejected(&two, Rejection::Unusable),
            Applied::Moved { to: JobStatus::Failed }
        );
    }

    /// An abandoned attempt's file is still on the disk, and publishing it
    /// would give the person the output of work they never saw finish.
    #[test]
    fn a_stale_attempts_output_is_not_published() {
        let (mut job, mut first) = synthesising();
        job.execution_interrupted(&mut first);
        job.dispatch("exec-2");
        // The lost engine's process was still alive and finished after all.
        assert!(matches!(job.execution_completed(&mut first), Applied::Stale { .. }));
        assert_eq!(
            first.state(),
            ExecutionStatus::Interrupted,
            "a ghost rewrote its own record"
        );
        assert!(matches!(job.take_published(&first), Applied::Stale { .. }));
        assert_eq!(job.state(), JobStatus::Running);
    }

    /// Publication needs something that finished. Otherwise a partial file
    /// becomes a take.
    #[test]
    fn an_unfinished_attempt_cannot_be_published() {
        let (mut job, exec) = synthesising();
        assert_eq!(
            job.take_published(&exec),
            Applied::NotPermitted { from: JobStatus::Running, to: JobStatus::Completed }
        );
        assert_eq!(job.state(), JobStatus::Running);
    }

    /// Publication happens once. A second one is the same crash-and-repeat case
    /// the engine's terminal events have.
    #[test]
    fn a_take_is_published_once() {
        let (mut job, mut exec) = synthesising();
        job.execution_completed(&mut exec);
        job.take_published(&exec);
        assert_eq!(
            job.take_published(&exec),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
    }

    /// A synthesis that failed in the engine produced nothing, so there is no
    /// decision left and the job fails with it.
    #[test]
    fn a_synthesis_that_failed_in_the_engine_needs_no_publication() {
        let (mut job, mut exec) = synthesising();
        assert_eq!(job.execution_failed(&mut exec), Applied::Moved { to: JobStatus::Failed });
        assert_eq!(exec.state(), ExecutionStatus::Failed);
    }

    #[test]
    fn a_finished_job_cannot_be_dispatched_again() {
        let (mut job, mut exec) = dispatched();
        job.execution_completed(&mut exec);
        assert_eq!(
            job.dispatch("exec-2"),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
        assert_eq!(job.current_execution(), Some("exec-1"));
    }
}
