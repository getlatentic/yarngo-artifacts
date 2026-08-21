//! Deleting a voice, which is several things that cannot happen at once.
//!
//! A row, a file, and a cache in another process. No transaction spans those,
//! so the order is chosen so that an interruption anywhere leaves something a
//! later run can finish, and never leaves the voice usable when the person has
//! been told it is going.
//!
//! The barrier comes first for that reason. From the moment `deletion_pending`
//! commits, nothing new may use the voice — before its recording is gone,
//! before the engine has forgotten it. Deleting the file first and the row
//! second would leave a window where the voice is offered and cannot speak; the
//! other way round leaves a window where it is refused and could have. Refusing
//! is the right way to be wrong.
//!
//! The voice is removed as something usable, not erased as something that
//! happened. Clips made with it keep pointing at the revision that made them.

use rusqlite::{params, OptionalExtension};
use yarngo_core::{DurableJobKind, Job, JobStatus, VoiceStatus};

use crate::{Result, Store, StoreError};

/// What the engine could say about forgetting a voice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalidation {
    /// Found and removed. The scope is every voice, because the cache is keyed
    /// on the waveform and cannot evict one speaker.
    Cleared { entries_removed: u64 },
    /// Nothing to remove: never conditioned, or the engine restarted since.
    /// A success, and the common one.
    AlreadyEmpty,
    /// A model is loaded and its cache cannot be found. The engine cannot show
    /// the voice is unreachable, so the process holding it has to end.
    UnsupportedLayout,
}

/// The engine, as deletion needs it. A trait so the sequence can be tested
/// against an engine that fails in each of its ways without one being present.
pub trait Conditioning {
    fn invalidate(&mut self) -> std::result::Result<Invalidation, String>;
    /// End the process holding the conditioning.
    ///
    /// This is the one that matters. The question deletion has to answer is
    /// whether the old process can still speak in the voice, and a process that
    /// is gone cannot. Whether anything takes its place is a different
    /// question, asked separately.
    fn terminate(&mut self) -> std::result::Result<(), String>;
    /// Start a replacement. Its failure leaves the application without an
    /// engine, which is a problem — but not one that justifies keeping a
    /// recording the person asked to delete.
    fn restart(&mut self) -> std::result::Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Deleted {
        files_removed: usize,
        /// The old process had to be ended to be sure it had forgotten.
        engine_terminated: bool,
        /// And nothing took its place. The deletion is complete regardless;
        /// this is reported so the application can say the engine is down
        /// rather than discovering it at the next generation.
        engine_unavailable: bool,
    },
    /// Already gone. Answered against the tombstone rather than failing, and
    /// without writing a second record of one deletion.
    AlreadyDeleted,
    /// The engine could not be made to forget and could not be ended, so
    /// nothing here can show the voice is unreachable. The recording stays and
    /// the voice stays `deletion_pending`: refused for new work, and
    /// finishable once the engine can be dealt with.
    Blocked { reason: String },
}

impl Store {
    fn voice_status(&self, voice_id: &str) -> Result<Option<VoiceStatus>> {
        let text: Option<String> = self
            .raw()
            .query_row(
                "SELECT status FROM voice_profiles WHERE id = ?1",
                params![voice_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match text.as_deref() {
            Some("active") => Some(VoiceStatus::Active),
            Some("deletion_pending") => Some(VoiceStatus::DeletionPending),
            Some("deleted") => Some(VoiceStatus::Deleted),
            Some(other) => return Err(StoreError::Invalid(format!("voice status {other:?}"))),
            None => None,
        })
    }

    /// Whether anything new may name this voice. False from the moment deletion
    /// becomes authoritative, which is before any of it has happened.
    pub fn voice_usable(&self, voice_id: &str) -> Result<bool> {
        Ok(self
            .voice_status(voice_id)?
            .is_some_and(VoiceStatus::usable_for_new_work))
    }

    /// Raise the barrier and record the work. One transaction: a voice that is
    /// refused for new work without a job to finish the removal would be a
    /// voice that never comes back and never goes.
    pub fn begin_voice_deletion(&mut self, voice_id: &str, job_id: &str, at: &str) -> Result<bool> {
        let Some(status) = self.voice_status(voice_id)? else {
            return Err(StoreError::Invalid(format!("no voice {voice_id}")));
        };
        if status != VoiceStatus::Active {
            // Already under way or already done. Neither writes a second job.
            return Ok(false);
        }

        let transaction = self.raw_mut().transaction()?;
        transaction.execute(
            "UPDATE voice_profiles SET status = 'deletion_pending' WHERE id = ?1",
            params![voice_id],
        )?;
        // Marked before anything is unlinked, so an interruption leaves the
        // intent recorded against the file rather than only in the job.
        transaction.execute(
            "UPDATE assets SET state = 'deletion_pending'
              WHERE state = 'active'
                AND id IN (SELECT source_asset_id FROM voice_revisions WHERE voice_id = ?1)",
            params![voice_id],
        )?;
        transaction.execute(
            "INSERT INTO jobs (id, kind, state, target_id, created_at, updated_at)
             VALUES (?1, 'voice_delete', 'queued', ?2, ?3, ?3)",
            params![job_id, voice_id, at],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Every file this voice put on the disk, from the records rather than from
    /// a guess at its name. A voice with more than one revision has more than
    /// one recording, and the obvious filename is only the first.
    pub fn voice_source_files(&self, voice_id: &str) -> Result<Vec<(String, String)>> {
        let mut statement = self.raw().prepare(
            "SELECT a.id, a.path
               FROM voice_revisions r
               JOIN assets a ON a.id = r.source_asset_id
              WHERE r.voice_id = ?1 AND a.state != 'deleted'",
        )?;
        let rows = statement.query_map(params![voice_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Carry a deletion from the barrier to the tombstone.
    ///
    /// Safe to call again on a voice that is part-way through: every step is
    /// written down, and each one is skipped if it has already happened. That
    /// is what makes a crash recoverable rather than a state to unpick by hand.
    pub fn finish_voice_deletion(
        &mut self,
        voice_id: &str,
        job_id: &str,
        engine: &mut dyn Conditioning,
        at: &str,
    ) -> Result<Outcome> {
        match self.voice_status(voice_id)? {
            Some(VoiceStatus::Deleted) => return Ok(Outcome::AlreadyDeleted),
            Some(VoiceStatus::DeletionPending) => {}
            Some(VoiceStatus::Active) => {
                return Err(StoreError::Invalid(format!(
                    "voice {voice_id} is still active; deletion was never begun"
                )))
            }
            None => return Err(StoreError::Invalid(format!("no voice {voice_id}"))),
        }

        let mut job = self
            .load_job(job_id)?
            .unwrap_or_else(|| Job::queued(job_id, DurableJobKind::VoiceDelete));
        if job.state() == JobStatus::Queued {
            job.dispatch(format!("{job_id}/1"));
            self.save_progress(&job, None, at)?;
        }

        // The engine forgets first. Removing the recording while the derived
        // conditioning is still resident would leave the part of the voice that
        // can actually speak.
        let mut terminated = false;
        let mut unavailable = false;
        match engine.invalidate() {
            Ok(Invalidation::Cleared { .. }) | Ok(Invalidation::AlreadyEmpty) => {}
            Ok(Invalidation::UnsupportedLayout) | Err(_) => {
                // It cannot show the voice is unreachable, so the process
                // holding it has to stop being. That, and only that, is what
                // makes the conditioning unreachable — a process that no longer
                // exists cannot speak in anybody's voice.
                if let Err(reason) = engine.terminate() {
                    return Ok(Outcome::Blocked { reason });
                }
                terminated = true;
                // The replacement is a separate concern. Refusing to finish the
                // deletion because no engine started would mean keeping a
                // recording the person asked to remove, in exchange for
                // nothing: the old one is already gone.
                unavailable = engine.restart().is_err();
            }
        }

        let mut removed = 0;
        for (asset_id, path) in self.voice_source_files(voice_id)? {
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                // Already gone counts as done: this may be a second run.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.raw().execute(
                        "UPDATE assets SET state = 'deletion_failed' WHERE id = ?1",
                        params![asset_id],
                    )?;
                    return Ok(Outcome::Blocked {
                        reason: format!("could not remove {path}: {error}"),
                    });
                }
            }
            self.raw().execute(
                "UPDATE assets SET state = 'deleted', deleted_at = ?2 WHERE id = ?1",
                params![asset_id, at],
            )?;
        }

        let transaction = self.raw_mut().transaction()?;
        transaction.execute(
            "UPDATE voice_profiles SET status = 'deleted', deleted_at = ?2 WHERE id = ?1",
            params![voice_id, at],
        )?;
        transaction.execute(
            "UPDATE jobs SET state = 'completed', updated_at = ?2, completed_at = ?2
              WHERE id = ?1",
            params![job_id, at],
        )?;
        transaction.commit()?;

        Ok(Outcome::Deleted {
            files_removed: removed,
            engine_terminated: terminated,
            engine_unavailable: unavailable,
        })
    }

    /// Deletions the last run did not finish. Found from the records — a voice
    /// held at the barrier with a job that never completed — rather than from
    /// anything remembered in memory, which is what a crash takes with it.
    pub fn unfinished_voice_deletions(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.raw().prepare(
            "SELECT j.target_id, j.id
               FROM jobs j
               JOIN voice_profiles v ON v.id = j.target_id
              WHERE j.kind = 'voice_delete'
                AND j.state NOT IN ('completed', 'failed', 'cancelled')
                AND v.status = 'deletion_pending'",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Record a finished take against a clip, unless the voice it was made with
    /// has been taken away since it started.
    ///
    /// The generation may well have completed: cancellation is cooperative and
    /// the engine can reach the end before it notices. What it cannot do is
    /// become a clip the person can play in a voice they deleted.
    pub fn commit_take(
        &mut self,
        clip_id: &str,
        take_id: &str,
        asset_id: &str,
        path: &str,
        at: &str,
    ) -> Result<bool> {
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
        if let Some(voice) = voice {
            if !self.voice_usable(&voice)? {
                return Ok(false);
            }
        }

        let transaction = self.raw_mut().transaction()?;
        transaction.execute(
            "INSERT INTO assets (id, kind, path, state, created_at)
             VALUES (?1, 'generated_clip', ?2, 'active', ?3)",
            params![asset_id, path, at],
        )?;
        transaction.execute(
            "INSERT INTO clip_takes (id, clip_id, audio_asset_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![take_id, clip_id, asset_id, at],
        )?;
        transaction.commit()?;
        Ok(true)
    }
}
