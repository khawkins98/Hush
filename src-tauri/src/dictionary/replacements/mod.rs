//! Post-transcription find/replace.
//!
//! Literal substring rules applied to the model's output before it
//! lands on the clipboard. Pure-logic, works without a Whisper model
//! loaded. Backed by the `replacements` table from migration 0001.
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

pub use sqlite::SqliteReplacementRepository;

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
}
