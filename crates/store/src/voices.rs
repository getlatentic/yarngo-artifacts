//! What a clip should say about the voice that spoke it.
//!
//! Derived, never stored. The database keeps facts — this clip was made with a
//! custom voice, that voice's profile is deleted — and the sentence a person
//! reads is assembled from them here. Storing the sentence would freeze a
//! wording, and worse, would let the two disagree.

use rusqlite::{params, OptionalExtension};

use crate::{Result, Store};

/// How a clip's voice should be described.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceProvenance {
    /// The model's own voice. Nobody's recording was involved.
    BuiltIn,
    /// A voice that is still here, under the name it currently has.
    Custom { label: String },
    /// A voice that has been removed. The clip was still spoken in it, which is
    /// why this is not `BuiltIn`: saying the model read it would be false.
    Deleted { label_at_generation: Option<String> },
}

/// One recorded permission, as it is written out for a person to read.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsentRecord {
    pub voice_id: String,
    pub label: String,
    pub granted_at: String,
    pub app_version: String,
    pub statement: String,
    pub source: String,
    pub reference_sha256: String,
    pub classification: String,
}

impl Store {
    /// Every permission this store holds, oldest first.
    ///
    /// Including the ones that name a subject this store has no voice for: a
    /// consent record outliving the voice it was given for is a reason to keep
    /// it, not to drop it, and an account of permissions that quietly omitted
    /// some would be worse than none.
    pub fn consent_records(&self) -> Result<Vec<ConsentRecord>> {
        let mut statement = self.raw().prepare(
            "SELECT COALESCE(r.voice_id, e.legacy_subject_id, ''),
                    COALESCE(p.display_name, ''),
                    e.occurred_at, e.app_version, e.statement, e.source,
                    e.reference_sha256, e.classification
               FROM consent_events e
               LEFT JOIN voice_revisions r ON r.id = e.voice_revision_id
               LEFT JOIN voice_profiles p  ON p.id = r.voice_id
              ORDER BY e.occurred_at, e.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ConsentRecord {
                voice_id: row.get(0)?,
                label: row.get(1)?,
                granted_at: row.get(2)?,
                app_version: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                statement: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                source: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                reference_sha256: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                classification: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Fill in a reference transcript that is not there.
    ///
    /// A column added after a store was brought across stays empty for whoever
    /// had already brought theirs. This fills absences only — a value that is
    /// present is the person's, whatever it says, and is left alone. Reports
    /// whether it had anything to do, so a start-up can say so once rather than
    /// every time.
    /// Voices whose reference text has never been checked against the
    /// recording, oldest first. `(voice_id, audio_path, text)`.
    pub fn unchecked_references(&self) -> Result<Vec<(String, String, String)>> {
        let mut statement = self.raw().prepare(
            "SELECT v.voice_id, a.path, v.reference_text
               FROM voice_revisions v JOIN assets a ON a.id = v.source_asset_id
              WHERE v.reference_checked_at IS NULL
                AND v.reference_text IS NOT NULL AND v.reference_text <> ''
                AND v.deleted_at IS NULL
              ORDER BY v.created_at",
        )?;
        let found = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(found)
    }

    /// Record what the recording was found to say, and how long what is left
    /// runs. `text` of `None` means it could not be established — the text
    /// stands, and it is marked checked so the same answer is not sought at
    /// every start.
    ///
    /// The duration is written with the text because checking shortens the
    /// recording to what the text covers, and a stored length that no longer
    /// matches the file is a number the interface shows and nothing produces.
    pub fn reference_checked(
        &self,
        voice_id: &str,
        text: Option<&str>,
        seconds: Option<f32>,
        at: &str,
    ) -> Result<()> {
        match text {
            Some(text) => self.raw().execute(
                "UPDATE voice_revisions
                    SET reference_text = ?2,
                        duration_seconds = COALESCE(?3, duration_seconds),
                        reference_checked_at = ?4
                  WHERE voice_id = ?1",
                rusqlite::params![voice_id, text, seconds, at],
            )?,
            None => self.raw().execute(
                "UPDATE voice_revisions SET reference_checked_at = ?2 WHERE voice_id = ?1",
                rusqlite::params![voice_id, at],
            )?,
        };
        Ok(())
    }

    pub fn fill_reference_text(&self, voice_id: &str, text: &str) -> Result<bool> {
        Ok(self.raw().execute(
            "UPDATE voice_revisions SET reference_text = ?2
              WHERE voice_id = ?1 AND reference_text IS NULL",
            rusqlite::params![voice_id, text],
        )? > 0)
    }

    pub fn clip_voice(&self, clip_id: &str) -> Result<Option<VoiceProvenance>> {
        let row = self
            .raw()
            .query_row(
                "SELECT c.voice_kind, c.voice_label_at_generation, p.display_name, p.status
                   FROM clips c
                   LEFT JOIN voice_revisions r ON r.id = c.voice_revision_id
                   LEFT JOIN voice_profiles  p ON p.id = r.voice_id
                  WHERE c.id = ?1",
                params![clip_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;

        let Some((kind, label_at_generation, current_label, status)) = row else {
            return Ok(None);
        };
        Ok(Some(match (kind.as_str(), status.as_deref()) {
            ("built_in", _) => VoiceProvenance::BuiltIn,
            // Gone from the moment removal is authoritative, not from the moment
            // the files are: a clip should not name a voice that is being taken
            // away as though it were still choosable.
            (_, Some("deleted")) | (_, Some("deletion_pending")) => VoiceProvenance::Deleted {
                label_at_generation,
            },
            (_, Some(_)) => VoiceProvenance::Custom {
                // What it is called now, unless the clip recorded what it was
                // called then.
                label: label_at_generation.or(current_label).unwrap_or_default(),
            },
            // Custom, but the revision it names is not there. Not built-in, and
            // not a voice that can be shown — the same answer as deleted, which
            // is what it is.
            (_, None) => VoiceProvenance::Deleted {
                label_at_generation,
            },
        }))
    }

    pub fn mark_voice_deleted(&self, voice_id: &str, at: &str) -> Result<()> {
        self.raw().execute(
            "UPDATE voice_profiles SET status = 'deleted', deleted_at = ?2 WHERE id = ?1",
            params![voice_id, at],
        )?;
        Ok(())
    }
}
