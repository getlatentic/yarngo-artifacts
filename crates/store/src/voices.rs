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

impl Store {
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
