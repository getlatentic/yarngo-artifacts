-- Preferences, which is what this always was.
--
-- Named for models because models were the first thing kept in it. The first
-- thing that is not — which speech runtime to start — would have had to go
-- somewhere called model_preferences, and a name that has to be explained is a
-- name that will mislead somebody.
ALTER TABLE model_preferences RENAME TO preferences;
