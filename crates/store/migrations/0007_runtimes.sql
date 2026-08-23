-- Which runtime versions are installed, and what each engine run was.
--
-- A runtime version is immutable once installed: its directory is created at
-- its final path and never moved, so "which one is active" is a fact in this
-- database rather than an arrangement of symlinks. Activation is one row
-- changing, which is as atomic as this application knows how to be.
CREATE TABLE runtimes (
    id           TEXT NOT NULL,
    version      TEXT NOT NULL,
    -- 'installing' until the engine has answered a handshake from its own
    -- directory; 'ready' after. A crash mid-install leaves 'installing', which
    -- startup treats as debris.
    state        TEXT NOT NULL CHECK (state IN ('installing', 'ready')),
    engine_api   INTEGER NOT NULL,
    installed_at TEXT NOT NULL,
    ready_at     TEXT,
    PRIMARY KEY (id, version)
);

-- Which runtime answered a session. Provenance for everything the session
-- produced: an execution names its session, and the session names the code.
ALTER TABLE engine_sessions ADD COLUMN runtime_id TEXT;
ALTER TABLE engine_sessions ADD COLUMN runtime_version TEXT;
ALTER TABLE engine_sessions ADD COLUMN engine_api INTEGER;
