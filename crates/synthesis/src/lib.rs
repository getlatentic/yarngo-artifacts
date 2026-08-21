//! Generation, from what the person asked for to something they have.
//!
//! The engine writes a file. That is all it does: it is told where to write,
//! it is not told what the file is for, and it records nothing. Everything that
//! makes that file a take happens here, afterwards, and can be told apart from
//! the generating — which is what lets a crash in the middle be finished rather
//! than repeated, and a voice deleted in the middle be honoured rather than
//! raced.
//!
//! The order is the whole design:
//!
//! 1. the durable intent exists before anything external is asked
//! 2. the engine is told where to write, and writes only there
//! 3. the engine finishing ends the attempt and settles nothing else
//! 4. the file is read, the voice is checked, and only then is there a take
//!
//! Anything that disappears partway leaves a record saying which of those had
//! happened, so a restart can carry on from there.

pub mod audio;
pub mod layout;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use speech_engine::protocol::Connection;
use yarngo_core::{DurableJobKind, Execution, Job, Rejection};
use yarngo_store::takes::{Output, Produced};
use yarngo_store::Store;

pub use layout::Layout;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] yarngo_store::StoreError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("no record of what {0} was asked to produce")]
    NoIntent(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The recording to speak with. Absent means the model's own voice.
#[derive(Clone, Debug)]
pub struct Reference {
    pub audio: String,
    pub text: Option<String>,
}

/// What the person asked for.
#[derive(Clone, Debug)]
pub struct Request {
    pub clip_id: String,
    pub text: String,
    pub reference: Option<Reference>,
    pub model: Option<String>,
    pub seed: Option<i64>,
}

/// How a generation ended, from the application's side rather than the
/// engine's. The engine's own outcome is a step inside these.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Published { take_id: String, path: PathBuf },
    /// The engine produced audio and the application would not have it.
    Rejected { reason: Rejection, detail: String },
    /// The engine could not produce it.
    Failed { detail: String },
    /// Stopped because it was asked to be.
    Cancelled,
    /// The engine went. The job is back in the queue for another one.
    Interrupted,
}

/// One generation, carried the whole way.
pub struct Synthesis<'a> {
    pub store: &'a mut Store,
    pub layout: &'a Layout,
    pub session_id: &'a str,
}

impl<'a> Synthesis<'a> {
    /// Everything durable, written before the engine is asked for anything.
    ///
    /// The job is left queued: it has not been dispatched, and a job recorded
    /// as running before anything is running is a job a restart would wait for.
    pub fn begin(
        &mut self,
        job_id: &str,
        execution_id: &str,
        request: &Request,
        at: &str,
    ) -> Result<Pending> {
        self.layout.prepare()?;
        let job = Job::queued(job_id, DurableJobKind::Synthesis);
        self.store.insert_job(&job, Some(&request.clip_id), at)?;
        let execution = Execution::queued(execution_id, job_id, self.session_id);
        // The attempt is written before what it will produce, because the one
        // refers to the other, and before either is asked for, because that is
        // the whole claim: nothing external happens that a record does not
        // already account for.
        self.store.save_progress(&job, Some(&execution), at)?;
        let staged = self.staged_for(&execution, &request.clip_id)?;
        Ok(Pending { job, execution, staged, request: request.clone() })
    }

    /// The whole of it, for a caller with nothing to do in between.
    pub fn generate(
        &mut self,
        pending: Pending,
        engine: &Connection,
        patience: Duration,
        at: &str,
    ) -> Result<Outcome> {
        match self.run(pending, engine, patience, at)? {
            Finished::Produced { mut job, execution, output } => {
                self.publish(&mut job, &execution, &output, at)
            }
            Finished::Settled(outcome) => Ok(outcome),
        }
    }

    /// A second attempt at a job that is back in the queue.
    ///
    /// The same intent, so nothing new is recorded for it: what is new is the
    /// attempt, and where that attempt will write. Each attempt has its own
    /// staged name, so a lost engine still holding a file cannot have that file
    /// published as its replacement's work.
    pub fn begin_again(
        &mut self,
        job: &Job,
        execution_id: &str,
        request: &Request,
        _at: &str,
    ) -> Result<Pending> {
        self.layout.prepare()?;
        let execution = Execution::queued(execution_id, &job.id, self.session_id);
        self.store.save_progress(job, Some(&execution), _at)?;
        let staged = self.staged_for(&execution, &request.clip_id)?;
        Ok(Pending { job: job.clone(), execution, staged, request: request.clone() })
    }

    /// Where this attempt will write, decided here and written down.
    fn staged_for(&self, execution: &Execution, clip_id: &str) -> Result<PathBuf> {
        let staged = self.layout.staged(&execution.id);
        self.store
            .intend_output(&execution.id, clip_id, &staged.to_string_lossy())?;
        Ok(staged)
    }

    /// Hand it to the engine and wait for the attempt to end.
    ///
    /// Returns without having settled the job when the engine produced audio:
    /// that is the point of the split. What the engine wrote is on the disk and
    /// recorded, and nothing has said whether it is a take.
    pub fn run(&mut self, pending: Pending, engine: &Connection, patience: Duration, at: &str) -> Result<Finished> {
        let Pending { mut job, mut execution, staged, request } = pending;

        // Recorded as running before the request goes, never after. A crash the
        // other way round leaves an engine working on something the database
        // thinks is still waiting, and a second attempt would generate it twice.
        job.dispatch(&execution.id);
        execution.dispatched();
        self.store.save_progress(&job, Some(&execution), at)?;

        let reply = engine.request("synthesis.generate", params(&request, &execution, &staged), patience);

        match reply {
            Ok(result) if result.get("state").and_then(Value::as_str) == Some("cancelled") => {
                job.execution_cancelled(&mut execution);
                self.store.save_progress(&job, Some(&execution), at)?;
                remove(&staged);
                Ok(Finished::Settled(Outcome::Cancelled))
            }
            Ok(result) => {
                // Terminal for the attempt, and a claim about nothing else. The
                // job stays open until the file has been looked at.
                job.execution_completed(&mut execution);
                self.store.record_output(&job, &execution, &produced(&result), at)?;
                let output = self.intent(&execution.id)?;
                Ok(Finished::Produced { job, execution, output })
            }
            Err(failure) if engine.has_ended() => {
                // Nobody is doing the work and nobody decided it should not be
                // done. The job goes back to waiting for an engine that can.
                let _ = failure;
                job.execution_interrupted(&mut execution);
                self.store.save_progress(&job, Some(&execution), at)?;
                remove(&staged);
                Ok(Finished::Settled(Outcome::Interrupted))
            }
            Err(failure) => {
                job.execution_failed(&mut execution);
                self.store.save_progress(&job, Some(&execution), at)?;
                remove(&staged);
                Ok(Finished::Settled(Outcome::Failed { detail: failure.to_string() }))
            }
        }
    }

    /// Look at what the engine made, and decide whether it is a take.
    ///
    /// This is what finishes a synthesis job. Everything it checks is checked
    /// now rather than when the work started: the file is read as it stands,
    /// and the voice is asked about as it stands, because both can have changed
    /// while the engine was busy.
    pub fn publish(&mut self, job: &mut Job, execution: &Execution, output: &Output, at: &str) -> Result<Outcome> {
        let staged = Path::new(&output.staged_path);
        let destination = self.layout.take(&output.clip_id, &output.take_id());
        // Either the file is where the engine was told to write it, or a
        // previous run of this already moved it and stopped before the take
        // existed. Both are the same situation: audio in hand, nothing decided.
        let source = if staged.exists() { staged.to_path_buf() } else { destination.clone() };

        if let Err(unusable) = audio::inspect(&source, output.audio_seconds) {
            return self.reject(job, execution, output, Rejection::Unusable, unusable.to_string(), at);
        }
        if !self.store.publication_permitted(&output.clip_id)? {
            return self.reject(
                job,
                execution,
                output,
                Rejection::VoiceDeleted,
                "the voice was deleted while this was generating".into(),
                at,
            );
        }

        if source != destination {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&source, &destination)?;
        }

        job.take_published(execution);
        let committed = self
            .store
            .publish_take(output, &destination.to_string_lossy(), job, execution, at)?;
        if !committed {
            // The deletion landed between the check and the write. The store
            // wrote nothing, so the persisted job is still the truth; take it
            // back and settle it the other way.
            *job = self
                .store
                .load_job(&execution.job_id)?
                .ok_or_else(|| Error::NoIntent(execution.job_id.clone()))?;
            remove(&destination);
            return self.reject(
                job,
                execution,
                output,
                Rejection::VoiceDeleted,
                "the voice was deleted while this was being published".into(),
                at,
            );
        }
        Ok(Outcome::Published { take_id: output.take_id(), path: destination })
    }

    fn reject(
        &mut self,
        job: &mut Job,
        execution: &Execution,
        output: &Output,
        reason: Rejection,
        detail: String,
        at: &str,
    ) -> Result<Outcome> {
        // The audio goes first. It was made from someone's voice, and a job
        // recorded as settled with the file still there is the one order that
        // leaves it behind.
        remove(Path::new(&output.staged_path));
        remove(&self.layout.take(&output.clip_id, &output.take_id()));
        job.take_rejected(execution, reason);
        self.store.save_progress(job, Some(execution), at)?;
        Ok(Outcome::Rejected { reason, detail })
    }

    fn intent(&self, execution_id: &str) -> Result<Output> {
        self.store
            .output_of(execution_id)?
            .ok_or_else(|| Error::NoIntent(execution_id.to_string()))
    }

    /// Finish what a crash left half-decided, and clear away what will never be
    /// decided at all.
    ///
    /// Called at startup, after sessions have been reconciled. Every attempt
    /// here produced audio the engine finished writing and that nothing has
    /// ruled on, so each one gets exactly the ruling it would have got.
    pub fn reconcile(&mut self, at: &str) -> Result<Vec<(String, Outcome)>> {
        let mut settled = Vec::new();
        for output in self.store.unpublished_outputs()? {
            let Some(mut job) = self.store.load_job(&job_of(self.store, &output.execution_id)?)? else {
                continue;
            };
            let Some(execution) = restored(self.store, &output.execution_id)? else {
                continue;
            };
            let outcome = self.publish(&mut job, &execution, &output, at)?;
            settled.push((output.execution_id.clone(), outcome));
        }
        for abandoned in self.store.abandoned_outputs()? {
            remove(Path::new(&abandoned.staged_path));
        }
        Ok(settled)
    }
}

/// How an attempt ended.
pub enum Finished {
    /// There is audio, and nothing has ruled on it. The job is deliberately
    /// still open: [`Synthesis::publish`] is what closes it.
    Produced {
        job: Job,
        execution: Execution,
        output: Output,
    },
    /// Nothing was produced, so there was nothing left to decide and the job
    /// is settled already.
    Settled(Outcome),
}

/// Everything written down, waiting for an engine.
pub struct Pending {
    pub job: Job,
    pub execution: Execution,
    pub staged: PathBuf,
    request: Request,
}

fn params(request: &Request, execution: &Execution, staged: &Path) -> Value {
    let mut params = json!({
        "job_id": execution.job_id,
        "execution_id": execution.id,
        "text": request.text,
        "output_path": staged.to_string_lossy(),
        "model": request.model,
        "seed": request.seed,
    });
    if let Some(reference) = &request.reference {
        params["reference_audio"] = json!(reference.audio);
        params["reference_text"] = json!(reference.text);
    }
    params
}

fn produced(result: &Value) -> Produced {
    Produced {
        audio_seconds: result.get("audio_s").and_then(Value::as_f64),
        generated_seconds: result.get("gen_s").and_then(Value::as_f64),
        seed: result.get("seed").and_then(Value::as_i64),
        sample_rate: result.get("sample_rate").and_then(Value::as_i64),
    }
}

/// Best effort, deliberately. A file that cannot be removed must not stop a job
/// being settled: the record of what happened is worth more than the tidying,
/// and the sweep at the next start will find it again.
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn job_of(store: &Store, execution_id: &str) -> Result<String> {
    store
        .execution_job(execution_id)?
        .ok_or_else(|| Error::NoIntent(execution_id.to_string()))
}

fn restored(store: &Store, execution_id: &str) -> Result<Option<Execution>> {
    Ok(store.restore_execution(execution_id)?)
}
