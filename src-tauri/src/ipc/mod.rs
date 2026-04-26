//! Tauri IPC layer — exposes the dictation pipeline to the frontend.
//!
//! Concept inspired by VoiceInk's hotkey-driven recording loop.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Hold the application's long-lived service handles (audio capture,
//!   transcription, history, replacements, vocabulary, settings, HTTP)
//!   inside [`AppState`], constructed once at startup and shared across
//!   Tauri command handlers via `tauri::State<AppState>`.
//! - Expose Tauri command handlers as thin wrappers that pull state and
//!   call into the underlying repository / capture / transcription
//!   modules. Orchestration of the dictation hot path lives in
//!   `commands::stop_dictation`, which delegates per-step to the
//!   helper functions in the same file (`load_vocabulary_prompt`,
//!   `load_replacement_rules`, `take_foreground_snapshot`,
//!   `spawn_history_create`, etc.).
//! - Capture the foreground app at the moment recording starts so the
//!   focused-app metadata is preserved even if Hush's own window grabs
//!   focus during the recording.
//!
//! ## Test seam (PRD §13.5)
//!
//! The orchestration is split into:
//!
//! - [`run_pipeline`] — pure(-ish) function that takes an `&dyn AudioCapture`
//!   plus an `&dyn Transcribe`, runs the audio→transcription path, trims,
//!   and returns the text. No Tauri or OS dep. Unit-tested with mock
//!   implementations of both traits.
//! - [`commands`] — thin Tauri command wrappers that pull state, call
//!   `run_pipeline`, and perform side effects (clipboard write, notification,
//!   foreground capture). Manual smoke checklists in `tests/manual/` cover
//!   these because they need a real Tauri app.

pub mod commands;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::transcription::download::CancelHandle;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::{AudioCapture, CpalAudioCapture};
use crate::db::SqliteDatabase;
use crate::dictionary::{
    ReplacementRepository, SqliteReplacementRepository, SqliteVocabularyRepository,
    VocabularyRepository,
};
use crate::history::{HistoryRepository, SqliteHistoryRepository};
use crate::settings::{SettingsRepository, SqliteSettingsRepository};
use crate::transcription::Transcribe;

/// Hot-swappable transcriber slot, shared by reference between
/// [`AppState`] and [`crate::meeting::SessionManager`]. The inner
/// `Option` is `None` until a model is loaded; the outer `Arc` lets
/// the meeting pump observe model hot-swaps via the same mutex
/// `model_select` writes through. Aliased for type-complexity
/// hygiene (clippy) and so the type is easy to grep for at every
/// hand-off point.
pub type TranscribeSlot = Arc<Mutex<Option<Arc<dyn Transcribe>>>>;

// Re-exports kept light: the command functions are referred to by their
// `commands::` path inside `generate_handler!` (Tauri's macro looks up a
// hidden `__cmd__<name>` sibling, which a `pub use` does not carry).

/// Hard cap on how many redirects the download client follows before
/// erroring out. Hugging Face's `/resolve/main/` path is observed to
/// redirect at most twice (`huggingface.co` →
/// `cas-bridge.xethub.hf.co` → a signed URL still on the same Xet
/// CDN); four leaves headroom for a future re-architecture.
const MAX_DOWNLOAD_REDIRECTS: usize = 4;

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
fn is_huggingface_host(host: Option<&str>) -> bool {
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

/// Snapshot of which application was in the foreground when dictation
/// started. Captured so the resulting history row records "you were
/// dictating into Slack / Notion / Mail" rather than "you were dictating
/// into Hush" (which is what `active-win-pos-rs` would report once the user
/// focuses our window to press the start button).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundApp {
    pub app_name: String,
    pub window_title: String,
}

/// Long-lived application state, registered with `tauri::Builder::manage`.
///
/// Ownership rules:
/// - `audio` is `Arc<dyn AudioCapture>` so a future hotkey layer can hold
///   its own clone and call `start`/`stop` without going through a
///   Tauri command.
/// - `transcribe` is `Option<Arc<dyn Transcribe>>` because the production
///   backend is gated behind the `whisper` Cargo feature *and* requires a
///   model path. When either is absent the `stop_dictation` command returns
///   [`commands::IpcError::TranscriptionUnavailable`] rather than crashing.
/// - `history` is `Arc<dyn HistoryRepository>` so the IPC layer can hold a
///   handle without knowing about the SQLite-specific impl. Tests of
///   history-touching commands swap in a deterministic mock at this seam.
/// - `pending_foreground` is captured on `start_dictation` and taken on
///   `stop_dictation`. The `Mutex` is for `&self` interior mutability; it
///   is never contended on the hot path because dictation is fundamentally
///   serial.
pub struct AppState {
    pub audio: Arc<dyn AudioCapture>,
    /// Wrapped in a `Mutex` so `model_select` can hot-swap the loaded
    /// transcriber at runtime without restarting the app. The lock is
    /// held for the duration of one of two operations only: a clone
    /// of the inner `Arc` (microseconds, on the dictation hot path) or
    /// a wholesale replacement (only when the user picks a new model
    /// in the picker). No async work happens inside the lock — the
    /// model file load is done on a `spawn_blocking` task and only
    /// the resulting `Arc` is moved into the lock. See
    /// `swap_transcriber` for the swap path; `stop_dictation` for the
    /// read path.
    ///
    /// Wrapped in `Arc` so the meeting pump (#122 PR2) can hold its
    /// own clone and read the current transcriber on each chunk
    /// without going back through `AppState`. Hot-swapping via the
    /// model picker writes through the shared `Arc`, so the pump
    /// picks up the new model on its next chunk automatically.
    pub transcribe: TranscribeSlot,
    pub history: Arc<dyn HistoryRepository>,
    pub replacements: Arc<dyn ReplacementRepository>,
    pub vocabulary: Arc<dyn VocabularyRepository>,
    pub settings: Arc<dyn SettingsRepository>,
    /// Meeting Mode session storage (Phase C foundation, refs #33 / #109).
    /// Read-side handle — browsing / deleting sessions reads from
    /// this. The write-side ([`Self::meeting_manager`]) is the
    /// stateful owner that opens / closes sessions and appends
    /// utterances.
    pub meetings: Arc<dyn crate::meeting::MeetingSessionRepository>,
    /// Meeting Mode session lifecycle owner (#110 manual-start MVP).
    /// Holds an in-memory pointer to the active session id so the
    /// IPC layer's `stop_dictation` path can route transcripts into
    /// the active session as utterances. `Arc` because the manager
    /// outlives any single command call and is shared across the
    /// `meeting_*` handlers and `stop_dictation`.
    pub meeting_manager: Arc<crate::meeting::SessionManager>,
    /// Directory the model picker scans for downloaded GGUF files
    /// (`<app_data>/models/`). Stored on AppState rather than
    /// re-resolving it on every IPC call so the picker has a single
    /// source of truth and tests can override it.
    pub models_dir: PathBuf,
    /// HTTP client shared across all model downloads. Cheap to clone;
    /// holds connection-pool state internally. One per app keeps
    /// keep-alive working across consecutive downloads (e.g. Tiny
    /// then Base on first launch).
    pub http: reqwest::Client,
    /// Cancel handles for in-flight downloads, keyed by model id.
    /// Inserted by `model_download` when it spawns a task; the cancel
    /// command flips the handle's flag; the spawned task removes its
    /// own entry on completion.
    pub downloads: Mutex<HashMap<String, CancelHandle>>,
    pub pending_foreground: Mutex<Option<ForegroundApp>>,
}

/// Builder for [`AppState`].
///
/// Replaces a 7-positional-arg constructor whose call sites read like an
/// unlabelled tuple. Each `.field(value)` call is self-documenting at the
/// call site, and adding a future required field (e.g. a download-state
/// service or a system-audio source) becomes a one-method addition rather
/// than a breaking-arg-list change at every caller.
///
/// `transcribe` is `Option<Arc<dyn Transcribe>>` rather than required
/// because the production backend is gated behind the `whisper` feature
/// AND a loaded model — both legitimately absent on a fresh install. The
/// rest are required; [`AppStateBuilder::build`] returns an error naming
/// the missing field if any of them are unset, which is more useful than
/// a panic when (e.g.) a future test refactor accidentally forgets one.
#[derive(Default)]
pub struct AppStateBuilder {
    audio: Option<Arc<dyn AudioCapture>>,
    transcribe: Option<Arc<dyn Transcribe>>,
    /// Pre-built `Arc<Mutex<...>>` for the transcribe slot. Set via
    /// [`AppStateBuilder::transcribe_arc`] when the caller (the
    /// production wiring in `build_default`) needs to share the same
    /// Arc with the meeting pump. When unset, `build` wraps
    /// [`Self::transcribe`] in a fresh Arc — the hot-swap surface
    /// stays inside `AppState` only.
    transcribe_arc: Option<TranscribeSlot>,
    history: Option<Arc<dyn HistoryRepository>>,
    replacements: Option<Arc<dyn ReplacementRepository>>,
    vocabulary: Option<Arc<dyn VocabularyRepository>>,
    settings: Option<Arc<dyn SettingsRepository>>,
    meetings: Option<Arc<dyn crate::meeting::MeetingSessionRepository>>,
    meeting_manager: Option<Arc<crate::meeting::SessionManager>>,
    models_dir: Option<PathBuf>,
}

impl AppStateBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn audio(mut self, audio: Arc<dyn AudioCapture>) -> Self {
        self.audio = Some(audio);
        self
    }

    /// Optional. `None` means "no transcriber loaded yet"; the IPC layer
    /// surfaces [`commands::IpcError::TranscriptionUnavailable`] for
    /// dictation calls while in this state.
    pub fn transcribe(mut self, transcribe: Option<Arc<dyn Transcribe>>) -> Self {
        self.transcribe = transcribe;
        self
    }

    /// Hand the builder the pre-built `Arc<Mutex<...>>` so the meeting
    /// pump can hold the same Arc and observe model hot-swaps. When
    /// supplied, the builder uses this directly instead of wrapping
    /// `transcribe()` in a fresh Arc.
    pub fn transcribe_arc(mut self, transcribe: TranscribeSlot) -> Self {
        self.transcribe_arc = Some(transcribe);
        self
    }

    pub fn history(mut self, history: Arc<dyn HistoryRepository>) -> Self {
        self.history = Some(history);
        self
    }

    pub fn replacements(mut self, replacements: Arc<dyn ReplacementRepository>) -> Self {
        self.replacements = Some(replacements);
        self
    }

    pub fn vocabulary(mut self, vocabulary: Arc<dyn VocabularyRepository>) -> Self {
        self.vocabulary = Some(vocabulary);
        self
    }

    pub fn settings(mut self, settings: Arc<dyn SettingsRepository>) -> Self {
        self.settings = Some(settings);
        self
    }

    pub fn meeting_manager(mut self, mgr: Arc<crate::meeting::SessionManager>) -> Self {
        self.meeting_manager = Some(mgr);
        self
    }

    pub fn meetings(mut self, meetings: Arc<dyn crate::meeting::MeetingSessionRepository>) -> Self {
        self.meetings = Some(meetings);
        self
    }

    pub fn models_dir(mut self, models_dir: PathBuf) -> Self {
        self.models_dir = Some(models_dir);
        self
    }

    /// Construct the [`AppState`], or return a descriptive error naming
    /// the first required field that wasn't set.
    pub fn build(self) -> Result<AppState> {
        Ok(AppState {
            audio: self
                .audio
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: audio not set"))?,
            transcribe: self
                .transcribe_arc
                .unwrap_or_else(|| Arc::new(Mutex::new(self.transcribe))),
            history: self
                .history
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: history not set"))?,
            replacements: self
                .replacements
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: replacements not set"))?,
            vocabulary: self
                .vocabulary
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: vocabulary not set"))?,
            settings: self
                .settings
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: settings not set"))?,
            meetings: self
                .meetings
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: meetings not set"))?,
            meeting_manager: self
                .meeting_manager
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: meeting_manager not set"))?,
            models_dir: self
                .models_dir
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: models_dir not set"))?,
            http: reqwest::Client::builder()
                // Whisper-large-v3 is ~3 GB; ten-minute timeout is on
                // the optimistic side of "any reasonable home
                // connection". Real fix is resumable downloads, but
                // that's out of scope for this PR.
                .timeout(std::time::Duration::from_secs(600))
                .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
                // Redirect policy is host-restricted, not just hop-
                // capped. The default `Policy::default()` follows up
                // to 10 redirects to *any* host — a BGP/DNS hijack of
                // huggingface.co could redirect to an arbitrary server
                // and we'd transfer bytes there before the SHA-256
                // verification rejects them. SHA still catches a
                // swapped file, but the bandwidth + latency leak to
                // the attacker's host is avoidable.
                //
                // We allow up to four hops (HF's `/resolve/main/`
                // typically goes huggingface.co → cdn-lfs.huggingface.co
                // → a signed URL on the same CDN; four leaves headroom
                // for a future re-architecture). Every hop must land on
                // a host inside the `huggingface.co` zone.
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= MAX_DOWNLOAD_REDIRECTS {
                        return attempt.error("too many redirects");
                    }
                    if is_huggingface_host(attempt.url().host_str()) {
                        attempt.follow()
                    } else {
                        attempt.error("redirect to host outside huggingface.co")
                    }
                }))
                .build()
                .expect("reqwest client should always build with default config"),
            downloads: Mutex::new(HashMap::new()),
            pending_foreground: Mutex::new(None),
        })
    }
}

impl AppState {
    /// Build the state used in production: the cpal audio backend, the
    /// SQLite-backed history repository at `db_path`, plus (when the
    /// `whisper` feature is enabled and `HUSH_MODEL_PATH` points at a
    /// readable GGUF file) a whisper transcriber loaded from that path.
    ///
    /// Why `db_path` is a parameter: the platform app-data directory is
    /// only resolvable from a Tauri `App` / `AppHandle`, which doesn't
    /// exist until `setup` runs. The caller in `lib.rs::run` does the
    /// resolution and hands us the path, so this function stays trivially
    /// testable.
    ///
    /// Why an env var for the model and not the database: the model
    /// picker UI is a M3 deliverable. For M1/M2 the env var keeps the
    /// spike unblocked without committing to a settings schema we'd have
    /// to migrate later. The eventual replacement is `settings.json` in
    /// the platform app-data directory, populated by the in-app picker.
    pub async fn build_default(db_path: &Path, models_dir: PathBuf) -> Result<Self> {
        let audio: Arc<dyn AudioCapture> = Arc::new(CpalAudioCapture::new());

        let db = SqliteDatabase::open(db_path)
            .await
            .with_context(|| format!("open database at {}", db_path.display()))?;
        let db = Arc::new(db);

        let history: Arc<dyn HistoryRepository> =
            Arc::new(SqliteHistoryRepository::new(Arc::clone(&db)));
        let replacements: Arc<dyn ReplacementRepository> =
            Arc::new(SqliteReplacementRepository::new(Arc::clone(&db)));
        let vocabulary: Arc<dyn VocabularyRepository> =
            Arc::new(SqliteVocabularyRepository::new(Arc::clone(&db)));
        let settings: Arc<dyn SettingsRepository> =
            Arc::new(SqliteSettingsRepository::new(Arc::clone(&db)));
        let meetings: Arc<dyn crate::meeting::MeetingSessionRepository> =
            Arc::new(crate::meeting::SqliteMeetingSessionRepository::new(db));
        // Resolve which transcriber to load at startup. Order:
        //   1. settings → `selected_model_id` → `<models_dir>/<filename>`
        //   2. legacy `HUSH_MODEL_PATH` env var (M1/M2 dev workflow)
        //   3. None — IPC surfaces `TranscriptionUnavailable`.
        // Step 1 resolves the M3 picker; step 2 keeps the existing dev
        // setup working until a user actually opens the picker once.
        let transcribe = build_transcriber(&settings, &models_dir).await;

        // The session manager needs the live audio + transcribe
        // handles to drive its own capture pump (#122 PR2). Wrap
        // transcribe in the same Arc<Mutex<...>> shape AppState uses
        // so model hot-swap propagates to in-flight meeting sessions
        // automatically — both AppState and the manager hold clones
        // of the same Arc.
        let transcribe_shared = Arc::new(Mutex::new(transcribe));
        let meeting_manager = Arc::new(crate::meeting::SessionManager::new(
            Arc::clone(&meetings),
            Arc::clone(&audio),
            Arc::clone(&transcribe_shared),
        ));

        AppStateBuilder::new()
            .audio(audio)
            .transcribe_arc(transcribe_shared)
            .history(history)
            .replacements(replacements)
            .vocabulary(vocabulary)
            .settings(settings)
            .meetings(meetings)
            .meeting_manager(meeting_manager)
            .models_dir(models_dir)
            .build()
    }
}

impl AppState {
    /// Hot-swap the loaded transcriber.
    ///
    /// Called from `model_select` after the user picks a model that
    /// has a downloaded file on disk. The lock is acquired only after
    /// the (potentially-slow) GGUF load completes on a blocking task,
    /// so the dictation hot path is never blocked on disk I/O.
    ///
    /// Returns the previous value so the caller can drop it explicitly
    /// if it wants — the default `Drop` is fine for `Arc<dyn Trait>`,
    /// but returning is cheap and lets callers diagnose "did we
    /// actually swap something?" if they care.
    pub fn swap_transcriber(
        &self,
        new: Option<Arc<dyn Transcribe>>,
    ) -> Result<Option<Arc<dyn Transcribe>>> {
        let mut guard = self
            .transcribe
            .lock()
            .map_err(|_| anyhow::anyhow!("transcribe mutex poisoned"))?;
        Ok(std::mem::replace(&mut *guard, new))
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
            .with_context(|| format!("load whisper model {} from {}", meta.id, path.display()))?;
        tracing::info!(
            model_id = %meta.id,
            path = %path.display(),
            "hot-loaded whisper model"
        );
        Ok(Some(Arc::new(transcriber) as Arc<dyn Transcribe>))
    }

    #[cfg(not(feature = "whisper"))]
    {
        Ok(None)
    }
}

/// Resolve the active transcriber backend. Pulled out so a test or a
/// future "reload model" command can call it without rebuilding the
/// rest of `AppState`.
#[cfg_attr(not(feature = "whisper"), allow(unused_variables))]
async fn build_transcriber(
    settings: &Arc<dyn SettingsRepository>,
    models_dir: &Path,
) -> Option<Arc<dyn Transcribe>> {
    #[cfg(feature = "whisper")]
    {
        use crate::settings::keys;
        use crate::transcription::catalog;

        // 1) Settings-driven path: model id → catalog → models_dir.
        if let Ok(Some(id)) = settings.get(keys::SELECTED_MODEL_ID).await {
            if let Some(meta) = catalog::find_by_id(&id) {
                let path = models_dir.join(&meta.filename);
                if path.exists() {
                    match crate::transcription::WhisperTranscription::new(&path) {
                        Ok(t) => {
                            tracing::info!(
                                model_id = %meta.id,
                                path = %path.display(),
                                "loaded selected whisper model"
                            );
                            return Some(Arc::new(t) as Arc<dyn Transcribe>);
                        }
                        Err(e) => {
                            tracing::error!(
                                error = ?e,
                                path = %path.display(),
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
        }

        // 2) Legacy dev path. Removed once the picker is mature enough
        //    that we can ask users to migrate.
        if let Ok(path) = std::env::var("HUSH_MODEL_PATH") {
            let path = std::path::PathBuf::from(path);
            match crate::transcription::WhisperTranscription::new(&path) {
                Ok(t) => {
                    tracing::info!(path = %path.display(), "loaded HUSH_MODEL_PATH whisper model");
                    return Some(Arc::new(t) as Arc<dyn Transcribe>);
                }
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        path = %path.display(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioDevice, CaptureFormat, CapturedAudio};
    use anyhow::anyhow;

    /// Mock that returns a fixed [`CapturedAudio`] from `stop`. `start` and
    /// `is_recording` keep just enough state for tests to assert on.
    struct MockAudio {
        captured: CapturedAudio,
        recording: std::sync::atomic::AtomicBool,
    }

    impl MockAudio {
        fn new(captured: CapturedAudio) -> Self {
            Self {
                captured,
                recording: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl AudioCapture for MockAudio {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![AudioDevice {
                id: "mock".into(),
                name: "Mock Mic".into(),
                is_default: true,
            }])
        }
        fn start(&self, _device_id: Option<&str>) -> anyhow::Result<()> {
            self.recording
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            self.recording
                .store(false, std::sync::atomic::Ordering::Release);
            Ok(self.captured.clone())
        }
        fn is_recording(&self) -> bool {
            self.recording.load(std::sync::atomic::Ordering::Acquire)
        }
    }

    struct EchoTranscribe {
        text: String,
    }

    impl Transcribe for EchoTranscribe {
        fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
            Ok(self.text.clone())
        }
    }

    struct FailingTranscribe;

    impl Transcribe for FailingTranscribe {
        fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
            Err(anyhow!("model exploded"))
        }
    }

    fn fake_audio() -> CapturedAudio {
        CapturedAudio {
            samples: vec![0.0_f32; 4],
            format: CaptureFormat {
                sample_rate: 48_000,
                channels: 1,
            },
        }
    }

    #[test]
    fn run_pipeline_trims_whitespace_from_model_output() {
        let audio = MockAudio::new(fake_audio());
        let transcribe = EchoTranscribe {
            text: "  hello world\n".into(),
        };
        let text = run_pipeline(&audio, &transcribe).unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn run_pipeline_propagates_audio_stop_failure() {
        struct BrokenAudio;
        impl AudioCapture for BrokenAudio {
            fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
                Ok(vec![])
            }
            fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
                Ok(())
            }
            fn stop(&self) -> anyhow::Result<CapturedAudio> {
                Err(anyhow!("device went away"))
            }
            fn is_recording(&self) -> bool {
                false
            }
        }

        let err = run_pipeline(&BrokenAudio, &EchoTranscribe { text: "x".into() })
            .unwrap_err()
            .to_string();
        assert!(err.contains("device went away"), "got: {err}");
    }

    #[test]
    fn run_pipeline_propagates_transcription_failure() {
        let audio = MockAudio::new(fake_audio());
        let err = run_pipeline(&audio, &FailingTranscribe)
            .unwrap_err()
            .to_string();
        assert!(err.contains("model exploded"), "got: {err}");
    }

    /// Tiny mock for unit tests that need an `Arc<dyn HistoryRepository>`
    /// in `AppState` but don't exercise its methods. Pinning the count to
    /// `0` keeps surface minimal — tests that need real behaviour against
    /// the SQLite-backed impl call the repository directly.
    pub(crate) struct NoopHistory;

    #[async_trait::async_trait]
    impl HistoryRepository for NoopHistory {
        async fn create(&self, _: crate::history::NewHistoryEntry) -> anyhow::Result<i64> {
            Ok(0)
        }
        async fn list(&self, _: i64, _: i64) -> anyhow::Result<Vec<crate::history::HistoryEntry>> {
            Ok(vec![])
        }
        async fn search(
            &self,
            _: &str,
            _: i64,
            _: i64,
        ) -> anyhow::Result<Vec<crate::history::HistoryEntry>> {
            Ok(vec![])
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn count(&self) -> anyhow::Result<i64> {
            Ok(0)
        }
    }

    /// Tiny mock that returns an empty rules list so the dictation
    /// pipeline behaves as if no replacements are configured. Tests that
    /// need actual replacement behaviour use the SQLite-backed repo
    /// directly rather than mocking the trait.
    pub(crate) struct NoopReplacements;

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::dictionary::ReplacementRule,
            crate::dictionary::NewReplacementRule,
            i64,
        > for NoopReplacements
    {
        async fn list(&self) -> anyhow::Result<Vec<crate::dictionary::ReplacementRule>> {
            Ok(vec![])
        }
        async fn create(
            &self,
            _: crate::dictionary::NewReplacementRule,
        ) -> anyhow::Result<crate::dictionary::ReplacementRule> {
            unreachable!("mock does not exercise create")
        }
        async fn update(&self, _: crate::dictionary::ReplacementRule) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Same shape as [`NoopReplacements`] / [`NoopHistory`] — empty
    /// list so the dictation pipeline behaves as if no vocab terms are
    /// configured. Tests that need real behaviour against the SQLite-
    /// backed repo call it directly.
    pub(crate) struct NoopVocabulary;

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::dictionary::VocabularyTerm,
            crate::dictionary::NewVocabularyTerm,
            i64,
        > for NoopVocabulary
    {
        async fn list(&self) -> anyhow::Result<Vec<crate::dictionary::VocabularyTerm>> {
            Ok(vec![])
        }
        async fn create(
            &self,
            _: crate::dictionary::NewVocabularyTerm,
        ) -> anyhow::Result<crate::dictionary::VocabularyTerm> {
            unreachable!("mock does not exercise create")
        }
        async fn update(&self, _: crate::dictionary::VocabularyTerm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Noop meeting-session repository — returns empty lists, eats
    /// inserts. Tests that exercise the IPC layer don't need real
    /// persistence here today; the streaming pump that actually
    /// writes to it lands in #110.
    pub(crate) struct NoopMeetings;

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::meeting::MeetingSession,
            crate::meeting::NewMeetingSession,
            i64,
        > for NoopMeetings
    {
        async fn list(&self) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
            Ok(vec![])
        }
        async fn create(
            &self,
            _: crate::meeting::NewMeetingSession,
        ) -> anyhow::Result<crate::meeting::MeetingSession> {
            unreachable!("mock does not exercise create")
        }
        async fn update(&self, _: crate::meeting::MeetingSession) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl crate::meeting::MeetingSessionRepository for NoopMeetings {
        async fn close_session(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn append_utterance(
            &self,
            _: crate::meeting::NewPersistedUtterance,
        ) -> anyhow::Result<crate::meeting::PersistedUtterance> {
            unreachable!("mock does not exercise append_utterance")
        }
        async fn list_utterances(
            &self,
            _: i64,
        ) -> anyhow::Result<Vec<crate::meeting::PersistedUtterance>> {
            Ok(vec![])
        }
        async fn set_notes(&self, _: i64, _: Option<String>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// In-memory settings store backed by a HashMap. Lighter than
    /// spinning up a SQLite for tests that just need to round-trip a
    /// few keys.
    pub(crate) struct MemSettings {
        pub map: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    #[async_trait::async_trait]
    impl SettingsRepository for MemSettings {
        async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.map.lock().unwrap().get(key).cloned())
        }
        async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
            self.map
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }
        async fn remove(&self, key: &str) -> anyhow::Result<()> {
            self.map.lock().unwrap().remove(key);
            Ok(())
        }
    }

    pub(crate) fn mock_state() -> AppState {
        AppStateBuilder::new()
            .audio(Arc::new(MockAudio::new(fake_audio())))
            .history(Arc::new(NoopHistory))
            .replacements(Arc::new(NoopReplacements))
            .vocabulary(Arc::new(NoopVocabulary))
            .settings(Arc::new(MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(NoopMeetings);
                m
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("mock_state: builder fields complete")
    }

    #[test]
    fn appstate_can_be_constructed_with_no_transcriber() {
        // Mirrors the runtime path where `--features whisper` is off or
        // `HUSH_MODEL_PATH` is unset: the app boots, device enumeration
        // works, and the IPC layer surfaces `TranscriptionUnavailable` on
        // stop. We just check construction here; the unavailable behaviour
        // is exercised by the `commands` module's runtime path.
        let state = mock_state();
        assert!(state.transcribe.lock().unwrap().is_none());
    }

    #[test]
    fn swap_transcriber_replaces_the_inner_arc_and_returns_previous() {
        // Round-7 architecture reviewer flagged that `swap_transcriber`
        // (called from `model_select` when the user picks a new model
        // with a downloaded file) had no test coverage. Pin the
        // observable contract: the new value lands inside the Mutex,
        // and the previous value is returned so the caller can drop
        // it explicitly if it cares to. A future change that
        // accidentally swaps in an async lock or a different replacement
        // strategy would fail this.

        struct StubTranscriber {
            label: &'static str,
        }
        impl crate::transcription::Transcribe for StubTranscriber {
            fn transcribe(&self, _: &crate::audio::CapturedAudio) -> anyhow::Result<String> {
                Ok(String::new())
            }
            fn model_label(&self) -> String {
                self.label.to_owned()
            }
        }

        let state = mock_state();
        // mock_state() leaves transcribe = None (no model loaded).
        assert!(state.transcribe.lock().unwrap().is_none());

        let first: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "first" });
        let prev = state
            .swap_transcriber(Some(first))
            .expect("first swap succeeds");
        assert!(prev.is_none(), "previous was None (mock_state baseline)");

        // Now confirm the swap actually landed.
        {
            let guard = state.transcribe.lock().unwrap();
            assert_eq!(
                guard.as_ref().map(|t| t.model_label()),
                Some("first".to_owned()),
                "new transcriber readable from inside the Mutex"
            );
        }

        // Second swap returns the first one as the "previous" value.
        let second: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "second" });
        let prev = state
            .swap_transcriber(Some(second))
            .expect("second swap succeeds");
        assert_eq!(
            prev.map(|t| t.model_label()),
            Some("first".to_owned()),
            "previous value returned to caller"
        );
        assert_eq!(
            state
                .transcribe
                .lock()
                .unwrap()
                .as_ref()
                .map(|t| t.model_label()),
            Some("second".to_owned()),
            "second transcriber landed"
        );

        // Swap to None to confirm the unload path works.
        let prev = state.swap_transcriber(None).expect("clear swap succeeds");
        assert_eq!(prev.map(|t| t.model_label()), Some("second".to_owned()));
        assert!(state.transcribe.lock().unwrap().is_none());
    }

    #[test]
    fn appstate_builder_errors_descriptively_on_missing_required_field() {
        // Round-7 architecture reviewer flagged that the builder's
        // self-documenting error messages had no test coverage. A future
        // refactor that "fixed" the error message wording (or stopped
        // ok_or_else'ing entirely) would silently regress the developer
        // experience of "I forgot a field — the error tells me which one."
        // Spot-check one required field. The message format ("audio not
        // set") is part of the developer-facing contract — pin it.
        let result = AppStateBuilder::new().build();
        // AppState doesn't implement Debug, so we can't use
        // `expect_err`; match on the Result instead.
        let err = match result {
            Ok(_) => panic!("empty builder must error, got Ok"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("audio not set"),
            "error must name the first missing required field; got: {msg}"
        );
    }

    #[test]
    fn huggingface_host_predicate_accepts_apex_and_subdomains() {
        // Pin the load-bearing security check: the download redirect
        // policy treats these as in-zone. Both `huggingface.co` and
        // `hf.co` are HF-owned; the Xet CDN that HF migrated large-
        // file serving to in 2025 lives on the `hf.co` zone (e.g.
        // `cas-bridge.xethub.hf.co`), not `huggingface.co`.
        assert!(is_huggingface_host(Some("huggingface.co")));
        assert!(is_huggingface_host(Some("cdn-lfs.huggingface.co")));
        assert!(is_huggingface_host(Some("cdn-lfs-us-1.huggingface.co")));
        assert!(is_huggingface_host(Some("hf.co")));
        assert!(is_huggingface_host(Some("xethub.hf.co")));
        assert!(is_huggingface_host(Some("cas-bridge.xethub.hf.co")));
    }

    #[test]
    fn huggingface_host_predicate_rejects_typosquats_and_lookalikes() {
        // Regression for "ends_with" naivety: `evilhuggingface.co`
        // (no leading dot) is not in zone but a sloppy `ends_with`
        // without the dot would accept it. The predicate must also
        // reject obvious off-domain hosts.
        assert!(!is_huggingface_host(Some("evilhuggingface.co")));
        assert!(!is_huggingface_host(Some("huggingface.co.attacker.com")));
        assert!(!is_huggingface_host(Some("attacker.com")));
        // hf.co-zone equivalents of the same trap.
        assert!(!is_huggingface_host(Some("myhf.co")));
        assert!(!is_huggingface_host(Some("hf.co.attacker.com")));
        assert!(!is_huggingface_host(Some("")));
        assert!(!is_huggingface_host(None));
    }
}
