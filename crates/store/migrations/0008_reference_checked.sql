-- When a voice's reference text was last checked against its recording.
--
-- The text has to describe the audio: a voice whose text claims words the
-- recording does not contain makes the model speak them before everything it
-- is later asked for. Voices enrolled before that was checked have the whole
-- script stored whether or not it was read, so they are repaired once — and
-- this is how a repaired one is told from a waiting one.
ALTER TABLE voice_revisions ADD COLUMN reference_checked_at TEXT;
