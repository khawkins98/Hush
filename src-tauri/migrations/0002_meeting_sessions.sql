-- Meeting Mode tables (Phase C foundation, refs #33 / #109).
--
-- Sessions group together a meeting's worth of utterances. Started
-- by the SessionManager (#110) when a meeting app produces audio,
-- ended on silence or app change.
--
-- Privacy invariant: ONLY transcripts and timestamps live here. Raw
-- audio bytes are never persisted — they live in a RAM ring buffer
-- during inference and are discarded once each window has been
-- transcribed. See `docs/system-audio-meeting-mode-proposal.md` for
-- the full architectural rationale.
CREATE TABLE IF NOT EXISTS meeting_sessions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Bundle id / process name of the foreground app at session
    -- start (e.g. "us.zoom.xos", "com.microsoft.teams2").
    app_name        TEXT NOT NULL,
    -- Classifier verdict at the time the session opened. One of
    -- "meeting" | "media" | "other". Persisted alongside the row
    -- so a future classifier change doesn't retroactively re-label
    -- past sessions.
    app_kind        TEXT NOT NULL,
    -- ISO-8601 UTC. Same `strftime('%Y-%m-%dT%H:%M:%SZ','now')`
    -- shape used by the history table.
    started_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- NULL while the session is in-flight; populated when the
    -- SessionManager closes the session.
    ended_at        TEXT,
    -- Denormalised counts so the sessions list view doesn't need
    -- a JOIN-with-COUNT for every row. Maintained by the
    -- SessionManager when it inserts utterances.
    speaker_count   INTEGER,
    utterance_count INTEGER NOT NULL DEFAULT 0,
    -- User-editable freeform note (post-meeting summary, action
    -- items, etc). Nullable; the panel renders an "add notes"
    -- affordance when empty.
    notes           TEXT
);

CREATE INDEX IF NOT EXISTS meeting_sessions_started_at_idx
    ON meeting_sessions(started_at DESC);

-- One row per utterance the streaming transcriber emitted as final.
-- (Non-final / partial utterances are forwarded via Tauri events
-- but not persisted — they're for live UI updates only.)
CREATE TABLE IF NOT EXISTS utterances (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    -- Offsets from the parent session's started_at, in ms.
    -- Cheaper to query than wall-clock timestamps and avoids
    -- storing the same wall-clock prefix on every row.
    started_at_ms   INTEGER NOT NULL,
    ended_at_ms     INTEGER NOT NULL,
    -- Diarization label ("Speaker A", "Speaker B", or user-renamed).
    -- NULL until Phase D (#111) ships speaker segmentation.
    speaker_label   TEXT,
    text            TEXT NOT NULL,
    -- Reserved column. v1 only persists is_final = 1 utterances
    -- (partials don't survive past the live UI update); kept here
    -- so a future Phase E feature ("show recent partials in the
    -- timeline view") can persist them without a schema change.
    is_final        INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS utterances_session_id_idx
    ON utterances(session_id, started_at_ms);

-- FTS5 index for cross-session search (the meeting-mode panel's
-- "find that thing my colleague said about X" feature). Matches
-- the same content/content_rowid pattern used by the history table.
CREATE VIRTUAL TABLE IF NOT EXISTS utterances_fts USING fts5(
    text,
    content='utterances',
    content_rowid='id'
);

-- Triggers keep the FTS index in sync with the base table. Same
-- shape as the history table's triggers in 0001_initial.sql.
CREATE TRIGGER IF NOT EXISTS utterances_ai AFTER INSERT ON utterances BEGIN
    INSERT INTO utterances_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS utterances_ad AFTER DELETE ON utterances BEGIN
    INSERT INTO utterances_fts(utterances_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS utterances_au AFTER UPDATE ON utterances BEGIN
    INSERT INTO utterances_fts(utterances_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO utterances_fts(rowid, text) VALUES (new.id, new.text);
END;
