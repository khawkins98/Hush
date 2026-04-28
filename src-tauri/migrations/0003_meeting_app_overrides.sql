-- Per-app classifier overrides (Phase E, refs #112).
--
-- The hard-coded `AppClassifier::default_table` covers the apps Hush
-- expects to encounter most often (Zoom, Teams, Slack, Spotify, …),
-- but real usage surfaces apps the table doesn't know about — and
-- occasionally classifies an app the user disagrees with (e.g. their
-- internal-tool web app should be treated as a meeting app).
--
-- This table holds user-supplied overrides. The classifier consults
-- it before falling back to the static defaults; an override row
-- with the same `app_name` as a default wins.
--
-- `app_name` matches whatever
-- `active-win-pos-rs::get_active_window().app_name` returns on each
-- platform — bundle id on macOS, process / window class elsewhere.
CREATE TABLE IF NOT EXISTS meeting_app_overrides (
    app_name   TEXT PRIMARY KEY,
    -- Classifier verdict the user has chosen. Mirrors
    -- `MeetingAppKind`'s serde repr: "meeting" | "media" | "other".
    -- A row with kind = "other" is the way to ignore an app the
    -- defaults marked as Meeting/Media.
    kind       TEXT NOT NULL CHECK (kind IN ('meeting', 'media', 'other')),
    -- Audit hint for the user: when did they add this override.
    -- Renders next to the row in the panel so a stale entry from
    -- months ago is identifiable. ISO-8601 UTC.
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
