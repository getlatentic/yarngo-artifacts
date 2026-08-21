//! Work Rust must be able to account for after an interruption.
//!
//! Two lifecycles, because they answer different questions. A job is what the
//! person asked for and it lasts until that is settled. An execution is one
//! engine's attempt at it, and it lasts until that engine stops attempting.
//!
//! Losing the engine ends an execution and does not end a job: nobody is doing
//! the work, but nobody decided it should not be done. The job goes back to
//! waiting and a later attempt picks it up, which is why an interruption is
//! terminal for an execution and absent from a job's states entirely.

use serde::{Deserialize, Serialize};

/// Work whose outcome Rust has to reconcile if the application or the sidecar
/// stops partway. Each of these coordinates something the database alone cannot
/// undo — a file written, an artefact fetched, a recording deleted — so a row
/// survives the process to say what was in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableJobKind {
    Synthesis,
    ModelInstall,
    ModelDelete,
    VoiceDelete,
}

impl DurableJobKind {
    /// Whether the engine finishing this is the whole of it.
    ///
    /// Synthesis is the case where it is not. The engine writes a file; whether
    /// that file becomes a take the person has is decided afterwards, by
    /// something that looks at the file and at what has happened to the voice
    /// since the work started. Answering that here — treating the engine's
    /// success as the job's — is the collapse this exists to prevent.
    pub fn completes_with_execution(self) -> bool {
        !matches!(self, Self::Synthesis)
    }
}

/// What became of what the person asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    /// Cancellation asked for, not yet reached. The work is still running and
    /// may still finish first — this is a request, not an outcome.
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    /// Whether this is the last thing that will ever be said about the job.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Every move the job registry will make. Anything absent is refused, so
    /// adding a state forces a decision about it rather than defaulting to
    /// permitted.
    pub fn can_move_to(self, next: Self) -> bool {
        use JobStatus::*;
        matches!(
            (self, next),
            (Queued, Running)
                // Never dispatched, so nothing has to be asked to stop.
                | (Queued, Cancelled)
                | (Queued, Failed)
                | (Running, CancelRequested)
                | (Running, Completed)
                | (Running, Failed)
                // The application looked at what the engine produced and would
                // not have it — the voice was deleted while this was running.
                // No cancellation was ever asked for: the deletion can happen
                // entirely between the engine finishing and anything checking,
                // and refusing to publish is that deletion taking effect.
                | (Running, Cancelled)
                // The engine went while this was running. Nobody is doing the
                // work and nobody decided it should not be done, so it waits
                // for an engine that can.
                | (Running, Queued)
                // The request lost the race. Finishing is the honest outcome,
                // and the caller learns the cancel arrived too late.
                | (CancelRequested, Completed)
                | (CancelRequested, Cancelled)
                | (CancelRequested, Failed)
        )
    }
}

/// One engine's attempt at a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// The engine stopped while this was running. Terminal for the attempt and
    /// says nothing about the job: no engine is doing it, which is a different
    /// claim from the work being over.
    Interrupted,
}

impl ExecutionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    pub fn can_move_to(self, next: Self) -> bool {
        use ExecutionStatus::*;
        matches!(
            (self, next),
            (Queued, Running)
                | (Queued, Cancelled)
                | (Queued, Interrupted)
                | (Running, Completed)
                | (Running, Failed)
                | (Running, Cancelled)
                | (Running, Interrupted)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutionStatus as Exec;
    use super::JobStatus::{self, *};

    /// Every state, so the table below can be exhaustive. The match is what
    /// keeps it honest: a new variant stops this compiling until it is listed.
    const ALL: [JobStatus; 6] = [Queued, Running, CancelRequested, Completed, Failed, Cancelled];

    #[test]
    fn every_state_appears_in_the_table() {
        for state in ALL {
            match state {
                Queued | Running | CancelRequested | Completed | Failed | Cancelled => {}
            }
        }
    }

    /// Stated once, checked against every pair. A move that is not written
    /// here is refused, so a new state cannot arrive quietly permitted.
    const ALLOWED: &[(JobStatus, JobStatus)] = &[
        (Queued, Running),
        (Queued, Cancelled),
        (Queued, Failed),
        (Running, CancelRequested),
        (Running, Completed),
        (Running, Failed),
        (Running, Cancelled),
        (Running, Queued),
        (CancelRequested, Completed),
        (CancelRequested, Cancelled),
        (CancelRequested, Failed),
    ];

    #[test]
    fn only_the_listed_moves_are_allowed() {
        for from in ALL {
            for to in ALL {
                let listed = ALLOWED.contains(&(from, to));
                assert_eq!(
                    from.can_move_to(to),
                    listed,
                    "{from:?} -> {to:?} should be {}",
                    if listed { "allowed" } else { "refused" }
                );
            }
        }
    }

    /// The invariant a late message from the engine must not be able to break:
    /// once the job has an outcome, that is the outcome.
    #[test]
    fn nothing_leaves_a_terminal_state() {
        for from in ALL.into_iter().filter(|s| s.is_terminal()) {
            for to in ALL {
                assert!(!from.can_move_to(to), "{from:?} moved to {to:?}");
            }
        }
    }

    /// Specifically: a completion arriving after the job was cancelled cannot
    /// turn it back into a success. The audio it refers to is not committed.
    #[test]
    fn a_late_completion_cannot_undo_a_cancellation() {
        assert!(!Cancelled.can_move_to(Completed));
        assert!(!Cancelled.can_move_to(Running));
    }

    /// But a job that finishes between the request and the checkpoint really
    /// did finish. Refusing that would be recording a cancellation that never
    /// happened.
    #[test]
    fn a_cancel_that_arrives_too_late_still_completes() {
        assert!(CancelRequested.can_move_to(Completed));
    }

    /// Cancelling something that never started needs nothing from the engine.
    #[test]
    fn a_queued_job_cancels_without_being_asked_to_stop() {
        assert!(Queued.can_move_to(Cancelled));
        assert!(!Queued.can_move_to(CancelRequested));
    }

    /// Losing the engine returns the work to the queue rather than ending it.
    /// An interruption is not an outcome, which is why a job has no such state.
    #[test]
    fn an_interrupted_job_goes_back_to_waiting() {
        assert!(Running.can_move_to(Queued));
        assert!(!Completed.can_move_to(Queued));
        assert!(!Cancelled.can_move_to(Queued));
    }

    /// A job whose cancellation was already asked for does not return to the
    /// queue when the engine goes: restarting it would resume work the person
    /// asked to stop.
    #[test]
    fn a_job_being_cancelled_does_not_come_back() {
        assert!(!CancelRequested.can_move_to(Queued));
        assert!(CancelRequested.can_move_to(Cancelled));
    }

    #[test]
    fn every_terminal_state_is_reachable() {
        for terminal in ALL.into_iter().filter(|s| s.is_terminal()) {
            assert!(
                ALL.into_iter().any(|from| from.can_move_to(terminal)),
                "{terminal:?} cannot be reached"
            );
        }
    }

    const ALL_EXEC: [Exec; 6] = [
        Exec::Queued,
        Exec::Running,
        Exec::Completed,
        Exec::Failed,
        Exec::Cancelled,
        Exec::Interrupted,
    ];

    #[test]
    fn every_execution_state_appears_in_the_table() {
        for state in ALL_EXEC {
            match state {
                Exec::Queued
                | Exec::Running
                | Exec::Completed
                | Exec::Failed
                | Exec::Cancelled
                | Exec::Interrupted => {}
            }
        }
    }

    #[test]
    fn only_the_listed_execution_moves_are_allowed() {
        let allowed = [
            (Exec::Queued, Exec::Running),
            (Exec::Queued, Exec::Cancelled),
            (Exec::Queued, Exec::Interrupted),
            (Exec::Running, Exec::Completed),
            (Exec::Running, Exec::Failed),
            (Exec::Running, Exec::Cancelled),
            (Exec::Running, Exec::Interrupted),
        ];
        for from in ALL_EXEC {
            for to in ALL_EXEC {
                assert_eq!(from.can_move_to(to), allowed.contains(&(from, to)), "{from:?} -> {to:?}");
            }
        }
    }

    /// An attempt that ended stays ended. Recovery makes another attempt; it
    /// does not revive this one.
    #[test]
    fn an_execution_never_leaves_a_terminal_state() {
        for from in ALL_EXEC.into_iter().filter(|s| s.is_terminal()) {
            for to in ALL_EXEC {
                assert!(!from.can_move_to(to), "{from:?} moved to {to:?}");
            }
        }
    }
}
