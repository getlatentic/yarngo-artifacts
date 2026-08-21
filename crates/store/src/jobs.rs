//! Jobs, the attempts at them, and the engine sessions those attempts ran in.
//!
//! The three exist together because recovery needs all of them. After a restart
//! the application must decide what was in flight, and the answer has to be a
//! fact rather than an inference: an attempt still marked running by a session
//! that has ended was interrupted, and nothing about clocks or timeouts enters
//! into it.

use rusqlite::{params, OptionalExtension, Transaction};
use yarngo_core::{DurableJobKind, Execution, ExecutionStatus, Job, JobStatus};

use crate::{Result, Store, StoreError};

/// What an engine session's ending resolved.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Attempts that were still running when their engine went.
    pub interrupted: Vec<String>,
    /// Jobs returned to the queue, because nobody is doing them and nobody
    /// decided they should not be done.
    pub requeued: Vec<String>,
    /// Jobs finished as cancelled: stopping had been asked for, and the engine
    /// going is how it took effect.
    pub cancelled: Vec<String>,
}

fn job_state(text: &str) -> Result<JobStatus> {
    Ok(match text {
        "queued" => JobStatus::Queued,
        "running" => JobStatus::Running,
        "cancel_requested" => JobStatus::CancelRequested,
        "completed" => JobStatus::Completed,
        "failed" => JobStatus::Failed,
        "cancelled" => JobStatus::Cancelled,
        other => return Err(StoreError::Invalid(format!("unknown job state {other:?}"))),
    })
}

fn job_state_text(state: JobStatus) -> &'static str {
    match state {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::CancelRequested => "cancel_requested",
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn kind_text(kind: DurableJobKind) -> &'static str {
    match kind {
        DurableJobKind::Synthesis => "synthesis",
        DurableJobKind::ModelInstall => "model_install",
        DurableJobKind::ModelDelete => "model_delete",
        DurableJobKind::VoiceDelete => "voice_delete",
    }
}

fn kind(text: &str) -> Result<DurableJobKind> {
    Ok(match text {
        "synthesis" => DurableJobKind::Synthesis,
        "model_install" => DurableJobKind::ModelInstall,
        "model_delete" => DurableJobKind::ModelDelete,
        "voice_delete" => DurableJobKind::VoiceDelete,
        other => return Err(StoreError::Invalid(format!("unknown job kind {other:?}"))),
    })
}

fn execution_state_text(state: ExecutionStatus) -> &'static str {
    match state {
        ExecutionStatus::Queued => "queued",
        ExecutionStatus::Running => "running",
        ExecutionStatus::Completed => "completed",
        ExecutionStatus::Failed => "failed",
        ExecutionStatus::Cancelled => "cancelled",
        ExecutionStatus::Interrupted => "interrupted",
    }
}

fn execution_state(text: &str) -> Result<ExecutionStatus> {
    Ok(match text {
        "queued" => ExecutionStatus::Queued,
        "running" => ExecutionStatus::Running,
        "completed" => ExecutionStatus::Completed,
        "failed" => ExecutionStatus::Failed,
        "cancelled" => ExecutionStatus::Cancelled,
        "interrupted" => ExecutionStatus::Interrupted,
        other => return Err(StoreError::Invalid(format!("unknown execution state {other:?}"))),
    })
}

/// Write what an attempt now says. Shared, so that anything settling a job as
/// part of a larger change writes the same row the same way.
pub(crate) fn write_execution(
    transaction: &Transaction<'_>,
    execution: &Execution,
    at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO job_executions (id, job_id, session_id, state, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
             state = excluded.state,
             finished_at = CASE
                 WHEN excluded.state IN ('completed','failed','cancelled','interrupted')
                 THEN ?5 ELSE job_executions.finished_at END",
        params![
            execution.id,
            execution.job_id,
            execution.session_id,
            execution_state_text(execution.state()),
            at
        ],
    )?;
    Ok(())
}

pub(crate) fn write_job(transaction: &Transaction<'_>, job: &Job, at: &str) -> Result<()> {
    transaction.execute(
        "UPDATE jobs SET state = ?2, current_execution_id = ?3, updated_at = ?4,
                         completed_at = CASE WHEN ?2 IN ('completed','failed','cancelled')
                                             THEN ?4 ELSE completed_at END
         WHERE id = ?1",
        params![
            job.id,
            job_state_text(job.state()),
            job.current_execution(),
            at
        ],
    )?;
    Ok(())
}

impl Store {
    pub fn open_session(&self, id: &str, backend: &str, at: &str) -> Result<()> {
        self.raw().execute(
            "INSERT INTO engine_sessions (id, backend, started_at) VALUES (?1, ?2, ?3)",
            params![id, backend, at],
        )?;
        Ok(())
    }

    pub fn end_session(&self, id: &str, at: &str, reason: &str) -> Result<()> {
        self.raw().execute(
            "UPDATE engine_sessions SET ended_at = ?2, exit_reason = ?3 WHERE id = ?1",
            params![id, at, reason],
        )?;
        Ok(())
    }

    pub fn insert_job(&self, job: &Job, target: Option<&str>, at: &str) -> Result<()> {
        self.raw().execute(
            "INSERT INTO jobs (id, kind, state, target_id, current_execution_id,
                               retry_of_job_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                job.id,
                kind_text(job.kind),
                job_state_text(job.state()),
                target,
                job.current_execution(),
                job.retry_of,
                at
            ],
        )?;
        Ok(())
    }

    /// Write what a job and its attempt now say, together. Separately, a crash
    /// between the two leaves a job pointing at an attempt that disagrees.
    pub fn save_progress(
        &mut self,
        job: &Job,
        execution: Option<&Execution>,
        at: &str,
    ) -> Result<()> {
        let transaction = self.raw_mut().transaction()?;
        if let Some(execution) = execution {
            write_execution(&transaction, execution, at)?;
        }
        write_job(&transaction, job, at)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load_job(&self, id: &str) -> Result<Option<Job>> {
        let row = self
            .raw()
            .query_row(
                "SELECT kind, state, current_execution_id, retry_of_job_id FROM jobs WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((kind_text, state_text, current, retry_of)) = row else {
            return Ok(None);
        };
        Ok(Some(Job::restored(
            id,
            kind(&kind_text)?,
            job_state(&state_text)?,
            current,
            retry_of,
        )))
    }

    /// Which job an attempt was for.
    pub fn execution_job(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .raw()
            .query_row(
                "SELECT job_id FROM job_executions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// An attempt read back as it was left.
    pub fn restore_execution(&self, id: &str) -> Result<Option<Execution>> {
        let row: Option<(String, String, String)> = self
            .raw()
            .query_row(
                "SELECT job_id, session_id, state FROM job_executions WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((job_id, session_id, state)) = row else {
            return Ok(None);
        };
        Ok(Some(Execution::restored(
            id,
            job_id,
            session_id,
            execution_state(&state)?,
        )))
    }

    pub fn execution_state(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .raw()
            .query_row(
                "SELECT state FROM job_executions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Settle everything an ended engine session left behind.
    ///
    /// Deterministic, and deliberately not clever: an attempt is interrupted
    /// because the session that owned it is over, full stop. The job's answer
    /// then follows from what was being asked of it — waiting for an engine, or
    /// waiting to stop.
    pub fn reconcile_ended_sessions(&mut self, at: &str) -> Result<Reconciled> {
        let orphaned: Vec<(String, String)> = {
            let mut statement = self.raw().prepare(
                "SELECT e.id, e.job_id
                   FROM job_executions e
                   JOIN engine_sessions s ON s.id = e.session_id
                  WHERE e.state IN ('queued', 'running')
                    AND s.ended_at IS NOT NULL",
            )?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };

        let mut settled = Reconciled::default();
        for (execution_id, job_id) in orphaned {
            let Some(mut job) = self.load_job(&job_id)? else { continue };
            let mut execution = Execution::started(&execution_id, &job_id, "");
            let was = job.state();
            job.execution_interrupted(&mut execution);
            self.save_progress(&job, Some(&execution), at)?;

            settled.interrupted.push(execution_id);
            match (was, job.state()) {
                (_, JobStatus::Queued) => settled.requeued.push(job_id),
                (JobStatus::CancelRequested, JobStatus::Cancelled) => {
                    settled.cancelled.push(job_id)
                }
                _ => {}
            }
        }
        Ok(settled)
    }
}
