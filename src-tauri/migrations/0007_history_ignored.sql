-- Add `ignored` flag to history rows.
-- Rows with ignored = 1 are visible in the history list so the user can
-- see that a press was detected but skipped, while being excluded from
-- stats aggregates (session count, word count, keystrokes saved) and
-- bulk export. The flag is set for recordings that were too short to
-- transcribe (< 1 s).
ALTER TABLE history ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0;
