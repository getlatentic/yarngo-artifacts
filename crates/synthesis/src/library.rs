//! What the application shows, read from and written to its own database.
//!
//! The shapes are the ones the interface already uses. Nothing here is a new
//! model of a clip or a voice — it is the same clip and the same voice, coming
//! from the place that now keeps them rather than from a process that was only
//! ever meant to run inference.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension};
use speech_engine::{Clip, Consent, Take, Voice};
use yarngo_store::{Result, Store};

/// Every clip that still has audio, newest first.
///
/// A clip whose takes have all gone is not listed: there is nothing to play and
/// nothing to look at, and a row that does neither is a row that only confuses.
pub fn clips(store: &Store) -> Result<Vec<Clip>> {
    let mut statement = store.raw().prepare(
        "SELECT c.id, c.name, c.text, r.voice_id, c.model_id, c.created_at
           FROM clips c
           LEFT JOIN voice_revisions r ON r.id = c.voice_revision_id
          WHERE c.deleted_at IS NULL
          ORDER BY c.created_at DESC, c.id DESC",
    )?;
    let rows: Vec<(String, String, String, Option<String>, Option<String>, String)> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut clips = Vec::new();
    for (id, name, text, voice_id, model, created) in rows {
        let takes = takes_of(store, &id)?;
        if takes.is_empty() {
            continue;
        }
        clips.push(Clip {
            title: title_of(&text),
            id,
            name,
            text,
            voice_id,
            model: model.unwrap_or_default(),
            created,
            takes,
        });
    }
    Ok(clips)
}

/// Newest first, which is the order the panel lists them in.
fn takes_of(store: &Store, clip_id: &str) -> Result<Vec<Take>> {
    let mut statement = store.raw().prepare(
        "SELECT t.id, a.path, t.audio_seconds, t.generated_seconds, t.seed, t.created_at
           FROM clip_takes t
           JOIN assets a ON a.id = t.audio_asset_id
          WHERE t.clip_id = ?1 AND a.state = 'active'
          ORDER BY t.created_at DESC, t.id DESC",
    )?;
    let takes = statement.query_map(params![clip_id], |row| {
        Ok(Take {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            audio_s: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0) as f32,
            gen_s: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0) as f32,
            seed: row.get::<_, Option<i64>>(4)?.map(|s| s as u32),
            created: row.get(5)?,
        })
    })?;
    Ok(takes.collect::<std::result::Result<_, _>>()?)
}

/// A title short enough for a sidebar row, from the words themselves.
fn title_of(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(45) {
        Some(_) => {
            let cut = text.char_indices().nth(44).map(|(i, _)| i).unwrap_or(text.len());
            format!("{}…", &text[..cut])
        }
        None => text.to_string(),
    }
}

/// Voices the person can still speak with.
///
/// A voice being deleted is not offered: the recording behind it is going or
/// gone, and offering it would be offering work that must be refused later.
pub fn voices(store: &Store) -> Result<Vec<Voice>> {
    let mut statement = store.raw().prepare(
        "SELECT p.id, p.display_name, a.path, r.duration_seconds, r.reference_text,
                e.statement, e.app_version, e.source
           FROM voice_profiles p
           JOIN voice_revisions r ON r.voice_id = p.id AND r.deleted_at IS NULL
           JOIN assets a          ON a.id = r.source_asset_id AND a.state = 'active'
           LEFT JOIN consent_events e ON e.voice_revision_id = r.id
                                     AND e.event_type = 'granted'
          WHERE p.status = 'active'
          GROUP BY p.id
          ORDER BY p.created_at",
    )?;
    let voices = statement.query_map([], |row| {
        Ok(Voice {
            voice_id: row.get(0)?,
            label: row.get(1)?,
            reference_audio: PathBuf::from(row.get::<_, String>(2)?),
            seconds: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0) as f32,
            reference_text: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            consent: Consent {
                statement: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                app_version: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                source: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            },
        })
    })?;
    Ok(voices.collect::<std::result::Result<_, _>>()?)
}

/// What is needed to speak in a voice, if it is still one the person has.
#[derive(Clone, Debug)]
pub struct Reference {
    pub revision: String,
    pub audio: String,
    /// What was read while it was recorded. The model conditions on it, so it
    /// travels with the recording rather than being looked up separately and
    /// forgotten.
    pub text: Option<String>,
    pub label: String,
}

pub fn reference(store: &Store, voice_id: &str) -> Result<Option<Reference>> {
    Ok(store
        .raw()
        .query_row(
            "SELECT r.id, a.path, r.reference_text, p.display_name
               FROM voice_profiles p
               JOIN voice_revisions r ON r.voice_id = p.id AND r.deleted_at IS NULL
               JOIN assets a          ON a.id = r.source_asset_id AND a.state = 'active'
              WHERE p.id = ?1 AND p.status = 'active'
              ORDER BY r.created_at DESC",
            params![voice_id],
            |row| {
                Ok(Reference {
                    revision: row.get(0)?,
                    audio: row.get(1)?,
                    text: row.get(2)?,
                    label: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// What a clip was made with, so generating again reads it from the clip rather
/// than from whatever happens to be selected now.
pub fn clip_voice(store: &Store, clip_id: &str) -> Result<Option<Option<String>>> {
    Ok(store
        .raw()
        .query_row(
            "SELECT r.voice_id FROM clips c
               LEFT JOIN voice_revisions r ON r.id = c.voice_revision_id
              WHERE c.id = ?1",
            params![clip_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Start a clip. Its voice and model are fixed here, because every reading of it
/// is a reading of the same words in the same voice.
#[allow(clippy::too_many_arguments)]
pub fn create_clip(
    store: &Store,
    id: &str,
    name: &str,
    text: &str,
    voice: Option<(&str, &str)>,
    model: &str,
    at: &str,
) -> Result<()> {
    let (revision, label) = match voice {
        Some((revision, label)) => (Some(revision), Some(label)),
        None => (None, None),
    };
    store.raw().execute(
        "INSERT INTO clips (id, name, text, voice_kind, voice_revision_id,
                            voice_label_at_generation, model_id, provenance, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'complete', ?8)",
        params![
            id,
            name,
            text,
            if revision.is_some() { "custom" } else { "built_in" },
            revision,
            label,
            model,
            at
        ],
    )?;
    Ok(())
}

pub fn rename_clip(store: &Store, clip_id: &str, name: &str) -> Result<()> {
    store.raw().execute(
        "UPDATE clips SET name = ?2 WHERE id = ?1",
        params![clip_id, name],
    )?;
    Ok(())
}

/// Remove a clip from the list, and the audio with it.
///
/// The row stays, marked, because takes and assets point at it and because a
/// clip that was here yesterday and is gone today is something the records
/// should be able to account for.
pub fn delete_clip(store: &mut Store, clip_id: &str, at: &str) -> Result<()> {
    let paths: Vec<String> = {
        let mut statement = store.raw().prepare(
            "SELECT a.path FROM clip_takes t
               JOIN assets a ON a.id = t.audio_asset_id
              WHERE t.clip_id = ?1",
        )?;
        let rows = statement.query_map(params![clip_id], |row| row.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let transaction = store.raw_mut().transaction()?;
    transaction.execute(
        "UPDATE assets SET state = 'deleted', deleted_at = ?2
          WHERE id IN (SELECT audio_asset_id FROM clip_takes WHERE clip_id = ?1)",
        params![clip_id, at],
    )?;
    transaction.execute(
        "UPDATE clips SET deleted_at = ?2 WHERE id = ?1",
        params![clip_id, at],
    )?;
    transaction.commit()?;
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Copy the words, not the readings. A duplicate is somewhere to write a
/// variation, and the audio it ends up with should be its own.
pub fn duplicate_clip(store: &Store, clip_id: &str, new_id: &str, at: &str) -> Result<()> {
    store.raw().execute(
        "INSERT INTO clips (id, name, text, voice_kind, voice_revision_id,
                            voice_label_at_generation, model_id, provenance, created_at)
         SELECT ?2, name || ' copy', text, voice_kind, voice_revision_id,
                voice_label_at_generation, model_id, provenance, ?3
           FROM clips WHERE id = ?1",
        params![clip_id, new_id, at],
    )?;
    Ok(())
}

/// Enrol a voice: the recording, the profile, the revision that names it, and
/// the consent it was given under, together or not at all.
pub fn register_voice(store: &mut Store, voice: &Voice, at: &str) -> Result<()> {
    let asset = format!("voice_reference:{}", voice.voice_id);
    let revision = format!("{}/r1", voice.voice_id);
    let transaction = store.raw_mut().transaction()?;
    transaction.execute(
        "INSERT INTO assets (id, kind, path, state, created_at)
         VALUES (?1, 'voice_reference', ?2, 'active', ?3)",
        params![asset, voice.reference_audio.to_string_lossy(), at],
    )?;
    transaction.execute(
        "INSERT INTO voice_profiles (id, display_name, status, created_at)
         VALUES (?1, ?2, 'active', ?3)",
        params![voice.voice_id, voice.label, at],
    )?;
    transaction.execute(
        "INSERT INTO voice_revisions
            (id, voice_id, source_asset_id, duration_seconds, reference_text, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            revision,
            voice.voice_id,
            asset,
            voice.seconds as f64,
            // Empty is absent: a voice enrolled without a script was cloned
            // from audio alone, and saying so is not the same as saying "".
            Some(voice.reference_text.as_str()).filter(|text| !text.trim().is_empty()),
            at
        ],
    )?;
    // Written with the voice, in the same transaction. A voice that exists
    // without the record of what was agreed to is a voice nothing can account
    // for, and the one order that can produce it is two transactions.
    transaction.execute(
        "INSERT INTO consent_events (id, voice_revision_id, classification, event_type,
                                     statement, app_version, source, occurred_at)
         VALUES (?1, ?2, 'linked', 'granted', ?3, ?4, ?5, ?6)",
        params![
            format!("consent:{}:0", voice.voice_id),
            revision,
            voice.consent.statement,
            voice.consent.app_version,
            voice.consent.source,
            at
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

/// Only the label changes. The recording, the consent it was given under, and
/// every clip already made with it are untouched.
pub fn rename_voice(store: &Store, voice_id: &str, label: &str) -> Result<()> {
    store.raw().execute(
        "UPDATE voice_profiles SET display_name = ?2 WHERE id = ?1",
        params![voice_id, label],
    )?;
    Ok(())
}
