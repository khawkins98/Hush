//! Push-to-talk (PTT) hotkey via the `rdev` crate.
//!
//! Concept inspired by VoiceInk's hold-to-talk hotkey path. Reimplemented
//! from observed public behaviour; no source code referenced. See §13.8 of
//! the PRD.
//!
//! ## Why `rdev` rather than `tauri-plugin-global-shortcut`
//!
//! The global-shortcut plugin (used for the toggle hotkey in `hotkey/mod.rs`)
//! exposes an `Activated`/`Pressed` state on every match but does not give
//! us a clean key-up signal that survives the platform back-ends. PTT
//! requires both edges of the key event so the recorder starts on key-down
//! and stops on key-up. `rdev` taps the OS event stream directly and
//! surfaces `KeyPress` and `KeyRelease` as separate variants, which is the
//! contract we need.
//!
//! ## Threading model
//!
//! `rdev::listen` blocks the calling thread for the lifetime of the
//! listener — it installs a low-level OS hook (CGEventTap on macOS, an X11
//! grab on Linux, a Windows hook on Windows) and pumps events from that
//! thread. We therefore run it on a dedicated `std::thread` whose only
//! responsibility is forwarding press/release events for the configured key
//! to the Tauri event bus. The thread is unjoined and intentionally lives
//! for the rest of the process: there is no clean way to stop `rdev` short
//! of process exit, and process exit will reap the thread for us.
//!
//! ## macOS Input Monitoring permission
//!
//! On first run, `rdev::listen` triggers the OS prompt asking the user to
//! grant the running binary Input Monitoring (and Accessibility) access in
//! System Settings → Privacy & Security. Until the permission is granted,
//! macOS silently drops events and `rdev` reports `Ok` from `listen` while
//! delivering nothing to the callback. There is no programmatic way around
//! this; the user must approve the prompt and (for some Tauri dev builds)
//! restart the app afterwards. This is documented in the README and
//! `learnings.md`. See PRD §10.
//!
//! ## Wayland degradation
//!
//! `rdev` 0.5's Linux back-end uses X11. Under Wayland, `listen` typically
//! returns immediately with `ListenError::EventTapError` (or a similar
//! variant depending on the compositor) and no events flow. We log the
//! error at `error` level and continue: PTT is unavailable, but the toggle
//! hotkey (which goes through the compositor's `XdgGlobalShortcuts` portal
//! via `tauri-plugin-global-shortcut`) and button-driven dictation still
//! work. PRD §10 documents GNOME on X11 as the supported initial target.
//!
//! ## Event contract with the frontend
//!
//! Two Tauri events are emitted, with `()` payload (mirroring
//! `hotkey:toggle`):
//!
//! - `hotkey:ptt-press` on key-down of the configured PTT key.
//! - `hotkey:ptt-release` on key-up of the same key.
//!
//! The frontend dispatches `start_dictation` on press (if not already
//! recording) and `stop_dictation` on release (if recording). Keeping the
//! state in the frontend means PTT and the toggle hotkey share one source
//! of truth for "are we recording right now?" — see the toggle's module
//! header for the rationale.
//!
//! Auto-repeat handling: most platforms do *not* fire repeat key events
//! through the low-level hook `rdev` uses (X11 will, but consecutive
//! KeyPress without a KeyRelease is harmless because the frontend ignores
//! a press while `recording` is already true). We don't try to dedupe at
//! this layer; the frontend's existing busy/recording flags are sufficient.

use std::thread;

use anyhow::{Context, Result};
use rdev::{listen, Event, EventType, Key};
use tauri::{AppHandle, Emitter, Runtime};

/// Default PTT key.
///
/// On macOS, `RightMeta` (the Right Cmd key) is the default: every Apple
/// keyboard has a Right Cmd, but not every Apple keyboard has a Right
/// Ctrl (Magic Keyboards / many MacBooks ship Ctrl on the left only).
/// Right Cmd is reachable by the right hand, doesn't conflict with
/// normal typing, and is rarely bound by other applications. The full
/// command shortcuts that *do* use Cmd are press-and-release chords
/// (⌘C, ⌘V) — holding Right Cmd alone is a no-op in macOS' default
/// bindings.
///
/// On other platforms `RightControl` stays the default — the
/// conventional choice for hold-to-talk in voice apps (Discord, OBS,
/// Mumble). The user can override either default via `HUSH_PTT_HOTKEY`.
///
/// Modifier-only keys also avoid the "press a letter to start
/// recording, but now you've typed that letter into the focused app"
/// footgun that letter-keys would create.
#[cfg(target_os = "macos")]
pub const DEFAULT_PTT_KEY: PttKey = PttKey::RightMeta;
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_PTT_KEY: PttKey = PttKey::RightControl;

/// Environment variable consulted at startup to override the default.
/// Mirrors `HUSH_TOGGLE_HOTKEY`. Once the settings UI lands (M3) this
/// becomes a development override rather than the primary mechanism.
pub const ENV_PTT_HOTKEY: &str = "HUSH_PTT_HOTKEY";

/// Force-disable the rdev PTT listener even on platforms where it would
/// otherwise auto-enable. Set `HUSH_PTT_DISABLE=1`.
pub const ENV_PTT_DISABLE: &str = "HUSH_PTT_DISABLE";

/// Force-enable the rdev PTT listener on platforms where it would
/// otherwise auto-disable. Set `HUSH_PTT_ENABLE=1`. Currently only
/// meaningful on macOS, where PTT stays opt-in: the macOS-26 abort
/// (#69) is fixed by pinning rdev to fufesou's fork (see Cargo.toml),
/// but enabling PTT triggers the Input Monitoring permission
/// prompt — a privacy surprise some users won't want without first
/// opting in.
pub const ENV_PTT_ENABLE: &str = "HUSH_PTT_ENABLE";

/// Event emitted to the frontend on PTT key-down.
pub const EVENT_PTT_PRESS: &str = "hotkey:ptt-press";

/// Event emitted to the frontend on PTT key-up.
pub const EVENT_PTT_RELEASE: &str = "hotkey:ptt-release";

/// Subset of `rdev::Key` we accept as PTT bindings.
///
/// We intentionally do *not* expose every `rdev::Key`. Letter and number
/// keys would conflict with normal typing; arrow keys with navigation;
/// `Function` (the Fn key) is not delivered consistently across platforms.
/// Restricting to a curated set means the parse step doubles as
/// validation, and the user can't shoot their foot off binding PTT to "a".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PttKey {
    RightControl,
    LeftControl,
    RightAlt,
    LeftAlt,
    RightShift,
    LeftShift,
    RightMeta,
    LeftMeta,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    CapsLock,
}

impl PttKey {
    /// Resolve the configured PTT key against an `rdev::Key` event.
    ///
    /// Pure function so the matching logic is unit-testable without
    /// spawning a listener thread.
    pub fn matches(self, key: Key) -> bool {
        matches!(
            (self, key),
            (PttKey::RightControl, Key::ControlRight)
                | (PttKey::LeftControl, Key::ControlLeft)
                | (PttKey::RightAlt, Key::AltGr)
                | (PttKey::LeftAlt, Key::Alt)
                | (PttKey::RightShift, Key::ShiftRight)
                | (PttKey::LeftShift, Key::ShiftLeft)
                | (PttKey::RightMeta, Key::MetaRight)
                | (PttKey::LeftMeta, Key::MetaLeft)
                | (PttKey::F1, Key::F1)
                | (PttKey::F2, Key::F2)
                | (PttKey::F3, Key::F3)
                | (PttKey::F4, Key::F4)
                | (PttKey::F5, Key::F5)
                | (PttKey::F6, Key::F6)
                | (PttKey::F7, Key::F7)
                | (PttKey::F8, Key::F8)
                | (PttKey::F9, Key::F9)
                | (PttKey::F10, Key::F10)
                | (PttKey::F11, Key::F11)
                | (PttKey::F12, Key::F12)
                | (PttKey::CapsLock, Key::CapsLock)
        )
    }

    /// Stable identifier for log output.
    pub fn as_str(self) -> &'static str {
        match self {
            PttKey::RightControl => "RightControl",
            PttKey::LeftControl => "LeftControl",
            PttKey::RightAlt => "RightAlt",
            PttKey::LeftAlt => "LeftAlt",
            PttKey::RightShift => "RightShift",
            PttKey::LeftShift => "LeftShift",
            PttKey::RightMeta => "RightMeta",
            PttKey::LeftMeta => "LeftMeta",
            PttKey::F1 => "F1",
            PttKey::F2 => "F2",
            PttKey::F3 => "F3",
            PttKey::F4 => "F4",
            PttKey::F5 => "F5",
            PttKey::F6 => "F6",
            PttKey::F7 => "F7",
            PttKey::F8 => "F8",
            PttKey::F9 => "F9",
            PttKey::F10 => "F10",
            PttKey::F11 => "F11",
            PttKey::F12 => "F12",
            PttKey::CapsLock => "CapsLock",
        }
    }
}

/// Parse a PTT key name. Case-insensitive, accepts a small set of common
/// aliases (`Ctrl` for `Control`, `Cmd`/`Super`/`Win` for `Meta`).
///
/// Pure function: no I/O, no globals. Exposed so unit tests can exercise
/// the parser without spawning a thread or touching `rdev`.
pub fn parse_ptt_key(raw: &str) -> Result<PttKey> {
    let normalised = raw.trim().to_ascii_lowercase().replace(['_', '-', ' '], "");
    let key = match normalised.as_str() {
        "rightcontrol" | "rightctrl" | "rctrl" | "rcontrol" => PttKey::RightControl,
        "leftcontrol" | "leftctrl" | "lctrl" | "lcontrol" => PttKey::LeftControl,
        "rightalt" | "ralt" | "altgr" => PttKey::RightAlt,
        "leftalt" | "lalt" | "alt" | "option" => PttKey::LeftAlt,
        "rightshift" | "rshift" => PttKey::RightShift,
        "leftshift" | "lshift" | "shift" => PttKey::LeftShift,
        // "Meta" is the umbrella name for the Win/Cmd/Super key. Accept the
        // platform-specific aliases users are likely to type.
        "rightmeta" | "rmeta" | "rightcmd" | "rcmd" | "rightsuper" | "rsuper" | "rightwin"
        | "rwin" => PttKey::RightMeta,
        "leftmeta" | "lmeta" | "leftcmd" | "lcmd" | "cmd" | "leftsuper" | "lsuper" | "super"
        | "leftwin" | "lwin" | "win" => PttKey::LeftMeta,
        "f1" => PttKey::F1,
        "f2" => PttKey::F2,
        "f3" => PttKey::F3,
        "f4" => PttKey::F4,
        "f5" => PttKey::F5,
        "f6" => PttKey::F6,
        "f7" => PttKey::F7,
        "f8" => PttKey::F8,
        "f9" => PttKey::F9,
        "f10" => PttKey::F10,
        "f11" => PttKey::F11,
        "f12" => PttKey::F12,
        "capslock" | "caps" => PttKey::CapsLock,
        other => {
            anyhow::bail!(
                "unrecognised PTT key {other:?} — accepted: RightControl, LeftControl, \
                 RightAlt, LeftAlt, RightShift, LeftShift, RightMeta, LeftMeta, F1–F12, CapsLock"
            );
        }
    };
    Ok(key)
}

/// Resolve the PTT key from an optional environment value, falling back to
/// the default. Pulled out as a pure function so tests can drive it
/// without setting real env vars.
pub fn resolve_ptt_key(env_value: Option<&str>) -> Result<PttKey> {
    match env_value {
        Some(raw) => {
            parse_ptt_key(raw).with_context(|| format!("invalid {ENV_PTT_HOTKEY} value: {raw:?}"))
        }
        None => Ok(DEFAULT_PTT_KEY),
    }
}

/// A push-to-talk key combination — one or more [`PttKey`]s that must all
/// be held simultaneously for PTT to fire.
///
/// "Combination" here means the AND of held keys, not a sequence: holding
/// `RightMeta + RightShift` together is the trigger; pressing them in any
/// order is fine, releasing any one of them releases PTT.
///
/// Single-key combos (the common case) work the same way: a one-element
/// combo of `RightMeta` is exactly the previous "PTT key = RightMeta"
/// behaviour.
///
/// Stored canonically: keys deduplicated, sorted by `as_str()` so two
/// equivalent combos compare equal regardless of insertion order. Empty
/// combos are rejected at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PttCombo {
    keys: Vec<PttKey>,
}

impl PttCombo {
    /// Build a combo from a non-empty iterator of keys. Returns an error
    /// for an empty input (an empty combo can never fire — it's almost
    /// certainly a bug at the call site).
    pub fn try_from_keys(keys: impl IntoIterator<Item = PttKey>) -> Result<Self> {
        let mut canon: Vec<PttKey> = keys.into_iter().collect();
        canon.sort_by_key(|k| k.as_str());
        canon.dedup();
        if canon.is_empty() {
            anyhow::bail!("PTT combo must contain at least one key");
        }
        Ok(Self { keys: canon })
    }

    /// Convenience for the single-key case (the historical default).
    pub fn single(key: PttKey) -> Self {
        Self { keys: vec![key] }
    }

    pub fn keys(&self) -> &[PttKey] {
        &self.keys
    }

    /// Render as a `+`-separated string for env-var / settings-DB
    /// storage, e.g. `RightMeta` or `RightMeta+RightShift`. Round-trips
    /// through [`parse_ptt_combo`].
    pub fn to_storage_string(&self) -> String {
        self.keys
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// Parse a combo from the storage / env-var format: one or more
/// [`PttKey`] names joined by `+`. Whitespace tolerated. Single-key
/// strings (no separator) parse cleanly to a 1-key combo, so any
/// existing single-key configuration round-trips.
pub fn parse_ptt_combo(raw: &str) -> Result<PttCombo> {
    let parts: Vec<&str> = raw
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        anyhow::bail!("empty PTT combo");
    }
    let mut keys = Vec::with_capacity(parts.len());
    for part in parts {
        keys.push(parse_ptt_key(part)?);
    }
    PttCombo::try_from_keys(keys)
}

/// State for matching a [`PttCombo`] against a stream of `rdev::Event`s.
///
/// Tracks which combo keys are currently held; the combo is active when
/// every key in it is held. Edge transitions (inactive → active and
/// active → inactive) are what the listener emits as press / release
/// events.
///
/// Lives separately from the listener thread so unit tests can drive it
/// with synthetic events.
#[derive(Debug, Default)]
pub struct ComboMatcher {
    held: std::collections::HashSet<PttKey>,
    active: bool,
}

/// What [`ComboMatcher::observe`] returns for a single event.
#[derive(Debug, PartialEq, Eq)]
pub enum ComboTransition {
    /// Combo just became fully held — emit PTT-press.
    Pressed,
    /// Combo just stopped being fully held — emit PTT-release.
    Released,
    /// No edge transition. Most events fall here (most keys aren't in
    /// the combo, or the held set didn't change the active bit).
    None,
}

impl ComboMatcher {
    /// Update the matcher with one event. Returns the transition (if
    /// any) so the caller can fire the corresponding Tauri event.
    pub fn observe(&mut self, event: &Event, combo: &PttCombo) -> ComboTransition {
        // Map the rdev key back to our PttKey, if it's one of the keys
        // we recognise. Events for keys outside the combo (most of
        // them) flow through without changing state.
        let (key, pressed) = match event.event_type {
            EventType::KeyPress(k) => match ptt_key_for(k) {
                Some(p) => (p, true),
                None => return ComboTransition::None,
            },
            EventType::KeyRelease(k) => match ptt_key_for(k) {
                Some(p) => (p, false),
                None => return ComboTransition::None,
            },
            _ => return ComboTransition::None,
        };

        // Only mutate held-set on combo-relevant keys. If the user
        // presses some random F-key we know about but it's not in
        // their combo, we don't need to track it — but tracking is
        // cheap (small HashSet) and lets us avoid recomputing
        // membership on every event.
        if pressed {
            self.held.insert(key);
        } else {
            self.held.remove(&key);
        }

        let now_active = combo.keys().iter().all(|k| self.held.contains(k));
        match (self.active, now_active) {
            (false, true) => {
                self.active = true;
                ComboTransition::Pressed
            }
            (true, false) => {
                self.active = false;
                ComboTransition::Released
            }
            _ => ComboTransition::None,
        }
    }
}

/// Reverse map from `rdev::Key` to our [`PttKey`] enum. Returns `None`
/// for any key outside the curated PTT set (letters, digits, arrows,
/// etc.) so the matcher cleanly ignores them.
fn ptt_key_for(key: Key) -> Option<PttKey> {
    Some(match key {
        Key::ControlRight => PttKey::RightControl,
        Key::ControlLeft => PttKey::LeftControl,
        Key::AltGr => PttKey::RightAlt,
        Key::Alt => PttKey::LeftAlt,
        Key::ShiftRight => PttKey::RightShift,
        Key::ShiftLeft => PttKey::LeftShift,
        Key::MetaRight => PttKey::RightMeta,
        Key::MetaLeft => PttKey::LeftMeta,
        Key::F1 => PttKey::F1,
        Key::F2 => PttKey::F2,
        Key::F3 => PttKey::F3,
        Key::F4 => PttKey::F4,
        Key::F5 => PttKey::F5,
        Key::F6 => PttKey::F6,
        Key::F7 => PttKey::F7,
        Key::F8 => PttKey::F8,
        Key::F9 => PttKey::F9,
        Key::F10 => PttKey::F10,
        Key::F11 => PttKey::F11,
        Key::F12 => PttKey::F12,
        Key::CapsLock => PttKey::CapsLock,
        _ => return None,
    })
}

/// Spawn the rdev listener thread.
///
/// Returns once the thread is launched; the thread itself runs forever.
/// We do not return a `JoinHandle` because there is no orderly way to
/// stop `rdev::listen` — process exit reaps it. If the call to `listen`
/// fails (Wayland with no X11, denied permission on macOS where rdev
/// reports it, OS hook unavailable) the error is logged on the worker
/// thread and the thread terminates without affecting the rest of the
/// app. The toggle hotkey and button-driven dictation continue to work.
///
/// # Errors
///
/// Returns an error only if the configured PTT key fails to parse from
/// the environment. Runtime listener failures are logged from the worker
/// thread, not bubbled here, because by the time `rdev::listen` blocks we
/// have no caller to return to.
pub fn register_ptt_listener<R: Runtime>(
    app: &AppHandle<R>,
    combo: std::sync::Arc<std::sync::RwLock<PttCombo>>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    let _ = app; // touched only on the platform branches below
    match ptt_enablement(active.load(std::sync::atomic::Ordering::SeqCst)) {
        PttEnablement::Enabled => { /* fall through to register */ }
        PttEnablement::DisabledByEnv => {
            tracing::info!(
                "PTT listener skipped: {ENV_PTT_DISABLE}=1 set. Toggle hotkey and \
                 button-driven dictation continue to work."
            );
            return Ok(());
        }
        PttEnablement::DisabledMacosDefault => {
            // Default-disable on macOS, opt-in via env var.
            //
            // The macOS-26 hard-abort that originally drove the
            // default-off behaviour (rdev calling
            // `TISGetInputSourceProperty` from a non-main thread,
            // hitting `dispatch_assert_queue_fail` — see #69) is
            // fixed by pinning to fufesou/rdev's fork (the one
            // RustDesk ships in production), which attaches the
            // CGEventTap to `CFRunLoopGetMain()` so the callback
            // runs on the main thread and TSM is happy. Narsil's
            // upstream PR #147 was incomplete — it only fixed the
            // `send` path, not `listen`. See Cargo.toml comment.
            //
            // We *could* flip the default to on, but PTT is still
            // worth opting into deliberately: it triggers the Input
            // Monitoring permission prompt on first install, and a
            // dictation app silently asking to read every keystroke
            // is a privacy surprise some users won't want. Once
            // #70 ships a settings-window toggle, the gate moves
            // there.
            tracing::warn!(
                "PTT listener skipped on macOS by default. The macOS-26 abort that previously \
                 made this unsafe is fixed by pinning rdev to fufesou's fork (see Cargo.toml), \
                 so {ENV_PTT_ENABLE}=1 is now safe. PTT stays opt-in because enabling it \
                 triggers the Input Monitoring permission prompt — toggle hotkey and \
                 button-driven dictation work without it."
            );
            return Ok(());
        }
    }

    // The settings DB is the source of truth for the active combo —
    // `combo` was populated by `build_default` from settings, with
    // env/default fallbacks. The env var stays available as a last-
    // resort override for power users / CI: if `HUSH_PTT_HOTKEY` is
    // set, we override the in-memory combo at startup. Settings
    // edits via the IPC continue to win after that.
    if let Ok(raw) = std::env::var(ENV_PTT_HOTKEY) {
        match parse_ptt_combo(&raw) {
            Ok(env_combo) => {
                if let Ok(mut guard) = combo.write() {
                    *guard = env_combo;
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "{ENV_PTT_HOTKEY} value rejected; using settings/default combo"
                );
            }
        }
    }

    let initial_label = combo
        .read()
        .map(|c| c.to_storage_string())
        .unwrap_or_else(|_| "<poisoned>".to_owned());

    // Capture by clone-and-move into the listener thread. `AppHandle` is
    // `Clone + Send` and is intended to be cheap to clone (it's an Arc
    // internally), so this is the supported way to hand it across thread
    // boundaries. The clone outlives the original because the thread is
    // detached and lives for the rest of the process.
    let app_handle = app.clone();
    let combo_for_thread = std::sync::Arc::clone(&combo);
    let active_for_thread = std::sync::Arc::clone(&active);

    thread::Builder::new()
        .name("hush-ptt".into())
        .spawn(move || {
            tracing::info!(ptt_combo = %initial_label, "starting PTT rdev listener");

            // Per-thread matcher state — tracks held keys + active
            // bit. The closure is `FnMut` so we can carry mutable
            // state across calls. Combo itself is read through the
            // shared `RwLock` on every event so a Settings UI edit
            // takes effect on the next keystroke without bouncing
            // the rdev thread.
            let mut matcher = ComboMatcher::default();

            let result = listen(move |event: Event| {
                // Runtime gating. The Settings UI flips this flag
                // when the user toggles Enabled — events are still
                // delivered to us by the OS, but we drop them
                // silently when off. The matcher state stays
                // updated either way so toggling back on doesn't
                // need a clean held-set (rare edge: combo keys
                // physically held during a toggle just re-engage
                // on the next press).
                if !active_for_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let combo = match combo_for_thread.read() {
                    Ok(c) => c.clone(),
                    Err(_) => return,
                };
                match matcher.observe(&event, &combo) {
                    ComboTransition::Pressed => emit(&app_handle, EVENT_PTT_PRESS),
                    ComboTransition::Released => emit(&app_handle, EVENT_PTT_RELEASE),
                    ComboTransition::None => {}
                }
            });

            // Reaching this point means `listen` returned an error
            // (typically Wayland or a permission failure). Log and let the
            // thread exit; the rest of the app keeps working without PTT.
            if let Err(err) = result {
                tracing::error!(
                    error = ?err,
                    "rdev listener exited; push-to-talk will be unavailable. \
                     On macOS, grant Input Monitoring in System Settings → Privacy & Security. \
                     On Linux, ensure you are running under X11 (Wayland is not supported by rdev 0.5)."
                );
            }
        })
        .context("failed to spawn PTT listener thread")?;

    Ok(())
}

/// Resolved enablement state for the rdev PTT listener.
///
/// Three possibilities, computed once at startup:
/// - `Enabled` — register the listener.
/// - `DisabledByEnv` — `HUSH_PTT_DISABLE=1` set; user opted out.
/// - `DisabledMacosDefault` — macOS-only; opt out by default until #69
///   ships a TSM-free event-tap implementation. User can override via
///   `HUSH_PTT_ENABLE=1` if on older macOS where rdev's TSM call works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PttEnablement {
    Enabled,
    DisabledByEnv,
    DisabledMacosDefault,
}

/// Resolve PTT enablement from the environment + persisted state +
/// platform default.
///
/// Pure function: pulled out of `register_ptt_listener` so unit tests can
/// drive the decision without spawning a Tauri runtime or touching the
/// real environment. `persisted_active` is the in-memory mirror of the
/// settings DB value, which already incorporates the platform default
/// (`build_default` falls back to `!cfg!(target_os = "macos")` when the
/// DB row is absent).
fn resolve_enablement(
    disable: Option<&str>,
    enable: Option<&str>,
    persisted_active: bool,
    is_macos: bool,
) -> PttEnablement {
    let truthy = |v: Option<&str>| {
        matches!(
            v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
            Some("1") | Some("true") | Some("yes") | Some("on")
        )
    };
    if truthy(disable) {
        return PttEnablement::DisabledByEnv;
    }
    if truthy(enable) {
        return PttEnablement::Enabled;
    }
    if persisted_active {
        return PttEnablement::Enabled;
    }
    if is_macos {
        // macOS without env enable AND without persisted-true: stay
        // off so the Input Monitoring permission prompt doesn't fire
        // on first launch.
        return PttEnablement::DisabledMacosDefault;
    }
    PttEnablement::Enabled
}

/// Production wrapper around [`resolve_enablement`] that reads the real
/// environment + the build-time `cfg(target_os)`.
fn ptt_enablement(persisted_active: bool) -> PttEnablement {
    let disable = std::env::var(ENV_PTT_DISABLE).ok();
    let enable = std::env::var(ENV_PTT_ENABLE).ok();
    resolve_enablement(
        disable.as_deref(),
        enable.as_deref(),
        persisted_active,
        cfg!(target_os = "macos"),
    )
}

/// Emit a Tauri event, swallowing failures with a warning. Same posture
/// as the toggle hotkey: if the frontend window is gone, the press is a
/// no-op; we don't want listener errors to kill the rdev thread.
fn emit<R: Runtime>(app: &AppHandle<R>, name: &'static str) {
    if let Err(e) = app.emit(name, ()) {
        tracing::warn!(error = ?e, event = name, "failed to emit PTT event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn mk_event(event_type: EventType) -> Event {
        Event {
            time: SystemTime::now(),
            unicode: None,
            event_type,
            platform_code: 0,
            position_code: 0,
            usb_hid: 0,
            extra_data: 0,
        }
    }
    fn mk_press(key: Key) -> Event {
        mk_event(EventType::KeyPress(key))
    }
    fn mk_release(key: Key) -> Event {
        mk_event(EventType::KeyRelease(key))
    }

    #[test]
    fn combo_single_key_press_release_round_trip() {
        let combo = PttCombo::single(PttKey::RightMeta);
        let mut m = ComboMatcher::default();
        assert_eq!(
            m.observe(&mk_press(Key::MetaRight), &combo),
            ComboTransition::Pressed
        );
        // Repeat KeyPress events shouldn't double-fire (active stays true).
        assert_eq!(
            m.observe(&mk_press(Key::MetaRight), &combo),
            ComboTransition::None
        );
        assert_eq!(
            m.observe(&mk_release(Key::MetaRight), &combo),
            ComboTransition::Released
        );
    }

    #[test]
    fn combo_two_key_requires_both_held() {
        let combo = PttCombo::try_from_keys([PttKey::RightMeta, PttKey::RightShift]).unwrap();
        let mut m = ComboMatcher::default();
        // First key alone — not active yet.
        assert_eq!(
            m.observe(&mk_press(Key::MetaRight), &combo),
            ComboTransition::None
        );
        // Second key — both held now, fires Pressed.
        assert_eq!(
            m.observe(&mk_press(Key::ShiftRight), &combo),
            ComboTransition::Pressed
        );
        // Release one — fires Released even though the other is still held.
        assert_eq!(
            m.observe(&mk_release(Key::ShiftRight), &combo),
            ComboTransition::Released
        );
        // Release the other — no-op (already inactive).
        assert_eq!(
            m.observe(&mk_release(Key::MetaRight), &combo),
            ComboTransition::None
        );
    }

    #[test]
    fn combo_ignores_keys_outside_curated_set() {
        let combo = PttCombo::single(PttKey::RightMeta);
        let mut m = ComboMatcher::default();
        // A letter key event is delivered by rdev but isn't in our
        // curated PttKey set — must not change matcher state.
        assert_eq!(
            m.observe(&mk_press(Key::KeyA), &combo),
            ComboTransition::None
        );
        assert_eq!(
            m.observe(&mk_press(Key::MetaRight), &combo),
            ComboTransition::Pressed
        );
    }

    #[test]
    fn combo_storage_round_trip() {
        let combo = PttCombo::try_from_keys([PttKey::RightMeta, PttKey::F1]).unwrap();
        let s = combo.to_storage_string();
        let parsed = parse_ptt_combo(&s).unwrap();
        assert_eq!(parsed, combo);
    }

    #[test]
    fn parse_combo_accepts_single_key_for_back_compat() {
        let parsed = parse_ptt_combo("RightControl").unwrap();
        assert_eq!(parsed, PttCombo::single(PttKey::RightControl));
    }

    #[test]
    fn parse_combo_rejects_empty() {
        assert!(parse_ptt_combo("").is_err());
        assert!(parse_ptt_combo("+++").is_err());
    }

    #[test]
    fn combo_dedups_and_sorts() {
        let a = PttCombo::try_from_keys([
            PttKey::RightShift,
            PttKey::RightMeta,
            PttKey::RightShift, // duplicate
        ])
        .unwrap();
        let b = PttCombo::try_from_keys([PttKey::RightMeta, PttKey::RightShift]).unwrap();
        assert_eq!(a, b, "canonical form makes equivalent combos compare equal");
    }

    #[test]
    fn parses_canonical_names() {
        assert_eq!(parse_ptt_key("RightControl").unwrap(), PttKey::RightControl);
        assert_eq!(parse_ptt_key("F12").unwrap(), PttKey::F12);
        assert_eq!(parse_ptt_key("CapsLock").unwrap(), PttKey::CapsLock);
    }

    #[test]
    fn parser_is_case_insensitive() {
        assert_eq!(parse_ptt_key("rightcontrol").unwrap(), PttKey::RightControl);
        assert_eq!(parse_ptt_key("RIGHTCONTROL").unwrap(), PttKey::RightControl);
        assert_eq!(
            parse_ptt_key("Right_Control").unwrap(),
            PttKey::RightControl
        );
        assert_eq!(
            parse_ptt_key("right-control").unwrap(),
            PttKey::RightControl
        );
    }

    #[test]
    fn parser_accepts_aliases() {
        // Common aliases users will reach for.
        assert_eq!(parse_ptt_key("RCtrl").unwrap(), PttKey::RightControl);
        assert_eq!(parse_ptt_key("AltGr").unwrap(), PttKey::RightAlt);
        assert_eq!(parse_ptt_key("Cmd").unwrap(), PttKey::LeftMeta);
        assert_eq!(parse_ptt_key("Super").unwrap(), PttKey::LeftMeta);
        assert_eq!(parse_ptt_key("Win").unwrap(), PttKey::LeftMeta);
        assert_eq!(parse_ptt_key("Option").unwrap(), PttKey::LeftAlt);
    }

    #[test]
    fn parser_rejects_unsupported_keys() {
        // Letter keys are intentionally not accepted — see `PttKey` doc.
        let err = parse_ptt_key("a").expect_err("letter keys must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.to_lowercase().contains("unrecognised"), "got: {msg}");

        let err = parse_ptt_key("space").expect_err("Space must be rejected");
        assert!(format!("{err:#}").to_lowercase().contains("unrecognised"));
    }

    #[test]
    fn resolve_falls_back_to_default_when_env_unset() {
        assert_eq!(resolve_ptt_key(None).unwrap(), DEFAULT_PTT_KEY);
    }

    #[test]
    fn resolve_uses_env_override_when_set() {
        assert_eq!(resolve_ptt_key(Some("F9")).unwrap(), PttKey::F9);
    }

    #[test]
    fn resolve_wraps_parse_error_with_env_var_name() {
        let err = resolve_ptt_key(Some("not-a-key")).expect_err("garbage must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("HUSH_PTT_HOTKEY"),
            "error should name the env var; got: {msg}"
        );
    }

    #[test]
    fn matches_correctly_distinguishes_left_and_right_modifiers() {
        // Left vs right modifiers are distinct on rdev — the whole point
        // of this trait is to keep them so. A regression here would mean
        // "RightControl" silently triggers on left-control too.
        assert!(PttKey::RightControl.matches(Key::ControlRight));
        assert!(!PttKey::RightControl.matches(Key::ControlLeft));
        assert!(PttKey::LeftControl.matches(Key::ControlLeft));
        assert!(!PttKey::LeftControl.matches(Key::ControlRight));
    }

    #[test]
    fn matches_ignores_unrelated_keys() {
        assert!(!PttKey::F12.matches(Key::F11));
        assert!(!PttKey::F12.matches(Key::Space));
        assert!(!PttKey::CapsLock.matches(Key::ShiftLeft));
    }

    // -- Enablement resolution -------------------------------------------
    //
    // Pinning the disable/enable matrix because regressing this is how
    // users get a hard crash on macOS 26+ — the assertions trap at the
    // OS level (dispatch_assert_queue_fail), not as a Rust panic, so
    // catch_unwind can't save us. The defence is to never spawn the
    // rdev listener on macOS by default. See #69 for the underlying bug.

    #[test]
    fn enablement_macos_disabled_by_default() {
        // No env, no persisted-active, on macOS → off (privacy
        // default — don't trigger Input Monitoring on first launch).
        assert_eq!(
            resolve_enablement(None, None, false, true),
            PttEnablement::DisabledMacosDefault
        );
    }

    #[test]
    fn enablement_macos_opt_in_via_env() {
        assert_eq!(
            resolve_enablement(None, Some("1"), false, true),
            PttEnablement::Enabled
        );
        assert_eq!(
            resolve_enablement(None, Some("true"), false, true),
            PttEnablement::Enabled
        );
    }

    #[test]
    fn enablement_macos_opt_in_via_persisted_state() {
        // Settings UI flipped enabled to true; should boot the
        // listener even without HUSH_PTT_ENABLE.
        assert_eq!(
            resolve_enablement(None, None, true, true),
            PttEnablement::Enabled
        );
    }

    #[test]
    fn enablement_disable_wins_over_enable_and_persisted() {
        // If the user sets HUSH_PTT_DISABLE=1, it overrides every
        // other signal — surprise-prevention when the user is
        // trying to stop a misbehaving listener.
        assert_eq!(
            resolve_enablement(Some("1"), Some("1"), true, true),
            PttEnablement::DisabledByEnv
        );
        assert_eq!(
            resolve_enablement(Some("1"), None, true, false),
            PttEnablement::DisabledByEnv
        );
    }

    #[test]
    fn enablement_non_macos_enabled_by_default() {
        assert_eq!(
            resolve_enablement(None, None, false, false),
            PttEnablement::Enabled
        );
    }

    #[test]
    fn enablement_non_macos_disable_via_env() {
        assert_eq!(
            resolve_enablement(Some("1"), None, false, false),
            PttEnablement::DisabledByEnv
        );
    }

    #[test]
    fn enablement_truthy_values_are_normalised() {
        // Be forgiving about HUSH_PTT_ENABLE=YES vs =yes vs ="1 " etc.
        assert_eq!(
            resolve_enablement(None, Some("YES"), false, true),
            PttEnablement::Enabled
        );
        assert_eq!(
            resolve_enablement(None, Some(" on "), false, true),
            PttEnablement::Enabled
        );
        // Anything else stays disabled (and we fall through to the
        // persisted/default check, which is also off here).
        assert_eq!(
            resolve_enablement(None, Some("0"), false, true),
            PttEnablement::DisabledMacosDefault
        );
        assert_eq!(
            resolve_enablement(None, Some(""), false, true),
            PttEnablement::DisabledMacosDefault
        );
    }
}
