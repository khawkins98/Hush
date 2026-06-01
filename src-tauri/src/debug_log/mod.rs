//! In-app debug log console (#532).
//!
//! Captures `tracing` events from the Rust backend into a ring buffer
//! and forwards them to the frontend in real time via a Tauri event.
//! The buffer persists until the app exits and is drained to new
//! windows/pages on demand via the `get_log_entries` IPC command.
//!
//! ## Architecture
//!
//! ```text
//! tracing macros → DebugLogLayer::on_event
//!                      │
//!                      ├─► ring buffer (cap 500, seq-numbered)
//!                      │
//!                      └─► AppHandle::emit("log:event", entry)
//!                            (only after handle is set AND the debug
//!                            console window is visible; lock dropped
//!                            before the emit call)
//! ```
//!
//! ## Why the emit is gated on console visibility (#986)
//!
//! Every `emit` to a webview becomes a WKWebView `evaluateJavaScript`
//! call on macOS, and those leak host-process memory *per call*
//! (WebKit bug 215729, unfixed upstream) on top of bmalloc page
//! retention. The debug window is pre-created (hidden) at startup with
//! a live `log:event` listener, so an ungated emit streams every log
//! line into a window nobody can see — during meetings that was
//! measured at ~160 MB/min of `WebKit Malloc` growth. While the
//! console is hidden, entries land in the ring buffer only; the
//! frontend re-syncs from the buffer (seq-deduplicated) when the
//! window becomes visible again.
//!
//! ## Startup ordering
//!
//! Events that fire before `setup()` sets the AppHandle accumulate in
//! the ring buffer. The frontend calls `get_log_entries` after
//! subscribing to `"log:event"` and uses the `seq` field to drop
//! duplicates — guaranteeing no events are lost across the gap.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;

/// A single captured log event, serialised over the `log:event` channel
/// and returned by `get_log_entries`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// Monotonically increasing sequence number — used by the frontend
    /// to deduplicate the initial `get_log_entries` snapshot against
    /// events already received via the live `log:event` listener.
    pub seq: u64,
    /// Wall-clock milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// Severity: `"TRACE"`, `"DEBUG"`, `"INFO"`, `"WARN"`, `"ERROR"`.
    pub level: String,
    /// The tracing target (usually the Rust module path).
    pub target: String,
    /// Full formatted line: the message field plus any structured
    /// fields in `key=value` form, space-separated.
    pub message: String,
}

/// Shared state between the tracing layer and the rest of the app.
///
/// Clone-cheap because the inner data is behind an `Arc`.
#[derive(Clone)]
pub struct DebugLogState {
    /// Sequence counter — incremented per-event so the frontend can
    /// deduplicate snapshot + live-stream.
    seq: Arc<AtomicU64>,
    /// Ring buffer, capacity 500.
    buffer: Arc<Mutex<VecDeque<LogEntry>>>,
    /// Set once during Tauri `setup()`. Write-once so we never pay
    /// a lock on the hot path; reads after `set` are lock-free.
    handle: Arc<OnceLock<AppHandle>>,
    /// Whether the debug-console window is currently visible. The
    /// `log:event` live-stream emit is gated on this (#986): each emit
    /// is a WKWebView `evaluateJavaScript` call that leaks per-call on
    /// macOS, so streaming into a hidden window is pure waste. Starts
    /// `false` (the window is created `visible: false`); flipped by
    /// `open_debug_window` and the hide-on-close handler.
    console_visible: Arc<AtomicBool>,
}

impl DebugLogState {
    pub fn new() -> Self {
        Self {
            seq: Arc::new(AtomicU64::new(0)),
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(500))),
            handle: Arc::new(OnceLock::new()),
            console_visible: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Call once from the Tauri `setup()` hook to enable live streaming.
    pub fn set_handle(&self, handle: AppHandle) {
        // `set` is a no-op if already set — safe to call more than once.
        let _ = self.handle.set(handle);
    }

    /// Record whether the debug-console window is visible. Live
    /// `log:event` streaming only happens while it is (#986).
    pub fn set_console_visible(&self, visible: bool) {
        self.console_visible.store(visible, Ordering::Relaxed);
    }

    /// Whether the debug-console window is currently visible.
    pub fn console_visible(&self) -> bool {
        self.console_visible.load(Ordering::Relaxed)
    }

    /// Return a snapshot of the current ring buffer contents in
    /// insertion order (oldest first). Used by `get_log_entries` to
    /// let the frontend catch up before its live listener was attached.
    pub fn snapshot(&self) -> Vec<LogEntry> {
        // Recover from a poisoned mutex rather than panicking — the log
        // buffer is non-critical and a previous thread panic should not
        // kill log snapshot reads.
        self.buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

impl Default for DebugLogState {
    fn default() -> Self {
        Self::new()
    }
}

/// Tracing subscriber layer. Attach with
/// `registry().with(DebugLogLayer::new(state))`.
pub struct DebugLogLayer {
    state: DebugLogState,
}

impl DebugLogLayer {
    pub fn new(state: DebugLogState) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for DebugLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let level = event.metadata().level().to_string();
        let target = event.metadata().target().to_string();

        // Visit all fields, collecting the "message" value and any
        // additional structured fields into a formatted line.
        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);

        let seq = self.state.seq.fetch_add(1, Ordering::Relaxed);
        let entry = LogEntry {
            seq,
            timestamp_ms: now,
            level,
            target,
            message: visitor.formatted(),
        };

        // 1. Append to ring buffer (capped at 500). Drop the lock
        //    before we emit to the frontend to avoid holding it
        //    during a potentially-slow Tauri IPC call.
        let handle_opt = {
            // Recover from a poisoned mutex rather than panicking — a
            // previous thread panic should not permanently disable logging.
            let mut buf = self.state.buffer.lock().unwrap_or_else(|e| e.into_inner());
            if buf.len() == 500 {
                buf.pop_front();
            }
            buf.push_back(entry.clone());
            // Cheaply clone the OnceLock pointer while the buffer
            // lock is held — we get the Option<AppHandle> outside.
            self.state.handle.get().cloned()
        };

        // 2. Forward to frontend — only if the handle has been set AND
        //    the debug console is actually visible (#986). Each emit is
        //    a WKWebView evaluateJavaScript call into the (possibly
        //    hidden) debug webview, and those leak host-process memory
        //    per call on macOS. While hidden, the ring buffer is the
        //    only sink; the console re-syncs from it on reopen.
        if let Some(handle) = handle_opt {
            if self.state.console_visible() {
                let _ = handle.emit("log:event", &entry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_visibility_defaults_to_hidden() {
        // The debug window is created `visible: false` in
        // tauri.conf.json, so a fresh state must start with streaming
        // disabled — otherwise the #986 leak comes back silently.
        let state = DebugLogState::new();
        assert!(!state.console_visible());
    }

    #[test]
    fn console_visibility_roundtrips_and_is_shared_across_clones() {
        // The layer holds one clone of the state and the IPC layer
        // another; the visibility flag must be shared, not per-clone.
        let state = DebugLogState::new();
        let layer_clone = state.clone();
        state.set_console_visible(true);
        assert!(layer_clone.console_visible());
        layer_clone.set_console_visible(false);
        assert!(!state.console_visible());
    }
}

// ---------------------------------------------------------------------------
// Field visitor
// ---------------------------------------------------------------------------

/// Accumulates tracing fields into a human-readable string:
/// `<message> key=value key=value …`
#[derive(Default)]
struct FieldCollector {
    message: Option<String>,
    extra: Vec<(String, String)>,
}

impl FieldCollector {
    fn formatted(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = self.message {
            parts.push(m);
        }
        for (k, v) in self.extra {
            parts.push(format!("{k}={v}"));
        }
        parts.join(" ")
    }
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(s);
        } else {
            self.extra.push((field.name().to_string(), s));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.extra
                .push((field.name().to_string(), value.to_string()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.extra
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.extra
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.extra
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.extra
            .push((field.name().to_string(), value.to_string()));
    }
}
