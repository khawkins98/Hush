//! Static catalog of speaker-embedding models for the D2 diarizer
//! (#111).
//!
//! Single-entry catalog today — wespeaker ResNet34-LM is the model
//! the OnnxDiarizer is built around. The catalog shape mirrors
//! [`crate::transcription::catalog`] so future variants (a smaller
//! / faster speaker model, a larger / more accurate one) drop in
//! without a refactor.
//!
//! ## Why a separate catalog instead of folding into the Whisper one
//!
//! Whisper models and speaker-embedding models are orthogonal: a
//! user who picks `whisper-large-v3` for transcription doesn't also
//! pick a speaker model — the diarizer has exactly one. Folding
//! both into the same catalog would force the picker UX to model
//! "engine type" (transcribe vs diarize) which is one more concept
//! the user shouldn't have to think about.

use serde::{Deserialize, Serialize};

/// Static metadata for a speaker-embedding model.
///
/// Field shape matches [`crate::transcription::catalog::ModelMetadata`]
/// minus the speed/accuracy ratings — we only ship one model so
/// per-model ratings would be misleading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizerModelMetadata {
    /// Stable identifier used in settings and IPC. Format
    /// `<family>-<variant>`; e.g. `wespeaker-resnet34-lm`.
    pub id: String,
    /// User-facing name shown in any future Settings → Diarizer card.
    pub display_name: String,
    /// Filename the model is expected to live under in the app's
    /// models directory.
    pub filename: String,
    /// On-disk size in MB.
    pub size_mb: u32,
    /// One-line description for any future picker UI.
    pub description: String,
    /// HTTP(S) URL to fetch the ONNX file from. Hard-coded to the
    /// upstream Wespeaker Hugging Face repo per the same hardening
    /// rationale as the Whisper catalog (one outbound origin, audit-
    /// able from one place).
    pub download_url: String,
    /// Expected SHA-256 of the downloaded file, hex-encoded.
    /// Verified against Hugging Face's git-lfs `oid` field on
    /// 2026-04-30. LFS oids are content-addressed so they cannot
    /// drift independently of the file itself; if upstream
    /// re-uploads under the same filename the auto-download will
    /// surface a clean SHA-mismatch error.
    pub sha256: String,
}

/// Identifier for the only diarizer model Hush ships today.
/// Constants like this make grep-ability easier than scattering
/// the literal string across the codebase.
pub const WESPEAKER_RESNET34_LM_ID: &str = "wespeaker-resnet34-lm";

/// Filename the wespeaker ONNX file is loaded from.
pub const WESPEAKER_RESNET34_LM_FILENAME: &str = "voxceleb_resnet34_LM.onnx";

/// Returns the diarizer-model catalog. Same shape as
/// [`crate::transcription::catalog::whisper_models`] — owned strings,
/// allocated per-call.
///
/// To refresh the SHA-256 against upstream:
/// ```ignore
/// curl -s "https://huggingface.co/api/models/Wespeaker/wespeaker-voxceleb-resnet34-LM/tree/main?expand=true" \
///   | python3 -c 'import sys,json; \
///     [print(f"{f[\"path\"]}: {f.get(\"lfs\",{}).get(\"oid\",\"?\")}") \
///      for f in json.load(sys.stdin) if f.get("path","").endswith(".onnx")]'
/// ```
pub fn diarizer_models() -> Vec<DiarizerModelMetadata> {
    vec![DiarizerModelMetadata {
        id: WESPEAKER_RESNET34_LM_ID.into(),
        display_name: "Wespeaker ResNet34-LM".into(),
        filename: WESPEAKER_RESNET34_LM_FILENAME.into(),
        size_mb: 26,
        description:
            "Speaker embedding model used to label transcripts with Speaker 1, 2, … in meetings."
                .into(),
        download_url:
            "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM/resolve/main/voxceleb_resnet34_LM.onnx"
                .into(),
        sha256: "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068".into(),
    }]
}

/// The default diarizer model. There's only one today; this exists
/// for symmetry with the Whisper catalog and so a future second
/// entry doesn't break callers.
pub fn default_diarizer_model() -> DiarizerModelMetadata {
    diarizer_models()
        .into_iter()
        .next()
        .expect("diarizer catalog is non-empty by construction")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_non_empty() {
        assert!(!diarizer_models().is_empty());
    }

    #[test]
    fn default_is_wespeaker_resnet34_lm() {
        assert_eq!(default_diarizer_model().id, WESPEAKER_RESNET34_LM_ID);
    }

    #[test]
    fn filename_matches_constant() {
        assert_eq!(
            default_diarizer_model().filename,
            WESPEAKER_RESNET34_LM_FILENAME
        );
    }

    #[test]
    fn sha256_is_64_hex_chars() {
        // SHA-256 hex is exactly 64 characters of [0-9a-f]; a typo
        // (truncation, accidental whitespace, uppercase A-F mix) is
        // a class of bug that would otherwise only surface at
        // first-download time.
        let sha = default_diarizer_model().sha256;
        assert_eq!(sha.len(), 64, "SHA-256 hex should be 64 chars: {sha}");
        assert!(
            sha.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "SHA-256 hex should be lowercase hex: {sha}"
        );
    }

    #[test]
    fn download_url_uses_huggingface_origin() {
        let url = default_diarizer_model().download_url;
        assert!(
            url.starts_with("https://huggingface.co/"),
            "download URL must be on huggingface.co for the redirect-policy allowlist: {url}"
        );
    }
}
