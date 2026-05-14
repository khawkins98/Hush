//! Vocabulary prompt-biasing for Whisper.
//!
//! Terms a user wants Whisper to recognise more reliably (proper nouns,
//! jargon, names that are otherwise mis-transcribed). At inference time
//! they are joined into a single comma-separated string and handed to
//! whisper.cpp's `set_initial_prompt`, which biases the decoder's
//! language model toward those tokens. Backed by the `dictionary_terms`
//! table from migration 0001.
//!
//! Why a comma-separated list rather than free-form prose: prose prompts
//! can accidentally bias the *content* of the transcription ("the user
//! is talking about X") rather than just the vocabulary. A bare list of
//! terms reads to the LM as "these tokens are likely to appear" without
//! implying a topic.
//!
//! Touches the [`crate::transcription::Transcribe`] trait so a
//! transcriber can opt into prompt-biasing without forcing every
//! backend to implement it (default impl ignores the prompt).

pub mod sqlite;

pub use sqlite::SqliteVocabularyRepository;

use serde::{Deserialize, Serialize};

use crate::repository::Repository;

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

/// Storage-boundary trait for vocabulary terms.
///
/// Pure marker trait that aliases
/// [`Repository<VocabularyTerm, NewVocabularyTerm, i64>`] under a
/// domain-meaningful name. The four CRUD methods (`list`, `create`,
/// `update`, `delete`) live on the [`Repository`] supertrait — see
/// `crate::repository` for the rationale.
///
/// Domain-specific contract for `create`: the underlying SQLite impl
/// errors on `UNIQUE` collision (the `dictionary_terms.term` column
/// has a `UNIQUE` constraint per migration 0001). The duplicate is
/// the user's signal to look at their existing list rather than a
/// silent no-op. `update` carries the same UNIQUE-collision contract.
pub trait VocabularyRepository:
    Repository<VocabularyTerm, NewVocabularyTerm, i64> + Send + Sync
{
}

/// Blanket impl mirroring [`crate::dictionary::ReplacementRepository`].
impl<T> VocabularyRepository for T where
    T: Repository<VocabularyTerm, NewVocabularyTerm, i64> + Send + Sync
{
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

/// Like [`format_vocabulary_prompt`] but caps at `max_chars` instead of
/// [`MAX_PROMPT_CHARS`]. Internal helper for the budget-aware
/// [`format_initial_prompt`].
fn format_vocabulary_prompt_capped(terms: &[VocabularyTerm], max_chars: usize) -> String {
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
        let separator = if accepted.is_empty() { 0 } else { 2 };
        let needed = trimmed.chars().count() + separator;
        if total_chars + needed > max_chars {
            break;
        }
        accepted.push(trimmed);
        total_chars += needed;
    }

    accepted.join(", ")
}

/// Combine a language-style prefix and a vocabulary term list into a
/// single Whisper initial-prompt string.
///
/// The combined string is capped at [`MAX_PROMPT_CHARS`] characters.
/// If only the prefix fits, the vocabulary list is truncated or dropped;
/// the prefix is never truncated — it is always short enough to fit
/// within budget (max ~40 chars).
///
/// When `style_prefix` is empty this delegates to
/// [`format_vocabulary_prompt`] directly (same behaviour, no structural
/// overhead).
///
/// # Format
///
/// ```text
/// Use British English spelling.
///
/// Hush, Tauri, whisper.cpp
/// ```
pub(crate) fn format_initial_prompt(style_prefix: &str, terms: &[VocabularyTerm]) -> String {
    if style_prefix.is_empty() {
        return format_vocabulary_prompt(terms);
    }

    // Header = "Use British English spelling.\n\n" (prefix + two newlines).
    // We reserve this space from the prompt budget before handing the
    // remainder to format_vocabulary_prompt_capped (which also enforces
    // the cap).
    let separator = "\n\n";
    let header = format!("{style_prefix}{separator}");
    let header_chars = header.chars().count();

    if header_chars >= MAX_PROMPT_CHARS {
        // Style prefix alone already fills the budget; emit it without vocab.
        return style_prefix
            .chars()
            .take(MAX_PROMPT_CHARS)
            .collect::<String>();
    }

    let remaining = MAX_PROMPT_CHARS - header_chars;

    // Trim the vocabulary section to fit within the remaining budget.
    let vocab_part = format_vocabulary_prompt_capped(terms, remaining);

    if vocab_part.is_empty() {
        style_prefix.to_owned()
    } else {
        format!("{header}{vocab_part}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
