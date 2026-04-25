//! Static catalog of Whisper model variants supported by Hush.
//!
//! ## Why a static catalog rather than a discovered list
//!
//! Whisper.cpp is the single transcription engine (PRD §5), and the
//! model line-up is fixed by upstream — there are five sizes (tiny,
//! base, small, medium, large-v3) and that's all the picker needs to
//! know about. Hardcoding the list:
//!
//! - Lets the picker show metadata (size, speed/accuracy ratings,
//!   description) without round-tripping a remote index.
//! - Means the app starts with a known set of models the picker can
//!   render greyed-out cards for, even before any have been downloaded.
//! - Avoids a dependency on a network-fetched manifest, in line with
//!   the "no cloud round-trip" privacy posture (§3).
//!
//! When the user wants Parakeet (see `memory/parakeet_request.md`),
//! revising PRD §5 changes this catalog and adds the engine selection.
//! For now: whisper variants only.
//!
//! ## Quality ratings
//!
//! `speed_rating` and `accuracy_rating` are 1–10 scores meant for the
//! card UI's bar visual, not for any decision logic. They reflect
//! upstream's published benchmarks roughly: tiny is fastest /
//! least-accurate, large-v3 is slowest / most-accurate, base is the
//! all-rounder default per PRD §6. The scores are deliberately
//! impressionistic; if we want hard numbers later we'll measure on a
//! reference machine and pin per-platform values.

use serde::{Deserialize, Serialize};

/// Static metadata for one model in the picker.
///
/// Owned `String` fields rather than `&'static str` so the type can
/// cross the Tauri IPC boundary as `Vec<ModelMetadata>` without
/// borrow-lifetime gymnastics. The catalog allocates these at first
/// access and clones cheaply enough for the frontend's needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMetadata {
    /// Stable identifier used in settings (`selected_model_id`) and IPC.
    /// Format `whisper-<size>` mirrors the upstream naming so it survives
    /// log greps. Not user-facing — see [`Self::display_name`].
    pub id: String,

    /// User-facing name shown on the picker card (e.g. "Whisper Base").
    pub display_name: String,

    /// Filename the model is expected to live under in the app's models
    /// directory (e.g. `ggml-base.bin`). Hush does not yet auto-download;
    /// users place files manually until that lands.
    pub filename: String,

    /// On-disk size in MB, for the picker card. Approximate — actual
    /// file sizes vary slightly between quantisation builds.
    pub size_mb: u32,

    /// 1–10 perceived-speed rating (10 = fastest). See module note on
    /// quality ratings.
    pub speed_rating: u8,

    /// 1–10 perceived-accuracy rating (10 = most accurate).
    pub accuracy_rating: u8,

    /// One-line description shown under the name on the card. Plain
    /// English; no jargon the user can't already see in the size or
    /// rating bars.
    pub description: String,

    /// Marks the model Hush recommends if the user has not picked yet.
    /// At most one model in the catalog has this set to `true`.
    pub is_default: bool,

    /// HTTP(S) URL to fetch the GGUF file from when the user clicks
    /// **Download**. Hard-coded against the upstream `ggerganov/whisper.cpp`
    /// Hugging Face mirror — no mirror configuration in v1; the URL is
    /// the only outbound network request the app ever makes, and we
    /// want it audit-able from one place.
    pub download_url: String,

    /// Expected SHA-256 of the downloaded file, hex-encoded.
    ///
    /// Used by the download orchestrator to verify integrity end-to-end.
    /// **Empty string means "verification not yet configured"** — the
    /// auto-download command refuses to start a download for such a
    /// model and surfaces a clear error to the user, falling back to
    /// "place file manually" until a contributor verifies the hash and
    /// fills it in. This is a deliberate gate, not a bug; see
    /// `learnings.md` for the trust-on-first-use trade we considered
    /// and rejected.
    pub sha256: String,
}

/// Base URL for the upstream Whisper GGUF mirror. Hard-coded; no
/// mirror selection in v1. If we ever need it, it goes here.
pub const WHISPER_DOWNLOAD_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Compute the canonical download URL for a Whisper variant given its
/// filename. Pulled out of the catalog body so it's testable on its
/// own and so the base URL only appears in one place.
fn download_url_for(filename: &str) -> String {
    format!("{WHISPER_DOWNLOAD_BASE}/{filename}")
}

/// Returns the static catalog. Allocates owned strings on each call —
/// call once at startup or per-IPC-command, not in a hot loop.
///
/// ## Why a function rather than a `lazy_static!` / `OnceCell`
///
/// The catalog is small (five entries, a few hundred bytes), so
/// allocating per-call is cheaper than the synchronisation cost of a
/// shared static `Vec` would be. The IPC command builds it once per
/// `model_list` call; nothing on the hot path consults this.
pub fn whisper_models() -> Vec<ModelMetadata> {
    // SHA-256 hashes deliberately empty for the initial cut. The
    // auto-download command checks for an empty string and refuses to
    // start the download with a clear "download manually until the
    // hash is verified" error. Fill in per-model as a contributor
    // verifies the hash from upstream — see TODO(#41).
    vec![
        ModelMetadata {
            id: "whisper-tiny".into(),
            display_name: "Whisper Tiny".into(),
            filename: "ggml-tiny.bin".into(),
            size_mb: 75,
            speed_rating: 10,
            accuracy_rating: 4,
            description: "Fastest variant. Good for quick notes; weak on accents and proper nouns."
                .into(),
            is_default: false,
            download_url: download_url_for("ggml-tiny.bin"),
            sha256: String::new(),
        },
        ModelMetadata {
            id: "whisper-base".into(),
            display_name: "Whisper Base".into(),
            filename: "ggml-base.bin".into(),
            size_mb: 142,
            speed_rating: 9,
            accuracy_rating: 6,
            description: "Recommended default. Solid accuracy at near-real-time speed.".into(),
            is_default: true,
            download_url: download_url_for("ggml-base.bin"),
            sha256: String::new(),
        },
        ModelMetadata {
            id: "whisper-small".into(),
            display_name: "Whisper Small".into(),
            filename: "ggml-small.bin".into(),
            size_mb: 466,
            speed_rating: 7,
            accuracy_rating: 8,
            description: "Better accuracy for technical jargon and accents. ~3× slower than base."
                .into(),
            is_default: false,
            download_url: download_url_for("ggml-small.bin"),
            sha256: String::new(),
        },
        ModelMetadata {
            id: "whisper-medium".into(),
            display_name: "Whisper Medium".into(),
            filename: "ggml-medium.bin".into(),
            size_mb: 1500,
            speed_rating: 5,
            accuracy_rating: 9,
            description: "High-accuracy. Recommended only on M-series Macs or recent x86.".into(),
            is_default: false,
            download_url: download_url_for("ggml-medium.bin"),
            sha256: String::new(),
        },
        ModelMetadata {
            id: "whisper-large-v3".into(),
            display_name: "Whisper Large v3".into(),
            filename: "ggml-large-v3.bin".into(),
            size_mb: 3094,
            speed_rating: 3,
            accuracy_rating: 10,
            description: "Top-tier accuracy. Slow on consumer hardware — for offline batch use."
                .into(),
            is_default: false,
            download_url: download_url_for("ggml-large-v3.bin"),
            sha256: String::new(),
        },
    ]
}

/// Look up a model by id. Returns `None` for unknown ids; callers
/// should treat that as "selection setting points at a model we no
/// longer recognise" and fall back to the default.
pub fn find_by_id(id: &str) -> Option<ModelMetadata> {
    whisper_models().into_iter().find(|m| m.id == id)
}

/// The catalog's default model — the one with `is_default = true`.
/// Panics in debug builds if the catalog has no default; in release
/// returns the first entry as a fallback so the app keeps booting.
pub fn default_model() -> ModelMetadata {
    let models = whisper_models();
    debug_assert!(
        models.iter().any(|m| m.is_default),
        "catalog must declare exactly one default model"
    );
    models
        .iter()
        .find(|m| m.is_default)
        .cloned()
        .unwrap_or_else(|| models.into_iter().next().expect("catalog is non-empty"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_expected_whisper_variants() {
        let ids: Vec<String> = whisper_models().into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&"whisper-tiny".to_string()));
        assert!(ids.contains(&"whisper-base".to_string()));
        assert!(ids.contains(&"whisper-small".to_string()));
        assert!(ids.contains(&"whisper-medium".to_string()));
        assert!(ids.contains(&"whisper-large-v3".to_string()));
    }

    #[test]
    fn exactly_one_default_model() {
        let count = whisper_models().iter().filter(|m| m.is_default).count();
        assert_eq!(count, 1, "catalog should declare exactly one default model");
    }

    #[test]
    fn default_model_is_whisper_base_per_prd() {
        // PRD §6: "Default to `base` Q5_0". If we ever change the
        // default this test reminds us to update the PRD too.
        assert_eq!(default_model().id, "whisper-base");
    }

    #[test]
    fn find_by_id_returns_known_model() {
        let m = find_by_id("whisper-tiny").expect("whisper-tiny must be in catalog");
        assert_eq!(m.display_name, "Whisper Tiny");
    }

    #[test]
    fn find_by_id_returns_none_for_unknown() {
        assert!(find_by_id("whisper-imaginary").is_none());
        assert!(find_by_id("").is_none());
    }

    #[test]
    fn ratings_are_within_1_to_10_range() {
        // Sanity-check the impressionistic ratings so a typo (e.g. 100
        // instead of 10) doesn't render off-card.
        for m in whisper_models() {
            assert!(
                (1..=10).contains(&m.speed_rating),
                "{} speed_rating out of range: {}",
                m.id,
                m.speed_rating
            );
            assert!(
                (1..=10).contains(&m.accuracy_rating),
                "{} accuracy_rating out of range: {}",
                m.id,
                m.accuracy_rating
            );
        }
    }

    #[test]
    fn size_mb_is_monotonic_with_accuracy() {
        // Whisper's size/quality curve is monotonic — bigger model =
        // higher accuracy. If we ever add a model that breaks this, we
        // should rethink the picker UX (size and accuracy bars
        // currently both grow rightward, so a non-monotonic catalog
        // would mislead the user into thinking they're the same metric).
        let models = whisper_models();
        let mut prev_size = 0u32;
        let mut prev_acc = 0u8;
        for m in &models {
            assert!(
                m.size_mb >= prev_size,
                "{}: size_mb regressed (catalog out of order?)",
                m.id
            );
            assert!(
                m.accuracy_rating >= prev_acc,
                "{}: accuracy_rating regressed",
                m.id
            );
            prev_size = m.size_mb;
            prev_acc = m.accuracy_rating;
        }
    }
}
