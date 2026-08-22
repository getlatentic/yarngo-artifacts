-- What the person read while the reference was recorded.
--
-- The model conditions better when it is told what the reference says, and the
-- enrolment script is that transcript exactly — reading a known sentence is
-- what removes any need to transcribe it afterwards. The legacy store kept it
-- and the first import dropped it, so a voice brought across was being cloned
-- from audio alone.
ALTER TABLE voice_revisions ADD COLUMN reference_text TEXT;
