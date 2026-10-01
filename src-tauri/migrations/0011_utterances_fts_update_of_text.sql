-- Narrow the utterances FTS update trigger to text changes.
--
-- 0002 created `utterances_au` as `AFTER UPDATE ON utterances`, so every
-- UPDATE of any column re-indexed the row in `utterances_fts` (an FTS5
-- 'delete' + re-insert). The only UPDATEs the app issues touch
-- `speaker_label` (session-end re-cluster, `relabel_utterances`) and
-- `speaker_identity_id` (identity resolution, speaker merge) — whole
-- sessions at a time, none of which change the indexed text. Firing only
-- on `UPDATE OF text` keeps the index exactly in sync (text is the only
-- indexed column) without that churn.
DROP TRIGGER IF EXISTS utterances_au;
CREATE TRIGGER utterances_au AFTER UPDATE OF text ON utterances BEGIN
    INSERT INTO utterances_fts(utterances_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO utterances_fts(rowid, text) VALUES (new.id, new.text);
END;
