//! Turning what an engine produced into something the person has.
//!
//! The engine finishing and the job finishing are different events, and this is
//! what happens between them. Rust decides where the audio goes before anything
//! is asked to make it, looks at the result, checks that the voice it was made
//! with is still one the person has, and only then does the file move into
//! place and the take exist.
//!
//! The order is chosen so that a crash anywhere in it can be finished later
//! rather than started again. There is no atomicity between a filesystem and a
//! database and none is claimed: what there is instead is a record of what the
//! attempt was told to produce, written before the engine sees the request, so
//! that a restart can always tell which of the steps have happened.

use rusqlite::{params, OptionalExtension};

use yarngo_core::{Execution, Job};

use crate::jobs::{write_execution, write_job};
use crate::{Result, Store};

/// What an attempt was asked to produce, and — once it has — what it did.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub execution_id: String,
    pub clip_id: String,
    pub staged_path: String,
    pub audio_seconds: Option<f64>,
    pub generated_seconds: Option<f64>,
    pub seed: Option<i64>,
    pub sample_rate: Option<i64>,
    /// Set when the engine reported. Absent means the file on the disk, if
    /// there is one, may be half-written.
    pub produced_at: Option<String>,
}

impl Output {
    /// The names the take and its audio will have. Derived from the attempt
    /// rather than from the clock, so that finishing an interrupted publication
    /// writes the same rows it would have written, and writes them once.
    pub fn take_id(&self) -> String {
        format!("take-{}", self.execution_id)
    }

    pub fn asset_id(&self) -> String {
        format!("generated_clip:{}", self.execution_id)
    }
}

impl Store {
    /// Record where this attempt's audio will go, before the engine is asked
    /// for it. What is on the disk afterwards is then always something the
    /// database expected.
    pub fn intend_output(&self, execution_id: &str, clip_id: &str, path: &str) -> Result<()> {
        self.raw().execute(
            "INSERT INTO execution_outputs (execution_id, clip_id, staged_path)
             VALUES (?1, ?2, ?3)",
            params![execution_id, clip_id, path],
        )?;
        Ok(())
    }

    /// The engine finished this attempt. Its own state and what it produced are
    /// written together: an execution recorded as complete with no account of
    /// what it made would leave a file nothing could identify.
    pub fn record_output(
        &mut self,
        job: &Job,
        execution: &Execution,
        produced: &Produced,
        at: &str,
    ) -> Result<()> {
        let transaction = self.raw_mut().transaction()?;
        transaction.execute(
            "UPDATE execution_outputs
                SET audio_seconds = ?2, generated_seconds = ?3, seed = ?4,
                    sample_rate = ?5, produced_at = ?6
              WHERE execution_id = ?1",
            params![
                execution.id,
                produced.audio_seconds,
                produced.generated_seconds,
                produced.seed,
                produced.sample_rate,
                at
            ],
        )?;
        write_execution(&transaction, execution, at)?;
        write_job(&transaction, job, at)?;
        transaction.commit()?;
        Ok(())
    }

    /// What the engine says it made.
    pub fn output_of(&self, execution_id: &str) -> Result<Option<Output>> {
        Ok(self
            .raw()
            .query_row(
                "SELECT clip_id, staged_path, audio_seconds, generated_seconds,
                        seed, sample_rate, produced_at
                   FROM execution_outputs WHERE execution_id = ?1",
                params![execution_id],
                |row| {
                    Ok(Output {
                        execution_id: execution_id.to_string(),
                        clip_id: row.get(0)?,
                        staged_path: row.get(1)?,
                        audio_seconds: row.get(2)?,
                        generated_seconds: row.get(3)?,
                        seed: row.get(4)?,
                        sample_rate: row.get(5)?,
                        produced_at: row.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// Whether the voice this clip was made with still permits a take to be
    /// added to it.
    ///
    /// A clip made with the model's own voice has nobody's recording behind it
    /// and is never barred. A clip made with someone's voice is barred the
    /// moment that voice stops being one the person has: work already running
    /// when they asked for it to go must not land afterwards.
    pub fn publication_permitted(&self, clip_id: &str) -> Result<bool> {
        let voice: Option<String> = self
            .raw()
            .query_row(
                "SELECT r.voice_id FROM clips c
                   JOIN voice_revisions r ON r.id = c.voice_revision_id
                  WHERE c.id = ?1",
                params![clip_id],
                |row| row.get(0),
            )
            .optional()?;
        match voice {
            Some(voice) => self.voice_usable(&voice),
            None => Ok(true),
        }
    }

    /// Commit the take. The audio is already where `path` says; this is the
    /// step that makes it the person's.
    ///
    /// One transaction for the asset, the take and the job, because a take
    /// pointing at an asset that does not exist and a job that finished without
    /// one are both worse than doing it again.
    ///
    /// Refuses, having written nothing, if the voice is no longer one the
    /// person has. The caller checks that too, because it has a file to remove
    /// and a job to settle — but the barrier is here as well, where the row
    /// would actually be written, so that forgetting to check cannot be what
    /// puts someone's deleted voice back in front of them.
    pub fn publish_take(
        &mut self,
        output: &Output,
        path: &str,
        job: &Job,
        execution: &Execution,
        at: &str,
    ) -> Result<bool> {
        if !self.publication_permitted(&output.clip_id)? {
            return Ok(false);
        }
        let transaction = self.raw_mut().transaction()?;
        transaction.execute(
            "INSERT INTO assets (id, kind, path, state, created_at)
             VALUES (?1, 'generated_clip', ?2, 'active', ?3)",
            params![output.asset_id(), path, at],
        )?;
        transaction.execute(
            "INSERT INTO clip_takes (id, clip_id, audio_asset_id, audio_seconds,
                                     generated_seconds, seed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                output.take_id(),
                output.clip_id,
                output.asset_id(),
                output.audio_seconds,
                output.generated_seconds,
                output.seed,
                at
            ],
        )?;
        write_execution(&transaction, execution, at)?;
        write_job(&transaction, job, at)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Whether this attempt's take is already the person's.
    pub fn take_exists(&self, execution_id: &str) -> Result<bool> {
        let take = format!("take-{execution_id}");
        Ok(self
            .raw()
            .query_row(
                "SELECT 1 FROM clip_takes WHERE id = ?1",
                params![take],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Attempts that produced audio nobody has decided about.
    ///
    /// What a restart has to finish. The engine reported, so the file is whole;
    /// the job has no outcome, so nothing has said whether that file is a take.
    /// Everything else — an attempt still running, one that was interrupted, a
    /// job already settled — is somebody else's to answer.
    pub fn unpublished_outputs(&self) -> Result<Vec<Output>> {
        let mut statement = self.raw().prepare(
            "SELECT o.execution_id, o.clip_id, o.staged_path, o.audio_seconds,
                    o.generated_seconds, o.seed, o.sample_rate, o.produced_at
               FROM execution_outputs o
               JOIN job_executions e ON e.id = o.execution_id
               JOIN jobs j           ON j.id = e.job_id
              WHERE o.produced_at IS NOT NULL
                AND e.state = 'completed'
                AND j.state NOT IN ('completed', 'failed', 'cancelled')
                AND NOT EXISTS (SELECT 1 FROM clip_takes t
                                 WHERE t.id = 'take-' || o.execution_id)
              ORDER BY o.produced_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Output {
                execution_id: row.get(0)?,
                clip_id: row.get(1)?,
                staged_path: row.get(2)?,
                audio_seconds: row.get(3)?,
                generated_seconds: row.get(4)?,
                seed: row.get(5)?,
                sample_rate: row.get(6)?,
                produced_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Staged audio for attempts that will never publish it.
    ///
    /// A job that ended without a take leaves whatever the engine wrote, and
    /// that audio was made from someone's voice. Returned so it can be removed
    /// rather than left in a directory nothing looks at again.
    pub fn abandoned_outputs(&self) -> Result<Vec<Output>> {
        let mut statement = self.raw().prepare(
            "SELECT o.execution_id, o.clip_id, o.staged_path, o.audio_seconds,
                    o.generated_seconds, o.seed, o.sample_rate, o.produced_at
               FROM execution_outputs o
               JOIN job_executions e ON e.id = o.execution_id
              WHERE NOT EXISTS (SELECT 1 FROM clip_takes t
                                 WHERE t.id = 'take-' || o.execution_id)
                AND (e.state IN ('failed', 'cancelled', 'interrupted')
                     OR EXISTS (SELECT 1 FROM jobs j
                                 WHERE j.id = e.job_id
                                   AND j.state IN ('completed', 'failed', 'cancelled')))",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Output {
                execution_id: row.get(0)?,
                clip_id: row.get(1)?,
                staged_path: row.get(2)?,
                audio_seconds: row.get(3)?,
                generated_seconds: row.get(4)?,
                seed: row.get(5)?,
                sample_rate: row.get(6)?,
                produced_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// What the engine reported making.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Produced {
    pub audio_seconds: Option<f64>,
    pub generated_seconds: Option<f64>,
    pub seed: Option<i64>,
    pub sample_rate: Option<i64>,
}
