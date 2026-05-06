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
//! ## File layout
//!
//! Front door (this file) — module declarations + re-exports only.
//!
//! - [`commands`] — Tauri `#[tauri::command]` handlers, grouped by domain
//!   (`commands/dictation.rs`, `commands/meeting.rs`, etc.).
//! - [`events`] — `EventEmitter` trait + production wrappers for
//!   forwarding meeting / recording events to the frontend.
//! - [`state`] — long-lived [`AppState`] type plus the production
//!   constructor `AppState::build_default`.
//! - [`builder`] — explicit-builder pattern ([`AppStateBuilder`])
//!   used by tests and called from `build_default`.
//! - [`pipeline`] — model-download redirect policy, transcriber
//!   loaders ([`load_transcriber_for_model`]), and the pure
//!   audio→transcription orchestrator ([`run_pipeline`]).
//!
//! Split from a single 2247-line `mod.rs` under #597 (item 6). No
//! behaviour change.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers depend on `AppState` composed from trait objects
//! (`Arc<dyn AudioCapture>`, `Arc<dyn Transcribe>`, etc.). Unit tests
//! substitute deterministic mocks at the trait seam without touching
//! cpal, SQLite, or whisper.

pub mod builder;
pub mod commands;
pub mod events;
pub mod pipeline;
pub mod state;

#[cfg(test)]
mod tests;

// Public API (consumed from `lib.rs` startup wiring + the meeting module).
pub use builder::AppStateBuilder;
pub use pipeline::{load_transcriber_for_model, run_pipeline};
pub use state::{AppState, DataServices, ForegroundApp, RuntimeFlags, TranscribeSlot};

// Crate-private re-exports — read by `lib.rs` (autostart-mode decode in the
// poller setup) and by `ipc::commands::settings` (the autostart IPC).
pub(crate) use state::{decode_autostart_mode, encode_autostart_mode};
