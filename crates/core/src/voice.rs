//! The lifecycle of a voice, which outlives its recording.
//!
//! A deleted voice keeps a row. Clips made with it point at the revision they
//! were spoken in, and that reference is what lets them say so afterwards — a
//! clip whose voice was nulled or pointed at the default would be claiming the
//! model read it. The row stays; the recording and the conditioning go.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceStatus {
    Active,
    /// Deletion is authoritative from here: no new synthesis or conditioning
    /// may reference the voice, and the work of removing its assets has begun.
    DeletionPending,
    Deleted,
}

/// What asking to delete a voice means, given where it already is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionRequest {
    /// Start the work.
    Begin,
    /// Already under way. Join the existing job rather than starting a second.
    AlreadyUnderWay,
    /// Already gone. Succeed against the existing tombstone, and do not write
    /// another audit event for a deletion that happened once.
    AlreadyDeleted,
}

impl VoiceStatus {
    pub fn can_move_to(self, next: Self) -> bool {
        use VoiceStatus::*;
        matches!((self, next), (Active, DeletionPending) | (DeletionPending, Deleted))
    }

    /// Whether the voice may be named by new work. False from the moment
    /// deletion becomes authoritative, not from the moment it finishes.
    pub fn usable_for_new_work(self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn deletion_request(self) -> DeletionRequest {
        match self {
            Self::Active => DeletionRequest::Begin,
            Self::DeletionPending => DeletionRequest::AlreadyUnderWay,
            Self::Deleted => DeletionRequest::AlreadyDeleted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VoiceStatus::{self, *};
    use super::DeletionRequest;

    const ALL: [VoiceStatus; 3] = [Active, DeletionPending, Deleted];

    #[test]
    fn every_state_appears_in_the_table() {
        for state in ALL {
            match state {
                Active | DeletionPending | Deleted => {}
            }
        }
    }

    #[test]
    fn only_the_listed_moves_are_allowed() {
        let allowed = [(Active, DeletionPending), (DeletionPending, Deleted)];
        for from in ALL {
            for to in ALL {
                assert_eq!(
                    from.can_move_to(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    /// Deletion is authoritative from the moment it is asked for, not from the
    /// moment the files are gone. Otherwise a generation started during the
    /// deletion would speak in a voice that is being removed.
    #[test]
    fn a_voice_stops_being_usable_before_it_is_gone() {
        assert!(Active.usable_for_new_work());
        assert!(!DeletionPending.usable_for_new_work());
        assert!(!Deleted.usable_for_new_work());
    }

    /// Deleting twice succeeds against the tombstone rather than failing or
    /// writing a second audit event for one deletion.
    #[test]
    fn deleting_is_idempotent() {
        assert_eq!(Active.deletion_request(), DeletionRequest::Begin);
        assert_eq!(DeletionPending.deletion_request(), DeletionRequest::AlreadyUnderWay);
        assert_eq!(Deleted.deletion_request(), DeletionRequest::AlreadyDeleted);
    }

    /// There is no way back. A deleted voice cannot be reactivated, because
    /// the recording it was derived from is gone.
    #[test]
    fn deletion_is_one_way() {
        assert!(!Deleted.can_move_to(Active));
        assert!(!DeletionPending.can_move_to(Active));
    }
}
