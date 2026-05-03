-- Per-app audio profiles — schema (refs #427 Item 5).
--
-- Extends `meeting_app_overrides` with two optional columns so a
-- future iteration can pin a preferred audio source and model to a
-- specific app. When focus moves to an app with a populated
-- profile, the foreground-app watcher will swap the active source
-- + model to the configured values before the next dictation
-- starts.
--
-- This migration only lands the storage. The IPC + frontend UI
-- that exposes the dropdowns and the foreground-watcher logic
-- that auto-switches both ship in follow-up PRs (#427 Item 5
-- staging). Existing rows get NULL in both columns; existing
-- read paths ignore the new columns until callers are extended
-- to consume them.
--
-- Why nullable + additive rather than a redesign: the override
-- table already serves the classifier-only use case (Meeting /
-- Media / Other), and most users won't pin per-app audio
-- profiles. Rows without profile values continue to work
-- exactly as before.

ALTER TABLE meeting_app_overrides
    ADD COLUMN preferred_audio_source TEXT;

ALTER TABLE meeting_app_overrides
    ADD COLUMN preferred_model_id TEXT;
