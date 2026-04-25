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

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::audio::{AudioCapture, CpalAudioCapture};
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
/// - `audio` is `Arc<dyn AudioCapture>` so a future hotkey layer (TODO(#5))
///   can hold its own clone and call `start`/`stop` without going through a
///   Tauri command.
/// - `transcribe` is `Option<Arc<dyn Transcribe>>` because the production
///   backend is gated behind the `whisper` Cargo feature *and* requires a
///   model path. When either is absent the `stop_dictation` command returns
///   [`commands::IpcError::TranscriptionUnavailable`] rather than crashing.
/// - `pending_foreground` is captured on `start_dictation` and taken on
///   `stop_dictation`. The `Mutex` is for `&self` interior mutability; it
///   is never contended on the hot path because dictation is fundamentally
///   serial.
pub struct AppState {
    pub audio: Arc<dyn AudioCapture>,
    pub transcribe: Option<Arc<dyn Transcribe>>,
    pub pending_foreground: Mutex<Option<ForegroundApp>>,
}

impl AppState {
    pub fn new(audio: Arc<dyn AudioCapture>, transcribe: Option<Arc<dyn Transcribe>>) -> Self {
        Self {
            audio,
            transcribe,
            pending_foreground: Mutex::new(None),
        }
    }

    /// Build the state used in production: the cpal audio backend, plus
    /// (when the `whisper` feature is enabled and `HUSH_MODEL_PATH` points
    /// at a readable GGUF file) a whisper transcriber loaded from that path.
    ///
    /// Why an env var rather than a settings file: the model picker UI is
    /// a M3 deliverable. For M1/M2 the env var keeps the spike unblocked
    /// without committing to a settings schema we'd have to migrate later.
    /// The eventual replacement is `settings.json` in the platform app-data
    /// directory, populated by the in-app picker.
    pub fn build_default() -> Self {
        let audio: Arc<dyn AudioCapture> = Arc::new(CpalAudioCapture::new());
        Self::new(audio, build_default_transcriber())
    }
}

#[cfg(feature = "whisper")]
fn build_default_transcriber() -> Option<Arc<dyn Transcribe>> {
    use std::path::PathBuf;

    let path = std::env::var("HUSH_MODEL_PATH").ok()?;
    let path = PathBuf::from(path);
    match crate::transcription::WhisperTranscription::new(&path) {
        Ok(t) => {
            tracing::info!(path = %path.display(), "loaded whisper model");
            Some(Arc::new(t) as Arc<dyn Transcribe>)
        }
        Err(e) => {
            // Don't fail-fast: the rest of the app is still useful (device
            // enumeration, settings) and the user can fix the env var
            // without restarting their dev loop.
            tracing::error!(error = ?e, path = %path.display(), "failed to load whisper model");
            None
        }
    }
}

#[cfg(not(feature = "whisper"))]
fn build_default_transcriber() -> Option<Arc<dyn Transcribe>> {
    // Without the `whisper` feature there is no production transcriber.
    // The IPC layer will surface `TranscriptionUnavailable` until either
    // the feature is enabled or a different `Transcribe` impl is plugged
    // in via [`AppState::new`].
    None
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

    #[test]
    fn appstate_can_be_constructed_with_no_transcriber() {
        // Mirrors the runtime path where `--features whisper` is off or
        // `HUSH_MODEL_PATH` is unset: the app boots, device enumeration
        // works, and the IPC layer surfaces `TranscriptionUnavailable` on
        // stop. We just check construction here; the unavailable behaviour
        // is exercised by the `commands` module's runtime path.
        let audio: Arc<dyn AudioCapture> = Arc::new(MockAudio::new(fake_audio()));
        let state = AppState::new(audio, None);
        assert!(state.transcribe.is_none());
    }
}
