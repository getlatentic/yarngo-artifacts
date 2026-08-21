//! What the application knows, independent of how it is drawn or stored.
//!
//! The lifecycles here are the ones Rust has to be able to reason about after
//! an interruption: which jobs were in flight, which voices were half deleted,
//! what a late message from the engine is allowed to change. They are written
//! as types with an exhaustive transition table rather than as prose, because
//! prose does not fail a build when a state is added and a case is forgotten.
//!
//! Two kinds of work are deliberately not modelled here. Loading a model and
//! conditioning a voice are [`EngineOperation`]s: they run in the sidecar, and
//! their results die with it, so recording them as durable state would be
//! recording something that stops being true without anyone noticing. Their
//! *inputs* — the voice revision, the recording, the consent it was given
//! under — are durable, and belong to the store.

pub mod execution;
pub mod job;
pub mod voice;

pub use execution::{Applied, Execution, Job, Observation};
pub use job::{DurableJobKind, ExecutionStatus, JobStatus};
pub use voice::VoiceStatus;

/// Work the engine performs whose result cannot outlive the engine.
///
/// No durable row: a restarted sidecar has loaded nothing and conditioned
/// nothing, so the truthful record after a restart is an absence, which is
/// what no row already means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineOperation {
    LoadModel,
    ConditionVoice,
}
