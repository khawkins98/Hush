//! Dictation-focused IPC command handlers.
//!
//! This module owns the microphone / system-audio source listing and the
//! single-shot dictation lifecycle (`start_dictation` / `stop_dictation`) plus
//! the helper functions and focused tests that keep that path readable. Shared
//! IPC types such as [`super::IpcError`] stay in `commands/mod.rs` so other
//! command groups (notably Meeting Mode) can reuse them without depending on
//! dictation internals.

mod pipeline;

use std::sync::Arc;

use tauri::{AppHandle, Emitter as _, State};

use crate::audio::{AudioSource, AudioSourceListing};
use crate::dictionary::apply_replacements;
use crate::history::NewHistoryEntry;
use crate::ipc::AppState;

use super::{poisoned, DictationResult, IpcError, IpcResult};

// Pipeline helpers are scoped to this module — re-imported here so
// the existing call sites in `start_dictation` / `stop_dictation`
// continue to read `helper(...)` instead of `pipeline::helper(...)`.
// `pub(super)` items in pipeline.rs are visible at this scope.
use pipeline::{
    fire_ready_notification, load_replacement_rules, load_vocabulary_prompt, spawn_history_create,
    start_dictation_inner, stop_audio_capture, strip_whisper_brackets, take_foreground_snapshot,
    write_to_clipboard,
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
    // User-facing minimum: anything under 1 s is treated as an
    // accidental tap or background noise rather than a deliberate
    // dictation press. The user sees a dimmed "Too short" history entry
    // so they know the press was detected, but no transcription runs
    // and the clipboard is left untouched.
    //
    // Note: this absorbs the old 200 ms crash-prevention floor —
    // whisper.cpp can't process near-empty audio, and 1 s is well
    // above that technical floor while still catching accidental
    // presses. ("No" / "yes" / "k" run 250–400 ms but that's faster
    // than most users hold a push-to-talk key intentionally.)
    const MIN_DICTATION_MS: i64 = 1000;
    let too_short = match duration_ms {
        Some(ms) => ms < MIN_DICTATION_MS,
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
        spawn_history_create(
            Arc::clone(&state.data.history),
            NewHistoryEntry {
                transcript: String::new(),
                app_name: foreground.as_ref().map(|f| f.app_name.clone()),
                window_title: foreground.as_ref().map(|f| f.window_title.clone()),
                model: String::new(),
                duration_ms,
                ignored: true,
            },
        );
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
        // Hide the HUD before returning; every other error path in
        // this function does so, and omitting it leaves the "Processing…"
        // overlay stuck on screen (#803).
        crate::hud::hide_async(&app);
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
    // Transition the HUD to the "Done" state so the user sees a
    // green "Copied!" confirmation before the HUD self-dismisses
    // (~1.5 s later, driven by the frontend) (#669). Fallback to
    // hide_async if the event can't be emitted so the HUD never
    // gets stuck. On the error paths above we still hide immediately
    // — "Copied!" is only meaningful after a successful write.
    if let Err(e) = crate::hud::set_state(&app, crate::hud::HudState::Done) {
        tracing::warn!(error = ?e, "emit hud:state(done) failed; hiding directly");
        crate::hud::hide_async(&app);
    }
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
            ignored: false,
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
    // Clone the handle so the spawned task can emit without holding
    // a reference to the outer `app` borrow past the task boundary.
    let app_for_spawn = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = manager_handle
            .append_if_active(&meeting_text, utterance_duration_ms)
            .await
        {
            tracing::error!(error = ?e, "failed to append utterance to active meeting session");
            // Surface the failure to the frontend (#696). The transcript
            // itself landed on the clipboard; this is a secondary data-
            // loss path that deserves a visible warning rather than a
            // silent log entry. `ok()` swallows emit errors — if the
            // window is already closed the warning is moot.
            if let Err(emit_err) = app_for_spawn.emit(
                "dictation:meeting-append-failed",
                serde_json::json!({ "error": e.to_string() }),
            ) {
                tracing::warn!(error = ?emit_err, "emit dictation:meeting-append-failed failed");
            }
        }
    });

    Ok(DictationResult {
        text,
        foreground,
        duration_ms,
    })
}

#[cfg(test)]
mod tests;
