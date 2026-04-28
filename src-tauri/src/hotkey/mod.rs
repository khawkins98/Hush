//! Global hotkeys: toggle-record (this module) and push-to-talk (`ptt`).
//!
//! Toggle-record uses `tauri-plugin-global-shortcut`; PTT uses `rdev`
//! directly so it can observe key-down and key-up as separate events.
//! The two coexist: the user may bind both simultaneously (they emit
//! distinct Tauri events so the frontend disambiguates). If the user
//! binds them to the same physical key, both will fire — that is the
//! user's choice; we log and continue rather than try to be clever.
//!
//! Below: original module header for the toggle hotkey.
//!
//! ---
//!
//! ## Toggle-record hotkey
//!
//! Global toggle-record hotkey via `tauri-plugin-global-shortcut`.
//!
//! Concept inspired by VoiceInk's KeyboardShortcuts-based hotkey handling.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Scope
//!
//! Closes the toggle-record half of #5. Push-to-talk (key-down / key-up
//! via `rdev`) is the second half — see `hotkey::ptt`. Toggle is
//! the canonical hotkey for the press / talk / press / paste flow;
//! PTT adds press-and-hold semantics on top.
//!
//! ## Architecture
//!
//! The shortcut handler does not run the dictation pipeline directly. It
//! emits a `hotkey:toggle` Tauri event, and the frontend — which already
//! owns the recording / busy / device-selection state — decides whether to
//! invoke `start_dictation` or `stop_dictation` against that state.
//! Concretely: one source of truth for the UI's recording flag and one
//! orchestration path (the existing IPC commands) for the pipeline.
//!
//! Backend-driven dictation (no frontend window open) is a future
//! enhancement and would re-use the standalone helpers in `ipc::*`.
//! Tauri keeps the window alive via the tray icon, so a listener is
//! always present today.
//!
//! ## Configuration
//!
//! The default hotkey is [`DEFAULT_TOGGLE_HOTKEY`]. The PTT combo
//! is editable in Settings → General → Hotkeys (`PttHotkeyEditor`);
//! the toggle hotkey itself is still env-configurable
//! (`HUSH_TOGGLE_HOTKEY`) but doesn't yet have a Settings UI for
//! rebinding.
//!
//! ## Platform notes
//!
//! - **macOS**: requires Input Monitoring permission for the shortcut to
//!   fire when Hush is unfocused. `tauri-plugin-global-shortcut` handles
//!   the registration plumbing; the OS prompt is owned by the user's
//!   Privacy & Security settings and surfaces on first capture attempt.
//! - **Wayland**: global hotkeys are compositor-dependent. We document
//!   GNOME as the primary target initially; other compositors may
//!   silently no-op the registration. See PRD §10.

pub mod ptt;

pub use ptt::{register_ptt_listener, EVENT_PTT_PRESS, EVENT_PTT_RELEASE};

use anyhow::{Context, Result};
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

/// Default global hotkey, in `tauri-plugin-global-shortcut` syntax.
///
/// `Ctrl+Alt+H` resolves to literal Control + Option/Alt + H on all
/// three platforms — this is `⌃⌥H` on macOS. Picked because:
///
/// - Free on macOS: no system shortcut, no Finder/app default, and
///   no overlap with the character-picker chord (`⌃⌘Space`) or
///   Spotlight (`⌘Space`). The earlier candidate `⌘+Shift+H` was
///   considered but conflicts with Finder's Go → Home shortcut.
/// - Free on Linux: GNOME/KDE/i3 don't bind `Ctrl+Alt+H` by default.
/// - Free on Windows: no system or common-app default.
/// - `H` for "Hush" — easy mnemonic.
/// - Sits in the same modifier-family VoiceInk uses (`⌃⌥V`), so
///   users coming from a similar tool find it immediately reachable.
///
/// Note: `Ctrl` here is *not* `CmdOrCtrl`. On macOS this is the
/// literal Control key, not Command — that's intentional, because
/// `Cmd+Alt+H` is "Hide All Other Apps" on macOS and would conflict.
///
/// Override with `HUSH_TOGGLE_HOTKEY` until the settings UI exposes
/// a picker.
pub const DEFAULT_TOGGLE_HOTKEY: &str = "Ctrl+Alt+H";

/// Environment variable consulted at app startup to override the
/// default toggle hotkey. The PTT combo has a Settings → General
/// → Hotkeys picker; the toggle hotkey itself is env-only today.
pub const ENV_TOGGLE_HOTKEY: &str = "HUSH_TOGGLE_HOTKEY";

/// Event name emitted to the frontend on hotkey press.
///
/// The payload is `()` — the event itself is the signal; the frontend
/// owns the toggle-state bookkeeping. Treating the event as a poke keeps
/// the contract trivial and side-step's Tauri's event-payload schema.
pub const EVENT_TOGGLE: &str = "hotkey:toggle";

/// Resolve the toggle hotkey from the environment, falling back to the
/// default. Pulled out as its own function so unit tests can exercise the
/// parsing without spawning a Tauri runtime.
pub fn resolve_toggle_hotkey(env_value: Option<&str>) -> Result<Shortcut> {
    let raw = env_value
        .map(str::to_owned)
        .unwrap_or_else(|| DEFAULT_TOGGLE_HOTKEY.to_owned());
    raw.parse::<Shortcut>()
        .with_context(|| format!("invalid hotkey expression: {raw:?}"))
}

/// Register the default toggle hotkey on the global-shortcut plugin.
///
/// Called from the Tauri `setup` hook; the handler that fires on press is
/// installed on the plugin's [`tauri_plugin_global_shortcut::Builder`]
/// itself in `lib.rs` so the closure can outlive any single shortcut and
/// dispatch on the [`Shortcut`] argument once we register more than one.
///
/// # Errors
///
/// Returns an error if the hotkey expression cannot be parsed, or if the
/// OS refuses the registration (already in use, missing permission, or —
/// on Wayland — the compositor doesn't expose the API). We surface this
/// from `setup` so the user sees it in the dev console; the rest of the
/// app continues to work without the hotkey.
pub fn register_default<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let env_value = std::env::var(ENV_TOGGLE_HOTKEY).ok();
    let shortcut = resolve_toggle_hotkey(env_value.as_deref())?;

    let display = shortcut_display(&shortcut);
    let display_for_log = display.clone();
    app.global_shortcut()
        .register(shortcut)
        .with_context(|| format!("failed to register hotkey {display}"))?;

    tracing::info!(hotkey = %display_for_log, "registered toggle-record hotkey");
    Ok(())
}

/// Handler installed on the global-shortcut plugin builder. Routes any
/// hotkey *press* (release ignored — the user is using a toggle, not
/// push-to-talk) to a `hotkey:toggle` event emitted to the frontend.
///
/// We deliberately swallow emit errors here: if the frontend window has
/// been destroyed there is no listener to receive, and the hotkey press
/// is effectively a no-op. Logging at warn-level keeps the failure
/// observable without spamming the console under normal operation.
pub fn handle_shortcut_event<R: Runtime>(
    app: &AppHandle<R>,
    _shortcut: &Shortcut,
    event: ShortcutEvent,
) {
    if event.state() != ShortcutState::Pressed {
        return;
    }
    if let Err(e) = app.emit(EVENT_TOGGLE, ()) {
        tracing::warn!(error = ?e, "failed to emit hotkey:toggle event");
    }
}

/// Render a [`Shortcut`] for log output. The `Display` impl on
/// `Shortcut` prints the platform-specific symbol set (e.g. `⌘⇧Space`),
/// which is great for users but harder to grep in CI logs; we print the
/// debug form, which is close to the registration string.
fn shortcut_display(shortcut: &Shortcut) -> String {
    format!("{shortcut:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_when_env_unset() {
        let parsed = resolve_toggle_hotkey(None).expect("default must parse");
        let default = DEFAULT_TOGGLE_HOTKEY
            .parse::<Shortcut>()
            .expect("constant must parse");
        assert_eq!(format!("{parsed:?}"), format!("{default:?}"));
    }

    #[test]
    fn resolves_override_from_env() {
        let parsed = resolve_toggle_hotkey(Some("Alt+F12")).expect("override must parse");
        // We don't compare against a hand-rolled Shortcut value because the
        // type's internal representation can change across plugin versions
        // — round-tripping through Debug is sufficient to confirm parsing.
        assert!(format!("{parsed:?}").contains("F12"), "got: {parsed:?}");
    }

    #[test]
    fn rejects_unparseable_expression() {
        let err = resolve_toggle_hotkey(Some("not-a-real-key"))
            .expect_err("garbage should fail to parse");
        let msg = format!("{err:#}");
        assert!(msg.contains("not-a-real-key"), "got: {msg}");
    }
}
