//! Reading the JSON store into SQLite, without touching it.
//!
//! The legacy files stay authoritative while this runs and afterwards. Nothing
//! here opens them for writing, and the whole snapshot lands in one transaction
//! or none of it does — a half-imported shadow would be worse than an empty
//! one, because it looks like an answer.
//!
//! Running it again compares rather than skips. `INSERT OR IGNORE` would make a
//! second import silent about exactly the thing worth knowing: that the same id
//! now carries different data. Same id and same data is a no-op; same id and
//! different data stops the import and says which field moved.
//!
//! What was never recorded is left null. The legacy store has no idea which
//! model revision made a clip or what the voice was called at the time, and
//! filling those in from today's values would turn an absence into a claim.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::{Result, Store, StoreError};

/// What one import did, and what it could not account for.
#[derive(Debug, Default, PartialEq)]
pub struct ImportReport {
    pub voices: usize,
    pub voice_revisions: usize,
    pub clips: usize,
    pub takes: usize,
    pub assets: usize,
    pub consent_events: usize,
    /// Rows already present and identical. A second run is all of these.
    pub unchanged: usize,
    /// Consent entries naming a voice this store has no record of. Kept, not
    /// dropped: whether they are development probes or evidence is not the
    /// importer's call to make, and the one irreversible option is the one it
    /// must not take on its own.
    pub consent_without_voice: Vec<String>,
    /// Files a record points at that are not on the disk.
    pub missing_files: Vec<String>,
}

#[derive(Debug)]
pub struct Legacy {
    pub clips: Vec<LegacyClip>,
    pub voices: BTreeMap<String, LegacyVoice>,
    pub consent: Vec<LegacyConsent>,
}

#[derive(Debug)]
pub struct LegacyClip {
    pub id: String,
    pub name: String,
    pub text: String,
    pub voice_id: Option<String>,
    pub model: Option<String>,
    pub created: String,
    pub takes: Vec<LegacyTake>,
}

#[derive(Debug)]
pub struct LegacyTake {
    pub id: String,
    pub path: String,
    pub audio_s: Option<f64>,
    pub gen_s: Option<f64>,
    pub seed: Option<i64>,
    pub created: String,
}

#[derive(Debug)]
pub struct LegacyVoice {
    pub label: String,
    pub reference_audio: String,
    pub seconds: Option<f64>,
    pub created: String,
}

#[derive(Debug)]
pub struct LegacyConsent {
    pub voice_id: String,
    pub statement: Option<String>,
    pub app_version: Option<String>,
    pub source: Option<String>,
    pub granted_at: String,
}

impl Legacy {
    /// Read the application's own files. `None` when they are not there.
    ///
    /// Here rather than in whatever needs it, because more than one thing does
    /// — the importer, and anything checking that this path leaves them alone
    /// — and two readers of the same files would eventually disagree about
    /// what they say.
    pub fn read(data_dir: &Path) -> Option<Self> {
        let clips_path = data_dir.join("clips/clips.json");
        let voices_path = data_dir.join("voices/voices.json");
        if !clips_path.exists() || !voices_path.exists() {
            return None;
        }
        let clips: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&clips_path).ok()?).ok()?;
        let voices: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&voices_path).ok()?).ok()?;

        let clips = clips
            .as_array()?
            .iter()
            .map(|clip| LegacyClip {
                id: clip["id"].as_str().unwrap_or_default().into(),
                name: clip["name"].as_str().unwrap_or_default().into(),
                text: clip["text"].as_str().unwrap_or_default().into(),
                voice_id: clip["voice_id"].as_str().map(Into::into),
                model: clip["model"].as_str().map(Into::into),
                created: clip["created"].as_str().unwrap_or_default().into(),
                takes: clip["takes"]
                    .as_array()
                    .map(|takes| {
                        takes
                            .iter()
                            .map(|take| LegacyTake {
                                id: take["id"].as_str().unwrap_or_default().into(),
                                path: take["path"].as_str().unwrap_or_default().into(),
                                audio_s: take["audio_s"].as_f64(),
                                gen_s: take["gen_s"].as_f64(),
                                seed: take["seed"].as_i64(),
                                created: take["created"].as_str().unwrap_or_default().into(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();

        let voices = voices
            .as_object()?
            .iter()
            .map(|(id, voice)| {
                (
                    id.clone(),
                    LegacyVoice {
                        label: voice["label"].as_str().unwrap_or_default().into(),
                        reference_audio: voice["reference_audio"].as_str().unwrap_or_default().into(),
                        seconds: voice["seconds"].as_f64(),
                        created: voice["created"].as_str().unwrap_or_default().into(),
                    },
                )
            })
            .collect();

        let consent = std::fs::read_to_string(data_dir.join("consent.log"))
            .map(|log| {
                log.lines()
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .map(|entry| LegacyConsent {
                        voice_id: entry["voice_id"].as_str().unwrap_or_default().into(),
                        statement: entry["statement"].as_str().map(Into::into),
                        app_version: entry["app_version"].as_str().map(Into::into),
                        source: entry["source"].as_str().map(Into::into),
                        granted_at: entry["granted_at"].as_str().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(Legacy { clips, voices, consent })
    }
}

/// The one revision a legacy voice has. Stable, so a second import matches.
fn revision_of(voice_id: &str) -> String {
    format!("{voice_id}/r1")
}

fn asset_id(kind: &str, key: &str) -> String {
    format!("{kind}:{key}")
}

/// Insert, or check that what is already there says the same thing.
///
/// `columns` are compared by name so a mismatch can say which one moved.
fn put(
    transaction: &Transaction<'_>,
    table: &str,
    id: &str,
    columns: &[(&str, rusqlite::types::Value)],
    report: &mut ImportReport,
) -> Result<bool> {
    let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
    let existing: Option<Vec<rusqlite::types::Value>> = transaction
        .query_row(
            &format!("SELECT {} FROM {table} WHERE id = ?1", names.join(", ")),
            params![id],
            |row| (0..names.len()).map(|i| row.get(i)).collect(),
        )
        .optional()?;

    if let Some(existing) = existing {
        for (index, (name, wanted)) in columns.iter().enumerate() {
            if &existing[index] != wanted {
                return Err(StoreError::Invalid(format!(
                    "{table} {id}: {name} was {:?}, the legacy store now says {wanted:?}",
                    existing[index]
                )));
            }
        }
        report.unchanged += 1;
        return Ok(false);
    }

    let placeholders: Vec<String> = (1..=columns.len() + 1).map(|i| format!("?{i}")).collect();
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&id];
    for (_, value) in columns {
        values.push(value);
    }
    transaction.execute(
        &format!(
            "INSERT INTO {table} (id, {}) VALUES ({})",
            names.join(", "),
            placeholders.join(", ")
        ),
        values.as_slice(),
    )?;
    Ok(true)
}

fn text(value: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(value.to_string())
}

fn maybe(value: Option<&str>) -> rusqlite::types::Value {
    match value {
        Some(value) => text(value),
        None => rusqlite::types::Value::Null,
    }
}

fn real(value: Option<f64>) -> rusqlite::types::Value {
    match value {
        Some(value) => rusqlite::types::Value::Real(value),
        None => rusqlite::types::Value::Null,
    }
}

impl Store {
    /// Bring a legacy snapshot in. One transaction: a failure anywhere leaves
    /// the database as it was.
    pub fn import_legacy(&mut self, legacy: &Legacy) -> Result<ImportReport> {
        let mut report = ImportReport::default();
        let transaction = self.raw_mut().transaction()?;

        for (voice_id, voice) in &legacy.voices {
            if !Path::new(&voice.reference_audio).exists() {
                report.missing_files.push(voice.reference_audio.clone());
            }
            let recording = asset_id("voice_reference", voice_id);
            if put(
                &transaction,
                "assets",
                &recording,
                &[
                    ("kind", text("voice_reference")),
                    ("path", text(&voice.reference_audio)),
                    ("state", text("active")),
                    ("created_at", text(&voice.created)),
                ],
                &mut report,
            )? {
                report.assets += 1;
            }
            if put(
                &transaction,
                "voice_profiles",
                voice_id,
                &[
                    ("display_name", text(&voice.label)),
                    ("status", text("active")),
                    ("created_at", text(&voice.created)),
                ],
                &mut report,
            )? {
                report.voices += 1;
            }
            if put(
                &transaction,
                "voice_revisions",
                &revision_of(voice_id),
                &[
                    ("voice_id", text(voice_id)),
                    ("source_asset_id", text(&recording)),
                    ("duration_seconds", real(voice.seconds)),
                    ("created_at", text(&voice.created)),
                ],
                &mut report,
            )? {
                report.voice_revisions += 1;
            }
        }

        for (index, consent) in legacy.consent.iter().enumerate() {
            let linked = legacy.voices.contains_key(&consent.voice_id);
            if !linked {
                report.consent_without_voice.push(consent.voice_id.clone());
            }
            let id = format!("consent:{}:{index}", consent.voice_id);
            if put(
                &transaction,
                "consent_events",
                &id,
                &[
                    (
                        "voice_revision_id",
                        if linked {
                            text(&revision_of(&consent.voice_id))
                        } else {
                            rusqlite::types::Value::Null
                        },
                    ),
                    (
                        "legacy_subject_id",
                        if linked {
                            rusqlite::types::Value::Null
                        } else {
                            text(&consent.voice_id)
                        },
                    ),
                    ("classification", text(if linked { "linked" } else { "legacy_orphan" })),
                    ("event_type", text("granted")),
                    ("statement", maybe(consent.statement.as_deref())),
                    ("app_version", maybe(consent.app_version.as_deref())),
                    ("source", maybe(consent.source.as_deref())),
                    ("occurred_at", text(&consent.granted_at)),
                ],
                &mut report,
            )? {
                report.consent_events += 1;
            }
        }

        for clip in &legacy.clips {
            let custom = clip.voice_id.as_ref().filter(|id| legacy.voices.contains_key(*id));
            if put(
                &transaction,
                "clips",
                &clip.id,
                &[
                    ("name", text(&clip.name)),
                    ("text", text(&clip.text)),
                    ("voice_kind", text(if custom.is_some() { "custom" } else { "built_in" })),
                    ("voice_revision_id", maybe(custom.map(|id| revision_of(id)).as_deref())),
                    // Never recorded by the legacy store, and inventing it from
                    // today's label would date a name to a time it did not have.
                    ("voice_label_at_generation", rusqlite::types::Value::Null),
                    ("model_id", maybe(clip.model.as_deref())),
                    ("provenance", text("legacy_partial")),
                    ("created_at", text(&clip.created)),
                ],
                &mut report,
            )? {
                report.clips += 1;
            }

            for take in &clip.takes {
                if !Path::new(&take.path).exists() {
                    report.missing_files.push(take.path.clone());
                }
                let audio = asset_id("generated_clip", &take.id);
                if put(
                    &transaction,
                    "assets",
                    &audio,
                    &[
                        ("kind", text("generated_clip")),
                        ("path", text(&take.path)),
                        ("state", text("active")),
                        ("created_at", text(&take.created)),
                    ],
                    &mut report,
                )? {
                    report.assets += 1;
                }
                if put(
                    &transaction,
                    "clip_takes",
                    &take.id,
                    &[
                        ("clip_id", text(&clip.id)),
                        ("audio_asset_id", text(&audio)),
                        ("audio_seconds", real(take.audio_s)),
                        ("generated_seconds", real(take.gen_s)),
                        (
                            "seed",
                            match take.seed {
                                Some(seed) => rusqlite::types::Value::Integer(seed),
                                None => rusqlite::types::Value::Null,
                            },
                        ),
                        ("created_at", text(&take.created)),
                    ],
                    &mut report,
                )? {
                    report.takes += 1;
                }
            }
        }

        transaction.commit()?;
        Ok(report)
    }
}
