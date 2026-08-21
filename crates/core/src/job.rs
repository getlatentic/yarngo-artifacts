//! Work Rust must be able to account for after an interruption.

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
    /// The engine stopped while this was in flight. Distinct from `Failed`:
    /// nothing about the work itself went wrong, and a retry is reasonable.
    Interrupted,
}

impl JobStatus {
    /// Whether this is the last thing that will ever be said about the job.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
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
                | (Running, Interrupted)
                // The request lost the race. Finishing is the honest outcome,
                // and the caller learns the cancel arrived too late.
                | (CancelRequested, Completed)
                | (CancelRequested, Cancelled)
                | (CancelRequested, Failed)
                | (CancelRequested, Interrupted)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::JobStatus::{self, *};

    /// Every state, so the table below can be exhaustive. The match is what
    /// keeps it honest: a new variant stops this compiling until it is listed.
    const ALL: [JobStatus; 7] = [
        Queued,
        Running,
        CancelRequested,
        Completed,
        Failed,
        Cancelled,
        Interrupted,
    ];

    #[test]
    fn every_state_appears_in_the_table() {
        for state in ALL {
            match state {
                Queued | Running | CancelRequested | Completed | Failed | Cancelled
                | Interrupted => {}
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
        (Running, Interrupted),
        (CancelRequested, Completed),
        (CancelRequested, Cancelled),
        (CancelRequested, Failed),
        (CancelRequested, Interrupted),
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

    #[test]
    fn every_terminal_state_is_reachable() {
        for terminal in ALL.into_iter().filter(|s| s.is_terminal()) {
            assert!(
                ALL.into_iter().any(|from| from.can_move_to(terminal)),
                "{terminal:?} cannot be reached"
            );
        }
    }
}
