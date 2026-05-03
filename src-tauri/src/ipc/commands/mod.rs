//! Tauri command handlers for the dictation pipeline.
//!
//! Kept thin: each command pulls long-lived services off [`AppState`]
//! and performs its OS side effects (clipboard write, native
//! notification, foreground-app capture) directly. The audio-then-
//! transcription path goes through [`super::run_pipeline`] for the
//! sake of unit-testability against mocks; the Tauri commands below
//! call the underlying trait methods inline so error classification
//! is structural rather than heuristic — see the note on
//! [`stop_dictation`] for the rationale.
//!
//! ## Command grouping
//!
//! As the surface has grown past a dozen commands, a quick map for
//! contributors landing here cold:
//!
//! - **Core dictation pipeline.** [`audio_list_sources`] (picker-
//!   shaped enumeration of mics + system-audio entry with capability
//!   flags), [`start_dictation`] (takes a discriminated
//!   [`crate::audio::AudioSource`]), [`stop_dictation`].
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

use std::sync::{Arc, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;

use crate::audio::{AudioSource, AudioSourceListing};
use crate::dictionary::{apply_replacements, format_vocabulary_prompt, ReplacementRule};
use crate::history::NewHistoryEntry;

use super::{AppState, ForegroundApp};

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

/// Enumerate every audio source the user can pick from in the source
/// picker — every input device plus the system-audio entry, with
/// `is_supported` flags per source so the frontend can render
/// not-yet-shipped options as disabled.
///
/// See [`crate::audio::AudioSourceListing`] for the wire shape.
#[tauri::command]
pub fn audio_list_sources(state: State<'_, AppState>) -> IpcResult<Vec<AudioSourceListing>> {
    state
        .audio
        .list_audio_sources()
        .map_err(|e| IpcError::Audio(e.to_string()))
}

// `open_settings` IPC deleted in #479 slice 3 — Settings is an
// inline panel inside the main window. The native menu's
// `Hush → Settings…` and the tray's "Open Settings…" emit
// `settings:goto-tab` directly; the main window's listener flips
// the active sidebar section + tab.

// `show_main_window` lives in `crate::ipc::commands::system` —
// extracted under #431.

/// Begin capturing from `source` (microphone or system audio).
///
/// Captures the foreground app *before* opening the input stream so the
/// snapshot is taken while the user's intended target window still has
/// focus — by the time the stream is open they may have alt-tabbed back to
/// Hush. We only commit the snapshot to [`AppState::pending_foreground`]
/// after `audio.start` succeeds, so a failed start does not leave a stale
/// snapshot in the slot. Shows the recording HUD as the last step (after
/// the audio stream is live) so a failed `start` doesn't flash the HUD on
/// then off.
///
/// If `source` is omitted the default mic is used — keeps the dictation
/// hot path one-click-from-the-hotkey for the no-options-touched case.
#[tauri::command]
pub fn start_dictation(
    app: AppHandle,
    state: State<'_, AppState>,
    source: Option<AudioSource>,
) -> IpcResult<()> {
    let source = source.unwrap_or_else(AudioSource::default_microphone);
    start_dictation_inner(&state, source)?;
    if state
        .runtime_flags
        .hud_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        if let Err(e) = crate::hud::show(&app) {
            tracing::error!(error = ?e, "failed to show recording HUD");
        }
        // Default the HUD to the Recording state. Pre-#291 the
        // HUD didn't carry an explicit state — Recording was the
        // only visual. The set_state call here is a no-op when
        // the HUD page hasn't subscribed to the event yet but
        // costs nothing; it's the symmetric counterpart to the
        // Processing transition in stop_dictation below.
        //
        // Carries `started_at_ms` so the HUD's elapsed-time
        // counter resets cleanly across back-to-back dictations
        // (#481). The persistent HUD page would otherwise hold
        // the previous session's `recordingStartedAt` whenever
        // its listener race-missed the event.
        if let Err(e) = crate::hud::set_state(
            &app,
            crate::hud::HudState::Recording {
                started_at_ms: crate::hud::now_unix_ms(),
            },
        ) {
            tracing::warn!(error = ?e, "emit hud:state(recording) failed");
        }
    }
    // Audio cue so the user knows the mic is hot without having
    // to glance at the HUD (#292; cross-platform synthesis #446).
    // Off by default;
    // fired only when both the master toggle AND the per-event
    // start sub-toggle are on (#463). The sub-toggle defaults
    // to true so existing master-on users keep hearing this cue.
    let cues_master = state
        .runtime_flags
        .sound_cues_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let cue_start = state
        .runtime_flags
        .sound_cue_start_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    crate::audio_cues::play_if_enabled(
        cues_master && cue_start,
        crate::audio_cues::CUE_RECORDING_START,
    );
    Ok(())
}

/// Tauri-free orchestration for `start_dictation`. Split out so tests can
/// drive it against a mock [`AudioCapture`] without spinning up a Tauri
/// runtime — the public command is a one-line wrapper that lifts the
/// `State<'_, AppState>` newtype off and forwards.
fn start_dictation_inner(state: &AppState, source: AudioSource) -> IpcResult<()> {
    // Pre-flight: refuse to open audio capture when no transcriber is
    // loaded. Pre-#195 this check lived only in `stop_dictation`, so a
    // user with no model would record audio (HUD up, mic hot, level
    // meter dancing), press Stop, and *only then* see the
    // "no transcription model loaded" error. The recording is wasted
    // — we have audio bytes nobody will ever transcribe — and the
    // user has spent N seconds waiting for an outcome that was never
    // possible. Fail fast at start so the error surfaces before
    // anyone speaks. The frontend's model-loaded banner gates the
    // Start button visually; this is the structural backstop for the
    // hotkey path that bypasses button gating.
    {
        let guard = state.transcribe.lock().map_err(poisoned)?;
        if guard.is_none() {
            return Err(IpcError::TranscriptionUnavailable);
        }
    }

    let foreground = capture_foreground();

    // Upfront mic-permission probe (#417). cpal's actual macOS-
    // mic-denial chain reads "Audio Unit: kAudioUnitErr_…" with
    // no "microphone" substring, so the post-call classifier in
    // the `.map_err` below rarely catches mic denials. Instead,
    // ask AVAuthorizationStatus directly before touching cpal:
    // if it's Denied (which `macos_perms` also normalises
    // Restricted into for UX purposes), surface the typed
    // variant upfront. NotDetermined falls through so the OS
    // prompt fires on the actual cpal call (the user hasn't
    // been asked yet). Same shape `meeting_start_manual` uses
    // post-#416 for SCK; this is the analogue for the dictation
    // mic path.
    //
    // Cfg-gated to macOS because the AVAuthorizationStatus
    // surface only exists there; on Linux/Windows the cpal
    // failure chain carries the platform-native diagnostic and
    // the post-call classifier handles it.
    #[cfg(target_os = "macos")]
    if matches!(source, AudioSource::Microphone(_)) {
        let mic_status = crate::macos_perms::read_all().microphone;
        if matches!(mic_status, crate::macos_perms::PermissionStatus::Denied) {
            return Err(IpcError::PermissionDenied("microphone".to_owned()));
        }
    }

    state.audio.start_with_source(source).map_err(|e| {
        // Promote permission-shaped chains to the typed
        // `PermissionDenied` variant (#386 / #416 close-out).
        // Mirrors the meeting_start_manual pattern so the
        // dictation start path goes through the same
        // discriminant the frontend can switch on.
        //
        // The mic arm of the classifier rarely fires from here
        // (#417): cpal's actual mic-denial chain doesn't
        // contain "microphone" or "not authorized", so this
        // branch mostly catches the SCK case (system-audio
        // source). Mic Denied is now caught upfront via the
        // AVAuthorizationStatus probe above; this stays
        // defensive for any future cpal text change AND for
        // the SCK case when the dictation source is
        // system-audio.
        if let Some(perm) = classify_permission_error(&e) {
            IpcError::PermissionDenied(perm.to_owned())
        } else {
            IpcError::Audio(e.to_string())
        }
    })?;

    *state.pending_foreground.lock().map_err(poisoned)? = foreground;

    Ok(())
}

/// Stop capturing, transcribe, apply post-transcription replacements,
/// write to clipboard, fire a notification, and return the text to the
/// frontend.
///
/// ## Fatal-vs-best-effort policy
///
/// The function deliberately treats some failures as fatal and others
/// as best-effort. The split is by *whether the user's deliverable is
/// affected*:
///
/// - **Fatal** (returns `Err` and dictation fails):
///   - Audio backend stop (no audio → no transcript)
///   - Transcription itself (no transcript → no clipboard write)
///   - Clipboard write (the user's actual artefact — without this,
///     they can't paste, which is the whole point)
/// - **Best-effort** (logged and skipped):
///   - Vocabulary prompt load (without it, transcription works but
///     loses prompt-bias for proper nouns/jargon)
///   - Replacement-rule load (without them, the raw transcript still
///     reaches the clipboard — replacements are polish)
///   - "Ready to paste" notification (Linux-without-daemon edge
///     case; user has the text on the clipboard regardless)
///   - History insert (fire-and-forget; logged at error level if it
///     fails. The user has their text; losing one history row is a
///     missed analytics moment, not a blocked workflow)
///
/// ## Why the audio/transcription split is structural, not heuristic
///
/// The audio-stop and transcription calls are made inline rather than
/// collapsed through a single helper, because each layer's error must
/// map to the right [`IpcError`] variant *structurally* — the frontend
/// dispatches recovery copy on `kind`. A previous attempt at substring-
/// classifying a merged error string was fragile: a whisper error
/// mentioning "device" was being routed to `Audio`. Splitting the
/// calls makes the boundary obvious and removes the heuristic.
///
/// Clipboard write is the user's actual artefact; if it fails we surface
/// the error to the frontend so the user knows the text wasn't pasteable.
/// The notification is courtesy and best-effort: if the platform refuses
/// to fire one (Linux without a notification daemon, for example), we
/// swallow the error and continue.
#[tauri::command]
pub async fn stop_dictation(
    app: AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<DictationResult> {
    let transcriber = {
        // Lock briefly, clone the Arc, drop the lock. The dictation
        // hot path only needs a snapshot of "what's loaded right now".
        // Hot-swap from `model_select` will land for the *next* call.
        let guard = state.transcribe.lock().map_err(poisoned)?;
        guard
            .as_ref()
            .ok_or(IpcError::TranscriptionUnavailable)?
            .clone()
    };

    // The user pressed Stop. Pre-#291 the HUD hid here, before
    // transcription ran — meaning users would see the HUD vanish,
    // switch to their target app, paste, and get stale clipboard
    // content because the transcription + clipboard-write
    // pipeline hadn't completed. Now the HUD switches to a
    // Processing visual (label "Processing…", static dot, no
    // level meter) and stays visible until the clipboard write
    // succeeds. Audio-error path still hides immediately —
    // showing a stuck Processing pill on a failed capture is
    // worse than no pill.
    let captured = stop_audio_capture(&state).map_err(|e| {
        crate::hud::hide_async(&app);
        e
    })?;
    if let Err(e) = crate::hud::set_state(&app, crate::hud::HudState::Processing) {
        tracing::warn!(error = ?e, "emit hud:state(processing) failed");
    }

    // Vocabulary + replacements load are best-effort. Inference itself
    // is fatal — without text there's nothing for the user to paste.
    let prompt = load_vocabulary_prompt(&state).await;
    // If the user has vocabulary terms configured but the loaded
    // backend can't act on them, warn once per dictation. This is the
    // place where "vocabulary terms silently produce no effect"
    // would otherwise hide. The check is gated on `!prompt.is_empty()`
    // so the no-vocab case doesn't spam the log on every dictation.
    if !prompt.is_empty() && !transcriber.supports_prompt_biasing() {
        tracing::warn!(
            backend = transcriber.model_label(),
            "vocabulary terms configured but the active transcription backend does not support prompt biasing — terms will not affect this transcript"
        );
    }
    // Inference goes through the streaming entry point with a single
    // chunk holding the whole captured buffer. For backends that
    // don't override `transcribe_chunks`, the default impl is byte-
    // identical to the prior `transcribe_with_prompt` call: same
    // audio, same prompt, single final utterance.
    //
    // Calling through the streaming surface here lets a future
    // Whisper-sliding-window or Parakeet backend emit multiple
    // partial utterances mid-recording without changing this call
    // site — the IPC layer just needs to flip from "concatenate
    // utterances at the end" to "forward each utterance via Tauri
    // event as it arrives." See the design memo at
    // `docs/system-audio-meeting-mode-proposal.md` for the Phase C
    // event-forwarding shape.
    let format = captured.format;
    // Compute recording duration before transcribe_chunks consumes the
    // sample buffer. `samples.len()` is the interleaved frame count
    // (channels * sample_rate * seconds), so wall-clock duration is
    // frames / (sample_rate * channels). `checked_div` guards the
    // (impossible) zero-format case so a corrupt format can't panic
    // the dictation hot path.
    let duration_ms: Option<i64> = (captured.samples.len() as u64)
        .saturating_mul(1000)
        .checked_div(format.sample_rate as u64 * format.channels.max(1) as u64)
        .map(|ms| ms as i64);

    // Short-press shortcut (#197): when the recording is too brief
    // to contain real speech, skip whisper inference and return a
    // friendly empty result. Pre-#197 a held-too-short PTT press
    // produced an empty / near-empty sample buffer that whisper-rs
    // rejected with "Input sample buffer was empty"; the IPC layer
    // surfaced that as a `Transcription` error and the result panel
    // showed the scary technical message. Now the empty-state
    // handling that #196 wired for `[BLANK_AUDIO]` covers this case
    // too — same path, no inference attempt, duration still shown.
    //
    // Threshold rationale: whisper.cpp's mel front-end needs at least
    // one frame (~32 ms at 16 kHz). Anything below 200 ms is well
    // below the floor of a deliberate utterance — even "no" /
    // "yes" / "k" runs ~250-400 ms in normal speech — so the
    // short-circuit is virtually free of false positives. Users
    // who hold the hotkey deliberately for longer always reach the
    // inference path.
    const MIN_TRANSCRIBE_MS: i64 = 200;
    let too_short = match duration_ms {
        Some(ms) => ms < MIN_TRANSCRIBE_MS,
        // Conservative: missing duration (impossible format) →
        // treat as too-short rather than crash whisper with
        // unknown input.
        None => true,
    };
    if too_short {
        let foreground = take_foreground_snapshot(&state)?;
        // Hide the Processing HUD on the too-short path —
        // there's no transcription happening, so the Processing
        // visual would falsely imply "still working". No
        // completion cue either; silence is the right signal
        // for a genuinely empty press (#291 / #292).
        crate::hud::hide_async(&app);
        // Don't write to the clipboard for an empty result —
        // pre-#197 this branch was unreachable so the question
        // didn't come up. The user just held the hotkey and got
        // nothing; the last clipboard contents (their previous
        // dictation, or whatever they copied manually) should
        // survive the no-op press.
        return Ok(DictationResult {
            text: String::new(),
            foreground,
            duration_ms,
        });
    }

    // Transcription error path — hide the Processing HUD before
    // returning so a transcription panic doesn't leave a stuck
    // "Processing…" pill on screen (#291). The error itself is
    // surfaced normally to the frontend's structured-error
    // renderer.
    let utterances = match transcriber.transcribe_chunks(&[captured.samples], format, &prompt) {
        Ok(u) => u,
        Err(e) => {
            crate::hud::hide_async(&app);
            return Err(IpcError::Transcription(e.to_string()));
        }
    };
    // Concatenate the final utterances. The default impl emits
    // exactly one; a future streaming backend may emit several.
    // Skip non-final utterances — those are partial revisions
    // intended for live UI updates, not the dictation hot path's
    // single clipboard write.
    let final_count = utterances.iter().filter(|u| u.is_final).count();
    let raw_text = utterances
        .iter()
        .filter(|u| u.is_final)
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    // Defensive guard against a streaming backend that emits ONLY
    // partial utterances (no finals) — without this check, the
    // filter above silently produces an empty string and the user
    // gets a clipboard with nothing in it, no error surfaced.
    // Round-7 technical-quality reviewer caught the silent-empty
    // failure mode on the future-streaming-backend path. The
    // default-impl one-shot path always emits exactly one final, so
    // this branch is only reachable for misbehaving overrides — we
    // surface the misbehaviour as a Transcription error rather than
    // letting it look like the user's audio was empty.
    if final_count == 0 && !utterances.is_empty() {
        return Err(IpcError::Transcription(
            "transcription backend emitted only partial utterances; no final transcript available"
                .to_owned(),
        ));
    }
    let rules = load_replacement_rules(&state).await;
    // Strip whisper.cpp's bracket sentinels (`[BLANK_AUDIO]`, `[NOISE]`,
    // …) before applying replacements + the clipboard write. The
    // sentinels are useful internal signals but a literal
    // "[BLANK_AUDIO]" in the user's clipboard is a paste-foot-gun and
    // a confusing "Transcription" panel readout. After stripping, an
    // all-sentinel result becomes the empty string — the frontend
    // renders a friendly "no audio detected" copy in that case.
    let stripped = strip_whisper_brackets(raw_text.trim());
    let text = apply_replacements(&stripped, &rules);

    write_to_clipboard(&app, &text)?;
    fire_ready_notification(&app);
    // Hide the Processing HUD now that the clipboard has the
    // transcript (#291). The user can paste safely from this
    // point on. Hide after the clipboard write so the HUD
    // doesn't flicker out before the user knows it's ready.
    crate::hud::hide_async(&app);
    // Completion cue so the user knows the clipboard is ready
    // without glancing at the HUD (#292; cross-platform #446).
    // Fired AFTER the clipboard write so the cue truly means
    // "safe to paste"; never fired on the error paths above.
    // Gated by master AND per-event complete sub-toggle (#463).
    let cues_master_done = state
        .runtime_flags
        .sound_cues_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let cue_complete = state
        .runtime_flags
        .sound_cue_complete_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    crate::audio_cues::play_if_enabled(
        cues_master_done && cue_complete,
        crate::audio_cues::CUE_TRANSCRIPTION_READY,
    );

    let foreground = take_foreground_snapshot(&state)?;
    spawn_history_create(
        Arc::clone(&state.data.history),
        NewHistoryEntry {
            transcript: text.clone(),
            app_name: foreground.as_ref().map(|f| f.app_name.clone()),
            window_title: foreground.as_ref().map(|f| f.window_title.clone()),
            model: transcriber.model_label(),
            duration_ms,
        },
    );

    // Meeting Mode (#110): if a session is active, also append this
    // transcript as a final utterance under that session. Fire-and-
    // forget on a tokio task so a meeting-repo failure doesn't block
    // the user's clipboard — same model as the history insert above.
    // Cumulative-ms timestamps come from the manager's
    // last-utterance-end logic; we hand it the duration of the
    // utterance text (computed from the recording's total frames).
    let utterance_duration_ms = utterances
        .iter()
        .filter(|u| u.is_final)
        .map(|u| (u.ended_at_ms.saturating_sub(u.started_at_ms)) as i64)
        .sum::<i64>();
    let manager_handle = Arc::clone(&state.meeting_manager);
    let meeting_text = text.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = manager_handle
            .append_if_active(&meeting_text, utterance_duration_ms)
            .await
        {
            tracing::error!(error = ?e, "failed to append utterance to active meeting session");
        }
    });

    Ok(DictationResult {
        text,
        foreground,
        duration_ms,
    })
}

/// Remove whisper.cpp's bracketed status sentinels (`[BLANK_AUDIO]`,
/// `[NOISE]`, `[MUSIC]`, `[INAUDIBLE]`, `[ MUSIC ]`, etc.) from a
/// transcript. Whisper emits these for silent / non-speech segments;
/// they're useful as an internal signal that "the model heard
/// nothing recognisable" but are confusing as user-facing transcript
/// text and as clipboard content. Returns the cleaned string, with
/// surrounding whitespace trimmed.
///
/// Pulled out as a free function so the regex-free implementation
/// has unit-test coverage for the cases the user hits most:
/// pure-blank, whitespace inside brackets, mixed real-text + sentinel
/// (the user did say something but whisper also marked a leading
/// silence segment), and case-insensitive variants.
fn strip_whisper_brackets(input: &str) -> String {
    // Build the output one char at a time; skip anything inside `[…]`.
    // The brackets are always single-line in whisper's output, so a
    // simple bracket-depth counter is enough — no need for a regex
    // dep just for this.
    let mut out = String::with_capacity(input.len());
    let mut depth: i32 = 0;
    for ch in input.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    // Collapse whitespace runs introduced by stripped brackets and
    // trim the edges. Splitting on whitespace and re-joining is
    // simpler than walking the string with a state machine and is
    // cheap on the ms-scale strings whisper produces.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stop the audio stream and return the captured samples, mapping the
/// backend error to [`IpcError::Audio`]. Split out so `stop_dictation`
/// can keep its HUD-hide-on-error step a single line.
fn stop_audio_capture(state: &AppState) -> IpcResult<crate::audio::CapturedAudio> {
    state
        .audio
        .stop()
        .map_err(|e| IpcError::Audio(e.to_string()))
}

/// Load the user's vocabulary terms and format them as the initial
/// Whisper prompt. Best-effort: a repository error logs and demotes
/// to the no-prompt path. The decoder treats an empty prompt as a no-op
/// (both via the trait's default `transcribe_with_prompt` and via
/// `set_initial_prompt` itself), so the caller never has to branch.
async fn load_vocabulary_prompt(state: &AppState) -> String {
    match state.data.vocabulary.list().await {
        Ok(terms) => format_vocabulary_prompt(&terms),
        Err(e) => {
            tracing::error!(error = ?e, "failed to load vocabulary; skipping prompt-biasing");
            String::new()
        }
    }
}

/// Load post-transcription find/replace rules. Best-effort: a failure
/// here demotes to "no rules applied" rather than failing the whole
/// dictation. The user already has audio captured and a transcript
/// pending; surfacing a rules-load error as fatal would block them on
/// a strictly-secondary feature.
async fn load_replacement_rules(state: &AppState) -> Vec<ReplacementRule> {
    match state.data.replacements.list().await {
        Ok(rules) => rules,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load replacement rules; skipping post-processing");
            Vec::new()
        }
    }
}

/// Pop the foreground snapshot captured at `start_dictation`. Returns
/// `None` if the slot is empty (which can happen if a hotkey-driven
/// start raced the snapshot capture). The `Mutex` is fenced via
/// [`poisoned`] so a panicked thread doesn't bring down a Tauri command.
fn take_foreground_snapshot(state: &AppState) -> IpcResult<Option<ForegroundApp>> {
    Ok(state.pending_foreground.lock().map_err(poisoned)?.take())
}

/// Write the final text to the system clipboard. Fatal on failure —
/// the clipboard is the user's actual artefact for this dictation.
fn write_to_clipboard(app: &AppHandle, text: &str) -> IpcResult<()> {
    app.clipboard()
        .write_text(text.to_owned())
        .map_err(|e| IpcError::Clipboard(e.to_string()))
}

/// Fire the "Ready to paste" courtesy notification. Best-effort: on
/// platforms without a notification daemon (e.g. Linux without
/// dbus/notify-send) we log and continue.
fn fire_ready_notification(app: &AppHandle) {
    if let Err(e) = app
        .notification()
        .builder()
        .title("Hush")
        .body("Ready to paste")
        .show()
    {
        tracing::warn!(error = ?e, "failed to fire 'ready to paste' notification");
    }
}

/// Persist `entry` to history on the Tauri async runtime. Fire-and-
/// forget: a failed insert is logged and swallowed, never bubbled to
/// the user — the clipboard write is what they care about. If history
/// ever becomes load-bearing for a downstream pipeline, this needs
/// revisiting.
fn spawn_history_create(
    history: Arc<dyn crate::history::HistoryRepository>,
    entry: NewHistoryEntry,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = history.create(entry).await {
            tracing::error!(error = ?e, "failed to persist transcription to history");
        }
    });
}

// History-browse commands (history_list, history_search,
// history_export_row_csv, history_delete, history_count,
// history_clear, get_dictation_stats) live in
// `crate::ipc::commands::history` — extracted under #431.

// Vocabulary + replacement-rule CRUD commands live in
// `crate::ipc::commands::dictionary` — extracted under #431. The
// pure-logic [`apply_replacements`] (used inside `stop_dictation`
// above) stays in `crate::dictionary`; only the thin IPC handlers
// moved.
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

/// Snapshot the current foreground window via `active-win-pos-rs`.
///
/// `active-win-pos-rs` exposes a Result with the unit type as its error,
/// which is not particularly informative. We collapse the failure case to
/// `None` because losing the foreground snapshot is a graceful degradation
/// — the dictation still works, history just won't have the per-app
/// metadata for that row.
fn capture_foreground() -> Option<ForegroundApp> {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => Some(ForegroundApp {
            app_name: w.app_name,
            window_title: w.title,
        }),
        Err(_) => {
            tracing::debug!("active-win-pos-rs returned no active window");
            None
        }
    }
}

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

    // -- start_dictation_inner regression tests ---------------------------
    //
    // These cover the foreground-leak fix surfaced in code review: a
    // failed `audio.start` must not overwrite or pollute the
    // `pending_foreground` slot. Using mock implementations of
    // `AudioCapture` rather than the cpal backend so we do not need a real
    // microphone or Tauri runtime.

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    use crate::audio::{AudioCapture, AudioDevice, CapturedAudio};
    use crate::ipc::AppState;
    use crate::transcription::Transcribe;

    /// Local Transcribe stub. The crate-root tests have an
    /// `EchoTranscribe` but it isn't `pub(crate)`; declaring a fresh
    /// one here keeps the dependency minimal.
    struct OkTranscribe;
    impl Transcribe for OkTranscribe {
        fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
            Ok("ok".to_owned())
        }
    }

    struct AudioThatFailsToStart;

    impl AudioCapture for AudioThatFailsToStart {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Err(anyhow!("device unplugged"))
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            unreachable!("stop should not be called when start fails")
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    /// Audio mock that surfaces a permission-shaped chain. Used to
    /// pin the classifier promotion in `start_dictation_inner`
    /// (#386 / #416 close-out): a chain containing
    /// "Screen Recording permission" should land as the typed
    /// `IpcError::PermissionDenied("screen-recording")` variant
    /// rather than a generic `IpcError::Audio(...)`.
    struct AudioThatFailsWithScreenRecordingDenial;

    impl AudioCapture for AudioThatFailsWithScreenRecordingDenial {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Err(anyhow!("query shareable content").context("Screen Recording permission required"))
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            unreachable!("stop should not be called when start fails")
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    struct AudioThatStarts {
        recording: AtomicBool,
    }

    impl AudioCapture for AudioThatStarts {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            self.recording.store(true, Ordering::Release);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            unreachable!()
        }
        fn is_recording(&self) -> bool {
            self.recording.load(Ordering::Acquire)
        }
    }

    #[test]
    fn start_dictation_does_not_overwrite_foreground_on_audio_start_failure() {
        let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsToStart);
        let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
        let state = crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .transcribe(Some(transcribe))
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
            .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })
            .meeting_app_overrides({
                let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
                o
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete");

        // Pre-populate the slot with a sentinel value so a regression in
        // the assignment order — assigning the new capture before
        // `audio.start` returns — would visibly overwrite it.
        *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
            app_name: "sentinel".into(),
            window_title: "sentinel".into(),
        });

        let err = start_dictation_inner(&state, AudioSource::default_microphone())
            .expect_err("audio.start fails");
        assert!(
            matches!(err, IpcError::Audio(_)),
            "expected IpcError::Audio, got {err:?}"
        );

        let after = state.pending_foreground.lock().unwrap().clone();
        assert_eq!(
            after.map(|f| f.app_name).as_deref(),
            Some("sentinel"),
            "pending_foreground was overwritten despite failed start"
        );
    }

    #[test]
    fn start_dictation_promotes_permission_shaped_error_to_typed_variant() {
        // #386 / #416 close-out: the classifier was added to the
        // meeting_start_manual boundary first; this pins the same
        // promotion at start_dictation. A permission-shaped chain
        // from the audio layer (today: SCK rejection when the user
        // picks the system-audio source as their dictation input)
        // must surface as `IpcError::PermissionDenied(...)` so the
        // frontend's PermissionsDialog launch heuristic can match
        // on `kind` instead of substring-scraping.
        let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsWithScreenRecordingDenial);
        let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
        let state = crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .transcribe(Some(transcribe))
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
            .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })
            .meeting_app_overrides({
                let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
                o
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete");

        let err = start_dictation_inner(&state, AudioSource::default_microphone())
            .expect_err("audio.start fails with permission-shaped chain");
        match err {
            IpcError::PermissionDenied(perm) => {
                assert_eq!(perm, "screen-recording");
            }
            other => {
                panic!("expected IpcError::PermissionDenied(\"screen-recording\"), got: {other:?}")
            }
        }
    }

    #[test]
    fn start_dictation_succeeds_and_leaves_a_foreground_slot_for_stop() {
        // Confirms the happy path actually does write into the slot —
        // otherwise the bug-fix above could be "we just never assign
        // anything", which would also pass the regression test in
        // isolation.
        let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatStarts {
            recording: AtomicBool::new(false),
        });
        let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
        let state = crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .transcribe(Some(transcribe))
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
            .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })
            .meeting_app_overrides({
                let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
                o
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete");

        // We can't observe the OS foreground app reliably from a test
        // process, so we just assert the call returned Ok and the slot is
        // *some* value (None or Some, both are acceptable — the OS may
        // genuinely have no active window in CI).
        start_dictation_inner(&state, AudioSource::default_microphone()).expect("should succeed");

        // Just prove the lock didn't poison and the slot is reachable.
        let _: Option<ForegroundApp> = state.pending_foreground.lock().unwrap().clone();
    }

    /// Suppress the dead-code warning that fires because [`Mutex`] is
    /// otherwise unused after the regression tests' construction —
    /// this is part of the type signature compile-check above.
    #[allow(dead_code)]
    fn _assert_state_mutex_holds_foreground(state: AppState) -> Mutex<Option<ForegroundApp>> {
        state.pending_foreground
    }

    #[test]
    fn start_dictation_returns_unavailable_when_no_transcriber_is_loaded() {
        // Pre-#195 this scenario silently opened audio capture and
        // failed at `stop_dictation` — the user spent N seconds
        // recording before learning no transcriber was loaded.
        // Pin the new pre-flight: no transcriber → fail fast, no
        // audio side effects, no foreground slot mutation.
        let audio_started = Arc::new(AtomicBool::new(false));
        let audio: Arc<dyn AudioCapture> = Arc::new(StartFlagAudio {
            started: Arc::clone(&audio_started),
        });
        let state = crate::ipc::AppStateBuilder::new()
            .audio(audio)
            // No `.transcribe(...)` — slot stays None.
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
            .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })
            .meeting_app_overrides({
                let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
                o
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete");

        let err = start_dictation_inner(&state, AudioSource::default_microphone())
            .expect_err("no-transcriber must surface as a hard error");
        assert!(
            matches!(err, IpcError::TranscriptionUnavailable),
            "expected TranscriptionUnavailable, got {err:?}"
        );
        assert!(
            !audio_started.load(Ordering::Acquire),
            "audio.start_with_source must NOT be called when no transcriber is loaded"
        );
    }

    /// Audio backend whose only job is recording whether `start_with_source`
    /// (or `start`) was called, so the pre-flight test can prove the
    /// audio path was skipped before the error returned.
    struct StartFlagAudio {
        started: Arc<AtomicBool>,
    }

    impl AudioCapture for StartFlagAudio {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            self.started.store(true, Ordering::Release);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            unreachable!("stop should not be called");
        }
        fn is_recording(&self) -> bool {
            self.started.load(Ordering::Acquire)
        }
    }

    // -- whisper bracket-sentinel stripping ------------------------------

    #[test]
    fn strip_brackets_drops_pure_blank_audio_sentinel() {
        // The exact case in #196's user report: whisper emitted
        // `[BLANK_AUDIO]` and the user saw it in the result panel
        // and on their clipboard.
        assert_eq!(super::strip_whisper_brackets("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn strip_brackets_drops_other_status_sentinels() {
        // Same shape, different label. Whisper produces these for
        // music / non-speech / unintelligible segments.
        for sentinel in [
            "[NOISE]",
            "[MUSIC]",
            "[ MUSIC ]",
            "[INAUDIBLE]",
            "[Sound effects]",
            "[laughter]",
        ] {
            assert_eq!(
                super::strip_whisper_brackets(sentinel),
                "",
                "sentinel {sentinel} should strip to empty"
            );
        }
    }

    #[test]
    fn strip_brackets_keeps_real_speech_around_a_silence_marker() {
        // Whisper sometimes prefixes a transcript with
        // `[BLANK_AUDIO]` when there's a leading silence segment —
        // the real speech follows. Keep the speech, drop the marker,
        // collapse the surrounding whitespace.
        assert_eq!(
            super::strip_whisper_brackets("[BLANK_AUDIO] hello world"),
            "hello world"
        );
        assert_eq!(
            super::strip_whisper_brackets("hello world [NOISE]"),
            "hello world"
        );
        assert_eq!(
            super::strip_whisper_brackets("first [NOISE] second"),
            "first second"
        );
    }

    #[test]
    fn strip_brackets_leaves_text_with_no_brackets_alone() {
        // The common path. Pin so a regression in the stripping
        // pass doesn't accidentally trim or reflow real
        // transcripts.
        assert_eq!(
            super::strip_whisper_brackets("Hello, world."),
            "Hello, world."
        );
    }

    #[test]
    fn strip_brackets_handles_nested_or_unbalanced_brackets_safely() {
        // Defensive: whisper isn't supposed to emit nested or
        // unbalanced brackets, but the depth counter shouldn't
        // panic if it does. Output may not be ideal — the goal is
        // "doesn't crash, doesn't drop more than it should."
        assert_eq!(super::strip_whisper_brackets("[[NESTED]]"), "");
        // A stray closing bracket is preserved (depth never goes
        // negative).
        assert_eq!(super::strip_whisper_brackets("hello]"), "hello]");
    }

    // -- stop_dictation helper tests --------------------------------------
    //
    // The Tauri command itself needs an `AppHandle` (clipboard +
    // notification + HUD), so it can't be unit-tested directly. The
    // helpers extracted from it can — these tests pin their behaviour
    // so the orchestration in `stop_dictation` stays trustworthy
    // through future refactors.

    use crate::dictionary::{
        NewVocabularyTerm, ReplacementRepository, ReplacementRule, VocabularyRepository,
        VocabularyTerm,
    };

    struct AudioThatStopsWith {
        captured: CapturedAudio,
    }

    impl AudioCapture for AudioThatStopsWith {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            Ok(self.captured.clone())
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    struct AudioThatFailsToStop;

    impl AudioCapture for AudioThatFailsToStop {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            Err(anyhow!("device went away"))
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    struct VocabWithTerms(Vec<VocabularyTerm>);

    #[async_trait::async_trait]
    impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for VocabWithTerms {
        async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
            Ok(self.0.clone())
        }
        async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
            unreachable!()
        }
        async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FailingVocab;

    #[async_trait::async_trait]
    impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for FailingVocab {
        async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
            Err(anyhow!("table missing"))
        }
        async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
            unreachable!()
        }
        async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FailingReplacements;

    #[async_trait::async_trait]
    impl crate::repository::Repository<ReplacementRule, crate::dictionary::NewReplacementRule, i64>
        for FailingReplacements
    {
        async fn list(&self) -> anyhow::Result<Vec<ReplacementRule>> {
            Err(anyhow!("table missing"))
        }
        async fn create(
            &self,
            _: crate::dictionary::NewReplacementRule,
        ) -> anyhow::Result<ReplacementRule> {
            unreachable!()
        }
        async fn update(&self, _: ReplacementRule) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn state_with(
        audio: Arc<dyn AudioCapture>,
        vocab: Arc<dyn VocabularyRepository>,
        replacements: Arc<dyn ReplacementRepository>,
    ) -> AppState {
        crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(replacements)
            .vocabulary(vocab)
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })
            .meeting_app_overrides({
                let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
                o
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                    Arc::new(crate::ipc::tests::NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete")
    }

    fn fixed_audio() -> CapturedAudio {
        CapturedAudio {
            samples: vec![0.5_f32; 8],
            format: crate::audio::CaptureFormat {
                sample_rate: 48_000,
                channels: 1,
            },
        }
    }

    #[test]
    fn stop_audio_capture_returns_captured_on_success() {
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let captured = stop_audio_capture(&state).expect("audio.stop ok");
        assert_eq!(captured.samples.len(), 8);
        assert_eq!(captured.format.sample_rate, 48_000);
    }

    #[test]
    fn stop_audio_capture_maps_backend_error_to_ipc_error_audio() {
        // Regression for the heuristic-classifier era: audio errors must
        // surface as `IpcError::Audio` so the frontend's switch-on-kind
        // dispatch picks the right recovery copy. This is *structural*
        // classification — there is no string match anywhere.
        let state = state_with(
            Arc::new(AudioThatFailsToStop),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let err = stop_audio_capture(&state).expect_err("stop fails");
        assert!(matches!(err, IpcError::Audio(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn load_vocabulary_prompt_formats_terms_when_present() {
        let terms = vec![
            VocabularyTerm {
                id: 1,
                term: "Hush".into(),
            },
            VocabularyTerm {
                id: 2,
                term: "whisper.cpp".into(),
            },
        ];
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(VocabWithTerms(terms.clone())),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let prompt = load_vocabulary_prompt(&state).await;
        // The exact format is owned by `format_vocabulary_prompt`; this
        // test just pins that the helper actually invokes the formatter
        // rather than returning empty.
        assert!(prompt.contains("Hush"), "got: {prompt}");
        assert!(prompt.contains("whisper.cpp"), "got: {prompt}");
    }

    #[tokio::test]
    async fn load_vocabulary_prompt_swallows_repository_errors() {
        // Repository failure must not block transcription — we demote
        // to the no-prompt path.
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(FailingVocab),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let prompt = load_vocabulary_prompt(&state).await;
        assert!(prompt.is_empty(), "got: {prompt}");
    }

    #[tokio::test]
    async fn load_replacement_rules_returns_empty_on_error() {
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(FailingReplacements),
        );

        let rules = load_replacement_rules(&state).await;
        assert!(rules.is_empty());
    }

    #[test]
    fn take_foreground_snapshot_pops_and_clears_the_slot() {
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );
        *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
            app_name: "Slack".into(),
            window_title: "#general".into(),
        });

        let popped = take_foreground_snapshot(&state).expect("not poisoned");
        assert_eq!(popped.as_ref().map(|f| f.app_name.as_str()), Some("Slack"));

        // Second take must be None: the slot is consumed, not cloned.
        let again = take_foreground_snapshot(&state).expect("not poisoned");
        assert!(again.is_none());
    }

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

        let sentinel: Arc<dyn crate::diarization::Diarize> = Arc::new(SwapSentinelDiarizer);
        let slot: crate::diarization::DiarizeSlot =
            Arc::new(std::sync::RwLock::new(Arc::clone(&sentinel)));

        let res = super::diarizer::swap_diarizer_after_download(&slot, &path);
        assert!(res.is_err(), "swap should reject a non-wespeaker file");

        // The slot still holds the sentinel — same Arc identity,
        // not a clone or replacement.
        let guard = slot.read().expect("slot read");
        assert!(
            Arc::ptr_eq(&*guard, &sentinel),
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
