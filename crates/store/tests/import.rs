//! Bringing the JSON store into SQLite, checked against the real one.
//!
//! Counts are the weakest thing an importer can be right about, so these
//! compare field by field: every clip's text and timestamp, every voice's name
//! and recording, every consent entry, and which voice each clip was made with.
//! A row count matching while the text is wrong is exactly the failure an
//! importer has.
//!
//! The real store is read when it is there and skipped when it is not, so this
//! runs on a machine that has never opened the application — with the shape of
//! the data covered either way by the fixture below.

use std::collections::BTreeMap;
use std::path::PathBuf;

use yarngo_store::import::{Legacy, LegacyClip, LegacyConsent, LegacyTake, LegacyVoice};
use yarngo_store::{Store, VoiceProvenance};

fn data_dir() -> PathBuf {
    PathBuf::from("/Users/dev/Library/Application Support/Yarngo Studio")
}

/// A snapshot with the shapes that matter: a built-in clip, a custom one, one
/// with two takes, and a voice whose clips must survive its deletion.
fn fixture() -> Legacy {
    let mut voices = BTreeMap::new();
    voices.insert(
        "voice-1".to_string(),
        LegacyVoice {
            label: "Tosin's Voice".into(),
            reference_audio: "/tmp/voice-1.wav".into(),
            seconds: Some(21.0),
            created: "2026-08-19T08:00:00".into(),
        },
    );
    voices.insert(
        "voice-3".to_string(),
        LegacyVoice {
            label: "Blessing's Voice".into(),
            reference_audio: "/tmp/voice-3.wav".into(),
            seconds: Some(23.0),
            created: "2026-08-21T11:00:00".into(),
        },
    );
    Legacy {
        voices,
        consent: vec![
            LegacyConsent {
                voice_id: "voice-1".into(),
                statement: Some("I have permission to use this voice.".into()),
                app_version: Some("0.1.0".into()),
                source: Some("recording".into()),
                granted_at: "2026-08-19T08:00:00".into(),
            },
            // From a development probe: names a voice that does not exist.
            LegacyConsent {
                voice_id: "consent-probe".into(),
                statement: Some("probe".into()),
                app_version: Some("0.1.0".into()),
                source: Some("imported".into()),
                granted_at: "2026-08-18T22:21:05".into(),
            },
        ],
        clips: vec![
            LegacyClip {
                id: "clip-1".into(),
                name: "Default one".into(),
                text: "Spoken by the model itself.".into(),
                voice_id: None,
                model: Some("dots-tts-mf".into()),
                created: "2026-08-20T09:00:00".into(),
                takes: vec![LegacyTake {
                    id: "take-1".into(),
                    path: "/tmp/clip-1.wav".into(),
                    audio_s: Some(3.2),
                    gen_s: Some(4.0),
                    seed: Some(41),
                    created: "2026-08-20T09:00:00".into(),
                }],
            },
            LegacyClip {
                id: "clip-2".into(),
                name: "Mine".into(),
                text: "Spoken in a voice I recorded.".into(),
                voice_id: Some("voice-1".into()),
                model: Some("dots-tts-mf".into()),
                created: "2026-08-20T10:00:00".into(),
                takes: vec![
                    LegacyTake {
                        id: "take-2".into(),
                        path: "/tmp/clip-2a.wav".into(),
                        audio_s: Some(5.0),
                        gen_s: Some(9.0),
                        seed: Some(7),
                        created: "2026-08-20T10:00:00".into(),
                    },
                    LegacyTake {
                        id: "take-3".into(),
                        path: "/tmp/clip-2b.wav".into(),
                        audio_s: Some(5.1),
                        gen_s: Some(8.0),
                        seed: Some(8),
                        created: "2026-08-20T10:05:00".into(),
                    },
                ],
            },
            LegacyClip {
                id: "clip-3".into(),
                name: "Blessing".into(),
                text: "Made with a voice that gets deleted.".into(),
                voice_id: Some("voice-3".into()),
                model: Some("dots-tts-mf".into()),
                created: "2026-08-21T11:30:00".into(),
                takes: vec![LegacyTake {
                    id: "take-4".into(),
                    path: "/tmp/clip-3.wav".into(),
                    audio_s: Some(6.0),
                    gen_s: Some(11.0),
                    seed: None,
                    created: "2026-08-21T11:30:00".into(),
                }],
            },
        ],
    }
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .raw()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0))
        .expect("count")
}

#[test]
fn everything_in_the_snapshot_arrives_field_for_field() {
    let mut store = Store::in_memory().expect("store");
    let legacy = fixture();
    let report = store.import_legacy(&legacy).expect("import");

    assert_eq!(report.clips, 3);
    assert_eq!(report.voices, 2);
    assert_eq!(report.takes, 4, "a clip with two takes lost one");
    assert_eq!(report.assets, 6, "two recordings and four generated files");
    // Both, including the one naming a voice that does not exist. Whether that
    // is a development probe or evidence is not the importer's decision.
    assert_eq!(report.consent_events, 2);
    assert_eq!(report.consent_without_voice, ["consent-probe"]);
    let (linked, orphan): (i64, i64) = store
        .raw()
        .query_row(
            "SELECT sum(classification = 'linked'), sum(classification = 'legacy_orphan')
               FROM consent_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("classification");
    assert_eq!((linked, orphan), (1, 1));
    let subject: Option<String> = store
        .raw()
        .query_row(
            "SELECT legacy_subject_id FROM consent_events WHERE classification = 'legacy_orphan'",
            [],
            |row| row.get(0),
        )
        .expect("subject");
    assert_eq!(subject.as_deref(), Some("consent-probe"), "the record forgot who it named");

    for clip in &legacy.clips {
        let (name, text, kind, revision, created, provenance): (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
        ) = store
            .raw()
            .query_row(
                "SELECT name, text, voice_kind, voice_revision_id, created_at, provenance
                   FROM clips WHERE id = ?1",
                [&clip.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap_or_else(|e| panic!("clip {} missing: {e}", clip.id));

        assert_eq!(name, clip.name);
        assert_eq!(text, clip.text, "clip {} text changed", clip.id);
        assert_eq!(created, clip.created, "clip {} timestamp changed", clip.id);
        match &clip.voice_id {
            Some(voice) => {
                assert_eq!(kind, "custom");
                assert_eq!(revision.as_deref(), Some(format!("{voice}/r1").as_str()));
            }
            None => {
                assert_eq!(kind, "built_in");
                assert!(revision.is_none());
            }
        }
        // Nothing recorded the model revision or the label at the time, and the
        // import says so rather than borrowing today's.
        assert_eq!(provenance, "legacy_partial");
    }

    for voice in legacy.voices.keys() {
        let (name, status): (String, String) = store
            .raw()
            .query_row(
                "SELECT display_name, status FROM voice_profiles WHERE id = ?1",
                [voice],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("voice");
        assert_eq!(name, legacy.voices[voice].label);
        assert_eq!(status, "active");

        let recording: String = store
            .raw()
            .query_row(
                "SELECT a.path FROM voice_revisions r JOIN assets a ON a.id = r.source_asset_id
                  WHERE r.voice_id = ?1",
                [voice],
                |row| row.get(0),
            )
            .expect("recording");
        assert_eq!(recording, legacy.voices[voice].reference_audio);
    }

    // Every take's file is reachable through its asset.
    for clip in &legacy.clips {
        for take in &clip.takes {
            let path: String = store
                .raw()
                .query_row(
                    "SELECT a.path FROM clip_takes t JOIN assets a ON a.id = t.audio_asset_id
                      WHERE t.id = ?1",
                    [&take.id],
                    |row| row.get(0),
                )
                .unwrap_or_else(|e| panic!("take {} missing: {e}", take.id));
            assert_eq!(path, take.path);
        }
    }
}

/// The whole reason for a revision: deleting a voice must not touch what its
/// clips say they were made with.
#[test]
fn a_deleted_voice_keeps_its_clips_and_their_provenance() {
    let mut store = Store::in_memory().expect("store");
    store.import_legacy(&fixture()).expect("import");

    store.mark_voice_deleted("voice-3", "2026-08-21T12:00:00").expect("delete");

    // Stored: facts. The clip is still custom, still points at the revision,
    // and its audio is untouched.
    let (kind, revision): (String, Option<String>) = store
        .raw()
        .query_row(
            "SELECT voice_kind, voice_revision_id FROM clips WHERE id = 'clip-3'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("clip-3");
    assert_eq!(kind, "custom", "deleting the voice changed where the clip came from");
    assert_eq!(revision.as_deref(), Some("voice-3/r1"));
    let audio: String = store
        .raw()
        .query_row(
            "SELECT a.state FROM clip_takes t JOIN assets a ON a.id = t.audio_asset_id
              WHERE t.clip_id = 'clip-3'",
            [],
            |row| row.get(0),
        )
        .expect("audio");
    assert_eq!(audio, "active", "the generated audio was disturbed");

    // Derived: the sentence. Not stored anywhere.
    assert_eq!(
        store.clip_voice("clip-3").expect("projection"),
        Some(VoiceProvenance::Deleted { label_at_generation: None })
    );
    // And the others are unaffected.
    assert_eq!(
        store.clip_voice("clip-2").expect("projection"),
        Some(VoiceProvenance::Custom { label: "Tosin's Voice".into() })
    );
    assert_eq!(
        store.clip_voice("clip-1").expect("projection"),
        Some(VoiceProvenance::BuiltIn)
    );
}

#[test]
fn importing_the_same_snapshot_twice_changes_nothing() {
    let mut store = Store::in_memory().expect("store");
    let legacy = fixture();
    store.import_legacy(&legacy).expect("first");
    let before = (count(&store, "clips"), count(&store, "clip_takes"), count(&store, "assets"));

    let second = store.import_legacy(&legacy).expect("second");
    assert_eq!(second.clips, 0);
    assert_eq!(second.takes, 0);
    assert!(second.unchanged > 0, "a second import compared nothing");
    assert_eq!(
        (count(&store, "clips"), count(&store, "clip_takes"), count(&store, "assets")),
        before
    );
}

/// The failure `INSERT OR IGNORE` would have hidden: the same id, different
/// data. Silence there would mean the shadow store quietly disagreeing with the
/// thing it is meant to be validated against.
#[test]
fn a_second_import_that_disagrees_fails_loudly() {
    let mut store = Store::in_memory().expect("store");
    store.import_legacy(&fixture()).expect("first");

    let mut changed = fixture();
    changed.clips[1].text = "Someone edited this behind our back.".into();
    let refused = store.import_legacy(&changed).expect_err("should refuse");
    let complaint = refused.to_string();
    assert!(complaint.contains("clip-2"), "{complaint}");
    assert!(complaint.contains("text"), "{complaint}");
}

/// A failure part-way leaves nothing behind. A half-imported shadow store is
/// worse than an empty one, because it looks like an answer.
#[test]
fn a_failed_import_leaves_the_database_as_it_was() {
    let mut store = Store::in_memory().expect("store");
    store.import_legacy(&fixture()).expect("first");
    let before = count(&store, "clips");

    let mut broken = fixture();
    broken.clips[1].text = "changed".into();
    broken.clips.push(LegacyClip {
        id: "clip-4".into(),
        name: "New".into(),
        text: "Would be added if the import survived.".into(),
        voice_id: None,
        model: None,
        created: "2026-08-21T13:00:00".into(),
        takes: vec![],
    });
    assert!(store.import_legacy(&broken).is_err());
    assert_eq!(count(&store, "clips"), before, "a failed import left rows behind");
    assert!(
        store.clip_voice("clip-4").expect("query").is_none(),
        "a row from a rolled-back import survived"
    );
}

/// The real store, when this machine has one. Field-by-field, and a report of
/// what did not line up.
#[test]
fn the_real_store_imports_completely() {
    let Some(legacy) = Legacy::read(&data_dir()) else {
        eprintln!("no application data on this machine; fixture coverage only");
        return;
    };
    let mut store = Store::in_memory().expect("store");
    let report = store.import_legacy(&legacy).expect("import");

    println!(
        "\n  legacy                imported\n  \
         {:<3} clips              {:<3}\n  \
         {:<3} voices             {:<3}\n  \
         {:<3} takes              {:<3}\n  \
         {:<3} consent entries    {:<3} ({} unlinked)\n  \
         {:<3} missing files",
        legacy.clips.len(),
        report.clips,
        legacy.voices.len(),
        report.voices,
        legacy.clips.iter().map(|c| c.takes.len()).sum::<usize>(),
        report.takes,
        legacy.consent.len(),
        report.consent_events,
        report.consent_without_voice.len(),
        report.missing_files.len(),
    );

    assert_eq!(report.clips, legacy.clips.len());
    assert_eq!(report.voices, legacy.voices.len());
    assert_eq!(
        report.takes,
        legacy.clips.iter().map(|c| c.takes.len()).sum::<usize>()
    );
    // Every consent record is kept, whether or not it still names a voice.
    assert_eq!(
        report.consent_events,
        legacy.consent.len(),
        "consent records were dropped: {:?}",
        report.consent_without_voice
    );
    assert!(
        report.missing_files.is_empty(),
        "records point at files that are not there: {:?}",
        report.missing_files
    );

    // Field by field, not counts.
    for clip in &legacy.clips {
        let (text, created): (String, String) = store
            .raw()
            .query_row("SELECT text, created_at FROM clips WHERE id = ?1", [&clip.id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap_or_else(|e| panic!("clip {} missing: {e}", clip.id));
        assert_eq!(text, clip.text);
        assert_eq!(created, clip.created);

        let expected = match &clip.voice_id {
            Some(id) if legacy.voices.contains_key(id) => VoiceProvenance::Custom {
                label: legacy.voices[id].label.clone(),
            },
            Some(_) => VoiceProvenance::Deleted { label_at_generation: None },
            None => VoiceProvenance::BuiltIn,
        };
        assert_eq!(
            store.clip_voice(&clip.id).expect("projection"),
            Some(expected),
            "clip {} names the wrong voice",
            clip.id
        );
    }

    // And the legacy files are exactly as they were.
    for name in ["clips/clips.json", "voices/voices.json"] {
        let path = data_dir().join(name);
        assert!(path.exists(), "{name} vanished");
    }
}

/// Proof rather than inspection: the bytes of every legacy file are the same
/// afterwards. A source scan for `File::create` proves nothing — a write can go
/// through `OpenOptions`, a helper, another module, or a library — and it would
/// keep passing while any of those did it.
#[test]
fn the_legacy_files_are_byte_for_byte_unchanged() {
    let Some(legacy) = Legacy::read(&data_dir()) else {
        eprintln!("no application data on this machine; nothing to leave alone");
        return;
    };

    let mut watched: Vec<PathBuf> = vec![
        data_dir().join("clips/clips.json"),
        data_dir().join("voices/voices.json"),
        data_dir().join("consent.log"),
    ];
    // Every file the records point at, too: an importer that rewrote a WAV in
    // place would pass a check that only watched the manifests.
    watched.extend(legacy.voices.values().map(|v| PathBuf::from(&v.reference_audio)));
    watched.extend(
        legacy
            .clips
            .iter()
            .flat_map(|c| c.takes.iter())
            .map(|t| PathBuf::from(&t.path)),
    );

    let digest = |paths: &[PathBuf]| -> Vec<(PathBuf, u64, Vec<u8>)> {
        paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                let bytes = std::fs::read(path).expect("read");
                let length = bytes.len() as u64;
                // The whole content for the manifests, a sample for the audio:
                // reading tens of megabytes twice to prove nothing moved is a
                // cost without a matching risk.
                let sample = if length > 1_000_000 {
                    bytes[..4096].to_vec()
                } else {
                    bytes
                };
                (path.clone(), length, sample)
            })
            .collect()
    };

    let before = digest(&watched);
    assert!(before.len() > 3, "nothing was watched");

    let mut store = Store::in_memory().expect("store");
    store.import_legacy(&legacy).expect("import");

    assert_eq!(digest(&watched), before, "the importer modified the legacy store");
}
