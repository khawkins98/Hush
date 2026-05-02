//! Tauri-backed implementations of [`crate::events::EventEmitter`].
//!
//! The trait itself moved to [`crate::events`] in #431 so both the
//! `ipc/` and `meeting/` layers can depend on a single emit-seam
//! without creating an import cycle. This file holds the production
//! wrapper around `tauri::AppHandle` and the test recorder — both
//! depend on `tauri::*` types and live here at the IPC layer rather
//! than next to the trait.
//!
//! ## Original motivation
//!
//! Pre-#315 the `download_diarizer_model` and `model_download`
//! paths took a `tauri::AppHandle` directly and fired events via
//! `app.emit(...)`. That made them un-testable from a normal
//! `#[tokio::test]` — there's no lightweight way to construct a real
//! `AppHandle` outside the Tauri runtime, and the diarizer audit-of-
//! audit (post-#308) flagged two coverage gaps as a result:
//!
//! - Duplicate-rejection guard inside the `state.downloads.lock()`
//!   critical section in `download_diarizer_model` was untested.
//! - Cancel-handle cleanup on the failure branch was untested.
//!
//! The Whisper download (`models::model_download`) had the same
//! shape and the same gaps. #315 introduced the trait seam; #431
//! lifted it to crate root so the meeting module could share it
//! (replacing its local `MeetingEventEmitter`).
//!
//! ## What this is *not*
//!
//! The trait covers one-shot emit only. It deliberately doesn't
//! model `app.listen(...)`, window-scoped emits, or webview-targeted
//! emits — none of those are on the test path today. Adding them
//! when a real consumer needs them is fine.

use crate::events::EventEmitter;

#[cfg(test)]
use std::sync::{Arc, Mutex};

/// Production implementation wrapping a `tauri::AppHandle`. Calls
/// `app.emit(...)` directly; failure is logged at warn (matching
/// the existing `let _ = app.emit(...)` callers' intent).
pub struct TauriEventEmitter<R: tauri::Runtime = tauri::Wry> {
    app: tauri::AppHandle<R>,
}

impl<R: tauri::Runtime> TauriEventEmitter<R> {
    pub fn new(app: tauri::AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: tauri::Runtime> EventEmitter for TauriEventEmitter<R> {
    fn emit_json(&self, event: &str, payload: serde_json::Value) {
        use tauri::Emitter as _;
        if let Err(e) = self.app.emit(event, payload) {
            tracing::warn!(error = ?e, event = %event, "tauri emit failed");
        }
    }
}

/// Test recorder — captures `(event, payload)` pairs into a
/// `Mutex<Vec<…>>` so a `#[tokio::test]` can assert on what was
/// emitted. Read back via [`RecordingEventEmitter::events`] (which
/// returns a snapshot — the recorder keeps the original entries
/// for cumulative assertions).
///
/// Cheaply cloneable via the inner `Arc` so a test can hold one
/// reference for assertions and pass another into the system
/// under test.
#[cfg(test)]
#[derive(Default, Clone)]
pub struct RecordingEventEmitter {
    inner: Arc<RecorderInner>,
}

#[cfg(test)]
#[derive(Default)]
struct RecorderInner {
    events: Mutex<Vec<(String, serde_json::Value)>>,
}

#[cfg(test)]
impl RecordingEventEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every `(event, payload)` pair recorded so far,
    /// in emission order. Cloned on call so the test can keep the
    /// snapshot stable across further activity.
    pub fn events(&self) -> Vec<(String, serde_json::Value)> {
        self.inner
            .events
            .lock()
            .expect("RecordingEventEmitter mutex poisoned")
            .clone()
    }

    /// Convenience for the common assertion shape: every emit
    /// for `event_name`, in order, as `serde_json::Value`s.
    pub fn payloads_for(&self, event_name: &str) -> Vec<serde_json::Value> {
        self.events()
            .into_iter()
            .filter_map(|(name, payload)| (name == event_name).then_some(payload))
            .collect()
    }
}

#[cfg(test)]
impl EventEmitter for RecordingEventEmitter {
    fn emit_json(&self, event: &str, payload: serde_json::Value) {
        self.inner
            .events
            .lock()
            .expect("RecordingEventEmitter mutex poisoned")
            .push((event.to_owned(), payload));
    }
}
