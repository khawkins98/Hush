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
pub mod export;
pub mod macos;
pub mod meeting;
pub mod models;

use std::sync::{Arc, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;

use crate::audio::{AudioSource, AudioSourceListing};
use crate::dictionary::{
    apply_replacements, format_vocabulary_prompt, NewReplacementRule, NewVocabularyTerm,
    ReplacementRule, VocabularyTerm,
};
use crate::history::{HistoryEntry, NewHistoryEntry};

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

/// Open (or focus, if already visible) the standalone Settings
/// window. Frontend invokes this from the main window's "Open
/// Settings" affordances; the macOS menu bar's `Hush → Settings…`
/// entry (⌘,) calls this directly from the menu-event handler in
/// [`crate::lib`].
#[tauri::command]
pub fn open_settings(app: AppHandle) -> IpcResult<()> {
    crate::settings_window::show(&app)
        .map_err(|e| IpcError::Internal(format!("open settings window: {e:#}")))
}

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
    if state.hud_enabled.load(std::sync::atomic::Ordering::Relaxed) {
        if let Err(e) = crate::hud::show(&app) {
            tracing::error!(error = ?e, "failed to show recording HUD");
        }
        // Default the HUD to the Recording state. Pre-#291 the
        // HUD didn't carry an explicit state — Recording was the
        // only visual. The set_state call here is a no-op when
        // the HUD page hasn't subscribed to the event yet but
        // costs nothing; it's the symmetric counterpart to the
        // Processing transition in stop_dictation below.
        if let Err(e) = crate::hud::set_state(&app, crate::hud::HudState::Recording) {
            tracing::warn!(error = ?e, "emit hud:state(recording) failed");
        }
    }
    // Audio cue: short "Tink" so the user knows the mic is hot
    // without having to glance at the HUD (#292). Off by default;
    // fired only when the user has opted in.
    crate::audio_cues::play_if_enabled(
        state
            .sound_cues_enabled
            .load(std::sync::atomic::Ordering::Relaxed),
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
        let _ = crate::hud::hide(&app);
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
        if let Err(e) = crate::hud::hide(&app) {
            tracing::warn!(error = ?e, "failed to hide HUD on too-short dictation");
        }
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
            if let Err(hide_err) = crate::hud::hide(&app) {
                tracing::warn!(error = ?hide_err, "failed to hide HUD on transcription error");
            }
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
    if let Err(e) = crate::hud::hide(&app) {
        tracing::error!(error = ?e, "failed to hide recording HUD");
    }
    // Completion cue: short "Glass" chime so the user knows the
    // clipboard is ready without glancing at the HUD (#292).
    // Fired AFTER the clipboard write so the cue truly means
    // "safe to paste"; never fired on the error paths above.
    crate::audio_cues::play_if_enabled(
        state
            .sound_cues_enabled
            .load(std::sync::atomic::Ordering::Relaxed),
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

/// Paginated list of history rows, newest first.
///
/// `limit` is hard-capped by the repository to a few hundred rows so a
/// misbehaving frontend cannot pull the entire table at once. `offset`
/// is clamped at 0.
#[tauri::command]
pub async fn history_list(
    state: State<'_, AppState>,
    limit: i64,
    offset: i64,
) -> IpcResult<Vec<HistoryEntry>> {
    state
        .data
        .history
        .list(limit, offset)
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// FTS5 search over transcript text. Empty / whitespace-only `query`
/// falls through to the full list, mirroring the UI's "type to filter"
/// pattern.
#[tauri::command]
pub async fn history_search(
    state: State<'_, AppState>,
    query: String,
    limit: i64,
    offset: i64,
) -> IpcResult<Vec<HistoryEntry>> {
    state
        .data
        .history
        .search(&query, limit, offset)
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// Export a single dictation history row as RFC-4180 CSV.
///
/// Two-arg shape: the user picks `path` via
/// `tauri-plugin-dialog`'s `save()` (which is a path picker only —
/// it doesn't write bytes for us), and Rust writes the CSV body
/// directly to that path. The capability for the main window grants
/// `dialog:allow-save` only; the backend handles the actual write
/// so we don't have to wire `tauri-plugin-fs` and broaden the
/// filesystem surface.
///
/// Schema: `id,created_at,duration_ms,app_name,model,transcript`.
/// Omitted: `window_title` (private; not in the export contract
/// from #357 phase 3a).
///
/// Per-row export uses `id` to look up the entry; bulk export
/// (pending) will reuse the same `history_csv_for_entries` helper.
#[tauri::command]
pub async fn history_export_row_csv(
    state: State<'_, AppState>,
    id: i64,
    path: String,
) -> IpcResult<()> {
    let all = state
        .data
        .history
        .list(i64::MAX, 0)
        .await
        .map_err(|e| IpcError::History(format!("history list: {e:#}")))?;
    let entry = all
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| IpcError::History(format!("history row {id} not found")))?;
    let body = history_csv_for_entries(std::slice::from_ref(&entry))
        .map_err(|e| IpcError::Internal(format!("CSV write: {e:#}")))?;
    tokio::fs::write(&path, body)
        .await
        .map_err(|e| IpcError::Internal(format!("write {path}: {e}")))?;
    Ok(())
}

/// Pure CSV-emit helper. Held outside the IPC entry point so unit
/// tests can call it without an `AppState` around the corner. RFC
/// 4180 escaping handled by the `csv` crate — embedded quotes /
/// newlines / commas are quote-wrapped + double-quoted as needed.
pub(super) fn history_csv_for_entries(entries: &[HistoryEntry]) -> anyhow::Result<String> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record([
        "id",
        "created_at",
        "duration_ms",
        "app_name",
        "model",
        "transcript",
    ])?;
    for e in entries {
        wtr.write_record(&[
            e.id.to_string(),
            e.created_at.clone(),
            e.duration_ms.map(|n| n.to_string()).unwrap_or_default(),
            e.app_name.clone().unwrap_or_default(),
            e.model.clone(),
            e.transcript.clone(),
        ])?;
    }
    let bytes = wtr.into_inner()?;
    Ok(String::from_utf8(bytes)?)
}

/// Delete a single history row. No-op (returns Ok) if `id` does not
/// exist — mirrors the trait contract.
#[tauri::command]
pub async fn history_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .history
        .delete(id)
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// Total row count, for paginators that need "page X of Y".
#[tauri::command]
pub async fn history_count(state: State<'_, AppState>) -> IpcResult<i64> {
    state
        .data
        .history
        .count()
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// Delete every history row. The frontend gates this behind a
/// confirmation prompt — there is no recovery once it lands.
/// Returns the number of rows that were removed so the UI can
/// surface "Cleared N transcripts" feedback. Calling against an
/// empty history is safe and returns `0`.
#[tauri::command]
pub async fn history_clear(state: State<'_, AppState>) -> IpcResult<i64> {
    state
        .data
        .history
        .clear()
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// Aggregate stats for the History stats bar (#293). Returns
/// session count, total words, total recording time, and total
/// transcript characters. Empty-history case returns all zeros so
/// the frontend can render a consistent shape.
#[tauri::command]
pub async fn get_dictation_stats(
    state: State<'_, AppState>,
) -> IpcResult<crate::history::DictationStats> {
    state
        .data
        .history
        .get_stats()
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

// -- Replacement-rule CRUD -----------------------------------------------
//
// Settings-shaped commands the frontend's "Replacements" panel binds to.
// All four are async because the underlying repository is async; the IPC
// surface is intentionally thin — the pure-logic [`apply_replacements`]
// is in `dictionary` and runs on the dictation hot-path inside
// `stop_dictation` above.

/// All replacement rules in `(sort_order, id)` order.
#[tauri::command]
pub async fn replacements_list(state: State<'_, AppState>) -> IpcResult<Vec<ReplacementRule>> {
    state
        .data
        .replacements
        .list()
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Insert a new replacement. Returns the persisted row (with the
/// database-assigned id) so the frontend can append it to its local list
/// without a follow-up `list` round-trip.
#[tauri::command]
pub async fn replacement_create(
    state: State<'_, AppState>,
    find_text: String,
    replace_text: String,
    sort_order: i64,
) -> IpcResult<ReplacementRule> {
    state
        .data
        .replacements
        .create(NewReplacementRule {
            find_text,
            replace_text,
            sort_order,
        })
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Update an existing replacement's fields. The frontend passes the full
/// rule (not a partial diff) so the backend never has to reason about
/// "which fields changed". No-op if `id` does not exist.
#[tauri::command]
pub async fn replacement_update(
    state: State<'_, AppState>,
    rule: ReplacementRule,
) -> IpcResult<()> {
    state
        .data
        .replacements
        .update(rule)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Delete a single replacement. No-op if `id` does not exist.
#[tauri::command]
pub async fn replacement_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .replacements
        .delete(id)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

// -- Vocabulary CRUD -----------------------------------------------------
//
// Errors here surface as `IpcError::Replacements` rather than a
// dedicated `Vocabulary` variant because users see one combined
// "Dictionary settings" surface in the UI for both subsystems —
// keeping the error `kind` unified means the frontend's error switch
// doesn't sprout two near-identical branches that drift over time.

/// All vocabulary terms in insertion order.
#[tauri::command]
pub async fn vocabulary_list(state: State<'_, AppState>) -> IpcResult<Vec<VocabularyTerm>> {
    state
        .data
        .vocabulary
        .list()
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Insert a new vocabulary term. The schema enforces `UNIQUE` on `term`,
/// so duplicates surface as an error here for the frontend to render.
#[tauri::command]
pub async fn vocabulary_create(
    state: State<'_, AppState>,
    term: String,
) -> IpcResult<VocabularyTerm> {
    state
        .data
        .vocabulary
        .create(NewVocabularyTerm { term })
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Update an existing vocabulary term. No-op if `id` does not exist.
#[tauri::command]
pub async fn vocabulary_update(state: State<'_, AppState>, term: VocabularyTerm) -> IpcResult<()> {
    state
        .data
        .vocabulary
        .update(term)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Delete a vocabulary term. No-op if `id` does not exist.
#[tauri::command]
pub async fn vocabulary_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .vocabulary
        .delete(id)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}
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

/// Returns whether the macOS first-run welcome has been shown and
/// dismissed for this install. The value is stored under
/// [`crate::settings::keys::FIRST_RUN_COMPLETED`] as the literal
/// string `"true"` once dismissed; any other state (including the
/// settings row being absent) reads as `false`.
#[tauri::command]
pub async fn get_first_run_completed(state: State<'_, AppState>) -> IpcResult<bool> {
    let value = state
        .settings
        .get(crate::settings::keys::FIRST_RUN_COMPLETED)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    Ok(value.as_deref() == Some("true"))
}

/// Persist that the user has dismissed the welcome modal. Idempotent;
/// calling twice is the same as once.
#[tauri::command]
pub async fn mark_first_run_completed(state: State<'_, AppState>) -> IpcResult<()> {
    state
        .settings
        .set(crate::settings::keys::FIRST_RUN_COMPLETED, "true")
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the recording-HUD-enabled flag. The Settings → General
/// toggle reads this on mount so the checkbox renders the
/// persisted value rather than always-checked. Defaults to `true`
/// when the row is absent — first-time users benefit from the
/// floating pill that confirms the mic is hot.
#[tauri::command]
pub fn get_hud_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state.hud_enabled.load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the recording-HUD-enabled flag and update the in-memory
/// `AppState` flag the dictation / meeting start paths read.
/// Stored as the literal `"true"` / `"false"` under
/// [`crate::settings::keys::HUD_ENABLED`] so the next launch reads
/// the same value back.
#[tauri::command]
pub async fn set_hud_enabled(state: State<'_, AppState>, enabled: bool) -> IpcResult<()> {
    set_hud_enabled_inner(&state, enabled).await
}

/// Tauri-free orchestration for [`set_hud_enabled`]. Tests exercise
/// this against a `MemSettings`-backed `AppState` rather than a
/// real Tauri runtime.
pub(crate) async fn set_hud_enabled_inner(state: &AppState, enabled: bool) -> IpcResult<()> {
    state
        .hud_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::HUD_ENABLED,
            if enabled { "true" } else { "false" },
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the audio-cues toggle (#292). Settings → General reads
/// this on mount. Default off — opt-in by design (intrusive in
/// shared spaces).
#[tauri::command]
pub fn get_sound_cues_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .sound_cues_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the audio-cues flag + update the AtomicBool. Same
/// shape as `set_hud_enabled`.
#[tauri::command]
pub async fn set_sound_cues_enabled(state: State<'_, AppState>, enabled: bool) -> IpcResult<()> {
    set_sound_cues_enabled_inner(&state, enabled).await
}

pub(crate) async fn set_sound_cues_enabled_inner(state: &AppState, enabled: bool) -> IpcResult<()> {
    state
        .sound_cues_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::SOUND_CUES_ENABLED,
            if enabled { "true" } else { "false" },
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the diarization-enabled flag (#111). Settings → Meeting reads
/// this on mount so the toggle renders the persisted value. Defaults
/// to `false` when the row is absent — diarization is opt-in until
/// the PR-B model-download path lands.
#[tauri::command]
pub fn get_diarization_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .diarization_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the diarization-enabled flag + update the AtomicBool. Same
/// shape as `set_hud_enabled`. Foundation PR (this one) only flips
/// the flag; the meeting pump's dispatch path will read it once PR-B
/// wires the `OnnxDiarizer` impl.
#[tauri::command]
pub async fn set_diarization_enabled(state: State<'_, AppState>, enabled: bool) -> IpcResult<()> {
    set_diarization_enabled_inner(&state, enabled).await
}

pub(crate) async fn set_diarization_enabled_inner(
    state: &AppState,
    enabled: bool,
) -> IpcResult<()> {
    state
        .diarization_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::DIARIZATION_ENABLED,
            if enabled { "true" } else { "false" },
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the live inference thread count (#255). Settings →
/// General reads this on mount so the slider renders the
/// persisted value rather than the cross-platform default.
#[tauri::command]
pub fn get_inference_threads(state: State<'_, AppState>) -> IpcResult<i32> {
    Ok(state
        .inference_threads
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the inference thread count + update the in-memory
/// atomic the loaded `WhisperTranscription` reads on every
/// inference call. Same pattern as `set_hud_enabled` —
/// optimistically updates the atomic + persists to settings.
/// Clamped to `[MIN_INFERENCE_THREADS, MAX_INFERENCE_THREADS]`
/// (1–16) so a malformed input can't push past whisper.cpp's
/// happy band.
#[tauri::command]
pub async fn set_inference_threads(state: State<'_, AppState>, threads: i32) -> IpcResult<()> {
    set_inference_threads_inner(&state, threads).await
}

pub(crate) async fn set_inference_threads_inner(state: &AppState, threads: i32) -> IpcResult<()> {
    let clamped = threads.clamp(1, 16);
    state
        .inference_threads
        .store(clamped, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::INFERENCE_THREADS,
            &clamped.to_string(),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Status of the diarizer model file (#301). The Settings →
/// Speakers panel reads this on mount + after every download
/// progress event so the UI can render "model not installed",
/// "downloading", or "ready" states accurately.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizeModelStatus {
    /// Whether the wespeaker `.onnx` file is present in
    /// `models_dir`. Frontend uses this to grey out the toggle and
    /// show the download affordance when `false`.
    pub downloaded: bool,
    /// Catalog display name ("wespeaker ResNet34-LM"). Lifted into
    /// the status (#351) so the panel can show *which* model is
    /// installed without duplicating the catalog on the frontend.
    pub display_name: String,
    /// Catalog-declared on-disk size (~26 MB). Surfaced in the UI
    /// so the user knows what they're committing to before
    /// clicking Download.
    pub size_mb: u32,
    /// Catalog-declared SHA-256 (hex). Returned alongside the
    /// status so the UI can show a "verified file" indicator
    /// post-download. Not user-facing per se, but useful for
    /// support / troubleshooting.
    pub sha256: String,
    /// Absolute path the user can copy-and-cd-into to drop the
    /// file manually if they prefer (or to verify the download
    /// landed where expected). Mirrors the same affordance as the
    /// Whisper picker.
    pub expected_path: String,
    /// Upstream URL the model was downloaded from. Linked from the
    /// Speakers panel so a user who wants to read the model card
    /// can click through (#351).
    pub source_url: String,
}

/// Read the diarizer model's status (#301). Cheap — single
/// filesystem stat. Called by Settings → Speakers on mount and
/// after each `model:download-done` / `model:download-failed`
/// Tauri event.
#[tauri::command]
pub fn get_diarizer_model_status(state: State<'_, AppState>) -> IpcResult<DiarizeModelStatus> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let path = state.models_dir.join(&model.filename);
    Ok(DiarizeModelStatus {
        downloaded: path.exists(),
        display_name: model.display_name,
        size_mb: model.size_mb,
        sha256: model.sha256,
        expected_path: path.to_string_lossy().into_owned(),
        source_url: model.download_url,
    })
}

/// Remove the installed wespeaker model and revert the diarizer
/// slot to NoopDiarizer (#351). The slot swap is the in-process
/// inverse of `download_diarizer_model`'s `swap_diarizer_after_download`
/// — the next meeting pump tick reads the new slot and stops
/// labelling utterances. Persists `diarization_enabled = false` so
/// the toggle in Settings reflects the new state and a future
/// re-install lands in a clean configuration.
///
/// No-op if the file isn't present (the user already removed it
/// out-of-band, or a parallel `remove` raced to completion). The
/// slot swap still runs so the in-memory state stays consistent
/// with the filesystem regardless of how the file disappeared.
#[tauri::command]
pub async fn remove_diarizer_model(state: State<'_, AppState>) -> IpcResult<()> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let path = state.models_dir.join(&model.filename);

    // Best-effort delete: a missing file is fine (idempotent), but
    // any other error (permission, IO failure) surfaces so the
    // user sees something rather than a silent partial state.
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

    // Revert the slot to a Noop. Mirror the recovery shape
    // `swap_diarizer_after_download` uses for write lock acquisition
    // — a transient panic shouldn't poison the slot for the rest
    // of the session. The guard is scoped to a block so it's
    // proven-dropped before the next await; otherwise the macro-
    // generated future fails to satisfy `Send`.
    {
        let mut slot = state
            .diarize_slot
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *slot = std::sync::Arc::new(crate::diarization::NoopDiarizer);
    }

    // Turn the toggle off in the persisted settings so the panel's
    // next read shows a consistent "no model + toggle off" state.
    // Errors here are non-fatal: the in-memory slot already swapped,
    // and a misaligned toggle setting is a UX papercut, not a
    // broken state.
    state
        .diarization_enabled
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if let Err(e) = state
        .settings
        .set(crate::settings::keys::DIARIZATION_ENABLED, "false")
        .await
    {
        tracing::warn!(error = %e, "remove_diarizer_model: persist toggle=false failed");
    }

    Ok(())
}

/// Begin downloading the wespeaker speaker-embedding model (#301).
/// Mirrors the `model_download` shape: returns immediately, the
/// download runs on a tokio task, progress is reported via Tauri
/// events. After a successful download we hot-swap the diarizer
/// slot so the new `OnnxDiarizer` takes effect on the next meeting
/// tick — no app restart needed.
///
/// Three Tauri events fan out the lifecycle, namespaced under the
/// existing `model:` prefix the Whisper picker uses:
/// - `model:download-progress` — `{ id, bytesReceived, bytesTotal }`
/// - `model:download-done` — `{ id, message: null }`
/// - `model:download-failed` — `{ id, message }`
///
/// `id` is always `"wespeaker-resnet34-lm"` for the diarizer
/// (matches `catalog::WESPEAKER_RESNET34_LM_ID`).
///
/// Implementation delegates to [`download_diarizer_model_inner`] —
/// the inner takes an [`crate::ipc::events::EventEmitter`] trait
/// instead of an `AppHandle` so tests can drive both the rejection
/// path and the failure-cleanup path without spinning up a real
/// Tauri runtime (#315).
#[tauri::command]
pub async fn download_diarizer_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<()> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let emitter: std::sync::Arc<dyn crate::ipc::events::EventEmitter> =
        std::sync::Arc::new(crate::ipc::events::TauriEventEmitter::new(app));
    download_diarizer_model_inner(
        DiarizerDownloadDeps {
            emitter,
            downloads: std::sync::Arc::clone(&state.downloads),
            http: state.http.clone(),
            diarize_slot: std::sync::Arc::clone(&state.diarize_slot),
            models_dir: state.models_dir.clone(),
        },
        model,
    )
    .await
}

/// Bundled dependencies the diarizer download needs at runtime.
/// Pulled out of [`AppState`] so [`download_diarizer_model_inner`]
/// can run from a `#[tokio::test]` without a real `AppHandle` /
/// `tauri::State` (#315).
pub(crate) struct DiarizerDownloadDeps {
    pub emitter: std::sync::Arc<dyn crate::ipc::events::EventEmitter>,
    pub downloads: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, crate::transcription::download::CancelHandle>,
        >,
    >,
    pub http: reqwest::Client,
    pub diarize_slot: crate::diarization::DiarizeSlot,
    pub models_dir: std::path::PathBuf,
}

/// Inner implementation of [`download_diarizer_model`]. Same
/// behaviour as the `#[tauri::command]` wrapper, but takes the
/// dependencies as plain values so tests can drive both:
///
/// - the duplicate-rejection guard inside the
///   `state.downloads.lock()` critical section (audit-2 fix); and
/// - the cancel-handle cleanup on the spawned task's failure
///   branch (mirrors the Whisper-download cleanup pattern).
///
/// `model` is the catalog entry to download. Production passes
/// `crate::diarization::catalog::default_diarizer_model()`; tests
/// can pass a custom entry with a deliberately bad URL to drive
/// the failure path without standing up a fake server.
pub(crate) async fn download_diarizer_model_inner(
    deps: DiarizerDownloadDeps,
    model: crate::diarization::catalog::DiarizerModelMetadata,
) -> IpcResult<()> {
    let id = model.id.clone();
    let dest = deps.models_dir.join(&model.filename);

    // Register a cancel handle + re-check on-disk presence inside
    // the same critical section. Reuses the `downloads` store —
    // same map the Whisper download path uses, keyed by id, so
    // the existing `model_cancel_download` IPC works for the
    // diarizer model with no extra wiring.
    //
    // The exists-check sits inside the lock to close a TOCTOU
    // race (audit-2): two rapid clicks could both pass the
    // exists-check before either took the lock. Holding the lock
    // for the existence test means a concurrent download that
    // just finished is observable as either "file exists now" or
    // "cancel handle still in flight" — caller gets a clean error
    // either way and we never start a duplicate download on top
    // of a freshly-finalized file.
    let cancel = crate::transcription::download::CancelHandle::new();
    {
        let mut guard = deps.downloads.lock().map_err(poisoned)?;
        if dest.exists() {
            return Err(IpcError::Settings(format!(
                "{} is already downloaded",
                model.display_name
            )));
        }
        if guard.contains_key(&id) {
            return Err(IpcError::Settings(format!(
                "{} is already downloading",
                model.display_name
            )));
        }
        guard.insert(id.clone(), cancel.clone());
    }

    let emitter_for_task = std::sync::Arc::clone(&deps.emitter);
    let downloads_for_task = std::sync::Arc::clone(&deps.downloads);
    let id_for_task = id.clone();
    let url = model.download_url.clone();
    let sha = model.sha256.clone();
    let http = deps.http.clone();
    let dest_for_task = dest.clone();
    let diarize_slot = std::sync::Arc::clone(&deps.diarize_slot);

    tauri::async_runtime::spawn(async move {
        let emitter_for_progress = std::sync::Arc::clone(&emitter_for_task);
        let id_for_progress = id_for_task.clone();
        let progress: Box<crate::transcription::download::ProgressCallback> =
            Box::new(move |update| {
                emitter_for_progress.emit(
                    "model:download-progress",
                    &crate::ipc::commands::models::DownloadProgress {
                        id: id_for_progress.clone(),
                        bytes_received: update.bytes_received,
                        bytes_total: update.bytes_total,
                    },
                );
            });

        let result = crate::transcription::download::download_with_progress(
            &http,
            &url,
            &dest_for_task,
            &sha,
            &cancel,
            &progress,
        )
        .await;

        // Drop the cancel handle on the way out, success or
        // failure. Same pattern the Whisper download uses; the
        // shared `downloads` map is the rejection-guard so
        // forgetting to clean up here would silently block
        // subsequent download attempts (audit-2 R-2).
        if let Ok(mut guard) = downloads_for_task.lock() {
            guard.remove(&id_for_task);
        }

        match result {
            Ok(()) => {
                // Hot-swap the diarizer. If OnnxDiarizer::new
                // succeeds, write it into the slot — the next
                // pump tick that runs with diarization_enabled=on
                // will use it.
                //
                // If the load fails (corrupted ONNX, ort init
                // error, feature compiled out), the file is on
                // disk but useless. Pre-audit-2 we emitted
                // `model:download-done` regardless — the UI then
                // showed "installed and verified" while the
                // diarizer was still Noop, leaving the user with
                // a feature that quietly didn't work. Now we
                // delete the bad file (so retry isn't blocked by
                // the `dest.exists()` guard at the top of the
                // function) and emit `model:download-failed` with
                // the load error, so the UI surfaces it the same
                // way as a network or SHA-mismatch failure.
                match swap_diarizer_after_download(&diarize_slot, &dest_for_task) {
                    Ok(()) => {
                        emitter_for_task.emit(
                            "model:download-done",
                            &crate::ipc::commands::models::DownloadStatus {
                                id: id_for_task,
                                message: None,
                            },
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %dest_for_task.display(),
                            "diarizer download succeeded but model load failed; \
                             deleting bad file and emitting download-failed so \
                             retry isn't blocked"
                        );
                        let _ = std::fs::remove_file(&dest_for_task);
                        emitter_for_task.emit(
                            "model:download-failed",
                            &crate::ipc::commands::models::DownloadStatus {
                                id: id_for_task,
                                message: Some(format!("model load failed: {e:#}")),
                            },
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    model_id = %id_for_task,
                    "diarizer download failed"
                );
                emitter_for_task.emit(
                    "model:download-failed",
                    &crate::ipc::commands::models::DownloadStatus {
                        id: id_for_task,
                        message: Some(format!("{e:#}")),
                    },
                );
            }
        }
    });

    Ok(())
}

/// Build a fresh `OnnxDiarizer` from the just-downloaded file and
/// swap it into the slot. Pulled out as a helper so the inline
/// download closure stays readable + so the cfg-gating around the
/// `diarization-onnx` feature lives in one spot.
fn swap_diarizer_after_download(
    slot: &crate::diarization::DiarizeSlot,
    model_path: &std::path::Path,
) -> anyhow::Result<()> {
    #[cfg(feature = "diarization-onnx")]
    {
        let onnx = crate::diarization::onnx::OnnxDiarizer::new(model_path)?;
        let mut guard = slot
            .write()
            .map_err(|e| anyhow::anyhow!("slot poisoned: {e}"))?;
        *guard = std::sync::Arc::new(onnx);
        Ok(())
    }
    #[cfg(not(feature = "diarization-onnx"))]
    {
        let _ = slot;
        let _ = model_path;
        Err(anyhow::anyhow!(
            "diarization-onnx feature not enabled in this build"
        ))
    }
}

/// Read the current Meeting-Mode auto-start mode. The Settings
/// → Meeting tab calls this on mount so the dropdown renders the
/// persisted value.
#[tauri::command]
pub fn get_meeting_autostart_mode(
    state: State<'_, AppState>,
) -> IpcResult<crate::meeting::MeetingAutostartMode> {
    Ok(crate::ipc::decode_autostart_mode(
        state
            .meeting_autostart_mode
            .load(std::sync::atomic::Ordering::Relaxed),
    ))
}

/// Persist the Meeting-Mode auto-start mode. Updates both the
/// in-memory atomic the foreground poller reads and the settings
/// row used at next-launch boot, so the value is observable to the
/// poller within the next 3 s tick without an app restart.
#[tauri::command]
pub async fn set_meeting_autostart_mode(
    state: State<'_, AppState>,
    mode: crate::meeting::MeetingAutostartMode,
) -> IpcResult<()> {
    state.meeting_autostart_mode.store(
        crate::ipc::encode_autostart_mode(mode),
        std::sync::atomic::Ordering::Relaxed,
    );
    state
        .settings
        .set(
            crate::settings::keys::MEETING_AUTOSTART_MODE,
            mode.as_setting(),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// TTL for the [`check_for_updates`] cache (#333). 15 minutes is
/// well below GitHub's 60-req/h unauthenticated rate-limit window
/// (so a single user under heavy clicking can't self-DoS) and well
/// above the spam-click threshold (so back-to-back clicks return
/// instantly). The window is also short enough that a user who
/// just installed an update sees the new "up to date" copy without
/// quitting the app.
pub const UPDATE_CHECK_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// LaunchAgent path-staleness flag (#317). #271's setup hook
/// re-registers the autostart plist with the current binary
/// path on every launch where autostart is enabled — but if
/// `enable()` fails (read-only home, fs permission issue) the
/// LaunchAgent still points at whatever path it had before, and
/// the user gets no signal. This IPC + the retry below give
/// Settings → General a way to surface the failure and let the
/// user trigger another attempt.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartPathStatus {
    /// True if `lib.rs::run`'s post-#271 re-register hit an
    /// error. False on every other path (autostart not enabled,
    /// re-register succeeded, or non-macOS where the flag is
    /// always false because the re-register block is gated to
    /// macOS).
    pub stale: bool,
}

#[tauri::command]
pub fn get_autostart_path_status(state: State<'_, AppState>) -> IpcResult<AutostartPathStatus> {
    Ok(AutostartPathStatus {
        stale: state
            .autostart_path_stale
            .load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// Retry the LaunchAgent re-register that failed at boot (#317).
/// Returns `true` if the retry succeeded (and clears the stale
/// flag so subsequent `get_autostart_path_status` calls see the
/// cleaner state); returns `false` if the retry also failed.
///
/// Settings → General's "Click to update" button calls this when
/// the user wants to retry without restarting the app.
#[tauri::command]
pub fn retry_autostart_registration(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<bool> {
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_autostart::ManagerExt;
        let mgr = app.autolaunch();
        match mgr.enable() {
            Ok(()) => {
                state
                    .autostart_path_stale
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    "autostart: retry_autostart_registration succeeded; LaunchAgent path is now current"
                );
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "autostart: retry_autostart_registration failed; flag stays set"
                );
                Ok(false)
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = state;
        Ok(true)
    }
}

/// Manual "Check for updates" probe (#223). Calls
/// [`crate::updater::check_for_updates`] against the app's shared
/// HTTP client; the result drives an in-app dialog.
///
/// Caches the last successful result for [`UPDATE_CHECK_TTL`]
/// (#333) so a spam-clicking user or a shared-IP environment
/// (corporate NAT, family Wi-Fi with multiple installs) can't burn
/// the unauthenticated-GitHub rate limit. Auto-update is the
/// separate [#10] follow-up.
///
/// [#10]: https://github.com/khawkins98/Hush/issues/10
#[tauri::command]
pub async fn check_for_updates(
    state: State<'_, AppState>,
) -> IpcResult<crate::updater::UpdateCheckResult> {
    check_for_updates_inner(&state, std::time::Instant::now()).await
}

/// Inner implementation that takes the current instant explicitly
/// so unit tests can pin time without an actual sleep. The IPC
/// command always passes `Instant::now()`.
pub(crate) async fn check_for_updates_inner(
    state: &AppState,
    now: std::time::Instant,
) -> IpcResult<crate::updater::UpdateCheckResult> {
    {
        let cached = state.last_update_check.lock().map_err(poisoned)?;
        if let Some((at, result)) = cached.as_ref() {
            if now.duration_since(*at) < UPDATE_CHECK_TTL {
                return Ok(result.clone());
            }
        }
    }
    let fresh = crate::updater::check_for_updates(&state.http).await?;
    *state.last_update_check.lock().map_err(poisoned)? = Some((now, fresh.clone()));
    Ok(fresh)
}

/// Clear the first-run-completed flag so the welcome modal renders
/// again on the next app launch. Used by the Settings → General
/// "Show welcome on next launch" affordance — useful for users
/// who dismissed the welcome too quickly and want to re-read the
/// permissions explainer.
#[tauri::command]
pub async fn reset_first_run(state: State<'_, AppState>) -> IpcResult<()> {
    state
        .settings
        .set(crate::settings::keys::FIRST_RUN_COMPLETED, "false")
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Configuration the Settings UI reads + writes for push-to-talk.
///
/// `combo` is the canonical `+`-separated key list (`RightMeta`,
/// `RightMeta+RightShift`, etc.). `enabled` mirrors the persisted
/// `ptt_enabled` settings flag. `listenerRunning` is a runtime
/// signal: true when the rdev thread is alive and gated by the
/// `enabled` flag, false when it wasn't started at boot. The UI
/// uses it to show "Restart Hush for Enable to take effect" when
/// the user toggles ON in a session that started with PTT off.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PttConfig {
    pub combo: Vec<String>,
    pub enabled: bool,
    pub listener_running: bool,
}

#[tauri::command]
pub fn ptt_get_config(state: State<'_, AppState>) -> IpcResult<PttConfig> {
    let combo = state
        .ptt_combo
        .read()
        .map_err(|_| IpcError::Internal("ptt_combo lock poisoned".into()))?
        .keys()
        .iter()
        .map(|k| k.as_str().to_string())
        .collect();
    let enabled = state.ptt_active.load(std::sync::atomic::Ordering::SeqCst);
    let listener_running = state
        .ptt_listener_spawned
        .load(std::sync::atomic::Ordering::SeqCst);
    Ok(PttConfig {
        combo,
        enabled,
        // True once the rdev thread is actually up. `ptt_set_config`
        // spawns it on demand the first time the user enables PTT,
        // so this transitions from false → true on first opt-in
        // without an app restart.
        listener_running,
    })
}

/// Update the user's PTT configuration. Combo is hot-swapped via
/// the shared `RwLock` (next keystroke uses the new combo). Enabled
/// is persisted + flipped on the runtime atomic; if the listener
/// wasn't running at boot, a restart is required for the change to
/// take effect (the listener can't be started mid-session because
/// rdev::listen has no clean stop API and starting it now would
/// trigger the OS permission prompt at a surprising moment).
///
/// Validates the combo before persisting — an empty combo or
/// unparseable key name returns `IpcError::Settings` and the
/// existing config is unchanged.
///
/// First-time opt-in: when `enabled` flips from false to true and
/// the rdev listener wasn't spawned at boot, this command starts
/// it on demand via `register_ptt_listener`. On macOS that's the
/// moment the Input Monitoring permission prompt fires — the user
/// has clicked Enable, so the prompt is no longer a surprise.
#[tauri::command]
pub async fn ptt_set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    combo: Vec<String>,
    enabled: bool,
) -> IpcResult<()> {
    // Build + validate the combo first, BEFORE touching state.
    // A bad input shouldn't half-apply (combo persisted, atomic
    // flipped) — validate up front and bail clean.
    let parsed_keys: Result<Vec<crate::hotkey::ptt::PttKey>, _> = combo
        .iter()
        .map(|s| crate::hotkey::ptt::parse_ptt_key(s))
        .collect();
    let parsed_keys =
        parsed_keys.map_err(|e| IpcError::Settings(format!("ptt_set_config: {e:#}")))?;
    let new_combo = crate::hotkey::ptt::PttCombo::try_from_keys(parsed_keys)
        .map_err(|e| IpcError::Settings(format!("ptt_set_config: {e:#}")))?;

    // Persist combo first so a crash between steps leaves the user
    // with their chosen combo on next launch even if the atomic /
    // enabled flip didn't reach the DB.
    state
        .settings
        .set(
            crate::settings::keys::PTT_COMBO,
            &new_combo.to_storage_string(),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    state
        .settings
        .set(
            crate::settings::keys::PTT_ENABLED,
            if enabled { "true" } else { "false" },
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;

    // Hot-swap the in-memory state — listener picks both up on the
    // next OS event without restarting.
    {
        let mut guard = state
            .ptt_combo
            .write()
            .map_err(|_| IpcError::Internal("ptt_combo lock poisoned".into()))?;
        *guard = new_combo;
    }
    state
        .ptt_active
        .store(enabled, std::sync::atomic::Ordering::SeqCst);

    // Spawn the rdev listener on demand if this is the first time
    // PTT is being enabled this session. The call is idempotent —
    // a second invocation with the spawned latch already true
    // returns Ok without touching the thread. On macOS, this is
    // the line that triggers the Input Monitoring permission
    // prompt; on the success path the user clicks Enable, sees
    // the prompt, grants, and PTT works without a restart.
    if enabled {
        if let Err(e) = crate::hotkey::ptt::register_ptt_listener(
            &app,
            std::sync::Arc::clone(&state.ptt_combo),
            std::sync::Arc::clone(&state.ptt_active),
            std::sync::Arc::clone(&state.ptt_listener_spawned),
        ) {
            // Best-effort: spawn failure is logged but shouldn't
            // un-persist the user's preference. They can try again
            // (or restart) and the listener will spin up on next
            // launch via lib.rs::setup since `ptt_enabled=true` is
            // already in the DB.
            tracing::error!(error = ?e, "failed to spawn PTT listener on demand");
        }
    }
    Ok(())
}

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
            .hud_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);

        set_hud_enabled_inner(&state, false)
            .await
            .expect("set_hud_enabled_inner ok");

        assert!(
            !state.hud_enabled.load(std::sync::atomic::Ordering::Relaxed),
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

        set_hud_enabled_inner(&state, false)
            .await
            .expect("set false ok");
        set_hud_enabled_inner(&state, true)
            .await
            .expect("set true ok");

        assert!(state.hud_enabled.load(std::sync::atomic::Ordering::Relaxed));
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
        set_inference_threads_inner(&state, 8)
            .await
            .expect("set ok");
        assert_eq!(
            state
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
        set_inference_threads_inner(&state, 999)
            .await
            .expect("set ok");
        assert_eq!(
            state
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
        set_inference_threads_inner(&state, 0)
            .await
            .expect("set ok");
        assert_eq!(
            state
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
                .diarization_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "default should be off"
        );

        set_diarization_enabled_inner(&state, true)
            .await
            .expect("set true ok");
        assert!(
            state
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

        set_diarization_enabled_inner(&state, false)
            .await
            .expect("set false ok");
        assert!(
            !state
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

        let res = swap_diarizer_after_download(&slot, &path);
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
        let result = check_for_updates_inner(&state, still_within)
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
        let result = check_for_updates_inner(&state, past_ttl).await;
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
        let result = check_for_updates_inner(&state, std::time::Instant::now()).await;
        // Network failure expected (no wiremock); we just want to
        // pin that this path is reached, not blocked by an empty
        // cache.
        match result {
            Ok(crate::updater::UpdateCheckResult::CheckFailed { .. }) => {}
            Ok(other) => panic!("expected fresh check to fail; got {other:?}"),
            Err(_) => {}
        }
    }

    // -- history CSV export (#357 phase 3a) ----------------------------

    fn history_entry(
        id: i64,
        transcript: &str,
        app: Option<&str>,
        duration: Option<i64>,
    ) -> HistoryEntry {
        HistoryEntry {
            id,
            transcript: transcript.to_owned(),
            app_name: app.map(str::to_owned),
            window_title: None,
            model: "ggml-base.bin".to_owned(),
            duration_ms: duration,
            created_at: "2026-05-01T10:00:00Z".to_owned(),
        }
    }

    #[test]
    fn history_csv_for_entries_emits_header_and_one_row_per_entry() {
        let entries = vec![
            history_entry(1, "Hello world", Some("iTerm2"), Some(2_500)),
            history_entry(2, "Second line", None, None),
        ];
        let csv = history_csv_for_entries(&entries).expect("csv ok");
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows; got: {csv:?}");
        assert_eq!(
            lines[0],
            "id,created_at,duration_ms,app_name,model,transcript"
        );
        assert!(lines[1].starts_with("1,2026-05-01T10:00:00Z,2500,iTerm2"));
        // None app / duration render as empty fields, not "null" or "None".
        assert_eq!(
            lines[2], "2,2026-05-01T10:00:00Z,,,ggml-base.bin,Second line",
            "got: {:?}",
            lines[2]
        );
    }

    #[test]
    fn history_csv_escapes_quotes_commas_and_newlines() {
        // RFC-4180 escape rules — embedded quotes get doubled,
        // commas / newlines force quote-wrapping. The csv crate
        // does the heavy lifting; this test pins that we route
        // through it correctly and don't accidentally hand-roll
        // an `escape` somewhere that would double-encode.
        let entries = vec![history_entry(
            7,
            "She said \"hi\", then\nleft.",
            Some("Notes,Inc"),
            None,
        )];
        let csv = history_csv_for_entries(&entries).expect("csv ok");
        // Transcript field: leading + trailing quote, embedded
        // quote doubled, newline preserved inside the quoted field.
        assert!(
            csv.contains("\"She said \"\"hi\"\", then\nleft.\""),
            "transcript should be quote-wrapped with doubled quotes\n{csv}"
        );
        // App-name field: comma triggers quoting too.
        assert!(
            csv.contains("\"Notes,Inc\""),
            "comma in app name should force quoting\n{csv}"
        );
    }

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
        emitter: std::sync::Arc<dyn crate::ipc::events::EventEmitter>,
        downloads: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<String, crate::transcription::download::CancelHandle>,
            >,
        >,
        models_dir: std::path::PathBuf,
    ) -> DiarizerDownloadDeps {
        DiarizerDownloadDeps {
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
        let emitter: std::sync::Arc<dyn crate::ipc::events::EventEmitter> =
            std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let deps = build_download_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        let result = download_diarizer_model_inner(deps, model.clone()).await;
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
        let emitter: std::sync::Arc<dyn crate::ipc::events::EventEmitter> =
            std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let model = make_test_diarizer_model("http://127.0.0.1:1/will-fail");
        let deps = build_download_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        download_diarizer_model_inner(deps, model.clone())
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
