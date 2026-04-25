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
//! 1. **Post-transcription find/replace** (`replacements` table) —
//!    literal substring rules applied to the model's output before it
//!    lands on the clipboard. Pure-logic, works without a Whisper model
//!    loaded.
//! 2. **Vocabulary prompt-biasing** (`dictionary_terms` table) — terms
//!    formatted into the Whisper decoder's initial prompt to nudge it
//!    toward proper nouns, jargon, and personal-vocabulary spellings.
//!    Touches the [`crate::transcription::Transcribe`] trait so a
//!    transcriber can opt into prompt-biasing without forcing every
//!    backend to implement it (default impl ignores the prompt).
//!
//! Both subsystems live here because they share the user's mental
//! model ("things I want Hush to know about my words") even though they
//! act at different points in the pipeline.
//!
//! ## Design notes
//!
//! - **Replacement rules are literal substrings**, not regex. Predictable
//!   for users and easier to test. If users start asking for word-boundary
//!   matches or case-insensitivity, we add an enum on the rule rather
//!   than reaching for `regex` (which would pull in a heavyweight dep
//!   for what is currently a small list of rules).
//! - **Empty `find_text` is silently skipped.** `str::replace` with an
//!   empty needle produces a wedge between every byte boundary, which is
//!   never what a user intended. Skipping is the friendlier behaviour.
//! - **Apply order is `(sort_order, id)`**. Stable across restarts; users
//!   can reorder rules deliberately, and rules they added later only
//!   sort below if they accept the default `sort_order = 0`.
//! - **The post-replacement string is what's persisted to history.** We
//!   apply once, store the result, never reapply. Matches the user's
//!   mental model that "what went to my clipboard is what's in history."
//!
//! ## Test seam (PRD §13.5)
//!
//! [`apply_replacements`] is pure: takes a `&str` and a `&[ReplacementRule]`
//! slice, returns the rewritten string. Heavily unit-tested. The
//! [`ReplacementRepository`] trait sits at the storage boundary so the
//! IPC layer holds `Arc<dyn ReplacementRepository>` and tests can mock at
//! that seam.

pub mod sqlite;

pub use sqlite::{SqliteReplacementRepository, SqliteVocabularyRepository};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A persisted replacement rule.
///
/// Mirrors the migration-0001 `replacements` table. A "rule" applies
/// `find_text` → `replace_text` to the transcribed string after Whisper
/// runs, in the order defined by `sort_order` (then `id` for ties).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementRule {
    pub id: i64,
    pub find_text: String,
    pub replace_text: String,
    pub sort_order: i64,
}

/// Fields callers supply when creating a new rule. Separate from
/// [`ReplacementRule`] so the database-generated id can't be
/// accidentally hand-rolled.
#[derive(Debug, Clone)]
pub struct NewReplacementRule {
    pub find_text: String,
    pub replace_text: String,
    pub sort_order: i64,
}

/// Repository trait at the storage boundary. `Send + Sync` so the IPC
/// layer holds an `Arc<dyn ReplacementRepository>` across async Tauri
/// commands; object-safe via `async-trait`.
#[async_trait]
pub trait ReplacementRepository: Send + Sync {
    /// All rules, sorted by `(sort_order, id)`. The list is expected to
    /// be small (single-user, hand-managed), so no pagination.
    async fn list(&self) -> Result<Vec<ReplacementRule>>;

    /// Insert a new rule and return the persisted row (with its assigned
    /// id) so the frontend can append it to its local list without an
    /// extra round-trip.
    async fn create(&self, rule: NewReplacementRule) -> Result<ReplacementRule>;

    /// Update an existing rule's fields. No-op if `id` does not exist —
    /// the frontend's intent (this row should hold these values) is
    /// satisfied either way.
    async fn update(&self, rule: ReplacementRule) -> Result<()>;

    /// Delete a single rule. No-op if `id` does not exist, mirroring the
    /// trait contract on [`crate::history::HistoryRepository::delete`].
    async fn delete(&self, id: i64) -> Result<()>;
}

/// Apply a slice of replacement rules to `text`, returning the rewritten
/// string. Pure — no I/O, no global state — so the call from the IPC
/// layer's `stop_dictation` is a single function call against the rules
/// the repository handed back.
///
/// Rules with empty `find_text` are silently skipped (see the module
/// header for the rationale). Order is `(sort_order, id)` and is stable.
pub fn apply_replacements(text: &str, rules: &[ReplacementRule]) -> String {
    // Sort a temporary view so the caller's slice stays untouched. The
    // repository already returns rules sorted, but defending against an
    // unsorted caller (mock impls, future cache layers) is cheap.
    let mut ordered: Vec<&ReplacementRule> = rules.iter().collect();
    ordered.sort_by_key(|r| (r.sort_order, r.id));

    let mut out = text.to_owned();
    for rule in ordered {
        if rule.find_text.is_empty() {
            continue;
        }
        // `str::replace` allocates a new String on every call; for the
        // realistic rule count (handful of personal corrections) this is
        // far below a millisecond and not worth a Rope or two-pass scan.
        out = out.replace(&rule.find_text, &rule.replace_text);
    }
    out
}

// -- Vocabulary prompt-biasing --------------------------------------------
//
// Terms a user wants Whisper to recognise more reliably (proper nouns,
// jargon, names that are otherwise mis-transcribed). At inference time
// they are joined into a single comma-separated string and handed to
// whisper.cpp's `set_initial_prompt`, which biases the decoder's
// language model toward those tokens.
//
// Why a comma-separated list rather than free-form prose: prose prompts
// can accidentally bias the *content* of the transcription ("the user
// is talking about X") rather than just the vocabulary. A bare list of
// terms reads to the LM as "these tokens are likely to appear" without
// implying a topic.

/// A persisted vocabulary term.
///
/// Mirrors the migration-0001 `dictionary_terms` table. The table
/// constrains `term` to be unique (case-sensitive at the SQL level);
/// the IPC layer surfaces violations as `IpcError::Replacements` for
/// now since both subsystems share the dictionary error variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyTerm {
    pub id: i64,
    pub term: String,
}

/// Fields a caller supplies when adding a term. Separate from
/// [`VocabularyTerm`] so the database-assigned id can't be hand-rolled.
#[derive(Debug, Clone)]
pub struct NewVocabularyTerm {
    pub term: String,
}

/// Repository trait at the storage boundary. Same `Send + Sync` +
/// `async-trait` shape as [`ReplacementRepository`], so the IPC layer
/// holds `Arc<dyn VocabularyRepository>` and tests can mock at the seam.
#[async_trait]
pub trait VocabularyRepository: Send + Sync {
    /// All terms, sorted by `id` (insertion order). Expected to be small
    /// (the user manages this list by hand), so no pagination.
    async fn list(&self) -> Result<Vec<VocabularyTerm>>;

    /// Insert a new term. Errors if the term already exists — the
    /// `dictionary_terms.term` column is `UNIQUE` per the schema, and
    /// the duplicate is the user's signal to look at their existing
    /// list rather than a silent no-op (which would be confusing in
    /// the UI). Returns the persisted row so the frontend can append
    /// without a follow-up `list`.
    async fn create(&self, new_term: NewVocabularyTerm) -> Result<VocabularyTerm>;

    /// Update an existing term's text. Errors on `UNIQUE` collision so
    /// the user can recover ("you already have this term"). No-op if
    /// `id` does not exist, mirroring the rest of the repos.
    async fn update(&self, term: VocabularyTerm) -> Result<()>;

    /// Delete a term. No-op if `id` does not exist.
    async fn delete(&self, id: i64) -> Result<()>;
}

/// Cap on the prompt string we hand to whisper.cpp. Whisper.cpp's
/// `whisper_full` accepts an arbitrary-length initial prompt but
/// internally tokenises and truncates to ~224 tokens (per the
/// upstream code and observed behaviour); past that the surplus is
/// silently dropped. We cap on character count rather than tokens so
/// the formatter stays dependency-free, picking a value comfortably
/// under the token limit.
pub const MAX_PROMPT_CHARS: usize = 1024;

/// Build the prompt string Whisper sees from a vocabulary list.
///
/// - Trims each term and skips empty ones.
/// - Deduplicates case-insensitively while preserving the first
///   spelling the user entered (e.g. `"Hush"` then `"hush"` keeps
///   `"Hush"`).
/// - Joins with `", "` so the LM reads it as a list of distinct
///   tokens rather than a phrase.
/// - Greedily fills up to [`MAX_PROMPT_CHARS`]; remaining terms are
///   silently dropped (rare in practice — 1024 chars holds many
///   hundred typical words).
///
/// Returns an empty string when the input has no usable terms; the
/// caller's transcriber should treat that as "no prompt" rather than
/// passing the empty string to `set_initial_prompt`.
pub fn format_vocabulary_prompt(terms: &[VocabularyTerm]) -> String {
    let mut seen_lower: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut accepted: Vec<&str> = Vec::with_capacity(terms.len());
    let mut total_chars: usize = 0;

    for term in terms {
        let trimmed = term.term.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if !seen_lower.insert(lower) {
            continue;
        }
        // Reserve room for the ", " separator (only counted from the
        // second term onwards). Bail when the next term would push us
        // past the cap rather than truncating mid-word.
        let separator = if accepted.is_empty() { 0 } else { 2 };
        let needed = trimmed.chars().count() + separator;
        if total_chars + needed > MAX_PROMPT_CHARS {
            break;
        }
        accepted.push(trimmed);
        total_chars += needed;
    }

    accepted.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, find: &str, replace: &str, sort_order: i64) -> ReplacementRule {
        ReplacementRule {
            id,
            find_text: find.to_owned(),
            replace_text: replace.to_owned(),
            sort_order,
        }
    }

    #[test]
    fn empty_rules_returns_input_unchanged() {
        assert_eq!(apply_replacements("hello world", &[]), "hello world");
    }

    #[test]
    fn applies_a_single_literal_replacement() {
        let rules = [rule(1, "world", "Hush", 0)];
        assert_eq!(apply_replacements("hello world", &rules), "hello Hush");
    }

    #[test]
    fn applies_multiple_rules_in_sort_order_then_id() {
        // Two rules with the same sort_order should run in id order:
        // rule#1 strips "um ", rule#2 capitalises the now-stripped text.
        let rules = [rule(2, "hello", "Hi", 0), rule(1, "um ", "", 0)];
        assert_eq!(apply_replacements("um hello there", &rules), "Hi there");
    }

    #[test]
    fn sort_order_overrides_id_order() {
        // rule#1 (sort=10) runs after rule#2 (sort=0) because sort_order
        // takes precedence over id.
        let rules = [rule(1, "Hi", "Hello", 10), rule(2, "hello", "Hi", 0)];
        assert_eq!(apply_replacements("hello world", &rules), "Hello world");
    }

    #[test]
    fn empty_find_text_is_silently_skipped() {
        // A rule with an empty needle would otherwise wedge replacement
        // markers between every char boundary; that's never what a user
        // wants. Skip without surfacing an error so a bad row in the db
        // can't break the whole pipeline.
        let rules = [rule(1, "", "X", 0), rule(2, "world", "Hush", 0)];
        assert_eq!(
            apply_replacements("hello world", &rules),
            "hello Hush",
            "empty find_text rule should be skipped, second rule still applies"
        );
    }

    #[test]
    fn empty_replace_text_acts_as_deletion() {
        let rules = [rule(1, "um ", "", 0)];
        assert_eq!(
            apply_replacements("um hello um world um", &rules),
            "hello world um"
        );
        // Note the trailing "um" stays because there's no following space
        // — exactly what the user asked for. Power-user solution is to
        // add a second rule for "um$"-style cases when (if ever) we add
        // regex support.
    }

    #[test]
    fn replacements_are_case_sensitive() {
        // We do not silently lower-case; if a user wants case-insensitive
        // they add multiple rules. Documented in the module header.
        let rules = [rule(1, "HELLO", "Hi", 0)];
        assert_eq!(
            apply_replacements("hello HELLO Hello", &rules),
            "hello Hi Hello"
        );
    }

    #[test]
    fn rules_chain_through_each_others_output() {
        // rule#1 produces "foo bar"; rule#2 then operates on the *result*,
        // turning the new "bar" into "baz". Sometimes a feature, sometimes
        // a foot-gun — documented because it's worth knowing about.
        let rules = [rule(1, "hello", "foo bar", 0), rule(2, "bar", "baz", 1)];
        assert_eq!(apply_replacements("hello world", &rules), "foo baz world");
    }

    #[test]
    fn unicode_replacement_works() {
        let rules = [rule(1, "cafe", "café", 0), rule(2, "naive", "naïve", 0)];
        assert_eq!(
            apply_replacements("a cafe with naive vibes", &rules),
            "a café with naïve vibes"
        );
    }

    // -- format_vocabulary_prompt ----------------------------------------

    fn term(id: i64, text: &str) -> VocabularyTerm {
        VocabularyTerm {
            id,
            term: text.to_owned(),
        }
    }

    #[test]
    fn empty_vocab_produces_empty_prompt() {
        assert_eq!(format_vocabulary_prompt(&[]), "");
    }

    #[test]
    fn single_term_produces_just_the_term() {
        let terms = [term(1, "Hush")];
        assert_eq!(format_vocabulary_prompt(&terms), "Hush");
    }

    #[test]
    fn multiple_terms_are_joined_with_comma_space() {
        let terms = [term(1, "Hush"), term(2, "Tauri"), term(3, "whisper.cpp")];
        assert_eq!(format_vocabulary_prompt(&terms), "Hush, Tauri, whisper.cpp");
    }

    #[test]
    fn whitespace_only_terms_are_skipped() {
        let terms = [term(1, "  "), term(2, "Hush"), term(3, "\t\n")];
        assert_eq!(format_vocabulary_prompt(&terms), "Hush");
    }

    #[test]
    fn each_term_is_trimmed() {
        // Trailing whitespace on a vocab entry would otherwise produce a
        // weird ", Hush ," in the prompt that the LM sees as meaningful
        // separator behaviour. Trim per-term to keep the prompt clean.
        let terms = [term(1, "  Hush  ")];
        assert_eq!(format_vocabulary_prompt(&terms), "Hush");
    }

    #[test]
    fn duplicates_are_deduplicated_case_insensitively() {
        // The first spelling wins so the user's intent on capitalisation
        // is preserved (proper-noun terms typically have a "correct"
        // form that the user added first).
        let terms = [term(1, "Hush"), term(2, "hush"), term(3, "HUSH")];
        assert_eq!(format_vocabulary_prompt(&terms), "Hush");
    }

    #[test]
    fn dedup_preserves_distinct_unicode_normalised_terms() {
        // Lowercasing handles the common ASCII-cased duplicates; we
        // don't try to NFC-normalise (would pull in `unicode-normalization`
        // for a tiny gain). Distinct unicode forms are kept separate.
        let terms = [term(1, "café"), term(2, "Cafe")];
        let prompt = format_vocabulary_prompt(&terms);
        assert!(prompt.contains("café"));
        assert!(prompt.contains("Cafe"));
    }

    #[test]
    fn prompt_is_capped_at_max_chars() {
        // Each term is 50 chars; with ", " separators we can fit roughly
        // floor((1024 + 2) / 52) = 19 terms before the cap. Generate
        // enough that we *must* truncate, then assert the cap holds.
        let terms: Vec<VocabularyTerm> = (0..50)
            .map(|i| term(i, &format!("term-{i:0>43}")))
            .collect();
        let prompt = format_vocabulary_prompt(&terms);
        assert!(prompt.chars().count() <= MAX_PROMPT_CHARS);
        // And we still got *something* — the cap doesn't cause a panic
        // or an empty result.
        assert!(!prompt.is_empty());
    }

    #[test]
    fn prompt_truncation_does_not_cut_mid_term() {
        // The cap is enforced before each candidate term is appended;
        // no partial term should ever appear in the output. Easiest way
        // to check this: every comma-separated chunk in the result is
        // a complete trimmed term from the input.
        let terms: Vec<VocabularyTerm> = (0..50)
            .map(|i| term(i, &format!("term-{i:0>43}")))
            .collect();
        let prompt = format_vocabulary_prompt(&terms);
        let chunks: Vec<&str> = prompt.split(", ").collect();
        for chunk in chunks {
            assert!(
                terms.iter().any(|t| t.term == chunk),
                "chunk {chunk:?} is not a complete input term"
            );
        }
    }
}
