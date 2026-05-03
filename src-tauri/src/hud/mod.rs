//! Recording HUD overlay — secondary Tauri window shown while
//! dictation is active.
//!
//! Closes the scaffold half of #21. The level-meter half (cpal
//! callbacks compute RMS, audio thread → Tauri event → frontend
//! animates a bar) is the natural follow-up; this module ships the
//! window-lifecycle plumbing only so the HUD can be toggled in
//! response to start/stop without yet streaming any audio data.
//!
//! ## Why a second Tauri window
//!
//! PRD §9 lists "transparent floating HUD with level meter" as
//! in-scope. The HUD's job is to be visible while another app is
//! focused — the user dictates *into* that app, so Hush's main
//! window is in the background. A second window labelled `hud`
//! with `decorations: false`, `transparent: true`,
//! `alwaysOnTop: true`, `skipTaskbar: true` (configured in
//! `tauri.conf.json`) is the standard pattern.
//!
//! The HUD loads `/hud` — a separate Svelte route that renders a
//! minimal "recording" indicator. No interactivity, no fetches; it
//! is essentially a status light driven by Tauri events.
//!
//! ## Show / hide policy
//!
//! - **Show** when `start_dictation` succeeds (the audio stream is
//!   open).
//! - **Hide** when `stop_dictation` returns (regardless of whether
//!   the transcription itself succeeded — the recording is over).
//! - **Hide** if the IPC layer ever exits an in-flight recording
//!   path early (e.g. an error after `audio.start` but before
//!   `audio.stop`). The IPC commands handle this directly today;
//!   moving to a more careful state machine is part of refactor #38.
//!
//! ## Why no level meter yet
//!
//! Streaming the audio level requires the cpal callback (which is
//! on the audio thread, can't directly emit Tauri events) to push
//! per-chunk RMS values through a channel that a Tauri-aware
//! dispatcher consumes. That's a non-trivial refactor of the
//! existing `audio::CpalAudioCapture` worker, and worth its own PR
//! per the wakeup-budget the polish review used. The HUD ships
//! today as "the window appears, the dot pulses". The level meter
//! lands when the audio module exposes a level callback / channel.
//!
//! ## Why not just show/hide a single window
//!
//! The main window is what the user opens to manage settings,
//! history, vocabulary, etc. Folding the HUD into it would mean
//! making the main window borderless / always-on-top during
//! recording, then restoring it afterwards. That's twice the OS
//! window state to juggle and visibly worse UX (the settings panes
//! disappear during recording). A second dedicated window keeps
//! both surfaces independent.

use anyhow::{Context, Result};
use tauri::{AppHandle, Manager, PhysicalPosition, Runtime};

/// Window label that matches the `tauri.conf.json` `windows[].label`.
/// Centralised here so a typo in one call site doesn't silently miss
/// the HUD window.
pub const HUD_LABEL: &str = "hud";

/// HUD logical width in CSS pixels. Mirrors `tauri.conf.json` so the
/// position math has a single source of truth — if the window is
/// resized, the corner offset stays accurate.
///
/// Bumped from 250 → 290 in #481 to give the elapsed-time readout
/// room for the H:MM:SS format meetings hit routinely. At the
/// previous 250 px width "Recording 1:28:46" overflowed the pill,
/// clipping the grip icon on the left and the dismiss button on
/// the right inside the `border-radius: 999px` corners. 290 px
/// fits "Recording 9:59:59" with normal padding to spare.
const HUD_LOGICAL_WIDTH: f64 = 290.0;

/// Top + right margin from the screen edge. Matches the visual
/// breathing room every other system HUD uses (Zoom, Discord, the
/// macOS Recording Indicator). Logical pixels.
const HUD_MARGIN: f64 = 40.0;

/// Make the HUD window visible. Best-effort: if the HUD window
/// doesn't exist (e.g. a misconfigured `tauri.conf.json`), logs an
/// error and returns Ok rather than failing the dictation start.
/// Loss of the HUD is a graceful degradation; the recording itself
/// is the user's deliverable.
pub fn show<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    match app.get_webview_window(HUD_LABEL) {
        Some(window) => {
            // Position before showing so the window doesn't visibly
            // jump from its previous spot. Computing on every show
            // (rather than once at startup) handles the case where
            // the user has moved the laptop to an external display
            // between dictations — the HUD always lands top-right of
            // whichever monitor the user is currently working on.
            //
            // Failure here is non-fatal: if monitor info is
            // unavailable for some reason, the OS picks the position
            // and the HUD still appears. Recording > placement.
            if let Err(e) = position_top_right(&window) {
                tracing::warn!(error = ?e, "failed to position HUD top-right; falling back to OS default");
            }
            show_without_activating(&window)?;
        }
        None => {
            tracing::error!(
                label = HUD_LABEL,
                "HUD window not found; check tauri.conf.json"
            );
        }
    }
    Ok(())
}

/// Show the window without stealing keyboard focus from the user's
/// active app (#262). On macOS, `WebviewWindow::show()` lowers to
/// `NSWindow makeKeyAndOrderFront:` which both reveals the window
/// AND activates the Hush process — keystrokes that follow the
/// recording-start hotkey land in the HUD instead of the user's
/// document.
///
/// Pre-fix the comment in this module claimed `acceptFirstMouse:
/// false` mitigated focus theft; that's wrong. `acceptFirstMouse`
/// only affects the first *mouse* click forwarded to window
/// content; it has zero effect on keyboard focus. The orderFront
/// path below uses the AppKit primitive that reveals the window
/// in the window list without making it key.
///
/// On non-macOS platforms `window.show()` does the right thing
/// already — Linux / Windows window managers don't have the same
/// mac-style "process activation on window show" behaviour.
fn show_without_activating<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        // SAFETY: `ns_window()` returns a valid NSWindow pointer
        // for the lifetime of the `WebviewWindow`. We don't store
        // or escape the pointer; we send exactly one synchronous
        // `orderFront:` message and discard it. The call runs on
        // the main thread (Tauri command path) so it can't race
        // with AppKit's main-thread teardown. `orderFront:` is
        // the AppKit primitive for "show in window list without
        // making key/active" — same call status-bar apps and
        // floating palettes use to surface their windows without
        // stealing focus from the foreground app.
        let ns_window_ptr = window.ns_window().context("retrieve NSWindow pointer")?;
        let ns_window = ns_window_ptr as *mut AnyObject;
        if ns_window.is_null() {
            // Fall back to the standard show path if NSWindow isn't
            // available (shouldn't happen but guards against UAF
            // if Tauri's internals change).
            return window.show().context("show HUD window");
        }
        unsafe {
            // `orderFront:` shows the window without activating the
            // app. `nil` sender == programmatic source.
            let _: () = msg_send![ns_window, orderFront: std::ptr::null_mut::<AnyObject>()];
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.show().context("show HUD window")
    }
}

/// Place the HUD `HUD_MARGIN` logical pixels from the top-right
/// corner of the monitor the user is currently working on.
///
/// "Currently working on" = the monitor containing the mouse cursor
/// (#266). Pre-fix this used `primary_monitor()`, which on a dual-
/// monitor setup where the user's main work happens on an external
/// display would put the HUD on the built-in MacBook screen — out
/// of sight unless they happened to glance at the other monitor.
///
/// Falls back to primary monitor if the cursor position can't be
/// resolved or no monitor matches the cursor (rare, but possible
/// during a display reconfigure).
///
/// Math is in physical pixels because Tauri's `Monitor` exposes
/// physical sizes; `set_position(PhysicalPosition)` matches.
fn position_top_right<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Result<()> {
    let monitor = active_monitor(window)?;
    let scale = monitor.scale_factor();
    let mon_pos = monitor.position();
    let mon_size = monitor.size();

    let hud_w_phys = (HUD_LOGICAL_WIDTH * scale) as i32;
    let margin_phys = (HUD_MARGIN * scale) as i32;

    let x = mon_pos.x + mon_size.width as i32 - hud_w_phys - margin_phys;
    let y = mon_pos.y + margin_phys;

    window
        .set_position(PhysicalPosition::new(x, y))
        .context("set HUD position")?;
    Ok(())
}

/// Pick the monitor the user is currently working on (#266).
/// Cursor-under-monitor wins; primary monitor fallback if the
/// cursor can't be located in any known monitor.
fn active_monitor<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Result<tauri::Monitor> {
    let monitors = window.available_monitors().context("list monitors")?;
    let cursor = window.cursor_position().ok();
    if let Some(pos) = cursor {
        // Cursor position is in physical pixels in Tauri 2 (see
        // `tauri::PhysicalPosition`). Each monitor exposes its
        // origin (`position()`) and size (`size()`) also in physical
        // pixels, so the containment check is a straight
        // axis-aligned bounding box test.
        let cursor_x = pos.x as i32;
        let cursor_y = pos.y as i32;
        if let Some(m) = monitors.iter().find(|m| {
            let origin = m.position();
            let size = m.size();
            cursor_x >= origin.x
                && cursor_x < origin.x + size.width as i32
                && cursor_y >= origin.y
                && cursor_y < origin.y + size.height as i32
        }) {
            return Ok(m.clone());
        }
    }
    window
        .primary_monitor()
        .context("query primary monitor")?
        .ok_or_else(|| anyhow::anyhow!("no primary monitor reported"))
}

/// Hide the HUD window. Symmetric with [`show`]; same graceful
/// degradation if the window is missing.
pub fn hide<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    if let Some(window) = app.get_webview_window(HUD_LABEL) {
        window.hide().context("hide HUD window")?;
    }
    Ok(())
}

/// Main-thread-safe wrappers for [`show`] / [`hide`] (#476).
///
/// `show` / `hide` lower to AppKit `orderFront:` / `orderOut:` on
/// the underlying NSWindow. AppKit window operations are
/// main-thread-only and macOS 26 enforces that strictly — calling
/// from a tokio worker triggers
/// `-[NSWMWindowCoordinator performTransactionUsingBlock:]` →
/// `_os_crash` → process abort.
///
/// Tauri runs sync `#[tauri::command]` handlers on the main
/// thread, but `pub async fn` handlers land on a tokio worker.
/// These helpers dispatch onto the main runloop so async command
/// handlers can show / hide the HUD safely; sync handlers can
/// keep calling `show` / `hide` directly (the dispatch hop would
/// be pointless cost there).
///
/// Best-effort: a dispatch failure is logged and swallowed
/// because a HUD-show/hide failure shouldn't fail the dictation
/// hot path. The actual show/hide runs asynchronously on the next
/// runloop tick — callers that need the visibility flip *before*
/// the next IPC return must remain on the main thread.
pub fn show_async<R: Runtime>(app: &AppHandle<R>) {
    let app_clone = app.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        if let Err(e) = show(&app_clone) {
            tracing::error!(error = ?e, "HUD show failed on main thread");
        }
    }) {
        tracing::error!(error = ?e, "failed to dispatch HUD show to main thread");
    }
}

pub fn hide_async<R: Runtime>(app: &AppHandle<R>) {
    let app_clone = app.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        if let Err(e) = hide(&app_clone) {
            tracing::error!(error = ?e, "HUD hide failed on main thread");
        }
    }) {
        tracing::error!(error = ?e, "failed to dispatch HUD hide to main thread");
    }
}

/// HUD lifecycle state — drives the frontend's render branch
/// (#291). The HUD stays visible across `recording` → `processing`
/// → hidden so the user has a continuous "Hush is still working"
/// signal during the transcription gap; pre-#291 the HUD vanished
/// the instant audio capture stopped, leading users to switch
/// apps and paste before the clipboard had been written.
#[derive(Debug, Clone, Copy)]
pub enum HudState {
    /// Audio capture is active. Pulsing dot + level meter. The
    /// `started_at_ms` field is the Unix-ms timestamp the backend
    /// captured at the moment it considered the recording started;
    /// the frontend anchors its elapsed-time counter to this so
    /// back-to-back sessions reset to 0:00 deterministically (#481).
    Recording { started_at_ms: u64 },
    /// Audio capture stopped, transcription + clipboard write in
    /// flight. Static dot, "Processing…" label, no level meter.
    Processing,
}

impl HudState {
    /// Wire-format state string the frontend matches on.
    fn state_str(self) -> &'static str {
        match self {
            HudState::Recording { .. } => "recording",
            HudState::Processing => "processing",
        }
    }
}

/// Wire payload for the `hud:state` Tauri event. Pre-#481 the
/// payload was a bare string; the frontend mounted the elapsed-
/// time counter from `Date.now()` at event-receipt time, which
/// drifted across back-to-back sessions whenever the persistent
/// HUD page's listener was racing with the show/emit pair on the
/// Rust side. The struct shape pins the start moment to the Rust
/// clock so the timer always reads 0:00 at the cycle the user
/// sees the pill flip into Recording.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HudStatePayload {
    state: &'static str,
    /// Unix-ms timestamp captured by the backend at the moment the
    /// recording started. Only populated for the Recording state.
    /// `None` for Processing — the frontend keeps the existing
    /// "freeze the timer at the final value" behaviour while
    /// transcription completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<u64>,
}

/// Tell the HUD to render a particular lifecycle state. Emits the
/// `hud:state` Tauri event with a `{ state, startedAtMs? }` JSON
/// payload; the HUD page listens on that event and switches its
/// visual. Best-effort: a missing emitter is logged and swallowed
/// because a HUD-event-emit failure shouldn't fail the dictation
/// hot path.
pub fn set_state<R: Runtime>(app: &AppHandle<R>, state: HudState) -> Result<()> {
    use tauri::Emitter as _;
    let started_at_ms = match state {
        HudState::Recording { started_at_ms } => Some(started_at_ms),
        HudState::Processing => None,
    };
    let payload = HudStatePayload {
        state: state.state_str(),
        started_at_ms,
    };
    app.emit("hud:state", &payload).context("emit hud:state")?;
    Ok(())
}

/// Capture the current Unix-ms timestamp for [`HudState::Recording`].
/// Helper so callsites don't repeat the `SystemTime::now()` boilerplate
/// and clamp-to-zero on the (extremely rare) pre-epoch system clock.
pub fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
