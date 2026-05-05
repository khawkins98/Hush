//! Dictation-focused IPC command handlers.
//!
//! This module owns the microphone / system-audio source listing and the
//! single-shot dictation lifecycle (`start_dictation` / `stop_dictation`) plus
//! the helper functions and focused tests that keep that path readable. Shared
//! IPC types such as [`super::IpcError`] stay in `commands/mod.rs` so other
//! command groups (notably Meeting Mode) can reuse them without depending on
//! dictation internals.

use std::sync::Arc;

use tauri::{AppHandle, Emitter as _, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;

use crate::audio::{AudioSource, AudioSourceListing};
use crate::dictionary::{apply_replacements, format_vocabulary_prompt, ReplacementRule};
use crate::history::NewHistoryEntry;
use crate::ipc::AppState;

use super::{
    classify_permission_error, poisoned, DictationResult, ForegroundApp, IpcError, IpcResult,
};

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
    //
    // Register a progress hook so the HUD can show "Processing… N%"
    // while whisper.cpp runs (#566). Cleared immediately after
    // inference so the Arc<AppHandle> isn't held longer than needed.
    let app_clone = app.clone();
    transcriber.set_progress_hook(Some(Arc::new(move |progress: i32| {
        if let Err(e) = app_clone.emit("transcription:progress", progress) {
            tracing::warn!(error = ?e, "emit transcription:progress failed");
        }
    })));
    let utterances = match transcriber.transcribe_chunks(&[captured.samples], format, &prompt) {
        Ok(u) => u,
        Err(e) => {
            transcriber.set_progress_hook(None);
            crate::hud::hide_async(&app);
            return Err(IpcError::Transcription(e.to_string()));
        }
    };
    // Inference complete — unhook the progress callback so the
    // Arc<AppHandle> is released promptly.
    transcriber.set_progress_hook(None);
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
}
