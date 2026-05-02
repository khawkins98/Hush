//! Event-emit seam for `#[tauri::command]` paths that fire Tauri
//! events from spawned tasks (#315).
//!
//! ## Motivation
//!
//! The `download_diarizer_model` and `model_download` paths take a
//! `tauri::AppHandle` so the spawned task can emit
//! `model:download-progress` / `-done` / `-failed` events. That makes
//! them un-testable from a normal `#[tokio::test]` — there's no
//! lightweight way to construct a real `AppHandle` outside the Tauri
//! runtime, and the diarizer audit-of-audit (post-#308) flagged two
//! coverage gaps that exist precisely because of this:
//!
//! - Duplicate-rejection guard inside the `state.downloads.lock()`
//!   critical section in `download_diarizer_model` is untested.
//! - Cancel-handle cleanup on the failure branch is untested.
//!
//! The Whisper download (`models::model_download`) has the same shape
//! and the same gaps.
//!
//! ## Shape
//!
//! [`EventEmitter`] is the trait the command bodies consume in place
//! of `AppHandle::emit(...)`. Production wires
//! [`TauriEventEmitter`] (a thin wrapper around `AppHandle`) at the
//! command's entry point; tests wire [`RecordingEventEmitter`] which
//! captures `(event, payload)` pairs into a `Mutex<Vec<…>>` so the
//! test can assert on what was emitted and in what order.
//!
//! The trait is object-safe — `Arc<dyn EventEmitter>` is the wire
//! shape. The single trait method takes a pre-serialized
//! [`serde_json::Value`] payload to keep object-safety; the
//! [`EventEmitter::emit`] convenience method on `&dyn` callers does
//! the `serde_json::to_value` conversion so consumers get to keep
//! their typed payload structs.
//!
//! ## What this is *not*
//!
//! Not a general-purpose Tauri-event abstraction layer. The trait
//! covers the one operation download paths need (one-shot emit). It
//! deliberately doesn't model `app.listen(...)`, window-scoped emits,
//! or webview-targeted emits — none of those are on the test path
//! today. Adding them when a real consumer needs them is fine.

use serde::Serialize;

#[cfg(test)]
use std::sync::{Arc, Mutex};

/// Erased emit surface — see module docs.
///
/// Implementors must be `Send + Sync` so commands can hold an
/// `Arc<dyn EventEmitter>` across `tauri::async_runtime::spawn`
/// boundaries. The `emit_json` method takes a pre-serialized JSON
/// value to keep the trait object-safe; consumers should call
/// [`EventEmitter::emit`] (the typed convenience helper below)
/// rather than serializing manually.
pub trait EventEmitter: Send + Sync {
    /// Emit `event` with `payload` (already serialized to a
    /// JSON value). Failure is the implementor's concern —
    /// production swallows + logs (matching `app.emit`'s
    /// existing fire-and-forget semantics); tests record the
    /// call regardless.
    fn emit_json(&self, event: &str, payload: serde_json::Value);
}

/// Convenience wrapper around the object-safe [`EventEmitter::emit_json`]
/// that does the `serde_json::to_value` conversion. Defined on
/// `dyn EventEmitter` so callers using the trait via `Arc<dyn …>`
/// pick it up automatically.
///
/// Serialization failures are logged + swallowed. The whole point
/// of the trait is to be a fire-and-forget seam mirroring
/// `app.emit`'s existing best-effort shape — the user's transcript
/// (or model download) is the load-bearing artifact, not a
/// progress event.
impl dyn EventEmitter {
    pub fn emit<T: Serialize>(&self, event: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(v) => self.emit_json(event, v),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    event = %event,
                    "EventEmitter: payload serialization failed; event dropped"
                );
            }
        }
    }
}

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
