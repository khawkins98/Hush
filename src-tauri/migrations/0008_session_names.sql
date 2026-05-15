-- User-editable name for dictation history rows and meeting sessions.
--
-- Both are nullable: existing rows keep NULL (the frontend renders a
-- "Name this…" placeholder in that case). The column is separate from
-- `notes` on meeting_sessions — `notes` is the post-meeting freeform
-- text block; `name` is a short title for the session.
ALTER TABLE history ADD COLUMN name TEXT;
ALTER TABLE meeting_sessions ADD COLUMN name TEXT;
