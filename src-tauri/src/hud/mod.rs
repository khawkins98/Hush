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
const HUD_LOGICAL_WIDTH: f64 = 220.0;

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
            // wherever the primary monitor currently is.
            //
            // Failure here is non-fatal: if monitor info is
            // unavailable for some reason, the OS picks the position
            // and the HUD still appears. Recording > placement.
            if let Err(e) = position_top_right(&window) {
                tracing::warn!(error = ?e, "failed to position HUD top-right; falling back to OS default");
            }
            window.show().context("show HUD window")?;
            // `set_focus(false)` would be ideal but Tauri 2 doesn't
            // expose a "show without focus" call directly — once a
            // hidden window appears it gets focus by default on most
            // platforms. The `acceptFirstMouse: false` config keeps
            // interaction-claiming minimal and the user's target app
            // retains typing focus on first input.
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

/// Place the HUD `HUD_MARGIN` logical pixels from the top-right
/// corner of the primary monitor.
///
/// Multi-monitor: uses the monitor's physical origin so the HUD lands
/// on whichever screen is the primary one *now*. We do the math in
/// physical pixels because Tauri's `Monitor` exposes physical sizes;
/// `set_position(PhysicalPosition)` matches.
fn position_top_right<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Result<()> {
    let monitor = window
        .primary_monitor()
        .context("query primary monitor")?
        .ok_or_else(|| anyhow::anyhow!("no primary monitor reported"))?;
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

/// Hide the HUD window. Symmetric with [`show`]; same graceful
/// degradation if the window is missing.
pub fn hide<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    if let Some(window) = app.get_webview_window(HUD_LABEL) {
        window.hide().context("hide HUD window")?;
    }
    Ok(())
}
