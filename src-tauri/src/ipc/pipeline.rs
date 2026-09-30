//! Pipeline orchestration: redirect-policy + transcriber loading + the
//! pure `run_pipeline` audio→transcription orchestrator.
//!
//! Extracted from `ipc/mod.rs` under #597 (item 6). No behaviour change.
//!
//! Three concerns share this file because they cross the same boundary
//! (between pure orchestration and the transcription/audio traits):
//!
//! - **Redirect policy** ([`redirect_decision`], [`is_huggingface_host`])
//!   — the security check that gates model downloads from
//!   Hugging Face hosts. Pulled out so the policy is unit-testable
//!   without a live `reqwest::redirect::Attempt` (which has no public
//!   constructor).
//! - **Transcriber loaders** ([`load_transcriber_for_model`],
//!   [`build_transcriber`]) — startup-time + hot-swap loading of
//!   Whisper GGUF models. `pub(super)` items are called from
//!   [`super::builder`]'s `AppState::build_default`.
//! - **Pipeline runner** ([`run_pipeline`]) — pure audio→transcription
//!   function exposed for unit tests; used by the dictation hot path
//!   in `commands::dictation`.

use std::path::Path;
use std::sync::atomic::{AtomicI32, AtomicU32};
use std::sync::Arc;

#[cfg(feature = "whisper")]
use anyhow::Context;
use anyhow::Result;

use crate::audio::AudioCapture;
use crate::settings::SettingsRepository;
use crate::transcription::Transcribe;

pub(crate) const MAX_DOWNLOAD_REDIRECTS: usize = 4;

/// Predicate for the redirect-policy closure: returns `true` iff
/// `host` is in one of Hugging Face's owned DNS zones. Both
/// `huggingface.co` and `hf.co` are HF-owned; the Xet content-
/// addressed storage that HF migrated large-file serving to in 2025
/// lives on `cas-bridge.xethub.hf.co`, which is a subdomain of
/// `hf.co` not `huggingface.co`. We need to allow the `hf.co` zone
/// or the model-download redirect chain dies — see PR #74 for the
/// regression that surfaced this.
///
/// Pulled out so the host-allowlist logic is unit-testable —
/// `reqwest::redirect::Attempt` has no public constructor, so the
/// closure as a whole is not, but this small predicate is the
/// load-bearing security check.
///
/// Care taken on the suffix match: `.huggingface.co` and `.hf.co`
/// (with leading dot) so a typo-squat like `evilhuggingface.co` or
/// `myhf.co` does not match.
pub(crate) fn is_huggingface_host(host: Option<&str>) -> bool {
    match host {
        Some(h) => {
            h == "huggingface.co"
                || h.ends_with(".huggingface.co")
                || h == "hf.co"
                || h.ends_with(".hf.co")
        }
        None => false,
    }
}

/// Outcome of the model-download redirect predicate, broken out
/// from the reqwest closure so the policy is unit-testable
/// (`reqwest::redirect::Attempt` has no public constructor — the
/// closure as a whole is not testable, but this is).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RedirectDecision {
    Follow,
    /// Static reasons rather than `Error<String>` so each branch
    /// matches against a `&'static str` in tests without
    /// stringifying.
    Stop(&'static str),
}

/// Pure logic behind the model-download redirect closure (#258).
///
/// Allows a hop when EITHER the destination is on an HF host OR
/// the immediately-previous URL was on an HF host. The second
/// clause covers HF → signed-CDN chains (S3, Cloudflare R2, etc.)
/// that surface when HF routes large-file serving through a
/// third-party object store. The signed URL itself isn't an HF
/// host, but the user trusts HF to redirect them to one — same
/// trust shape browsers use.
///
/// Only HTTPS is ever followed; an http:// destination is
/// rejected even from an HF origin (downgrade defence).
///
/// Caps at `MAX_DOWNLOAD_REDIRECTS` regardless of host trust.
pub(crate) fn redirect_decision(
    previous: &[reqwest::Url],
    destination: &reqwest::Url,
) -> RedirectDecision {
    if previous.len() >= MAX_DOWNLOAD_REDIRECTS {
        return RedirectDecision::Stop("too many redirects");
    }
    if destination.scheme() != "https" {
        return RedirectDecision::Stop("redirect to non-HTTPS scheme");
    }
    let dest_is_hf = is_huggingface_host(destination.host_str());
    let previous_is_hf = previous
        .last()
        .map(|u| is_huggingface_host(u.host_str()))
        .unwrap_or(false);
    if dest_is_hf || previous_is_hf {
        RedirectDecision::Follow
    } else {
        RedirectDecision::Stop(
            "redirect from non-HF host to non-HF host (signed-URL chain not extending HF origin)",
        )
    }
}

/// Try to load the GGUF for a single catalog model id. Returns `None`
/// if the model isn't in the catalog, the file isn't on disk, or the
/// `whisper` Cargo feature is off. Returns an error if the file is on
/// disk but `WhisperTranscription::new` fails — the caller decides
/// whether to surface that to the user or silently fall through.
///
/// Pulled out as its own function so `model_select` can hot-load a
/// specific model without going through the full startup-time
/// fallback chain in [`build_transcriber`] (which also tries the
/// legacy `HUSH_MODEL_PATH` env var, irrelevant once the user is
/// driving the picker).
#[cfg_attr(not(feature = "whisper"), allow(unused_variables))]
pub fn load_transcriber_for_model(
    model_id: &str,
    models_dir: &Path,
    inference_threads: &Arc<AtomicI32>,
    mic_gain_db: &Arc<AtomicU32>,
) -> Result<Option<Arc<dyn Transcribe>>> {
    #[cfg(feature = "whisper")]
    {
        use crate::transcription::catalog;

        let Some(meta) = catalog::find_by_id(model_id) else {
            return Ok(None);
        };
        let path = models_dir.join(&meta.filename);
        if !path.exists() {
            return Ok(None);
        }
        let transcriber = crate::transcription::WhisperTranscription::new(&path)
            .with_context(|| format!("load whisper model {} from {}", meta.id, path.display()))?
            .with_inference_threads(Arc::clone(inference_threads))
            .with_mic_gain_db(Arc::clone(mic_gain_db));
        tracing::info!(
            model_id = %meta.id,
            path = %path.display(),
            "hot-loaded whisper model"
        );
        Ok(Some(Arc::new(transcriber) as Arc<dyn Transcribe>))
    }

    #[cfg(not(feature = "whisper"))]
    {
        let _ = inference_threads;
        let _ = mic_gain_db;
        Ok(None)
    }
}

/// Load a Whisper model from disk on a blocking thread (#561).
///
/// `WhisperTranscription::new` mmaps the GGUF file and initialises the
/// whisper.cpp context — typically 1–2 s for large models. Wrapping it in
/// `spawn_blocking` frees the tokio executor thread so `tokio::join!` can
/// drive two concurrent loads on separate blocking threads.
#[cfg(feature = "whisper")]
async fn load_whisper_model(
    path: std::path::PathBuf,
) -> anyhow::Result<crate::transcription::WhisperTranscription> {
    tokio::task::spawn_blocking(move || crate::transcription::WhisperTranscription::new(&path))
        .await
        .map_err(|e| anyhow::anyhow!("whisper model load task panicked: {e}"))?
}

/// Resolve the active transcriber backend. Pulled out so a test or a
/// future "reload model" command can call it without rebuilding the
/// rest of `AppState`.
///
/// `pub(crate)` so [`super::builder`]'s `AppState::build_default` and
/// the post-meeting-stop reload path in `commands::meeting` can both
/// call this without duplicating the model-selection logic.
#[cfg_attr(not(feature = "whisper"), allow(unused_variables))]
pub(crate) async fn build_transcriber(
    settings: &Arc<dyn SettingsRepository>,
    models_dir: &Path,
    inference_threads: &Arc<AtomicI32>,
    mic_gain_db: &Arc<AtomicU32>,
) -> Option<Arc<dyn Transcribe>> {
    #[cfg(feature = "whisper")]
    {
        use crate::settings::keys;
        use crate::transcription::catalog;

        // Read the persisted selection once; we branch on its presence.
        let selected_id = settings.get(keys::SELECTED_MODEL_ID).await.ok().flatten();

        if let Some(ref id) = selected_id {
            // 1) Explicit selection: try only the picked model. When an
            //    explicit choice is present we never silently swap to a
            //    different model — the user's intent wins.
            if let Some(meta) = catalog::find_by_id(id) {
                let path = models_dir.join(&meta.filename);
                if path.exists() {
                    let path_display = path.display().to_string();
                    match load_whisper_model(path).await {
                        Ok(t) => {
                            tracing::info!(
                                model_id = %meta.id,
                                path = %path_display,
                                "loaded selected whisper model"
                            );
                            return Some(Arc::new(
                                t.with_inference_threads(Arc::clone(inference_threads))
                                    .with_mic_gain_db(Arc::clone(mic_gain_db)),
                            ) as Arc<dyn Transcribe>);
                        }
                        Err(e) => {
                            tracing::error!(
                                error = ?e,
                                path = %path_display,
                                "selected model failed to load; falling back"
                            );
                        }
                    }
                } else {
                    tracing::warn!(
                        model_id = %id,
                        path = %path.display(),
                        "selected model file is missing; falling back"
                    );
                }
            } else {
                tracing::warn!(
                    model_id = %id,
                    "selected model id is not in the catalog; falling back"
                );
            }
        } else {
            // 1b) No explicit selection: mirror `model_list`'s implicit-
            //     default logic. When SELECTED_MODEL_ID is absent the
            //     frontend shows the catalog default as already selected;
            //     load it here so the backend slot agrees with the UI.
            let default_meta = catalog::default_model();
            let default_path = models_dir.join(&default_meta.filename);
            if default_path.exists() {
                let path_display = default_path.display().to_string();
                match load_whisper_model(default_path).await {
                    Ok(t) => {
                        tracing::info!(
                            model_id = %default_meta.id,
                            path = %path_display,
                            "loaded catalog-default whisper model (no explicit selection)"
                        );
                        return Some(Arc::new(
                            t.with_inference_threads(Arc::clone(inference_threads))
                                .with_mic_gain_db(Arc::clone(mic_gain_db)),
                        ) as Arc<dyn Transcribe>);
                    }
                    Err(e) => {
                        tracing::error!(
                            error = ?e,
                            path = %path_display,
                            "catalog-default model failed to load; falling back"
                        );
                    }
                }
            }
        }

        // 2) Legacy dev path. Removed once the picker is mature enough
        //    that we can ask users to migrate.
        if let Ok(path_str) = std::env::var("HUSH_MODEL_PATH") {
            let path = std::path::PathBuf::from(path_str);
            let path_display = path.display().to_string();
            match load_whisper_model(path).await {
                Ok(t) => {
                    tracing::info!(path = %path_display, "loaded HUSH_MODEL_PATH whisper model");
                    return Some(Arc::new(
                        t.with_inference_threads(Arc::clone(inference_threads))
                            .with_mic_gain_db(Arc::clone(mic_gain_db)),
                    ) as Arc<dyn Transcribe>);
                }
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        path = %path_display,
                        "HUSH_MODEL_PATH failed to load"
                    );
                }
            }
        }

        None
    }

    #[cfg(not(feature = "whisper"))]
    {
        // Without the `whisper` feature there is no production
        // transcriber. The IPC layer surfaces `TranscriptionUnavailable`.
        let _ = mic_gain_db;
        None
    }
}

/// Pure orchestration function — exposed so unit tests can exercise the
/// audio→transcription path with mocked implementations of both traits,
/// without needing a Tauri runtime, an audio device, or a real Whisper
/// model. The Tauri command wrapper handles the OS side effects on top.
pub fn run_pipeline(
    audio: &dyn AudioCapture,
    transcribe: &dyn Transcribe,
) -> anyhow::Result<String> {
    let captured = audio.stop()?;
    let raw = transcribe.transcribe(&captured)?;
    Ok(raw.trim().to_owned())
}
