-- What the take measured when it was accepted. On the revision rather than the
-- asset: the numbers describe this recording of this voice, and a voice that
-- sounds wrong later is diagnosed from them.
ALTER TABLE voice_revisions ADD COLUMN snr_db REAL;
ALTER TABLE voice_revisions ADD COLUMN sample_rate_hz INTEGER;
