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

    state
        .audio
        .start_with_source(source)
        .map_err(|e| IpcError::Audio(e.to_string()))?;

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
        size_mb: model.size_mb,
        sha256: model.sha256,
        expected_path: path.to_string_lossy().into_owned(),
    })
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
#[tauri::command]
pub async fn download_diarizer_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<()> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let id = model.id.clone();
    let dest = state.models_dir.join(&model.filename);

    // Register a cancel handle + re-check on-disk presence inside
    // the same critical section. Reuses `AppState::downloads` —
    // same store the Whisper download path uses, keyed by id, so
    // the existing `model_cancel_download` IPC works for the
    // diarizer model with no extra wiring.
    //
    // The exists-check moved inside the lock to close a TOCTOU
    // race (audit-2): two rapid clicks could both pass the
    // exists-check before either took the lock. Holding the lock
    // for the existence test means a concurrent download that
    // just finished is observable as either "file exists now" or
    // "cancel handle still in flight" — caller gets a clean error
    // either way and we never start a duplicate download on top
    // of a freshly-finalized file.
    let cancel = crate::transcription::download::CancelHandle::new();
    {
        let mut guard = state.downloads.lock().map_err(poisoned)?;
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

    let app_for_task = app.clone();
    let id_for_task = id.clone();
    let url = model.download_url.clone();
    let sha = model.sha256.clone();
    let http = state.http.clone();
    let dest_for_task = dest.clone();
    // Hold an Arc-clone of the slot for post-download swap.
    let diarize_slot = std::sync::Arc::clone(&state.diarize_slot);
    let downloads_app = app.clone();

    tauri::async_runtime::spawn(async move {
        use tauri::{Emitter, Manager};
        let app_for_progress = app_for_task.clone();
        let id_for_progress = id_for_task.clone();
        let progress: Box<crate::transcription::download::ProgressCallback> =
            Box::new(move |update| {
                let _ = app_for_progress.emit(
                    "model:download-progress",
                    crate::ipc::commands::models::DownloadProgress {
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
        // failure. Same pattern the Whisper download uses.
        if let Some(state) = downloads_app.try_state::<AppState>() {
            if let Ok(mut guard) = state.downloads.lock() {
                guard.remove(&id_for_task);
            }
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
                // the `dest.exists()` guard at the top of
                // `download_diarizer_model`) and emit
                // `model:download-failed` with the load error,
                // so the UI surfaces it the same way as a
                // network or SHA-mismatch failure.
                match swap_diarizer_after_download(&diarize_slot, &dest_for_task) {
                    Ok(()) => {
                        let _ = app_for_task.emit(
                            "model:download-done",
                            crate::ipc::commands::models::DownloadStatus {
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
                        let _ = app_for_task.emit(
                            "model:download-failed",
                            crate::ipc::commands::models::DownloadStatus {
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
                let _ = app_for_task.emit(
                    "model:download-failed",
                    crate::ipc::commands::models::DownloadStatus {
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

/// Manual "Check for updates" probe (#223). Calls
/// [`crate::updater::check_for_updates`] against the app's shared
/// HTTP client; the result drives an in-app dialog. Idempotent —
/// the user can click as many times as they like; no background
/// polling lives here. Auto-update is the separate
/// [#10] follow-up.
///
/// [#10]: https://github.com/khawkins98/Hush/issues/10
#[tauri::command]
pub async fn check_for_updates(
    state: State<'_, AppState>,
) -> IpcResult<crate::updater::UpdateCheckResult> {
    crate::updater::check_for_updates(&state.http).await
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
}
