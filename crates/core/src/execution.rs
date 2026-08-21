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
//! A ghost. A job may be attempted more than once — after the engine died, or
//! after a retry — and the abandoned attempt can still be talking. Its messages
//! name a job that exists and a state that is plausible. What makes them wrong
//! is only which attempt they came from, so that is what is checked first.

use crate::job::{DurableJobKind, JobStatus};

/// What the engine has reported about an attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Completed,
    Failed,
    Cancelled,
    /// The engine stopped while this was running. Not a failure of the work.
    Interrupted,
}

impl Observation {
    fn target(&self) -> JobStatus {
        match self {
            Self::Completed => JobStatus::Completed,
            Self::Failed => JobStatus::Failed,
            Self::Cancelled => JobStatus::Cancelled,
            Self::Interrupted => JobStatus::Interrupted,
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
}

/// One durable job, and the attempt currently speaking for it.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub kind: DurableJobKind,
    state: JobStatus,
    current_execution: Option<String>,
}

impl Job {
    pub fn queued(id: impl Into<String>, kind: DurableJobKind) -> Self {
        Self {
            id: id.into(),
            kind,
            state: JobStatus::Queued,
            current_execution: None,
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
    pub fn observe(&mut self, execution_id: &str, observation: Observation) -> Applied {
        // Attempt first. A stale message can carry a state that would otherwise
        // be a legal move, and checking the move first would take it.
        if self.current_execution.as_deref() != Some(execution_id) {
            return Applied::Stale {
                saw: execution_id.to_string(),
                current: self.current_execution.clone(),
            };
        }
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        let target = observation.target();
        if !self.state.can_move_to(target) {
            return Applied::NotPermitted {
                from: self.state,
                to: target,
            };
        }
        self.state = target;
        Applied::Moved { to: target }
    }

    /// The engine is gone, so whatever it was running is not running.
    pub fn engine_lost(&mut self) -> Applied {
        if self.state.is_terminal() {
            return Applied::AlreadyFinished { state: self.state };
        }
        self.current_execution = None;
        if !self.state.can_move_to(JobStatus::Interrupted) {
            // Never dispatched: it is still waiting, and a new engine can take
            // it. Not interrupted, because nothing interrupted it.
            return Applied::NotPermitted {
                from: self.state,
                to: JobStatus::Interrupted,
            };
        }
        self.state = JobStatus::Interrupted;
        Applied::Moved {
            to: JobStatus::Interrupted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Applied, Job, Observation};
    use crate::job::{DurableJobKind, JobStatus};

    fn running() -> Job {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        assert_eq!(job.dispatch("exec-1"), Applied::Moved { to: JobStatus::Running });
        job
    }

    /// Terminal events are sent once by a process that did not crash halfway
    /// through saying so. The second one must change nothing.
    #[test]
    fn a_repeated_completion_changes_nothing() {
        let mut job = running();
        assert_eq!(
            job.observe("exec-1", Observation::Completed),
            Applied::Moved { to: JobStatus::Completed }
        );
        assert_eq!(
            job.observe("exec-1", Observation::Completed),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
        assert_eq!(job.state(), JobStatus::Completed);
    }

    /// And a different outcome arriving second does not overwrite the first.
    #[test]
    fn a_second_outcome_does_not_replace_the_first() {
        let mut job = running();
        job.observe("exec-1", Observation::Completed);
        assert_eq!(
            job.observe("exec-1", Observation::Failed),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
        assert_eq!(job.state(), JobStatus::Completed);
    }

    /// The one that matters most: a completion cannot undo a cancellation.
    #[test]
    fn a_completion_cannot_undo_a_cancellation() {
        let mut job = running();
        job.request_cancel();
        assert_eq!(
            job.observe("exec-1", Observation::Cancelled),
            Applied::Moved { to: JobStatus::Cancelled }
        );
        assert_eq!(
            job.observe("exec-1", Observation::Completed),
            Applied::AlreadyFinished { state: JobStatus::Cancelled }
        );
        assert_eq!(job.state(), JobStatus::Cancelled);
    }

    /// But work that finished before it reached a checkpoint really finished.
    #[test]
    fn a_cancellation_that_arrived_too_late_still_completes() {
        let mut job = running();
        job.request_cancel();
        assert_eq!(job.state(), JobStatus::CancelRequested);
        assert_eq!(
            job.observe("exec-1", Observation::Completed),
            Applied::Moved { to: JobStatus::Completed }
        );
    }

    /// An abandoned attempt can still be talking. What it says is about work
    /// the application stopped believing in.
    ///
    /// Interrupted is an outcome, not a pause: retrying is a new job against
    /// the same target rather than a second attempt at this one. That keeps
    /// "terminal means terminal" without exception, and a job that could return
    /// from a terminal state is exactly the door a late message walks through.
    #[test]
    fn an_event_from_an_abandoned_attempt_is_ignored() {
        let mut job = running();
        job.engine_lost();
        assert_eq!(job.state(), JobStatus::Interrupted);
        assert_eq!(job.current_execution(), None, "a lost engine still owns the job");

        // The retry is its own job, and a new attempt owns that one.
        let mut job = Job::queued("job-2", DurableJobKind::Synthesis);
        job.dispatch("exec-2");
        assert_eq!(
            job.observe("exec-1", Observation::Completed),
            Applied::Stale {
                saw: "exec-1".into(),
                current: Some("exec-2".into())
            }
        );
        assert_eq!(job.state(), JobStatus::Running, "a ghost moved the job");
        assert_eq!(job.current_execution(), Some("exec-2"));
    }

    /// Checked before the transition, because a stale message often carries a
    /// state that would be a legal move if it were current.
    #[test]
    fn staleness_is_decided_before_the_transition_is() {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        job.dispatch("exec-2");
        // Completed is a legal move from Running. It is refused for whose it is.
        assert!(matches!(
            job.observe("exec-1", Observation::Completed),
            Applied::Stale { .. }
        ));
        assert_eq!(job.state(), JobStatus::Running);
    }

    /// A job is one attempt-chain. Handing a running one to a second attempt
    /// would leave two engines believing they own it, and the first one's
    /// eventual message indistinguishable from the second's.
    #[test]
    fn a_running_job_cannot_be_handed_to_another_attempt() {
        let mut job = running();
        assert!(matches!(job.dispatch("exec-2"), Applied::NotPermitted { .. }));
        assert_eq!(job.current_execution(), Some("exec-1"));
    }

    /// Nothing the engine says about a job it was never given can move it.
    #[test]
    fn an_event_for_an_undispatched_job_is_stale() {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        assert_eq!(
            job.observe("exec-1", Observation::Completed),
            Applied::Stale {
                saw: "exec-1".into(),
                current: None
            }
        );
        assert_eq!(job.state(), JobStatus::Queued);
    }

    /// A job still waiting when the engine dies has not been interrupted —
    /// nothing was doing it. It stays where a new engine can pick it up.
    #[test]
    fn losing_the_engine_leaves_a_queued_job_queued() {
        let mut job = Job::queued("job-1", DurableJobKind::VoiceDelete);
        assert!(matches!(job.engine_lost(), Applied::NotPermitted { .. }));
        assert_eq!(job.state(), JobStatus::Queued);
    }

    /// Cancelling something never dispatched needs nothing from the engine.
    #[test]
    fn cancelling_a_queued_job_finishes_it_outright() {
        let mut job = Job::queued("job-1", DurableJobKind::Synthesis);
        assert_eq!(job.request_cancel(), Applied::Moved { to: JobStatus::Cancelled });
        assert_eq!(job.state(), JobStatus::Cancelled);
    }

    #[test]
    fn a_finished_job_cannot_be_dispatched_again() {
        let mut job = running();
        job.observe("exec-1", Observation::Completed);
        assert_eq!(
            job.dispatch("exec-2"),
            Applied::AlreadyFinished { state: JobStatus::Completed }
        );
        assert_eq!(job.current_execution(), Some("exec-1"));
    }
}
