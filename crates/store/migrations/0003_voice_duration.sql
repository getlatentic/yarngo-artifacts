-- How long the recording behind a revision is.
--
-- The legacy store recorded this and the first import dropped it, so a voice
-- enrolled before this column existed showed no length at all. Nullable, and
-- backfilled by re-running the import rather than guessed from the file: a
-- recording that has since been deleted has no length to read, and inventing
-- one would be a claim about audio nobody can check.
ALTER TABLE voice_revisions ADD COLUMN duration_seconds REAL;
