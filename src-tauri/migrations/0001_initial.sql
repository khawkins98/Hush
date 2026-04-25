-- Initial schema for Hush v1.
-- Tables: history, dictionary_terms, replacements, settings.

CREATE TABLE IF NOT EXISTS history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    transcript  TEXT    NOT NULL,
    app_name    TEXT,
    window_title TEXT,
    model       TEXT    NOT NULL,
    duration_ms INTEGER,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE VIRTUAL TABLE IF NOT EXISTS history_fts
USING fts5(transcript, content='history', content_rowid='id');

CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
    INSERT INTO history_fts(rowid, transcript) VALUES (new.id, new.transcript);
END;

CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
    INSERT INTO history_fts(history_fts, rowid, transcript) VALUES ('delete', old.id, old.transcript);
END;

CREATE TABLE IF NOT EXISTS dictionary_terms (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    term  TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS replacements (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    find_text   TEXT NOT NULL,
    replace_text TEXT NOT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Defaults
INSERT OR IGNORE INTO settings (key, value) VALUES
    ('model',              'base'),
    ('hotkey_toggle',      'CmdOrCtrl+Shift+Space'),
    ('hotkey_push_to_talk', ''),
    ('audio_device',       ''),
    ('launch_at_login',    'false'),
    ('update_channel',     'stable'),
    ('telemetry_enabled',  'false');
