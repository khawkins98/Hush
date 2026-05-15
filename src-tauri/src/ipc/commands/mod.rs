//! Shared IPC types and command-module map.
//!
//! The larger command groups live in sibling modules (`dictation`,
//! `history`, `meeting`, `models`, …). This file keeps the cross-
//! cutting IPC wire types and helpers those modules share:
//! [`DictationResult`], [`IpcError`], [`poisoned`], and the
//! permission-error classifier reused by dictation + meeting paths.
//!
//! ## Command grouping
//!
//! As the surface has grown past a dozen commands, a quick map for
//! contributors landing here cold:
//!
//! - **Core dictation pipeline.** Commands live in
//!   `commands/dictation.rs` — source listing plus the single-shot
//!   `start_dictation` / `stop_dictation` lifecycle.
//! - **History (read-only browse + delete).** [`history_list`],
//!   [`history_search`], [`history_delete`], [`history_count`].
//! - **Replacements (post-transcription find/replace CRUD).**
//!   [`replacements_list`], [`replacement_create`],
//!   [`replacement_update`], [`replacement_delete`].
//! - **Vocabulary (Whisper prompt-bias CRUD).**
//!   [`vocabulary_list`], [`vocabulary_create`],
//!   [`vocabulary_update`], [`vocabulary_delete`].
//! - **Model picker.** [`model_list`], [`model_select`].
//! - **Meeting Mode (refs #33 / #109).** Commands live in
//!   `commands/meeting.rs`. Sessions are populated by the
//!   `SessionManager` chunking pump (`meeting::manager::run_pump`);
//!   the panel renders an empty state when no sessions exist yet.

// Meeting Mode commands (refs #33 / #109) live in their own
// submodule — extracted under #82 to give the largest cohesive
// command group its own seam. `lib.rs` references them via their
// full path (e.g. `ipc::commands::meeting::meeting_start_manual`)
// because Tauri's `generate_handler!` is path-sensitive: it generates
// a hidden `__cmd__<name>` symbol as a sibling of each command, and
// `pub use` re-exports do not carry that symbol with them. See the
// 2026-04-25 entry in `learnings.md`.
pub mod debug;
pub mod diarizer;
pub mod dictation;
pub mod dictionary;
pub mod export;
pub mod history;
pub mod meeting;
pub mod models;
pub mod permissions;
pub mod ptt;
pub mod settings;
pub mod system;
pub mod updater;

use std::sync::PoisonError;

use serde::Serialize;

use super::ForegroundApp;

/// What the frontend gets back from `stop_dictation`.
///
/// `text` is what was written to the clipboard (after vocabulary-prompt
/// biasing during inference, whisper bracket-sentinel stripping, and
/// post-transcription replacement rules). When whisper produces only
/// silence-marker output (`[BLANK_AUDIO]`, `[NOISE]`, `[MUSIC]` —
/// see [`strip_whisper_brackets`]), `text` is empty so the frontend
/// can render a friendly "no audio detected" rather than the raw
/// sentinel.
/// `foreground` is the app + window title captured *at start* of the
/// recording — not at stop, because by stop time the user has alt-tabbed
/// back to Hush and "current foreground" would always be us. The backend
/// already inserts a history row with this metadata via the
/// fire-and-forget `spawn_history_create` helper in `stop_dictation`, so
/// the frontend doesn't need to round-trip it back through `history_*`.
/// `duration_ms` is the wall-clock length of the audio that was
/// captured — surfaces in the result block so the user sees "Recorded
/// for 4.2s" regardless of whether transcription found anything.
/// `None` only when the format was malformed (impossible in practice,
/// but `checked_div` returns Option for the zero-format case).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationResult {
    pub text: String,
    pub foreground: Option<ForegroundApp>,
    pub duration_ms: Option<i64>,
}

/// Errors returned across the IPC boundary.
///
/// Tauri serialises whatever the command returns; we use a tagged enum so
/// the frontend can switch on `kind` for user-facing copy and recovery
/// hints without parsing free-form `Display` strings.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "kebab-case")]
pub enum IpcError {
    #[error("audio: {0}")]
    Audio(String),

    #[error("transcription: {0}")]
    Transcription(String),

    /// Surfaced when no transcription backend is configured at the time
    /// of `stop_dictation`. Either the user hasn't picked a downloaded
    /// model yet (the model picker is shipped — first-run users see a
    /// banner pointing them at it; the `Start recording` button is
    /// disabled in that state) or the binary was built without the
    /// `whisper` Cargo feature (UI-only contributors using
    /// `npm run tauri:ui-only`). The frontend's recovery copy points at
    /// the in-app picker and the legacy `HUSH_MODEL_PATH` env-var path
    /// is no longer surfaced to end users.
    #[error("no transcription model loaded (pick one in the model picker, or rebuild with the whisper feature)")]
    TranscriptionUnavailable,

    #[error("clipboard: {0}")]
    Clipboard(String),

    /// Settings repository (SQLite) error or the picker resolved a
    /// model id we don't know about. Surfaced separately because the
    /// frontend recovery copy is "pick a model from the catalog"
    /// rather than the dictionary-shaped "your settings" framing.
    #[error("settings: {0}")]
    Settings(String),

    /// History repository (SQLite) error — failed insert, list, search,
    /// or delete. Surfaced separately from `Internal` so the frontend
    /// can offer history-specific recovery copy ("History list failed,
    /// try again") rather than the generic "restart Hush".
    #[error("history: {0}")]
    History(String),

    /// Replacements repository (SQLite) error — failed CRUD on the
    /// dictionary's replacements table. Same rationale as `History`:
    /// a kebab-case kind (`replacements`) so the frontend can switch on
    /// it for tailored recovery copy.
    #[error("replacements: {0}")]
    Replacements(String),

    /// Meeting-session repository (SQLite) error — failed CRUD on
    /// `meeting_sessions` / `utterances` / `meeting_app_overrides`.
    /// Surfaced separately from `Settings` so the frontend's panel
    /// can switch on `meeting-sessions` for tailored recovery copy.
    /// Reachable through the lifecycle commands (start_manual /
    /// stop_manual / session_get / etc.) and the override CRUD.
    #[error("meeting-sessions: {0}")]
    MeetingSessions(String),

    /// Permission-shaped failure surfaced from a deeper error chain
    /// (typically SCK / TCC / AVFoundation rejections wrapped through
    /// `meeting_start_manual` or the dictation start path). Payload
    /// is the permission name in kebab-case: `"screen-recording"`,
    /// `"microphone"`, or `"input-monitoring"`. Pre-#386 these were
    /// emitted as `MeetingSessions(message)` and the frontend
    /// substring-matched against the wrapped chain to detect them
    /// — fragile, since any future error mentioning "screen
    /// recording" in unrelated context would trigger the
    /// permissions-dialog launch heuristic. Classifying once at the
    /// IPC boundary lets the frontend match on `kind` instead of
    /// scraping copy.
    #[error("permission-denied: {0}")]
    PermissionDenied(String),

    /// The audio input device the user picked has disconnected
    /// mid-session (USB unplugged, AirPods walked out of range, webcam
    /// disabled). Surfaced as a distinct variant from `Audio(String)`
    /// so the frontend can render a clear "microphone disconnected"
    /// message and (in PR 2 of #587) drive an auto-fallback offer
    /// without substring-matching the inner error chain. The inner
    /// `String` is the same device name the user saw in the source
    /// picker — captured at session start because it's no longer
    /// reachable via `cpal::Device::name()` once the device is gone
    /// (#587). Tuple-variant shape matches the other IPC errors so
    /// the wire format stays `{ kind: "audio-device-lost", message:
    /// "MacBook Microphone" }`.
    #[error("audio-device-lost: {0}")]
    AudioDeviceLost(String),

    /// Auto-update is wired in code but the runtime support isn't
    /// active in this build — typically because the maintainer
    /// hasn't completed Steps 1–4 of the #10 plan (signing keypair,
    /// `tauri.conf.json` `plugins.updater` block, CI secrets, and
    /// plugin registration). Surfaced as a typed variant so the
    /// frontend's About-tab install flow can `kind`-match on it
    /// (showing the manual-install fallback) instead of substring-
    /// matching a free-form `Internal` message — same rationale as
    /// `PermissionDenied`'s carve-out from `MeetingSessions` (#386).
    #[error("updater-unavailable")]
    UpdaterUnavailable,

    /// `stop_dictation` was called while a meeting session is active.
    /// The meeting pump owns the audio backend for the duration of the
    /// session; dictation's stop path must not tear it down (#880).
    #[error("meeting-session-active")]
    MeetingSessionActive,

    /// In-process state guard panicked while a lock was held. Should not
    /// happen in practice — only the IPC commands lock our internal
    /// mutexes and they don't panic — but a poisoned lock surfacing here
    /// is preferable to a `panic!` in a Tauri command, which can
    /// destabilise the renderer process.
    #[error("internal: {0}")]
    Internal(String),
}

pub(crate) type IpcResult<T> = std::result::Result<T, IpcError>;

/// Convert a `PoisonError` into an `IpcError::Internal` so callers can use
/// the `?` operator instead of `.expect("…mutex")`. Centralised so the
/// message string is consistent across call sites.
pub(super) fn poisoned<T>(_: PoisonError<T>) -> IpcError {
    IpcError::Internal("internal state lock poisoned".to_owned())
}

/// Validate a user-chosen export path returned by the dialog plugin.
/// Rejects paths with `..` components, which could escape the intended
/// location regardless of surrounding directory. The dialog plugin
/// already anchors paths to user-accessible locations; this guard
/// enforces that a compromised frontend cannot use `..` to write
/// somewhere unexpected.
/// Validate a user-chosen export path returned by the dialog plugin.
///
/// Guards applied, in order:
/// - Empty path rejected.
/// - Relative paths rejected — a dialog-supplied path is always absolute;
///   a relative path implies the renderer constructed it manually, which is
///   enough reason to reject it (#883).
/// - Paths with `..` components rejected — prevents traversal past any
///   anchor the OS dialog would otherwise enforce (#883).
pub(super) fn validate_export_path(path: &str) -> IpcResult<()> {
    use std::path::{Component, Path};
    if path.is_empty() {
        return Err(IpcError::Internal("export path is empty".into()));
    }
    let p = Path::new(path);
    if p.is_relative() {
        return Err(IpcError::Internal(format!(
            "relative export path rejected: {path:?}"
        )));
    }
    if p.components().any(|c| c == Component::ParentDir) {
        return Err(IpcError::Internal(format!(
            "unsafe export path rejected: {path:?}"
        )));
    }
    Ok(())
}

/// Inspect an error chain and, if it looks permission-shaped,
/// return the permission name (`"microphone"` or `"input-monitoring"`)
/// so a caller can promote it to [`IpcError::PermissionDenied`] (#386).
/// Uses the same substring patterns the frontend's pre-typed-variant
/// heuristic used — just runs once at the IPC boundary instead of
/// leaking the detection into UI code.
///
/// Patterns:
/// - AVFoundation mic refusals land with `"microphone"` plus
///   `"not authorized"`.
/// - rdev / IOKit Input Monitoring rejections include
///   `"input monitoring"` verbatim.
///
/// Returns `None` for any error chain that doesn't match,
/// preserving the existing wrap-as-`MeetingSessions(...)` behaviour
/// for the unrecognised case.
pub(crate) fn classify_permission_error(err: &anyhow::Error) -> Option<&'static str> {
    let chain = format!("{err:#}").to_lowercase();
    if chain.contains("microphone") && chain.contains("not authorized") {
        return Some("microphone");
    }
    if chain.contains("input monitoring") {
        return Some("input-monitoring");
    }
    None
}

// Core dictation commands (`audio_list_sources`, `start_dictation`,
// `stop_dictation`) plus their helper functions live in
// `crate::ipc::commands::dictation` — extracted under #541 to give the
// most-edited pipeline its own seam.

// History-browse commands (history_list, history_search,
// history_export_row_csv, history_delete, history_count,
// history_clear, get_dictation_stats) live in
// `crate::ipc::commands::history` — extracted under #431.

// Vocabulary + replacement-rule CRUD commands live in
// `crate::ipc::commands::dictionary` — extracted under #431. The
// pure-logic [`apply_replacements`] still lives in `crate::dictionary`
// and is consumed by `dictation::stop_dictation`; only the thin IPC
// handlers moved.
//
// Model-picker commands (catalog / select / download / cancel /
// remove + types + download events) live in
// `crate::ipc::commands::models` — extracted under #82.
//
// Meeting Mode commands live in `crate::ipc::commands::meeting` —
// also extracted under #82.

// -- First-run / onboarding ----------------------------------------------
//
// Two thin commands wrapping the existing `SettingsRepository` for the
// macOS first-run welcome modal. Only macOS frontends consult these —
// the welcome flow is gated by `cfg!(target_os = "macos")` on the
// frontend's onMount path. Backend-side the commands are
// platform-independent because the settings table doesn't care which
// OS is reading it.
//
// The macOS-specific framing for the modal is documented in
// `learnings.md`: rdev's `listen` triggers the Input Monitoring
// prompt at app startup with no programmatic detection of grant
// state, and cpal triggers the Microphone prompt the first time
// recording starts. The welcome flow educates the user on what just
// happened (or what will happen on first record) and points them at
// System Settings if they declined.

// First-run flag commands (get_first_run_completed,
// mark_first_run_completed, reset_first_run) live in
// `crate::ipc::commands::system` — extracted under #431.

// HUD / sound-cues / diarization / inference-threads / meeting-
// autostart-mode get/set commands live in
// `crate::ipc::commands::settings` — extracted under #431.

// Diarizer model commands (DiarizeModelStatus,
// get_diarizer_model_status, remove_diarizer_model,
// download_diarizer_model, download_diarizer_model_inner,
// swap_diarizer_after_download) live in
// `crate::ipc::commands::diarizer` — extracted under #431.

/// TTL for the [`check_for_updates`] cache (#333). 15 minutes is
/// well below GitHub's 60-req/h unauthenticated rate-limit window
/// (so a single user under heavy clicking can't self-DoS) and well
/// above the spam-click threshold (so back-to-back clicks return
/// instantly). The window is also short enough that a user who
/// just installed an update sees the new "up to date" copy without
/// quitting the app.
pub const UPDATE_CHECK_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

// AutostartPathStatus, get_autostart_path_status,
// retry_autostart_registration, check_for_updates,
// check_for_updates_inner all live in
// `crate::ipc::commands::system` — extracted under #431.

// PttConfig + ptt_get_config + ptt_set_config live in
// `crate::ipc::commands::ptt` — extracted under #431.

// Permission-related commands (privacy-pane open / diagnose /
// reset) live in `crate::ipc::commands::permissions` —
// extracted under #82, renamed from `macos` under #597 in
// preparation for cross-platform permission impls (#106 / #107).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_error_serialises_with_tag_and_message() {
        let json = serde_json::to_string(&IpcError::Audio("device gone".into())).unwrap();
        assert!(json.contains("\"kind\":\"audio\""), "got: {json}");
        assert!(json.contains("\"message\":\"device gone\""), "got: {json}");
    }

    // sanitise_meeting_sources tests live in `meeting.rs`'s own
    // `mod tests` block alongside the function they exercise.

    #[test]
    fn ipc_error_unavailable_has_no_message_field() {
        // The unit variant has no payload, so the `content = "message"`
        // attribute should produce just the tag with no `message` key.
        let json = serde_json::to_string(&IpcError::TranscriptionUnavailable).unwrap();
        assert!(
            json.contains("\"kind\":\"transcription-unavailable\""),
            "got: {json}"
        );
        assert!(!json.contains("\"message\""), "got: {json}");
    }

    #[test]
    fn ipc_error_internal_serialises_with_kebab_case_kind() {
        // The `Internal` variant exists specifically so a poisoned
        // mutex does not panic the Tauri command. Confirm it round-
        // trips through serde with the same shape as the other
        // payload-bearing variants — the frontend's switch-on-kind
        // dispatch depends on this.
        let json = serde_json::to_string(&IpcError::Internal("locked".into())).unwrap();
        assert!(json.contains("\"kind\":\"internal\""), "got: {json}");
        assert!(json.contains("\"message\":\"locked\""), "got: {json}");
    }

    // -- classify_permission_error (#386) --------------------------------

    #[test]
    fn classify_permission_microphone_requires_both_terms() {
        // The mic classifier needs *both* "microphone" and
        // "not authorized" so a generic "microphone level low"
        // log message doesn't trigger the dialog.
        let positive = anyhow::anyhow!("microphone access not authorized");
        assert_eq!(classify_permission_error(&positive), Some("microphone"));
        let negative = anyhow::anyhow!("microphone level too low");
        assert_eq!(classify_permission_error(&negative), None);
    }

    #[test]
    fn classify_permission_input_monitoring() {
        let err = anyhow::anyhow!("Input Monitoring permission denied");
        assert_eq!(classify_permission_error(&err), Some("input-monitoring"));
    }

    #[test]
    fn classify_permission_returns_none_for_unrelated_chain() {
        // The substring patterns are intentionally narrow: a
        // generic "audio device gone" failure should fall through
        // to the existing wrap path, not get re-classified as a
        // permission issue.
        let err = anyhow::anyhow!("audio device disconnected mid-stream");
        assert_eq!(classify_permission_error(&err), None);
    }

    #[test]
    fn ipc_error_permission_denied_serde_round_trip() {
        // Wire shape pinned for the frontend's discriminant
        // check: `kind: "permission-denied", message: "<perm>"`.
        let json =
            serde_json::to_string(&IpcError::PermissionDenied("screen-recording".into())).unwrap();
        assert!(
            json.contains("\"kind\":\"permission-denied\""),
            "got: {json}"
        );
        assert!(
            json.contains("\"message\":\"screen-recording\""),
            "got: {json}"
        );
    }

    // Dictation-focused tests live in `dictation.rs` alongside the
    // helpers they exercise (issue #541).

    // ---- HUD-enabled IPC commands ----------------------------------------

    #[tokio::test]
    async fn set_hud_enabled_persists_false_to_settings_and_atomic() {
        let state = crate::ipc::tests::mock_state();
        // Default at construction is `true`; flip to false and verify
        // both the in-memory atomic and the persisted settings row.
        state
            .runtime_flags
            .hud_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);

        super::settings::set_hud_enabled_inner(&state, false)
            .await
            .expect("set_hud_enabled_inner ok");

        assert!(
            !state
                .runtime_flags
                .hud_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "atomic should reflect the new false value"
        );
        let persisted = state
            .settings
            .get(crate::settings::keys::HUD_ENABLED)
            .await
            .expect("settings get ok");
        assert_eq!(
            persisted.as_deref(),
            Some("false"),
            "persisted row should match the literal serde encoding"
        );
    }

    #[tokio::test]
    async fn set_hud_enabled_persists_true_after_a_round_trip() {
        // Round-trip false → true so we cover the both-directions
        // path. A single-direction test would miss a regression
        // where `set_hud_enabled` only ever wrote "false".
        let state = crate::ipc::tests::mock_state();

        super::settings::set_hud_enabled_inner(&state, false)
            .await
            .expect("set false ok");
        super::settings::set_hud_enabled_inner(&state, true)
            .await
            .expect("set true ok");

        assert!(state
            .runtime_flags
            .hud_enabled
            .load(std::sync::atomic::Ordering::Relaxed));
        let persisted = state
            .settings
            .get(crate::settings::keys::HUD_ENABLED)
            .await
            .expect("settings get ok");
        assert_eq!(persisted.as_deref(), Some("true"));
    }

    // ---- Inference-threads IPC commands ---------------------------------

    #[tokio::test]
    async fn set_inference_threads_persists_value_within_bounds() {
        let state = crate::ipc::tests::mock_state();
        super::settings::set_inference_threads_inner(&state, 8)
            .await
            .expect("set ok");
        assert_eq!(
            state
                .runtime_flags
                .inference_threads
                .load(std::sync::atomic::Ordering::Relaxed),
            8,
            "atomic should hold the requested thread count"
        );
        let persisted = state
            .settings
            .get(crate::settings::keys::INFERENCE_THREADS)
            .await
            .expect("settings get ok");
        assert_eq!(persisted.as_deref(), Some("8"));
    }

    #[tokio::test]
    async fn set_inference_threads_clamps_above_max() {
        // Anyone hand-editing the settings row could push past the
        // upper bound; the inner setter must clamp so a malformed
        // value can't reach `set_n_threads`.
        let state = crate::ipc::tests::mock_state();
        super::settings::set_inference_threads_inner(&state, 999)
            .await
            .expect("set ok");
        assert_eq!(
            state
                .runtime_flags
                .inference_threads
                .load(std::sync::atomic::Ordering::Relaxed),
            16
        );
        let persisted = state
            .settings
            .get(crate::settings::keys::INFERENCE_THREADS)
            .await
            .expect("settings get ok");
        assert_eq!(persisted.as_deref(), Some("16"));
    }

    #[tokio::test]
    async fn set_inference_threads_clamps_below_min() {
        let state = crate::ipc::tests::mock_state();
        super::settings::set_inference_threads_inner(&state, 0)
            .await
            .expect("set ok");
        assert_eq!(
            state
                .runtime_flags
                .inference_threads
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn set_diarization_enabled_round_trips_through_atomic_and_settings() {
        // Foundation PR (#111). Default at construction is false; flip
        // on, verify both the atomic + persisted row, then flip off and
        // verify both directions land. A single-direction test would
        // miss a regression where the writer only ever stored one value.
        let state = crate::ipc::tests::mock_state();
        assert!(
            !state
                .runtime_flags
                .diarization_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "default should be off"
        );

        super::settings::set_diarization_enabled_inner(&state, true)
            .await
            .expect("set true ok");
        assert!(
            state
                .runtime_flags
                .diarization_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "atomic should reflect true"
        );
        assert_eq!(
            state
                .settings
                .get(crate::settings::keys::DIARIZATION_ENABLED)
                .await
                .expect("settings get ok")
                .as_deref(),
            Some("true"),
        );

        super::settings::set_diarization_enabled_inner(&state, false)
            .await
            .expect("set false ok");
        assert!(
            !state
                .runtime_flags
                .diarization_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "atomic should reflect false"
        );
        assert_eq!(
            state
                .settings
                .get(crate::settings::keys::DIARIZATION_ENABLED)
                .await
                .expect("settings get ok")
                .as_deref(),
            Some("false"),
        );
    }

    // ---- check_for_updates cache (#333) --------------------------------

    #[tokio::test]
    async fn check_for_updates_returns_cached_result_within_ttl() {
        // Seed the cache with a fixed UpToDate result, then call
        // check_for_updates_inner with a `now` that's just inside the
        // TTL window. The inner must short-circuit and return the
        // seeded value without touching the network — wiremock isn't
        // running, so any HTTP call would fail loudly.
        let state = crate::ipc::tests::mock_state();
        let seeded = crate::updater::UpdateCheckResult::UpToDate {
            current: "0.2.0".to_string(),
        };
        let seed_at = std::time::Instant::now();
        *state.last_update_check.lock().unwrap() = Some((seed_at, seeded.clone()));

        // Just inside the TTL → cache hit.
        let still_within = seed_at + UPDATE_CHECK_TTL - std::time::Duration::from_secs(1);
        let result = super::system::check_for_updates_inner(&state, still_within)
            .await
            .expect("cache hit ok");
        assert_eq!(result, seeded);
    }

    #[tokio::test]
    async fn check_for_updates_bypasses_cache_after_ttl() {
        // Past the TTL the inner has to fall through to the network
        // path. Without a wiremock server running the call fails —
        // we don't care about the kind, only that the cache layer
        // is no longer short-circuiting. A successful "fresh" path
        // is exercised by the wiremock tests in `updater::tests`.
        let state = crate::ipc::tests::mock_state();
        let seeded = crate::updater::UpdateCheckResult::UpToDate {
            current: "0.2.0".to_string(),
        };
        let seed_at = std::time::Instant::now();
        *state.last_update_check.lock().unwrap() = Some((seed_at, seeded));

        // Past the TTL → cache miss → network call. The runner may
        // or may not have network access; if it does the real GitHub
        // API may return any valid result. The only thing we must
        // NOT see is the stale seeded value (version "0.2.0") —
        // that would mean the cache was not bypassed.
        let past_ttl = seed_at + UPDATE_CHECK_TTL + std::time::Duration::from_secs(1);
        let result = super::system::check_for_updates_inner(&state, past_ttl).await;
        match result {
            Ok(crate::updater::UpdateCheckResult::UpToDate { ref current })
                if current == "0.2.0" =>
            {
                panic!("cache was not bypassed — got the stale seeded value back")
            }
            _ => {
                // Any other outcome (CheckFailed, UpdateAvailable,
                // UpToDate with a real version, or Err) means the
                // network path was reached as required.
            }
        }
    }

    #[tokio::test]
    async fn check_for_updates_with_no_cache_calls_through() {
        // Empty cache → no short-circuit. Same shape as the
        // post-TTL test, just confirming the None path also falls
        // through. The network call may succeed or fail depending
        // on runner connectivity — both are valid outcomes; we only
        // care that the code path is reached (not stuck behind a
        // non-existent cache entry).
        let state = crate::ipc::tests::mock_state();
        assert!(state.last_update_check.lock().unwrap().is_none());
        let _result =
            super::system::check_for_updates_inner(&state, std::time::Instant::now()).await;
        // Any Ok or Err result confirms the path was exercised.
    }

    // History CSV export (#357 phase 3a) tests live in
    // `crate::ipc::commands::history` — moved alongside the helper
    // under #431.
    // Diarizer tests live in `crate::ipc::commands::diarizer` (#711).
}
