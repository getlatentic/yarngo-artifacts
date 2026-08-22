-- A fingerprint of the recording the permission was given about.
--
-- What the application promises: the wording as it stood, the build, and a
-- fingerprint of the audio it referred to. Without it a consent record names a
-- voice and cannot show which recording that voice was actually made from.
ALTER TABLE consent_events ADD COLUMN reference_sha256 TEXT;
