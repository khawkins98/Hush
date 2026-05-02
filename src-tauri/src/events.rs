//! Unified event-emit trait shared between the IPC layer and the
//! meeting/dictation modules (#431).
//!
//! ## Why this lives at crate root
//!
//! Both `crate::ipc` and `crate::meeting` need an event-emit
//! abstraction:
//!
//! - **`ipc/`** — the `download_diarizer_model` and `model_download`
//!   commands fire `model:download-progress` / `-done` / `-failed`
//!   from spawned tasks and have to be testable without a real Tauri
//!   runtime (#315).
//! - **`meeting/`** — the pump task fires `meeting:source-failed`
//!   when a per-source capture path drops mid-session, and the
//!   meeting module is held to a stricter "stay Tauri-agnostic"
//!   discipline so its tests can run without `tauri::*`.
//!
//! Pre-#431 each layer had its own narrow trait — `EventEmitter` in
//! `ipc/events.rs` and `MeetingEventEmitter` in `meeting/manager.rs`
//! — with two production wrappers in `ipc/mod.rs` doing essentially
//! the same job. The architecture audit flagged the duplication;
//! merging the two into one trait at the crate root closes it
//! without creating a `meeting → ipc` import cycle (the existing
//! direction is `ipc → meeting`).
//!
//! ## What's here vs in `ipc/events.rs`
//!
//! - **Here** — the trait itself, the typed `emit` convenience that
//!   serializes for callers, and the production [`NoopEventEmitter`]
//!   used by tests + `SessionManager::new_for_test`.
//! - **`ipc/events.rs`** — the production [`crate::ipc::events::TauriEventEmitter`]
//!   that wraps a `tauri::AppHandle`, and the test recorder
//!   [`crate::ipc::events::RecordingEventEmitter`]. Both impl this
//!   trait but live there because they depend on `tauri::*` types
//!   that the `meeting` module deliberately avoids.

use serde::Serialize;

/// Erased emit surface — see module docs.
///
/// Implementors must be `Send + Sync` so commands and pump tasks
/// can hold an `Arc<dyn EventEmitter>` across `tokio::spawn` /
/// `tauri::async_runtime::spawn` boundaries. The [`emit_json`](Self::emit_json)
/// method takes a pre-serialized JSON value to keep the trait
/// object-safe; consumers should call the typed [`emit`](#method.emit)
/// convenience method on `&dyn EventEmitter` rather than serializing
/// payloads by hand.
pub trait EventEmitter: Send + Sync {
    /// Emit `event` with `payload` (already serialized to a JSON
    /// value). Failure is the implementor's concern — production
    /// swallows + logs (matching `app.emit`'s existing fire-and-
    /// forget semantics); tests record the call regardless.
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

/// No-op emitter. Production code constructs a
/// [`crate::ipc::events::TauriEventEmitter`]; this variant means
/// "I don't care about emit events here" — used by
/// [`crate::meeting::SessionManager::new_for_test`] and by unit
/// tests that don't assert on event payloads.
///
/// Distinct from the test-only `RecordingEventEmitter` (which
/// captures payloads for assertion) and from `TauriEventEmitter`
/// (which actually emits): all three impl the same trait, callers
/// pick by what they want from the seam.
pub struct NoopEventEmitter;

impl EventEmitter for NoopEventEmitter {
    fn emit_json(&self, _event: &str, _payload: serde_json::Value) {}
}
