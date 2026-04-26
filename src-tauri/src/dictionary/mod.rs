//! Personal Dictionary — find/replace and vocabulary biasing.
//!
//! Concept inspired by VoiceInk's Personal Dictionary. Reimplemented from
//! observed public behaviour; no source code referenced. See §13.8 of the
//! PRD.
//!
//! ## Scope of this module
//!
//! Two related-but-distinct subsystems live under "Personal Dictionary",
//! each backed by its own table in migration 0001:
//!
//! 1. **Post-transcription find/replace** ([`replacements`] submodule,
//!    `replacements` table) — literal substring rules applied to the
//!    model's output before it lands on the clipboard. Pure-logic, works
//!    without a Whisper model loaded.
//! 2. **Vocabulary prompt-biasing** ([`vocabulary`] submodule,
//!    `dictionary_terms` table) — terms formatted into the Whisper
//!    decoder's initial prompt to nudge it toward proper nouns, jargon,
//!    and personal-vocabulary spellings. Touches the
//!    [`crate::transcription::Transcribe`] trait so a transcriber can
//!    opt into prompt-biasing without forcing every backend to implement
//!    it (default impl ignores the prompt).
//!
//! Both subsystems live here because they share the user's mental
//! model ("things I want Hush to know about my words") even though they
//! act at different points in the pipeline. They share nothing in code
//! beyond the database connection and migration 0001 — see each
//! submodule for its own design notes and tests.

pub mod replacements;
pub mod vocabulary;

pub use replacements::{
    apply_replacements, NewReplacementRule, ReplacementRepository, ReplacementRule,
    SqliteReplacementRepository,
};
pub use vocabulary::{
    format_vocabulary_prompt, NewVocabularyTerm, SqliteVocabularyRepository, VocabularyRepository,
    VocabularyTerm, MAX_PROMPT_CHARS,
};
