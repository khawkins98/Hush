-- Speaker identity persistence (#667).
--
-- `speaker_identities` stores one row per known cross-session speaker.
-- The `embedding` BLOB is a 256 × f32 (little-endian IEEE 754) = 1024-byte
-- running-mean centroid of all utterances ever linked to this identity.
-- Zeroized on delete (SQLite has no native crypto-erase; the DELETE + VACUUM
-- pattern is documented in the privacy notice). 
--
-- Privacy: embeddings are biometrics (voice fingerprints). The feature is
-- opt-in; the `speaker_identity_enabled` settings key controls it.
-- This table is only populated when the user has explicitly opted in.

CREATE TABLE IF NOT EXISTS speaker_identities (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- User-editable display name. NULL = auto-assigned provisional
    -- label ("Speaker 1", "Speaker 2", ...) — rendered with an
    -- "assign a name" affordance in the UI.
    display_name    TEXT,
    -- 1024-byte BLOB: 256 × f32 little-endian. Running weighted-mean
    -- centroid across all utterances ever assigned to this identity.
    embedding       BLOB NOT NULL,
    -- Total utterance count across all sessions (used to weight the
    -- running mean and for cold-start guard: skip matching when count < 5).
    utterance_count INTEGER NOT NULL DEFAULT 0,
    -- 'provisional' = auto-created but not yet confirmed; 'confirmed' =
    -- user has assigned a name or explicitly confirmed the match.
    -- Frontend can surface a "confirm these are the same person?" card.
    confidence_state TEXT NOT NULL DEFAULT 'provisional'
        CHECK(confidence_state IN ('provisional', 'confirmed')),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- Enforce exact embedding length.
    CHECK(length(embedding) = 1024)
);

-- Link utterances to a cross-session speaker identity. NULL while
-- speaker_identity_enabled is off or before the session-close
-- identity-resolution pass runs.
ALTER TABLE utterances ADD COLUMN speaker_identity_id INTEGER
    REFERENCES speaker_identities(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS utterances_speaker_identity_id_idx
    ON utterances(speaker_identity_id);
