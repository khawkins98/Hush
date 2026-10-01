-- Speaker-embedding version (#1013).
--
-- #1013 added per-utterance cepstral mean normalisation (and a
-- kaldi-compatible front end) to the diarizer, matching how the
-- wespeaker model was trained. That moves every embedding into a
-- different region of the space: voiceprints stored before it are not
-- comparable with ones computed after it, so matching them would give
-- meaningless distances.
--
-- Rather than delete the user's identities (and the names they gave
-- them), each row now records which embedding pipeline produced it.
-- Existing rows become version 1 ("pre-CMN") and are excluded from
-- automatic matching but keep labelling the past meetings they are
-- linked to; new sessions create version-2 identities. Merging a new
-- identity into an old one (`speaker_merge`) carries the new voiceprint
-- over. The constant lives in `speakers::CURRENT_EMBEDDING_VERSION`.
ALTER TABLE speaker_identities
    ADD COLUMN embedding_version INTEGER NOT NULL DEFAULT 1;

-- The saved speaker-split threshold was tuned against the old
-- embedding distribution and means something different now; drop it so
-- the new default applies (the Settings slider can set it again).
DELETE FROM settings WHERE key = 'diarizer_threshold';
