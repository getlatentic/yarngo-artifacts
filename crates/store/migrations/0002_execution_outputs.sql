-- What an attempt was told to produce, and what it produced.
--
-- The path is written before the engine is asked for anything, because Rust
-- decides where output goes. An engine free to name its own file could collide
-- with a take that already exists, or write somewhere a deletion would never
-- look for it.
--
-- The rest is written when the engine reports, in the same transaction as the
-- execution's own state, and is everything publication needs. A crash between
-- the engine finishing and the take being committed can then be finished later
-- rather than started again: the audio is on the disk and this says whose it
-- is, how long it is, and what seed made it.
CREATE TABLE execution_outputs (
    execution_id      TEXT PRIMARY KEY,
    -- The clip this take belongs to, decided when the person pressed Generate.
    clip_id           TEXT NOT NULL,
    staged_path       TEXT NOT NULL,
    audio_seconds     REAL,
    generated_seconds REAL,
    seed              INTEGER,
    sample_rate       INTEGER,
    -- Null until the engine has reported. Its presence is the difference
    -- between a file that may be half-written and one that is finished.
    produced_at       TEXT,

    FOREIGN KEY (execution_id) REFERENCES job_executions(id) ON DELETE CASCADE,
    FOREIGN KEY (clip_id)      REFERENCES clips(id)          ON DELETE RESTRICT
);
