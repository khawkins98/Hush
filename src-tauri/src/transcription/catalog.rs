//! Static catalog of Whisper model variants supported by Hush.
//!
//! ## Why a static catalog rather than a discovered list
//!
//! Whisper.cpp is the single transcription engine (PRD §5), and the
//! model line-up is fixed by upstream — a handful of sizes plus the
//! quantized builds upstream publishes alongside them — and that's all
//! the picker needs to know about. Hardcoding the list:
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
//!
//! ## Quantized variants
//!
//! Upstream publishes `q5_*` / `q8_0` builds of each model next to the
//! full-precision files. q8_0 is commonly measured as indistinguishable
//! from f16 and q5 as the last "near-lossless" step — at roughly half
//! / a third of the file size, and a proportionally smaller resident
//! footprint, which is the constraint Hush cares most about (see
//! `docs/memory-debugging.md`). whisper.cpp loads them natively; nothing
//! else in the pipeline changes. We list a curated few rather than
//! every build so the picker stays scannable, and skip the `.en`
//! English-only builds: they silently fail non-English speech.
//!
//! This breaks the "bigger file = more accurate" line the picker used to
//! imply (Turbo q8_0 beats Medium at ~60% of its size), so the catalog is
//! ordered by model family, each quantized build next to its full one.

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

    /// Filename the model is expected to live under in the app's
    /// models directory (e.g. `ggml-base.bin`). Resolved by
    /// `crate::transcription::download` (auto-download from
    /// Hugging Face, SHA-256 verified) or by manual placement.
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
/// The catalog is small (under a dozen entries, a few KB), so
/// allocating per-call is cheaper than the synchronisation cost of a
/// shared static `Vec` would be. The IPC command builds it once per
/// `model_list` call; nothing on the hot path consults this.
pub fn whisper_models() -> Vec<ModelMetadata> {
    // SHA-256 hashes sourced from Hugging Face's git-lfs `oid` field
    // for ggerganov/whisper.cpp (closes #41). Verified 2026-04-26 by
    // pulling the tiny model and confirming `shasum -a 256` matched
    // the API value. The other four are taken from the same API
    // response in the same call; LFS oids are content-addressed so
    // they cannot drift independently of the file itself, but if
    // upstream re-uploads the model with the same filename these
    // hashes need updating and the auto-download will surface a
    // clean SHA-256 mismatch error to the user.
    //
    // To refresh:
    //
    //     curl -s "https://huggingface.co/api/models/ggerganov/whisper.cpp/tree/main?expand=true" \
    //       | python3 -c 'import sys,json; \
    //         [print(f"{f[\"path\"]}: {f.get(\"lfs\",{}).get(\"oid\",\"?\")}") \
    //          for f in json.load(sys.stdin) if f.get("path","").startswith("ggml-")]'
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
            sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21".into(),
        },
        ModelMetadata {
            id: "whisper-base".into(),
            display_name: "Whisper Base".into(),
            filename: "ggml-base.bin".into(),
            size_mb: 142,
            speed_rating: 9,
            accuracy_rating: 6,
            description: "Fast and lightweight. Good for quick notes on lower-end hardware; weaker on accents and jargon.".into(),
            is_default: false,
            download_url: download_url_for("ggml-base.bin"),
            sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe".into(),
        },
        ModelMetadata {
            id: "whisper-small-q8_0".into(),
            display_name: "Whisper Small (compact)".into(),
            filename: "ggml-small-q8_0.bin".into(),
            size_mb: 264,
            speed_rating: 7,
            accuracy_rating: 8,
            description: "Small, 8-bit quantized. Near-identical accuracy to Small at about half the download and memory.".into(),
            is_default: false,
            download_url: download_url_for("ggml-small-q8_0.bin"),
            sha256: "49c8fb02b65e6049d5fa6c04f81f53b867b5ec9540406812c643f177317f779f".into(),
        },
        ModelMetadata {
            id: "whisper-small".into(),
            display_name: "Whisper Small".into(),
            filename: "ggml-small.bin".into(),
            size_mb: 466,
            speed_rating: 7,
            accuracy_rating: 8,
            description: "Recommended default. Noticeably better accuracy for accents and technical vocabulary at near-real-time speed on Apple Silicon.".into(),
            is_default: true,
            download_url: download_url_for("ggml-small.bin"),
            sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b".into(),
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
            sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208".into(),
        },
        ModelMetadata {
            id: "whisper-large-v3-turbo-q5_0".into(),
            display_name: "Whisper Turbo (smallest)".into(),
            filename: "ggml-large-v3-turbo-q5_0.bin".into(),
            size_mb: 574,
            speed_rating: 7,
            accuracy_rating: 9,
            description: "Turbo, 5-bit quantized. Most of Turbo's accuracy at about a third of its size — the lightest high-accuracy option.".into(),
            is_default: false,
            download_url: download_url_for("ggml-large-v3-turbo-q5_0.bin"),
            sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2".into(),
        },
        ModelMetadata {
            id: "whisper-large-v3-turbo-q8_0".into(),
            display_name: "Whisper Turbo (compact)".into(),
            filename: "ggml-large-v3-turbo-q8_0.bin".into(),
            size_mb: 874,
            speed_rating: 7,
            accuracy_rating: 10,
            description: "Turbo, 8-bit quantized. Near-identical accuracy to Turbo at about half the download and memory.".into(),
            is_default: false,
            download_url: download_url_for("ggml-large-v3-turbo-q8_0.bin"),
            sha256: "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1".into(),
        },
        ModelMetadata {
            id: "whisper-large-v3-turbo".into(),
            display_name: "Whisper Turbo".into(),
            filename: "ggml-large-v3-turbo.bin".into(),
            size_mb: 1625,
            speed_rating: 7,
            accuracy_rating: 10,
            description:
                "Distilled Large v3. Near-large accuracy at roughly 8× the speed; the modern default for accuracy-leaning work."
                    .into(),
            is_default: false,
            download_url: download_url_for("ggml-large-v3-turbo.bin"),
            sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69".into(),
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
            sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2".into(),
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
        // Removing an id strands users whose saved selection points at it
        // (they silently fall back to the default), so this list only grows.
        let ids: Vec<String> = whisper_models().into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&"whisper-tiny".to_string()));
        assert!(ids.contains(&"whisper-base".to_string()));
        assert!(ids.contains(&"whisper-small".to_string()));
        assert!(ids.contains(&"whisper-medium".to_string()));
        assert!(ids.contains(&"whisper-large-v3-turbo".to_string()));
        assert!(ids.contains(&"whisper-large-v3".to_string()));
        assert!(ids.contains(&"whisper-small-q8_0".to_string()));
        assert!(ids.contains(&"whisper-large-v3-turbo-q5_0".to_string()));
        assert!(ids.contains(&"whisper-large-v3-turbo-q8_0".to_string()));
    }

    #[test]
    fn exactly_one_default_model() {
        let count = whisper_models().iter().filter(|m| m.is_default).count();
        assert_eq!(count, 1, "catalog should declare exactly one default model");
    }

    #[test]
    fn default_model_is_whisper_small_per_prd() {
        // PRD §6: "Default to `small` Q5_0". If we ever change the
        // default this test reminds us to update the PRD too.
        assert_eq!(default_model().id, "whisper-small");
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
    fn full_precision_size_is_monotonic_with_accuracy() {
        // Across full-precision models the size/quality curve is
        // monotonic — bigger model = higher accuracy. Quantized builds
        // deliberately break that line (see module note), so they are
        // excluded here and checked against their own family below.
        let mut prev_size = 0u32;
        let mut prev_acc = 0u8;
        for m in whisper_models().iter().filter(|m| !is_quantized(m)) {
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

    fn is_quantized(m: &ModelMetadata) -> bool {
        m.id.contains("-q5_") || m.id.contains("-q8_")
    }

    #[test]
    fn quantized_variants_are_smaller_and_no_more_accurate_than_full() {
        // A quantized card must sit next to (before) its full-precision
        // sibling, be smaller, and never claim *more* accuracy — the
        // rating bars would otherwise tell users to prefer the lossy build.
        let models = whisper_models();
        for (i, q) in models.iter().enumerate().filter(|(_, m)| is_quantized(m)) {
            let base_id = q.id.rsplit_once('-').map(|(b, _)| b).unwrap();
            let base = find_by_id(base_id)
                .unwrap_or_else(|| panic!("{}: no full-precision sibling {base_id}", q.id));
            let base_pos = models.iter().position(|m| m.id == base_id).unwrap();
            assert!(i < base_pos, "{}: should be listed before {base_id}", q.id);
            assert!(
                q.size_mb < base.size_mb,
                "{}: not smaller than {base_id}",
                q.id
            );
            assert!(
                q.accuracy_rating <= base.accuracy_rating,
                "{}: rated more accurate than {base_id}",
                q.id
            );
            assert!(q.filename.starts_with("ggml-") && q.filename.ends_with(".bin"));
        }
    }

    #[test]
    fn every_downloadable_model_has_a_sha256() {
        // The downloader refuses models without a hash; a new catalog
        // entry missing one would show a Download button that can't work.
        for m in whisper_models() {
            assert_eq!(m.sha256.len(), 64, "{}: sha256 missing or malformed", m.id);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{}", m.id);
        }
    }
}
