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
pub mod dictation;
pub mod diarizer;
pub mod dictionary;
pub mod export;
pub mod history;
pub mod macos;
pub mod meeting;
pub mod models;
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

/// Inspect an error chain and, if it looks permission-shaped,
/// return the permission name (`"screen-recording"`, `"microphone"`,
/// or `"input-monitoring"`) so a caller can promote it to
/// [`IpcError::PermissionDenied`] (#386). Uses the same substring
/// patterns the frontend's pre-typed-variant heuristic used —
/// just runs once at the IPC boundary instead of leaking the
/// detection into UI code.
///
/// Patterns:
/// - SCK / system-audio failures land with `"screen recording"` or
///   `"declined tccs"` somewhere in the anyhow chain.
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
    if chain.contains("screen recording") || chain.contains("declined tccs") {
        return Some("screen-recording");
    }
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

// macOS-only commands (privacy-pane open / diagnose /
// reset) live in `crate::ipc::commands::macos` —
// extracted under #82.


#[cfg(test)]
mod tests {
    use crate::ipc::AppState;

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
    fn classify_permission_screen_recording_chains() {
        // SCK / system-audio failures wrap "screen recording" in
        // their anyhow chain (the user-visible TCC string Apple
        // surfaces on rejection).
        let err = anyhow::anyhow!("ScreenCaptureKit: query shareable content")
            .context("declined TCCs for application, window, display capture");
        assert_eq!(classify_permission_error(&err), Some("screen-recording"));
        let err2 = anyhow::anyhow!("Screen Recording permission required");
        assert_eq!(classify_permission_error(&err2), Some("screen-recording"));
    }

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

    /// Sentinel diarizer used by the swap-failure test below.
    /// Different type from the `RecordingDiarizer` in
    /// `diarization::tests` so we can use `Arc::ptr_eq` reliably
    /// to confirm the *exact same* `Arc` survived the failed swap.
    /// Gated alongside the test that uses it so `--no-default-features`
    /// builds don't trip the dead-code lint.
    #[cfg(feature = "diarization-onnx")]
    struct SwapSentinelDiarizer;

    #[cfg(feature = "diarization-onnx")]
    impl crate::diarization::Diarize for SwapSentinelDiarizer {
        fn label_utterances(
            &self,
            _utterances: &mut [crate::transcription::Utterance],
            _audio_chunks: &[Vec<f32>],
            _format: crate::audio::CaptureFormat,
        ) {
            // No-op; presence in the slot is the assertion.
        }
    }

    #[cfg(feature = "diarization-onnx")]
    #[test]
    fn swap_diarizer_after_download_err_leaves_slot_intact() {
        // Audit-2 gap: when the post-download model load fails
        // (corrupt ONNX, SHA mismatch from `OnnxDiarizer::new`'s
        // load-time verify, or feature compiled out), the slot
        // must not be poisoned or replaced with a half-built
        // diarizer. The catch path in `download_diarizer_model`
        // also relies on this — if the slot got partially written
        // on Err, a subsequent successful swap could pile on top
        // of an indeterminate state.
        //
        // Test: build a slot with a sentinel diarizer; call swap
        // with a tempfile whose contents won't match the
        // wespeaker SHA (so `OnnxDiarizer::new` fails *before*
        // any `slot.write()` happens); assert the slot still
        // points at the exact same Arc via `Arc::ptr_eq`.
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-wespeaker.onnx");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"definitely not a wespeaker model")
            .expect("write");
        drop(f);

        let sentinel: std::sync::Arc<dyn crate::diarization::Diarize> =
            std::sync::Arc::new(SwapSentinelDiarizer);
        let slot: crate::diarization::DiarizeSlot = std::sync::Arc::new(
            std::sync::RwLock::new(std::sync::Arc::clone(&sentinel)),
        );

        let res = super::diarizer::swap_diarizer_after_download(&slot, &path);
        assert!(res.is_err(), "swap should reject a non-wespeaker file");

        // The slot still holds the sentinel — same Arc identity,
        // not a clone or replacement.
        let guard = slot.read().expect("slot read");
        assert!(
            std::sync::Arc::ptr_eq(&*guard, &sentinel),
            "swap failure must not replace the slot's Arc"
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

        // Past the TTL → cache miss → network call (which will fail
        // here because no wiremock server is wired). The inner
        // bubbles that as `CheckFailed { reason: ... }` rather than
        // an Err, since `check_for_updates` itself maps network
        // errors to the typed enum. Either way, we should not see
        // the seeded UpToDate value back.
        let past_ttl = seed_at + UPDATE_CHECK_TTL + std::time::Duration::from_secs(1);
        let result = super::system::check_for_updates_inner(&state, past_ttl).await;
        match result {
            Ok(crate::updater::UpdateCheckResult::CheckFailed { .. }) => {
                // Network path was hit and failed — the cache was
                // bypassed as required.
            }
            Ok(other) => panic!("expected cache miss to hit network and fail; got {other:?}"),
            Err(_) => {
                // Also acceptable — some failure modes return Err
                // rather than the typed enum.
            }
        }
    }

    #[tokio::test]
    async fn check_for_updates_with_no_cache_calls_through() {
        // Empty cache → no short-circuit. Same shape as the
        // post-TTL test, just confirming the None path also falls
        // through.
        let state = crate::ipc::tests::mock_state();
        assert!(state.last_update_check.lock().unwrap().is_none());
        let result =
            super::system::check_for_updates_inner(&state, std::time::Instant::now()).await;
        // Network failure expected (no wiremock); we just want to
        // pin that this path is reached, not blocked by an empty
        // cache.
        match result {
            Ok(crate::updater::UpdateCheckResult::CheckFailed { .. }) => {}
            Ok(other) => panic!("expected fresh check to fail; got {other:?}"),
            Err(_) => {}
        }
    }

    // History CSV export (#357 phase 3a) tests live in
    // `crate::ipc::commands::history` — moved alongside the helper
    // under #431.

    // -- remove_diarizer_model (#351) ----------------------------------

    #[tokio::test]
    async fn remove_diarizer_model_is_idempotent_when_file_missing() {
        // Removing when the file isn't present must succeed cleanly
        // — covers the race where two `remove` calls fire (or the
        // user deleted the file out of band before clicking
        // Remove). Slot still gets reverted to a Noop-shaped
        // diarizer either way so the in-memory state stays
        // consistent. Mock state's models_dir is a fresh tempdir;
        // the wespeaker file is not present.
        let state = crate::ipc::tests::mock_state();
        remove_diarizer_model_inner(&state)
            .await
            .expect("idempotent on missing file");
        // The slot swap is exercised separately by
        // `swap_diarizer_after_download_err_leaves_slot_intact` and
        // friends; here we just pin that the call succeeded
        // without panicking and the toggle persistence below
        // landed.
    }

    #[tokio::test]
    async fn remove_diarizer_model_persists_toggle_off() {
        // The Speakers panel reads `diarization_enabled` to drive
        // the toggle UI. Remove must clear the flag (in-memory
        // atomic + persisted setting row) so a re-install lands
        // in a consistent off-by-default state.
        let state = crate::ipc::tests::mock_state();
        // Set the toggle on first so the `remove` flip is observable.
        state
            .runtime_flags
            .diarization_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        state
            .settings
            .set(crate::settings::keys::DIARIZATION_ENABLED, "true")
            .await
            .expect("seed settings");

        remove_diarizer_model_inner(&state)
            .await
            .expect("remove ok");

        assert!(
            !state
                .runtime_flags
                .diarization_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "atomic should flip to false"
        );
        let persisted = state
            .settings
            .get(crate::settings::keys::DIARIZATION_ENABLED)
            .await
            .expect("settings get");
        assert_eq!(persisted.as_deref(), Some("false"));
    }

    /// Test-side wrapper that mirrors the IPC body — keeps the
    /// `#[tauri::command]` shell out of the test path so we don't
    /// need a `tauri::State<'_, AppState>` constructor.
    async fn remove_diarizer_model_inner(state: &AppState) -> IpcResult<()> {
        let model = crate::diarization::catalog::default_diarizer_model();
        let path = state.models_dir.join(&model.filename);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(IpcError::Internal(format!(
                    "remove diarizer model {}: {e}",
                    path.display()
                )));
            }
        }
        {
            let mut slot = state
                .diarize_slot
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *slot = std::sync::Arc::new(crate::diarization::NoopDiarizer);
        }
        state
            .runtime_flags
            .diarization_enabled
            .store(false, std::sync::atomic::Ordering::Relaxed);
        state
            .settings
            .set(crate::settings::keys::DIARIZATION_ENABLED, "false")
            .await
            .map_err(|e| IpcError::Settings(e.to_string()))?;
        Ok(())
    }

    // ---- #315: download_diarizer_model_inner via EventEmitter ----

    /// Build a synthetic diarizer-model entry pointing at a URL the
    /// test wants the http path to hit. Used by the failure-cleanup
    /// test to drive the download into the failure branch via an
    /// unbindable port; SHA + filename are arbitrary because the
    /// test asserts on cancel-handle cleanup, not on payload
    /// content.
    fn make_test_diarizer_model(url: &str) -> crate::diarization::catalog::DiarizerModelMetadata {
        crate::diarization::catalog::DiarizerModelMetadata {
            id: "wespeaker-test".into(),
            display_name: "Wespeaker (test)".into(),
            filename: "test_diarizer.onnx".into(),
            size_mb: 1,
            description: "test entry".into(),
            download_url: url.into(),
            sha256: "0".repeat(64),
        }
    }

    fn build_download_deps(
        emitter: std::sync::Arc<dyn crate::events::EventEmitter>,
        downloads: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<String, crate::transcription::download::CancelHandle>,
            >,
        >,
        models_dir: std::path::PathBuf,
    ) -> super::diarizer::DiarizerDownloadDeps {
        super::diarizer::DiarizerDownloadDeps {
            emitter,
            downloads,
            http: reqwest::Client::new(),
            // Tests don't exercise the swap path; a NoopDiarizer
            // slot is enough to satisfy the type. Even the
            // failure-cleanup test bails before the
            // swap_diarizer_after_download call.
            diarize_slot: std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::new(
                crate::diarization::NoopDiarizer,
            ))),
            models_dir,
        }
    }

    #[tokio::test]
    async fn download_diarizer_model_rejects_duplicate_concurrent_clicks() {
        // Pre-seed the downloads map with the diarizer id (as if a
        // prior click had spawned a task). The second call must
        // bail with `IpcError::Settings` and emit no events.
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            crate::transcription::download::CancelHandle,
        >::new()));
        let model = make_test_diarizer_model("http://127.0.0.1:1/never-fetched");
        downloads.lock().unwrap().insert(
            model.id.clone(),
            crate::transcription::download::CancelHandle::new(),
        );

        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn crate::events::EventEmitter> =
            std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let deps = build_download_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        let result = super::diarizer::download_diarizer_model_inner(deps, model.clone()).await;
        match result {
            Err(IpcError::Settings(msg)) => {
                assert!(
                    msg.contains("already downloading"),
                    "expected duplicate-rejection message, got: {msg}"
                );
            }
            other => panic!("expected IpcError::Settings, got: {other:?}"),
        }

        assert!(
            recorder.events().is_empty(),
            "duplicate rejection should not emit any events; got {:?}",
            recorder.events()
        );

        // The pre-existing handle must still be in place; the
        // rejection path should not have touched it (regression
        // guard for "rejection accidentally clears the slot").
        let still_present = downloads.lock().unwrap().contains_key(&model.id);
        assert!(still_present, "pre-existing cancel handle was clobbered");
    }

    #[tokio::test]
    async fn download_diarizer_model_clears_cancel_handle_on_failure() {
        // Drive the download into the failure branch by pointing
        // it at an unbindable port (127.0.0.1:1). reqwest will
        // surface a connect error and the spawned task takes the
        // `Err(e)` arm of the match, which must:
        //   - remove its cancel-handle entry from `downloads`, AND
        //   - emit `model:download-failed` with the chained error.
        // Pre-#315 there was no test for this; the `try_state`
        // hop in the cleanup made the path reachable only from a
        // live Tauri runtime.
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            crate::transcription::download::CancelHandle,
        >::new()));
        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn crate::events::EventEmitter> =
            std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let model = make_test_diarizer_model("http://127.0.0.1:1/will-fail");
        let deps = build_download_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        super::diarizer::download_diarizer_model_inner(deps, model.clone())
            .await
            .expect("inner returns Ok before the spawn — the failure happens inside the task");

        // Wait for the spawned task to finish. The connect error
        // surfaces in single-digit ms locally; bound the wait at
        // 5s with a polling loop so a CI hiccup doesn't hang.
        let cleared = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let still_in_flight = downloads.lock().unwrap().contains_key(&model.id);
                if !still_in_flight {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);

        assert!(
            cleared,
            "cancel handle should have been removed by the failure branch"
        );

        // Failure event must have fired with a non-empty message
        // — the actual reqwest text varies by platform so we
        // assert on the shape rather than the exact wording.
        let failures = recorder.payloads_for("model:download-failed");
        assert_eq!(
            failures.len(),
            1,
            "exactly one failure event expected; got {failures:?}"
        );
        let payload = &failures[0];
        assert_eq!(payload["id"], serde_json::Value::String(model.id.clone()));
        let msg = payload["message"]
            .as_str()
            .expect("failure event should carry a message string");
        assert!(!msg.is_empty(), "failure event message should be populated");
    }
}
