-- The application's durable truth. Rust alone opens this; the engine is handed
-- what it needs and reports back, and nothing it says is written here without
-- passing the lifecycle rules first.

-- Which engine process ran something. An attempt still marked running by a
-- session that has ended was interrupted — a fact, rather than a guess from
-- timestamps.
CREATE TABLE engine_sessions (
    id          TEXT PRIMARY KEY,
    backend     TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT,
    exit_reason TEXT
);

-- What the person asked for. Lasts until that is settled, across as many
-- attempts as it takes.
CREATE TABLE jobs (
    id                   TEXT PRIMARY KEY,
    kind                 TEXT NOT NULL,
    state                TEXT NOT NULL,
    target_id            TEXT,
    current_execution_id TEXT,
    -- The person asked again after a real failure. The original keeps its
    -- outcome; this is a separate intent that remembers where it came from.
    retry_of_job_id      TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    completed_at         TEXT,
    error_code           TEXT,
    error_message        TEXT,

    FOREIGN KEY (retry_of_job_id) REFERENCES jobs(id) ON DELETE RESTRICT
);

-- One engine's attempt at a job. Kept rather than folded into the job, because
-- "cancelled after being asked to stop" and "vanished when its engine was
-- killed" are the same job outcome and very different diagnoses.
CREATE TABLE job_executions (
    id            TEXT PRIMARY KEY,
    job_id        TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    state         TEXT NOT NULL,
    started_at    TEXT,
    finished_at   TEXT,
    error_code    TEXT,
    error_message TEXT,

    FOREIGN KEY (job_id)     REFERENCES jobs(id)            ON DELETE RESTRICT,
    FOREIGN KEY (session_id) REFERENCES engine_sessions(id) ON DELETE RESTRICT
);

CREATE INDEX idx_job_executions_job ON job_executions(job_id);
CREATE INDEX idx_job_executions_session ON job_executions(session_id);
CREATE INDEX idx_jobs_state ON jobs(state);

-- Files this application put on the disk, and what is meant to happen to them.
-- A row and a file cannot be changed in one transaction, so the row records the
-- intent and startup finishes whatever was in progress.
CREATE TABLE assets (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,         -- voice_reference | generated_clip
    path       TEXT NOT NULL,
    state      TEXT NOT NULL,         -- active | deletion_pending | deleted | deletion_failed
    checksum   TEXT,
    bytes      INTEGER,
    created_at TEXT NOT NULL,
    deleted_at TEXT
);

CREATE INDEX idx_assets_state ON assets(state);

-- A voice as the person knows it: a name they chose and can change.
CREATE TABLE voice_profiles (
    id           TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    status       TEXT NOT NULL,       -- active | deletion_pending | deleted
    created_at   TEXT NOT NULL,
    deleted_at   TEXT
);

-- What a clip was actually spoken in. Immutable, and never deleted while a clip
-- points at it: renaming the profile must not rewrite what past clips claim,
-- and deleting the voice must not make them claim the model read them.
CREATE TABLE voice_revisions (
    id              TEXT PRIMARY KEY,
    voice_id        TEXT NOT NULL,
    source_asset_id TEXT,
    created_at      TEXT NOT NULL,
    deleted_at      TEXT,

    FOREIGN KEY (voice_id)        REFERENCES voice_profiles(id) ON DELETE RESTRICT,
    FOREIGN KEY (source_asset_id) REFERENCES assets(id)         ON DELETE RESTRICT
);

CREATE INDEX idx_voice_revisions_voice ON voice_revisions(voice_id);

-- What the person agreed to, when, and against which revision. Events rather
-- than a flag: revoking is something that happened, not the absence of
-- something that did.
CREATE TABLE consent_events (
    id                TEXT PRIMARY KEY,
    voice_revision_id TEXT NOT NULL,
    event_type        TEXT NOT NULL,  -- granted | revoked
    statement         TEXT,
    app_version       TEXT,
    source            TEXT,
    occurred_at       TEXT NOT NULL,

    FOREIGN KEY (voice_revision_id) REFERENCES voice_revisions(id) ON DELETE RESTRICT
);

CREATE INDEX idx_consent_events_revision ON consent_events(voice_revision_id);

CREATE TABLE clips (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    text              TEXT NOT NULL,
    -- Where the voice came from, and nothing about what became of it. A clip
    -- made with a custom voice stays custom after that voice is deleted; the
    -- deletion is the profile's state, and saying so here would be two places
    -- to look and one of them wrong.
    voice_kind        TEXT NOT NULL,  -- built_in | custom
    voice_revision_id TEXT,
    -- What the voice was called when this was made. Presentation history, kept
    -- apart from the revision, which is about which recording was used: a
    -- rename changes the name and not the identity. Null where it was never
    -- recorded, which is every clip made before this column existed.
    voice_label_at_generation TEXT,
    model_id          TEXT,
    -- What was not recorded when this clip was made. Legacy rows say so rather
    -- than borrowing today's values and presenting them as history.
    provenance        TEXT NOT NULL,  -- complete | legacy_partial
    created_at        TEXT NOT NULL,
    deleted_at        TEXT,

    FOREIGN KEY (voice_revision_id) REFERENCES voice_revisions(id) ON DELETE RESTRICT,
    CHECK (voice_kind IN ('built_in', 'custom')),
    CHECK (voice_kind = 'built_in' OR voice_revision_id IS NOT NULL)
);

-- One reading of a clip. Editing the text keeps the old audio, so a clip is
-- several of these and the newest is only the newest.
CREATE TABLE clip_takes (
    id             TEXT PRIMARY KEY,
    clip_id        TEXT NOT NULL,
    audio_asset_id TEXT NOT NULL,
    audio_seconds  REAL,
    generated_seconds REAL,
    seed           INTEGER,
    created_at     TEXT NOT NULL,

    FOREIGN KEY (clip_id)        REFERENCES clips(id)  ON DELETE RESTRICT,
    FOREIGN KEY (audio_asset_id) REFERENCES assets(id) ON DELETE RESTRICT
);

CREATE INDEX idx_clip_takes_clip ON clip_takes(clip_id);
CREATE INDEX idx_clips_voice_revision ON clips(voice_revision_id);
CREATE INDEX idx_clips_created ON clips(created_at);

-- Settings that outlive the engine. What model is loaded is the engine's to
-- know and dies with it; which one the person chose is not.
CREATE TABLE model_preferences (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
