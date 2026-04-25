//! Tauri IPC layer — exposes the dictation pipeline to the frontend.
//!
//! Concept inspired by VoiceInk's hotkey-driven recording loop.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Hold the application's long-lived service handles (audio capture,
//!   transcription) inside [`AppState`], constructed once at startup and
//!   shared across Tauri command handlers via `tauri::State<AppState>`.
//! - Expose a small, M2-scoped command surface (`list_input_devices`,
//!   `start_dictation`, `stop_dictation`) — enough to drive the
//!   "press button → record → transcribe → paste" loop end-to-end. Per the
//!   PRD §11 milestone plan, the hotkey, history, dictionary, and settings
//!   commands land in later milestones (M2 hotkey, M3 storage / picker, M4
//!   dictionary).
//! - Capture the foreground app at the moment recording starts so the
//!   focused-app metadata is preserved even if Hush's own window grabs focus
//!   during the recording.
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

// Re-exports kept light: the command functions are referred to by their
// `commands::` path inside `generate_handler!` (Tauri's macro looks up a
// hidden `__cmd__<name>` sibling, which a `pub use` does not carry).

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
    pub transcribe: Option<Arc<dyn Transcribe>>,
    pub history: Arc<dyn HistoryRepository>,
    pub replacements: Arc<dyn ReplacementRepository>,
    pub vocabulary: Arc<dyn VocabularyRepository>,
    pub settings: Arc<dyn SettingsRepository>,
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

impl AppState {
    pub fn new(
        audio: Arc<dyn AudioCapture>,
        transcribe: Option<Arc<dyn Transcribe>>,
        history: Arc<dyn HistoryRepository>,
        replacements: Arc<dyn ReplacementRepository>,
        vocabulary: Arc<dyn VocabularyRepository>,
        settings: Arc<dyn SettingsRepository>,
        models_dir: PathBuf,
    ) -> Self {
        Self {
            audio,
            transcribe,
            history,
            replacements,
            vocabulary,
            settings,
            models_dir,
            http: reqwest::Client::builder()
                // Whisper-large-v3 is ~3 GB; ten-minute timeout is on
                // the optimistic side of "any reasonable home
                // connection". Real fix is resumable downloads, but
                // that's out of scope for this PR.
                .timeout(std::time::Duration::from_secs(600))
                .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client should always build with default config"),
            downloads: Mutex::new(HashMap::new()),
            pending_foreground: Mutex::new(None),
        }
    }

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
        let settings: Arc<dyn SettingsRepository> = Arc::new(SqliteSettingsRepository::new(db));

        // Resolve which transcriber to load at startup. Order:
        //   1. settings → `selected_model_id` → `<models_dir>/<filename>`
        //   2. legacy `HUSH_MODEL_PATH` env var (M1/M2 dev workflow)
        //   3. None — IPC surfaces `TranscriptionUnavailable`.
        // Step 1 resolves the M3 picker; step 2 keeps the existing dev
        // setup working until a user actually opens the picker once.
        let transcribe = build_transcriber(&settings, &models_dir).await;

        Ok(Self::new(
            audio,
            transcribe,
            history,
            replacements,
            vocabulary,
            settings,
            models_dir,
        ))
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
        async fn insert(&self, _: crate::history::NewHistoryEntry) -> anyhow::Result<i64> {
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
    impl ReplacementRepository for NoopReplacements {
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
    impl VocabularyRepository for NoopVocabulary {
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
        let audio: Arc<dyn AudioCapture> = Arc::new(MockAudio::new(fake_audio()));
        let history: Arc<dyn HistoryRepository> = Arc::new(NoopHistory);
        let replacements: Arc<dyn ReplacementRepository> = Arc::new(NoopReplacements);
        let vocabulary: Arc<dyn VocabularyRepository> = Arc::new(NoopVocabulary);
        let settings: Arc<dyn SettingsRepository> = Arc::new(MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        AppState::new(
            audio,
            None,
            history,
            replacements,
            vocabulary,
            settings,
            std::path::PathBuf::from("/tmp/hush-test-models"),
        )
    }

    #[test]
    fn appstate_can_be_constructed_with_no_transcriber() {
        // Mirrors the runtime path where `--features whisper` is off or
        // `HUSH_MODEL_PATH` is unset: the app boots, device enumeration
        // works, and the IPC layer surfaces `TranscriptionUnavailable` on
        // stop. We just check construction here; the unavailable behaviour
        // is exercised by the `commands` module's runtime path.
        let state = mock_state();
        assert!(state.transcribe.is_none());
    }
}
