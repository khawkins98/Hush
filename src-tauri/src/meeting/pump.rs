//! Streaming pump task body for an active meeting session.
//!
//! Spawned by [`super::SessionManager::start_manual`] once the audio
//! handles + per-source [`StreamingTranscribeSession`]s are open;
//! drains every handle on a [`PUMP_TICK`] cadence, feeds the drained
//! samples into the corresponding streaming session, and dispatches
//! returned utterances. Lives in its own file (rather than inside
//! `manager.rs`) because the pump's body is the heaviest single
//! concentration of logic in the meeting module — extracting it
//! lets `manager.rs` stay focused on the session-state +
//! lifecycle methods (#431).
//!
//! ## What the pump does per tick
//!
//! 1. Drain audio for every source into a per-source scratch buffer
//!    (cheap; no inference). This frees the audio handle's internal
//!    buffer for the next ~500 ms of capture.
//! 2. For each source with a streaming inference session, move the
//!    session into [`tokio::task::spawn_blocking`] so whisper
//!    inference doesn't block a tokio worker, feed it the drained
//!    samples, drain its produced utterances, and put it back.
//! 3. Mirror the drained samples into the per-source diarizer audio
//!    buffer (#111 PR-F) so the diarizer can slice each utterance's
//!    audio out by `[started_at_ms, ended_at_ms)` later.
//! 4. Accumulate `(source_label, utterances, audio)` triples into a
//!    [`TickBucket`] vector. Once every source has produced its
//!    inference for the tick, run [`diarize_and_dispatch_merged`] to
//!    label the chronologically-merged batch and dispatch each
//!    source's labelled slice.
//!
//! On cancel, [`run_pump`] flushes each streaming session's tail
//! utterances (via `finish()`), runs them through the same merge-
//! sort-label-split pipeline, and clears the per-session partials map.
//!
//! ## Privacy invariant (load-bearing)
//!
//! The pump never persists raw audio. Drained samples live only as
//! long as the spawn-blocking inference closure that consumes them;
//! the diarizer audio buffer holds the canonical-format mirror used
//! for embedding extraction and is dropped at session end.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use zeroize::Zeroize;

use crate::audio::{apply_mic_gain, AudioCapture, AudioSession, AudioSource, CaptureFormat};
use crate::dictionary::apply_replacements;
use crate::transcription::{StreamingTranscribeSession, Transcribe, Utterance};

use super::events::{
    emit_audio_device_lost, emit_audio_device_restored, emit_meeting_source_failed,
    emit_meeting_tail_dropped, emit_utterance_append_failed,
};
use super::recovery::SourceRecoveryState;
use super::{MeetingSessionRepository, NewPersistedUtterance};

/// Pump tick interval — how often the streaming pump pulls samples
/// from each audio handle and feeds them into the per-source
/// streaming inference session. Inference itself happens internally
/// to the streaming session at its own cadence (the `infer_interval_ms`
/// config in `transcription::streaming`); this is just the rate at
/// which fresh audio reaches the streaming session's buffer.
///
/// 500 ms is a balance: short enough to keep the streaming session's
/// rolling window fresh (~6 ticks per inference at the default 3 s
/// inference interval), long enough to amortize the per-tick
/// `drain_into` round-trip + lock overhead. Tighter ticks would
/// raise CPU baseline noticeably; looser ticks would make the
/// streaming session's "I have new samples to consider" gate land
/// late and add jitter to the partial-update cadence.
pub(super) const PUMP_TICK: Duration = Duration::from_millis(500);

/// Maximum time `flush_sessions` waits for a streaming tail-finish
/// inference. 60 s gives slow machines enough headroom for a full
/// 30 s window while still preventing an infinite hang if whisper
/// stalls. The underlying spawn_blocking task is not cancelled on
/// timeout — it continues running and will eventually finish or
/// be cleaned up at process exit; the pump just stops waiting for it.
const STREAMING_FINISH_TIMEOUT: Duration = Duration::from_secs(60);

/// How many ticks between each reconnect-watcher check (#611).
/// At the default 500 ms tick this gives a ~5 s check cadence —
/// fast enough to feel responsive to a replug, slow enough to avoid
/// hammering `list_input_devices()` every tick.
const RECONNECT_CHECK_INTERVAL: u32 = 10;

/// Canonical capture format passed to the diarizer. Audio is
/// resampled to 16 kHz mono inside `AudioRollingBuffer::append`
/// (#300) so by the time the dispatch path hands chunks to the
/// diarizer they're all in this format — the parameter exists so
/// the trait doesn't grow a per-chunk format dimension.
const CANONICAL_FORMAT: CaptureFormat = CaptureFormat {
    sample_rate: 16_000,
    channels: 1,
};

/// Owned context handed to the pump task at spawn time. Bundles the
/// per-session state plus shared handles so the task signature stays
/// readable. Indices into `sources`, `handles`, and
/// `streaming_sessions` correspond to the same source.
pub(super) struct PumpContext {
    pub session_id: i64,
    pub repo: Arc<dyn MeetingSessionRepository>,
    pub sources: Vec<AudioSource>,
    /// One handle per source, `None` once a source is being swapped or
    /// has been permanently stopped. `Option` so the fallback path can
    /// `take()` the old handle (which triggers Drop → cpal `Cmd::Stop`)
    /// before calling `start_session` for the fallback — the cpal
    /// worker rejects a second `Cmd::Start` while the singleton mic
    /// Session slot is occupied.
    pub handles: Vec<Option<Box<dyn AudioSession>>>,
    /// One streaming inference session per source, parallel to
    /// `sources` and `handles`. `None` means streaming was not
    /// available for that source at start time (no transcriber, or
    /// the backend's `start_stream` errored). The pump treats those
    /// sources as audio-only — drains them so the buffer doesn't
    /// grow unbounded, but feeds nothing to inference.
    pub streaming_sessions: Vec<Option<Box<dyn StreamingTranscribeSession>>>,
    /// Shared in-memory partials store (the manager's field). The
    /// pump's per-tick dispatch updates entries keyed by speaker
    /// label as inference returns partials, and removes them when
    /// inference returns the matching final.
    pub partials: Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    pub cancel: Arc<AtomicBool>,
    /// Notify the frontend when a per-source path drops out
    /// mid-session. The pump fires this on the inference panic
    /// path and the streaming-feed/drain failure path that today
    /// only emit `tracing::warn!` lines the user never sees.
    pub event_emitter: Arc<dyn crate::events::EventEmitter>,
    /// Diarization seam (#111). The pump runs every batch of finals
    /// through this before stamping the source-derived label, so a
    /// non-Noop impl can override `"mic"` / `"system"` with
    /// per-speaker labels.
    pub diarize: Arc<dyn crate::diarization::Diarize>,
    /// Live microphone gain in dB (#531). Shared Arc from `RuntimeFlags`;
    /// applied to the drained capture-format samples before they enter
    /// both the streaming inference session and the diarizer audio buffer.
    pub mic_gain_db: Arc<AtomicU32>,
    /// Audio backend — used to open fallback handles on device loss
    /// and to list devices for the reconnect watcher (#611).
    pub audio: Arc<dyn AudioCapture>,
    /// Transcriber snapshot taken at session start. `None` if no
    /// model was loaded when the session opened. Passed into
    /// `start_stream` when recreating a streaming session after fallback
    /// or reconnect. Intentionally a snapshot (not the live TranscribeSlot)
    /// to preserve the existing invariant that model hot-swap affects
    /// the *next* session, not the current one.
    pub transcribe: Option<Arc<dyn Transcribe>>,
    /// Pump start time — used to compute per-source stream epoch offsets
    /// when a streaming session is recreated mid-meeting. Utterance
    /// timestamps from the new session are relative to the new stream's
    /// start; adding `stream_epoch_offsets_ms[i]` before persistence
    /// makes them relative to the meeting session start instead.
    pub session_start: Instant,
    /// Vocabulary prompt passed to `start_stream` at session-open and
    /// whenever a streaming session is recreated on device reconnect or
    /// fallback. Empty string disables prompt-biasing (same as the
    /// pre-#913 behaviour).
    pub vocab_prompt: String,
    /// Post-transcription find/replace rules applied to final
    /// utterances in [`tick_inference`] before they enter the dispatch
    /// pipeline. Empty slice is a no-op. Snapshotted at session-open
    /// so mid-session rule edits take effect on the next session.
    pub replacement_rules: Arc<Vec<crate::dictionary::ReplacementRule>>,
    /// Fired exactly once, by [`run_pump`], the moment every audio
    /// handle has been explicitly stopped (device released) and the
    /// tail samples fed into the streaming sessions — and *before* the
    /// slow tail flush (`finish()` + diarize + persist) begins.
    /// `stop_manual` awaits this so it can return promptly while the
    /// pump task keeps finalizing in the background (background
    /// finalization — see `docs/meeting-background-finalization-proposal.md`).
    /// `Option` so `run_pump` can `take()` it (a `oneshot::Sender` is
    /// not `Clone`); `None` after it has fired or for a pump that was
    /// spawned without a checkpoint (Drop-on-shutdown path).
    pub audio_released_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Cross-session speaker identity store (#667). The background
    /// finalization tail (moved out of `stop_manual`) reads the
    /// diarizer's session centroids and resolves them against known
    /// identities through this store. Cloned from the manager.
    pub speaker_store: Arc<dyn crate::speakers::SpeakerStore>,
    /// Whether speaker identity resolution is enabled (#667). Shared
    /// Arc from `RuntimeFlags::speaker_identity_enabled`. Read at the
    /// end of the pump to gate the centroid → identity resolution.
    pub speaker_identity_enabled: Arc<AtomicBool>,
}

/// Per-tick mutable working state for [`run_pump`]. Bundled so the
/// sub-functions (`tick_drain_sources`, `tick_inference`,
/// `tick_recovery_check`, `flush_sessions`) share one `&mut` rather
/// than a long list of individual `&mut` parameters (#655).
struct PumpTickState {
    /// Per-source scratch buffer reused across ticks.
    drain_buffers: Vec<Vec<f32>>,
    /// Rolling audio buffer in canonical 16 kHz mono (#111 PR-F).
    audio_buffers: Vec<crate::meeting::audio_buffer::AudioRollingBuffer>,
    /// Last successful drain format per source (used for zero-fill
    /// when `drain_into` fails so the diarizer buffer stays aligned).
    last_known_formats: Vec<Option<CaptureFormat>>,
    /// Accumulated final-utterance count per source (#533 diagnostic).
    final_counts: Vec<u64>,
    /// Accumulated blank-final count per source (#533 diagnostic).
    blank_counts: Vec<u64>,
    /// Per-source device-loss / reconnect state machine.
    recovery_states: Vec<SourceRecoveryState>,
    /// Stream epoch offset in ms per source. Non-zero after a
    /// mid-session handle swap (fallback or reconnect) so that
    /// stream-relative timestamps become meeting-relative.
    stream_epoch_offsets_ms: Vec<u64>,
    /// Whether the first-drain RMS diagnostic was already logged
    /// per source (#533).
    first_drain_logged: Vec<bool>,
}

impl PumpTickState {
    fn new(ctx: &PumpContext) -> Self {
        let n = ctx.handles.len();
        debug_assert_eq!(n, ctx.sources.len());
        debug_assert_eq!(n, ctx.streaming_sessions.len());
        PumpTickState {
            drain_buffers: (0..n).map(|_| Vec::new()).collect(),
            audio_buffers: (0..n)
                .map(|_| crate::meeting::audio_buffer::AudioRollingBuffer::new())
                .collect(),
            last_known_formats: vec![None; n],
            final_counts: vec![0; n],
            blank_counts: vec![0; n],
            recovery_states: vec![SourceRecoveryState::Active; n],
            stream_epoch_offsets_ms: vec![0; n],
            first_drain_logged: vec![false; n],
        }
    }
}

impl Drop for PumpTickState {
    fn drop(&mut self) {
        // Zeroize raw PCM scratch buffers so meeting audio doesn't linger
        // in heap memory after the pump ends (#869).
        for buf in &mut self.drain_buffers {
            buf.zeroize();
        }
    }
}

/// Pump task body. Loops on a `PUMP_TICK` cadence: drain each audio
/// handle into its per-source buffer, feed the buffer into the
/// streaming inference session, dispatch returned utterances
/// (partials → in-memory map, finals → DB). On cancel, calls
/// `finish()` on each streaming session to flush the tail and
/// persists those finals.
///
/// All errors are logged and swallowed — the pump is fire-and-forget
/// from the spawn point's perspective, and a transient drain or
/// inference failure shouldn't tear down the user's session.
pub(super) async fn run_pump(mut ctx: PumpContext) {
    // Reset diarizer cluster state at the top of every session so speaker
    // IDs from a previous meeting don't bleed into this one (#794).
    ctx.diarize.reset();

    tracing::info!(
        session_id = ctx.session_id,
        sources = ?ctx.sources.iter().map(|s| s.kind_label()).collect::<Vec<_>>(),
        streaming_sources = ctx.streaming_sessions.iter().filter(|s| s.is_some()).count(),
        "meeting pump: starting"
    );

    // Log once if any source has no streaming session — this is the
    // first thing to check when utterances are 0. Happens when the
    // transcription backend is unavailable at start time.
    for (i, session) in ctx.streaming_sessions.iter().enumerate() {
        if session.is_none() {
            tracing::warn!(
                session_id = ctx.session_id,
                source_kind = ctx.sources[i].kind_label(),
                "meeting pump: no streaming transcription session for source; \
                 audio will be drained but not transcribed"
            );
        }
    }

    let mut state = PumpTickState::new(&ctx);
    let mut tick_count: u32 = 0;
    let mut tick_buckets: Vec<TickBucket> = Vec::new();

    loop {
        // Sleep with periodic cancel polls. The pump tick is shorter
        // than the previous chunk-and-restart cycle (500 ms vs 10 s),
        // so the per-poll cancel-flag check happens on every tick
        // boundary directly.
        if ctx.cancel.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(PUMP_TICK).await;
        if ctx.cancel.load(Ordering::Acquire) {
            break;
        }

        let tick_formats = tick_drain_sources(&mut ctx, &mut state);

        tick_inference(&mut ctx, &mut state, &tick_formats, &mut tick_buckets).await;

        if !tick_buckets.is_empty() {
            diarize_and_dispatch_merged(
                ctx.session_id,
                std::mem::take(&mut tick_buckets),
                &ctx.diarize,
                &ctx.partials,
                &ctx.repo,
                ctx.event_emitter.as_ref(),
            )
            .await;
        }

        tick_count = tick_count.wrapping_add(1);
        if tick_count % RECONNECT_CHECK_INTERVAL == 0 {
            tick_recovery_check(&mut ctx, &mut state);
        }
    }

    // One final drain + inference pass to capture audio that arrived in
    // the hardware ring buffer since the last tick drained it (#797).
    // `tick_drain_sources` reads the ring buffer into `drain_buffers`.
    // `tick_inference` feeds those samples into each streaming session
    // and emits any utterances it finds into `tail_buckets`. Then the
    // regular `flush_sessions` call below calls `session.finish()` to
    // flush the streaming session's own internal buffer — those finals
    // also land in `tail_buckets` and are dispatched together.
    let final_tick_formats = tick_drain_sources(&mut ctx, &mut state);
    tick_inference(&mut ctx, &mut state, &final_tick_formats, &mut tick_buckets).await;
    if !tick_buckets.is_empty() {
        diarize_and_dispatch_merged(
            ctx.session_id,
            std::mem::take(&mut tick_buckets),
            &ctx.diarize,
            &ctx.partials,
            &ctx.repo,
            ctx.event_emitter.as_ref(),
        )
        .await;
    }

    // Explicit, ack-waited release of every audio handle BEFORE the slow
    // tail flush (background finalization, see the proposal). `AudioSession::stop()`
    // round-trips `Cmd::Stop` through the cpal/SCK worker and *returns the
    // final drained buffer* — so the device singleton is actually free the
    // moment this returns (not "eventually, when ctx drops"), and no tail
    // audio is lost across the drain→stop gap. We feed those returned tail
    // samples into the matching streaming session exactly as `tick_inference`
    // feeds drained samples (mic-gain → diarizer-buffer append → `feed`) so
    // the subsequent `finish()` still sees them. Relying on the handle's
    // `Drop` instead would (a) not wait for the worker to process the Stop,
    // so a new capture could still hit "recording already in progress", and
    // (b) discard the returned `CapturedAudio` entirely.
    release_audio_handles(&mut ctx, &mut state);

    // Audio is released. Tell the frontend so it clears `activeId`
    // (unblocking dictation/PTT) and shows a "finishing…" indicator while
    // the background tail processes.
    crate::meeting::events::emit_meeting_finalizing(ctx.event_emitter.as_ref(), ctx.session_id);

    // Signal the audio-released checkpoint so `stop_manual` can return.
    // Everything past this point runs in the background; the receiver may
    // have dropped if `stop_manual` hit its timeout fallback, hence the
    // ignored send result.
    if let Some(tx) = ctx.audio_released_tx.take() {
        let _ = tx.send(());
    }

    // Tail flush: finish each streaming session, merge-sort-label-split, dispatch.
    let mut tail_buckets: Vec<TickBucket> = Vec::new();
    flush_sessions(&mut ctx, &mut state, &mut tail_buckets).await;
    if !tail_buckets.is_empty() {
        diarize_and_dispatch_merged(
            ctx.session_id,
            tail_buckets,
            &ctx.diarize,
            &ctx.partials,
            &ctx.repo,
            ctx.event_emitter.as_ref(),
        )
        .await;
    }

    // Belt-and-braces: clear partials for this session id.
    if let Ok(mut guard) = ctx.partials.write() {
        guard.remove(&ctx.session_id);
    }

    // Per-source final-utterance summary (#533 diagnostic).
    for (source, (count, blanks)) in ctx
        .sources
        .iter()
        .zip(state.final_counts.iter().zip(state.blank_counts.iter()))
    {
        let real_finals = count.saturating_sub(*blanks);
        tracing::info!(
            session_id = ctx.session_id,
            source_kind = source.kind_label(),
            finals = count,
            real_finals = real_finals,
            blank_finals = blanks,
            "meeting pump: per-source utterance summary (#533 diagnostic)"
        );
    }

    // Background finalization tail (moved out of `stop_manual`, see the
    // proposal). Runs *after* the tail flush so all utterances — including
    // the tail `finish()` finals — are in the DB before identities are
    // resolved and the row is closed.
    //
    // Cross-session speaker identity resolution (#667): reads THIS session's
    // diarizer cluster centroids and matches them against known identities.
    // Safe to run here because a new meeting start awaits this finalization
    // (the manager's single `finalizing` lane), so nothing has `reset()` the
    // shared diarizer between this pump's start-of-session reset and now.
    // Snapshot the centroids *before* any future pump resets them.
    if ctx.speaker_identity_enabled.load(Ordering::Relaxed) {
        let centroids = ctx.diarize.session_centroids();
        if !centroids.is_empty() {
            crate::meeting::lifecycle::resolve_speaker_identities(
                Arc::clone(&ctx.speaker_store),
                ctx.session_id,
                centroids,
            )
            .await;
        }
    }

    // Close the session row. Behaviour change vs the old foreground close:
    // a failure here can no longer surface a retry to an already-returned
    // Stop — it is logged and `reconcile_orphan_sessions` closes the row on
    // next launch (#249), the same guarantee as a crash.
    if let Err(e) = ctx.repo.close_session(ctx.session_id).await {
        tracing::warn!(
            error = ?e,
            session_id = ctx.session_id,
            "meeting finalization: close_session failed; orphan reconcile will close it next launch"
        );
    }

    // Emit session-ended so the frontend clears the "finishing…" indicator.
    // Emitted last so a `meeting_session_get` fired from the frontend's
    // handler sees `ended_at` already set in the DB.
    crate::meeting::events::emit_meeting_session_ended(ctx.event_emitter.as_ref(), ctx.session_id);

    tracing::info!(session_id = ctx.session_id, "meeting pump: stopped");
}

/// Explicitly stop every audio handle (ack-waited) and feed the returned
/// tail samples into the matching streaming session — mirroring exactly how
/// [`tick_inference`] feeds drained samples (mic-gain on the mic source,
/// diarizer-buffer append for slice alignment, then `feed`). This frees the
/// cpal/SCK capture singleton *now* and guarantees no tail-loss across the
/// final-drain→stop gap. Called once, on the cancel path, before the tail
/// flush. Errors are logged and swallowed — a stop failure on one source
/// must not block releasing the others or the rest of finalization.
fn release_audio_handles(ctx: &mut PumpContext, state: &mut PumpTickState) {
    #[allow(clippy::needless_range_loop)] // parallel index into handles/sources/streaming_sessions
    for i in 0..ctx.handles.len() {
        let Some(handle) = ctx.handles[i].take() else {
            continue;
        };
        let captured = match handle.stop() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    source_kind = ctx.sources[i].kind_label(),
                    session_id = ctx.session_id,
                    "meeting pump: audio stop on release failed"
                );
                continue;
            }
        };
        let mut samples = captured.samples;
        if samples.is_empty() {
            continue;
        }
        // Mirror tick_inference: apply mic gain only to the microphone
        // source (system-audio must not be distorted, #865).
        if matches!(ctx.sources[i], AudioSource::Microphone(_)) {
            let gain_db = f32::from_bits(ctx.mic_gain_db.load(Ordering::Relaxed));
            apply_mic_gain(&mut samples, gain_db);
        }
        // Mirror the tail samples into the diarizer rolling buffer so the
        // tail-flush utterance slicing stays aligned (same as tick_inference).
        state.audio_buffers[i].append(&samples, captured.format);
        // Feed into the streaming session so `finish()` sees the tail.
        if let Some(session) = ctx.streaming_sessions[i].as_mut() {
            if let Err(e) = session.feed(&samples) {
                tracing::warn!(
                    error = ?e,
                    source_kind = ctx.sources[i].kind_label(),
                    session_id = ctx.session_id,
                    "meeting pump: feeding released tail samples failed; tail may be short"
                );
            }
        }
        samples.zeroize(); // (#869/#930) don't leave PCM lingering in heap
    }
}

fn tick_drain_sources(
    ctx: &mut PumpContext,
    state: &mut PumpTickState,
) -> Vec<Option<CaptureFormat>> {
    // Drain audio for every source first (cheap, no inference),
    // then run inference per source. The drain step takes
    // microseconds; the inference step takes milliseconds-to-
    // seconds inside the streaming session's `drain` if a new
    // inference window has matured. Splitting the loop bounds
    // each source's audio buffer to the tick window plus the
    // few-ms drain.
    let mut tick_formats: Vec<Option<CaptureFormat>> = vec![None; ctx.handles.len()];
    #[allow(clippy::needless_range_loop)]
    for i in 0..ctx.sources.len() {
        // Skip sources with no active handle (Dead or LostAwaitingReconnect).
        // Calling drain_into on a dead handle produces the same error every
        // tick — running through the whole inference / zero-fill /
        // event-emission path on every tick would spam logs.
        let Some(handle) = ctx.handles[i].as_ref() else {
            continue;
        };
        let buf = &mut state.drain_buffers[i];
        // Zeroize before reuse: buf holds the previous tick's audio bytes
        // from the last spawn_blocking round-trip.  clear() sets len = 0
        // without touching the backing allocation, so those samples would
        // survive until the next tick's drain overwrites them.
        buf.zeroize();
        match handle.drain_into(buf) {
            Ok(format) => {
                tracing::debug!(
                    session_id = ctx.session_id,
                    source_kind = ctx.sources[i].kind_label(),
                    samples = buf.len(),
                    "meeting pump: drained"
                );
                tick_formats[i] = Some(format);
                state.last_known_formats[i] = Some(format);
                // Log first-drain RMS once per source (#533 diagnostic).
                // Near-zero RMS = device opened but producing silence;
                // non-zero = audio flowing, so any 0-utterance result
                // means Whisper's no_speech_thold is gating the output.
                if !state.first_drain_logged[i] {
                    let rms = if buf.is_empty() {
                        0.0
                    } else {
                        let sum_sq: f64 = buf.iter().map(|s| (*s as f64) * (*s as f64)).sum();
                        (sum_sq / buf.len() as f64).sqrt()
                    };
                    tracing::info!(
                        session_id = ctx.session_id,
                        source_kind = ctx.sources[i].kind_label(),
                        samples = buf.len(),
                        rms,
                        "meeting pump: first-drain RMS (#533 diagnostic; <0.001 suggests capture silence)"
                    );
                    state.first_drain_logged[i] = true;
                }
            }
            Err(e) => {
                // Log the first-drain diagnostic even on failure so
                // a device that never returns audio produces a line.
                if !state.first_drain_logged[i] {
                    tracing::info!(
                        session_id = ctx.session_id,
                        source_kind = ctx.sources[i].kind_label(),
                        samples = 0usize,
                        rms = 0.0f64,
                        "meeting pump: first-drain RMS (#533 diagnostic; drain failed)"
                    );
                    state.first_drain_logged[i] = true;
                }
                // Device-disconnect detection (#587 / #611). The cpal
                // backend signals device-gone via the typed
                // [`crate::audio::DeviceLost`] error wrapped in
                // anyhow. Distinguishing it from transient drain
                // failures matters because:
                //
                // - A genuine disconnect will keep returning the
                //   same error every tick — zero-filling forever
                //   is wasteful and surfaces no signal.
                // - The user needs an unambiguous "your mic
                //   disconnected" signal, not a generic
                //   "drain_into failed" warn-log.
                //
                // For microphone sources we attempt auto-fallback to
                // the system default first (#611). For SystemAudio the
                // source is permanently Dead — there is no "system
                // default system audio" fallback concept.
                //
                // Handle swap ordering: we must stop the old handle
                // BEFORE calling start_session for the fallback.
                // The cpal worker rejects Cmd::Start while its
                // singleton mic Session slot is occupied, and
                // DrainBuffer returning DeviceLost does NOT release
                // the slot. Drop triggers CpalMicSessionHandle::Drop
                // which sends Cmd::Stop via the same mpsc channel;
                // FIFO ordering guarantees Stop is processed before
                // the subsequent Start from start_session.
                if let Some(lost) = e.downcast_ref::<crate::audio::DeviceLost>() {
                    let lost_device = lost.device.clone();
                    match &ctx.sources[i] {
                        AudioSource::Microphone(_) => {
                            tracing::warn!(
                                device = %lost_device,
                                source_kind = ctx.sources[i].kind_label(),
                                session_id = ctx.session_id,
                                "meeting pump: mic disconnected; attempting fallback"
                            );
                            let original_source = ctx.sources[i].clone();
                            // Release the dead handle first (see comment above).
                            drop(ctx.handles[i].take());
                            ctx.streaming_sessions[i] = None;

                            let fallback_source = AudioSource::default_microphone();
                            match open_source_handle(
                                &ctx.audio,
                                ctx.transcribe.as_ref(),
                                &fallback_source,
                                &ctx.vocab_prompt,
                            ) {
                                Ok((new_handle, new_stream)) => {
                                    let fallback_device_name = ctx
                                        .audio
                                        .list_input_devices()
                                        .ok()
                                        .and_then(|devs| {
                                            devs.into_iter().find(|d| d.is_default).map(|d| d.name)
                                        })
                                        .unwrap_or_else(|| "default microphone".to_owned());
                                    tracing::info!(
                                        fallback = %fallback_device_name,
                                        source_kind = ctx.sources[i].kind_label(),
                                        session_id = ctx.session_id,
                                        "meeting pump: fallback opened; continuing capture"
                                    );
                                    state.stream_epoch_offsets_ms[i] =
                                        ctx.session_start.elapsed().as_millis() as u64;
                                    state.audio_buffers[i] =
                                        crate::meeting::audio_buffer::AudioRollingBuffer::new();
                                    state.last_known_formats[i] = None;
                                    state.first_drain_logged[i] = false;
                                    if let Ok(mut guard) = ctx.partials.write() {
                                        if let Some(per_session) = guard.get_mut(&ctx.session_id) {
                                            per_session.remove(ctx.sources[i].speaker_tag());
                                        }
                                    }
                                    ctx.handles[i] = Some(new_handle);
                                    ctx.streaming_sessions[i] = new_stream;
                                    // If transcription was active but start_stream
                                    // failed, audio capture is running but whisper
                                    // won't produce utterances — surface this.
                                    if ctx.transcribe.is_some()
                                        && ctx.streaming_sessions[i].is_none()
                                    {
                                        emit_meeting_source_failed(
                                            ctx.event_emitter.as_ref(),
                                            ctx.session_id,
                                            ctx.sources[i].speaker_tag(),
                                            "audio capture restored at fallback device but transcription could not restart",
                                            false,
                                        );
                                    }
                                    state.recovery_states[i] = SourceRecoveryState::Fallback {
                                        original_source,
                                        original_device_name: lost_device.clone(),
                                    };
                                    emit_audio_device_lost(
                                        ctx.event_emitter.as_ref(),
                                        ctx.session_id,
                                        ctx.sources[i].speaker_tag(),
                                        &lost_device,
                                        Some(fallback_device_name.as_str()),
                                    );
                                }
                                Err(fe) => {
                                    tracing::warn!(
                                        error = ?fe,
                                        source_kind = ctx.sources[i].kind_label(),
                                        session_id = ctx.session_id,
                                        "meeting pump: fallback open failed; awaiting reconnect"
                                    );
                                    state.recovery_states[i] =
                                        SourceRecoveryState::LostAwaitingReconnect {
                                            original_source,
                                            original_device_name: lost_device.clone(),
                                        };
                                    emit_audio_device_lost(
                                        ctx.event_emitter.as_ref(),
                                        ctx.session_id,
                                        ctx.sources[i].speaker_tag(),
                                        &lost_device,
                                        None,
                                    );
                                }
                            }
                        }
                        _ => {
                            // SystemAudio and any future non-mic sources
                            // have no fallback concept; mark Dead.
                            tracing::error!(
                                device = %lost_device,
                                source_kind = ctx.sources[i].kind_label(),
                                session_id = ctx.session_id,
                                "meeting pump: audio device disconnected; ending source"
                            );
                            emit_meeting_source_failed(
                                ctx.event_emitter.as_ref(),
                                ctx.session_id,
                                // speaker_tag() ("mic"/"system") matches the
                                // frontend's "mic" branch and the lifecycle
                                // startup emit (#810/#815).
                                ctx.sources[i].speaker_tag(),
                                "audio device disconnected mid-session",
                                true,
                            );
                            drop(ctx.handles[i].take());
                            state.recovery_states[i] = SourceRecoveryState::Dead;
                            ctx.streaming_sessions[i] = None;
                        }
                    }
                    continue;
                }
                tracing::warn!(
                    error = ?e,
                    source_kind = ctx.sources[i].kind_label(),
                    session_id = ctx.session_id,
                    "meeting pump: drain_into failed for tick"
                );
                // Zero-fill the diarizer buffer for this tick (#553).
                // The streaming transcription session continues advancing
                // its internal timeline even when drain fails, so without
                // a compensating append the diarizer buffer falls behind
                // and slice_ms() returns misaligned audio for subsequent
                // utterances. Silence is a better approximation than a gap.
                if let Some(fmt) = state.last_known_formats[i] {
                    let zero_samples = (fmt.sample_rate as f64
                        * PUMP_TICK.as_secs_f64()
                        * fmt.channels as f64) as usize;
                    let zeros = vec![0f32; zero_samples];
                    state.audio_buffers[i].append(&zeros, fmt);
                    tracing::debug!(
                        session_id = ctx.session_id,
                        source_kind = ctx.sources[i].kind_label(),
                        zero_samples,
                        "meeting pump: zero-filled diarizer buffer to compensate for drain failure"
                    );
                }
            }
        }
    }

    tick_formats
}

async fn tick_inference(
    ctx: &mut PumpContext,
    state: &mut PumpTickState,
    tick_formats: &[Option<CaptureFormat>],
    tick_buckets: &mut Vec<TickBucket>,
) {
    // For each source with a streaming session, feed the drained
    // samples and run an inference tick. Move the session into
    // `spawn_blocking` so whisper inference doesn't block the
    // tokio worker; the helper returns the session along with
    // its drained utterances so we can put it back.
    //
    // Index loop rather than `iter().enumerate()` because we
    // mutate three parallel `Vec`s — `streaming_sessions`,
    // `drain_buffers`, and `sources` — and need split-borrow
    // semantics on each. Restructuring to a single iterator
    // would either require interior mutability on each slot
    // or unsafe pointer arithmetic; the indexed loop is the
    // clearest shape for this pattern.
    #[allow(clippy::needless_range_loop)]
    for i in 0..ctx.sources.len() {
        // Skip sources without a streaming session — drained
        // samples are discarded. Logging only on the first
        // skipped tick per source to avoid flooding the
        // tracing layer (every 500 ms for the whole session).
        if ctx.streaming_sessions[i].is_none() {
            continue;
        }
        // Take the session out so we can move it into
        // spawn_blocking. The `Option` slot stays None until we
        // put it back at the bottom of this iteration.
        // Defensive take: pre-#246 this was `.unwrap()`, but
        // a future refactor that drains in a different order
        // would panic the pump task. Skip the source for this
        // tick if the slot was already taken.
        let Some(session) = ctx.streaming_sessions[i].take() else {
            tracing::warn!(
                source_kind = ctx.sources[i].speaker_tag(),
                "meeting pump: streaming session slot already empty; skipping tick"
            );
            continue;
        };

        // Apply mic gain to the drained raw samples (#531) before
        // feeding them to both the diarizer buffer and the streaming
        // inference session. Only applies to the microphone source —
        // mic_gain_db is a microphone-only setting and must not distort
        // system-audio samples captured from the tap (#865).
        if matches!(ctx.sources[i], AudioSource::Microphone(_)) {
            let gain_db = f32::from_bits(ctx.mic_gain_db.load(Ordering::Relaxed));
            apply_mic_gain(&mut state.drain_buffers[i], gain_db);
        }

        let samples = std::mem::take(&mut state.drain_buffers[i]);
        let source_label = ctx.sources[i].speaker_tag().to_owned();
        let session_id = ctx.session_id;

        // Mirror the drained samples into the diarizer's rolling
        // buffer (#111 PR-F). Done before the `samples` move so
        // we don't have to clone — `audio_buffer::append` does
        // its own resample/downmix copy. Skip if drain_into
        // failed and we don't know the format for this tick.
        if let Some(format) = tick_formats[i] {
            state.audio_buffers[i].append(&samples, format);
        }

        // spawn_blocking isolates whisper inference from the tokio
        // worker pool. infer_start/elapsed_ms are recorded here so
        // the "inference tick" log can distinguish "pump ran, whisper
        // was slow" (elapsed_ms large) from "pump ran, gate never
        // opened" (no "inference ran" lines in streaming.rs at all).
        let infer_start = std::time::Instant::now();
        let join =
            tokio::task::spawn_blocking(
                move || -> (
                    Box<dyn StreamingTranscribeSession>,
                    Vec<f32>,
                    Result<Vec<Utterance>>,
                ) {
                    let mut session = session;
                    if !samples.is_empty() {
                        if let Err(e) = session.feed(&samples) {
                            return (session, samples, Err(e));
                        }
                    }
                    let result = session.drain();
                    (session, samples, result)
                },
            )
            .await;
        let infer_elapsed_ms = infer_start.elapsed().as_millis();

        let (returned_session, returned_buf, drain_result) = match join {
            Ok(triple) => triple,
            Err(join_err) => {
                tracing::error!(
                    error = ?join_err,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: streaming inference task panicked; \
                     leaving streaming disabled for this source for the rest of the session"
                );
                // Session is gone (panicked closure dropped it).
                // Leave the slot None so subsequent ticks skip
                // this source. Notify the frontend so the panel
                // can surface "this source dropped" rather than
                // silently rendering "still recording".
                emit_meeting_source_failed(
                    ctx.event_emitter.as_ref(),
                    session_id,
                    &source_label,
                    "transcription task panicked",
                    false,
                );
                continue;
            }
        };

        // Restore the session + buffer for the next tick.
        ctx.streaming_sessions[i] = Some(returned_session);
        state.drain_buffers[i] = returned_buf;

        let utterances = match drain_result {
            Ok(u) => {
                tracing::debug!(
                    session_id,
                    source_kind = source_label,
                    utterances = u.len(),
                    elapsed_ms = infer_elapsed_ms,
                    // utterances = 0 here + "inference ran" in streaming.rs
                    // means the gate opened but produced nothing. Cross with
                    // raw_segments from streaming.rs to distinguish "whisper
                    // filtered via no_speech_thold" from "streaming gate
                    // never opened".
                    "meeting pump: inference tick"
                );
                u
            }
            Err(e) => {
                let reason = format!("{e}");
                tracing::warn!(
                    error = ?e,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: streaming feed/drain failed for tick"
                );
                // Drop the session so subsequent ticks skip this
                // source — keeping a wedged session in the slot
                // would loop the same warning every 500 ms for
                // the rest of the meeting.
                ctx.streaming_sessions[i] = None;
                emit_meeting_source_failed(
                    ctx.event_emitter.as_ref(),
                    session_id,
                    &source_label,
                    &reason,
                    false,
                );
                continue;
            }
        };

        // Slice each utterance's audio out of the rolling
        // buffer for the diarizer (#111 PR-F). Parallel to
        // `utterances`. Empty `Vec` if the utterance's audio
        // dropped past the buffer horizon (very rare — would
        // require a >30 s utterance + late drain).
        // Audio is sliced using LOCAL stream-relative timestamps
        // before the epoch offset is applied — the rolling buffer
        // index matches the per-stream clock.
        let audio: Vec<Vec<f32>> = utterances
            .iter()
            .map(|u| state.audio_buffers[i].slice_ms(u.started_at_ms, u.ended_at_ms))
            .collect();

        // Apply per-source epoch offset (#611): when a streaming
        // session is recreated mid-meeting (fallback or reconnect),
        // its internal timestamps restart from 0. Adding the offset
        // makes persisted timestamps relative to meeting start.
        let epoch_ms = state.stream_epoch_offsets_ms[i];
        let utterances: Vec<Utterance> = if epoch_ms > 0 {
            utterances
                .into_iter()
                .map(|mut u| {
                    u.started_at_ms += epoch_ms;
                    u.ended_at_ms += epoch_ms;
                    u
                })
                .collect()
        } else {
            utterances
        };

        // Accumulate this source's utterances into the tick
        // bucket. The per-tick `diarize_and_dispatch_merged`
        // call below runs the diarizer once over the merged +
        // chronologically-sorted batch, then splits the labelled
        // result back per source for dispatch (#206).
        // Count finals before moving utterances into the bucket
        // (#533 diagnostic — logged at session end).
        state.final_counts[i] += utterances.iter().filter(|u| u.is_final).count() as u64;
        state.blank_counts[i] += utterances
            .iter()
            .filter(|u| u.is_final && (u.text == "[BLANK_AUDIO]" || u.text.trim().is_empty()))
            .count() as u64;

        // Apply post-transcription replacement rules to final utterances (#913).
        // Partials are live-updating text the user sees before finalisation;
        // applying rules to partials would cause flicker. Only finals are
        // processed here, mirroring the dictation path's apply_replacements call.
        let utterances: Vec<Utterance> = if !ctx.replacement_rules.is_empty() {
            utterances
                .into_iter()
                .map(|mut u| {
                    if u.is_final {
                        u.text = apply_replacements(&u.text, &ctx.replacement_rules);
                    }
                    u
                })
                .collect()
        } else {
            utterances
        };

        tick_buckets.push(TickBucket {
            source_label,
            utterances,
            audio,
        });
    }
}

fn tick_recovery_check(ctx: &mut PumpContext, state: &mut PumpTickState) {
    // Reconnect watcher: every RECONNECT_CHECK_INTERVAL ticks,
    // scan the device list for any source that is in Fallback or
    // LostAwaitingReconnect state and check whether the original
    // device has come back (#611). List devices once per interval
    // and reuse across all sources to avoid redundant OS queries.
    let maybe_devs = ctx.audio.list_input_devices().ok();
    if let Some(devs) = maybe_devs {
        let dev_names: std::collections::HashSet<String> =
            devs.iter().map(|d| d.name.clone()).collect();
        for i in 0..ctx.sources.len() {
            let Some((original_source, original_device_name)) =
                state.recovery_states[i].reconnect_target()
            else {
                continue;
            };

            if !dev_names.contains(&original_device_name) {
                continue;
            }

            // Original device is back. Drop any fallback handle
            // first (same FIFO-ordering reason as the DeviceLost arm).
            drop(ctx.handles[i].take());
            ctx.streaming_sessions[i] = None;

            match open_source_handle(
                &ctx.audio,
                ctx.transcribe.as_ref(),
                &original_source,
                &ctx.vocab_prompt,
            ) {
                Ok((new_handle, new_stream)) => {
                    tracing::info!(
                        device = %original_device_name,
                        source_kind = ctx.sources[i].kind_label(),
                        session_id = ctx.session_id,
                        "meeting pump: original device reconnected; restoring"
                    );
                    state.stream_epoch_offsets_ms[i] =
                        ctx.session_start.elapsed().as_millis() as u64;
                    state.audio_buffers[i] =
                        crate::meeting::audio_buffer::AudioRollingBuffer::new();
                    state.last_known_formats[i] = None;
                    state.first_drain_logged[i] = false;
                    if let Ok(mut guard) = ctx.partials.write() {
                        if let Some(per_session) = guard.get_mut(&ctx.session_id) {
                            per_session.remove(ctx.sources[i].speaker_tag());
                        }
                    }
                    ctx.handles[i] = Some(new_handle);
                    ctx.streaming_sessions[i] = new_stream;
                    // Same guard as the fallback path: if start_stream failed
                    // the audio is flowing but whisper is dark.
                    if ctx.transcribe.is_some() && ctx.streaming_sessions[i].is_none() {
                        emit_meeting_source_failed(
                            ctx.event_emitter.as_ref(),
                            ctx.session_id,
                            ctx.sources[i].speaker_tag(),
                            "audio capture restored but transcription could not restart",
                            false,
                        );
                    }
                    state.recovery_states[i] = SourceRecoveryState::Active;
                    emit_audio_device_restored(
                        ctx.event_emitter.as_ref(),
                        ctx.session_id,
                        ctx.sources[i].speaker_tag(),
                        &original_device_name,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        device = %original_device_name,
                        source_kind = ctx.sources[i].kind_label(),
                        session_id = ctx.session_id,
                        "meeting pump: reconnect attempt failed despite device being listed"
                    );
                }
            }
        }
    }
}

async fn flush_sessions(
    ctx: &mut PumpContext,
    state: &mut PumpTickState,
    tail_buckets: &mut Vec<TickBucket>,
) {
    // Cancel — flush each streaming session. `finish` drains
    // anything still in the rolling window as finals; we persist
    // those before returning so `stop_manual` sees the
    // tail-of-conversation utterances. Same merge-sort-label-split
    // shape as the per-tick path (#206) so the tail flush can't
    // re-introduce the per-source independent-A/B regression.
    #[allow(clippy::needless_range_loop)] // see explanation in the tick loop above
    for i in 0..ctx.sources.len() {
        let Some(session) = ctx.streaming_sessions[i].take() else {
            continue;
        };
        let source_label = ctx.sources[i].speaker_tag().to_owned();
        let session_id = ctx.session_id;
        let join = tokio::time::timeout(
            STREAMING_FINISH_TIMEOUT,
            tokio::task::spawn_blocking(move || session.finish()),
        )
        .await;
        let finals = match join {
            Err(_elapsed) => {
                tracing::warn!(
                    session_id,
                    source_kind = source_label,
                    timeout_secs = STREAMING_FINISH_TIMEOUT.as_secs(),
                    "meeting pump: streaming finish timed out; tail dropped (blocking task still running)"
                );
                emit_meeting_tail_dropped(ctx.event_emitter.as_ref(), session_id, &source_label);
                continue;
            }
            Ok(Err(join_err)) => {
                tracing::error!(
                    error = ?join_err,
                    session_id,
                    "meeting pump: streaming finish task panicked"
                );
                emit_meeting_tail_dropped(ctx.event_emitter.as_ref(), session_id, &source_label);
                continue;
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(
                    error = ?e,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: streaming finish failed; tail dropped"
                );
                emit_meeting_tail_dropped(ctx.event_emitter.as_ref(), session_id, &source_label);
                continue;
            }
            Ok(Ok(Ok(u))) => u,
        };
        let tail_audio: Vec<Vec<f32>> = finals
            .iter()
            .map(|u| state.audio_buffers[i].slice_ms(u.started_at_ms, u.ended_at_ms))
            .collect();
        // Apply epoch offset (same as tick path) so tail utterances
        // from a replaced stream have meeting-relative timestamps.
        let epoch_ms = state.stream_epoch_offsets_ms[i];
        let finals: Vec<Utterance> = if epoch_ms > 0 {
            finals
                .into_iter()
                .map(|mut u| {
                    u.started_at_ms += epoch_ms;
                    u.ended_at_ms += epoch_ms;
                    u
                })
                .collect()
        } else {
            finals
        };
        // All tail utterances from finish() are finals (#533 diagnostic).
        state.final_counts[i] += finals.len() as u64;
        state.blank_counts[i] += finals
            .iter()
            .filter(|u| u.text == "[BLANK_AUDIO]" || u.text.trim().is_empty())
            .count() as u64;
        tail_buckets.push(TickBucket {
            source_label,
            utterances: finals,
            audio: tail_audio,
        });
    }
}

/// One source's worth of utterances for the merge-sort-label-split
/// pump dispatch (#206). The pump accumulates these per tick (and
/// once at tail flush), then `diarize_and_dispatch_merged` runs the
/// diarizer over the chronologically-merged batch and dispatches
/// each source's labelled slice through `dispatch_utterances`.
///
/// `pub(super)` rather than fully private so the manager's tests
/// (which still live in `manager.rs`) can construct buckets to
/// drive `diarize_and_dispatch_merged` and `dispatch_utterances`
/// directly without going through a real pump task.
pub(super) struct TickBucket {
    pub source_label: String,
    pub utterances: Vec<Utterance>,
    /// Per-utterance audio in canonical 16 kHz mono — parallel to
    /// `utterances`. `audio[i]` is the slice of audio that
    /// produced `utterances[i]`. Empty `Vec` for an utterance
    /// whose audio dropped out of the pump's rolling buffer
    /// horizon (very rare: requires a 30+ second utterance).
    /// Threaded into [`diarize_and_dispatch_merged`] so the
    /// diarizer trait gets real audio chunks instead of `&[]`.
    pub audio: Vec<Vec<f32>>,
}

/// Diarize + dispatch a tick's worth of utterances across all
/// sources, in chronological order (#206).
///
/// Pre-#206 the dispatch was per-source: the pump called
/// `diarize.label_utterances` once per source bucket and dispatched
/// each separately. The diarizer never saw mic + system audio
/// interleaved, so its alternating-talker heuristic produced
/// `"Speaker A" / "Speaker B"` independently inside each source
/// stream — meaning "Speaker A" referred to a different actual
/// speaker on a mic+system meeting depending on which source the
/// utterance came from.
///
/// The fix here is purely structural: tag each utterance with its
/// source-bucket index, sort the merged list by `started_at_ms`,
/// run the diarizer once, then split the labelled result back into
/// per-source slices (preserving original source order) for the
/// existing `dispatch_utterances` path. The trait surface is
/// unchanged; the wiring carries the cross-source coordination.
pub(super) async fn diarize_and_dispatch_merged(
    session_id: i64,
    buckets: Vec<TickBucket>,
    diarize: &Arc<dyn crate::diarization::Diarize>,
    partials: &Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    repo: &Arc<dyn MeetingSessionRepository>,
    event_emitter: &dyn crate::events::EventEmitter,
) {
    if buckets.is_empty() {
        return;
    }

    // Hold the source labels in original order — the dispatch loop
    // at the bottom needs them, but the merge step consumes the
    // bucket vec.
    let source_labels: Vec<String> = buckets.iter().map(|b| b.source_label.clone()).collect();

    // Fast path: a single source skips the merge-sort-label-split entirely.
    // Most Record-mode sessions use only one source (mic-only), so the common
    // path avoids the O(N log N) sort and the diarizer overhead entirely.
    // Dispatch the single bucket's utterances directly using the source's own
    // label (same outcome as the full path with `source_labels.len() == 1`).
    if source_labels.len() == 1 {
        let bucket = buckets.into_iter().next().unwrap();
        dispatch_utterances(
            session_id,
            &source_labels[0],
            bucket.utterances,
            partials,
            repo,
            event_emitter,
        )
        .await;
        return;
    }

    // Tag each utterance with its source bucket index AND its
    // per-utterance audio chunk, then move into a flat
    // `(idx, utterance, audio)` vec. Audio comes from the pump's
    // rolling per-source buffer (#111 PR-F) — already in
    // canonical 16 kHz mono so the diarizer sees a homogeneous
    // batch.
    let mut tagged: Vec<(usize, Utterance, Vec<f32>)> = Vec::new();
    for (idx, bucket) in buckets.into_iter().enumerate() {
        // bucket.audio is parallel to bucket.utterances; if the
        // pump drifted we'd see a length mismatch — log and
        // continue with empty audio chunks so the diarizer falls
        // through to source-only labels rather than panicking.
        let bucket_audio = if bucket.audio.len() == bucket.utterances.len() {
            bucket.audio
        } else {
            tracing::warn!(
                source = %bucket.source_label,
                utterances = bucket.utterances.len(),
                audio_chunks = bucket.audio.len(),
                "diarize_and_dispatch_merged: bucket audio/utterance length mismatch; \
                 falling back to empty audio for this bucket"
            );
            vec![Vec::new(); bucket.utterances.len()]
        };
        for (u, audio) in bucket.utterances.into_iter().zip(bucket_audio) {
            tagged.push((idx, u, audio));
        }
    }

    if tagged.is_empty() {
        return;
    }

    // Sort by start time. `sort_by_key` is stable, so utterances
    // sharing a `started_at_ms` keep their original per-source
    // arrival order — important when mic + system happen to
    // produce simultaneous finals and we don't want a race-y
    // re-ordering on every tick.
    tagged.sort_by_key(|(_, u, _)| u.started_at_ms);

    // Split tags from utterances (move out, no clones). Diarizer
    // takes `&mut [Utterance]` so it sees the chronological
    // sequence and labels accordingly. Audio chunks are parallel
    // to the utterance vec.
    let mut bucket_indices: Vec<usize> = Vec::with_capacity(tagged.len());
    let mut chronological: Vec<Utterance> = Vec::with_capacity(tagged.len());
    let mut chronological_audio: Vec<Vec<f32>> = Vec::with_capacity(tagged.len());
    for (idx, u, audio) in tagged {
        bucket_indices.push(idx);
        chronological.push(u);
        chronological_audio.push(audio);
    }
    // Single-source guard (#369). When the user records with only
    // one source bucket — the canonical case once the unified
    // Record flow lands and Screen Recording isn't granted — the
    // ONNX diarizer can produce spurious Speaker A / Speaker B
    // alternation against a single talker (~50–200 ms of inference
    // wasted per utterance). Skip the call; `dispatch_utterances`
    // falls back to the source-derived `"mic"` / `"system"` label,
    // which is what the user wants in the mic-only Record case
    // anyway. Multi-source buckets still hit the diarizer
    // unconditionally — that's where it earns its keep.
    if source_labels.len() > 1 {
        // Only feed finals to the diarizer (#800). Partial utterances
        // are near-duplicate embeddings that bleed into cluster history,
        // wasting ~50–100 ms inference per partial and increasing 1-NN
        // drift. Extract finals by index, run label_utterances, then copy
        // labels back. dispatch_utterances handles the source-tag fall-
        // through for any unlabelled partials.
        let final_idxs: Vec<usize> = chronological
            .iter()
            .enumerate()
            .filter_map(|(i, u)| if u.is_final { Some(i) } else { None })
            .collect();
        if !final_idxs.is_empty() {
            let mut final_utts: Vec<Utterance> = final_idxs
                .iter()
                .map(|&i| chronological[i].clone())
                .collect();
            let final_audio: Vec<Vec<f32>> = final_idxs
                .iter()
                .map(|&i| chronological_audio[i].clone())
                .collect();
            let diarize_bg = Arc::clone(diarize);
            let labeled_result = tokio::task::spawn_blocking(move || {
                diarize_bg.label_utterances(&mut final_utts, &final_audio, CANONICAL_FORMAT);
                final_utts
            })
            .await;
            match labeled_result {
                Ok(labeled_utts) => {
                    for (&orig_i, labeled) in final_idxs.iter().zip(labeled_utts) {
                        chronological[orig_i].speaker_label = labeled.speaker_label;
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        session_id,
                        "meeting pump: diarize label_utterances task panicked; utterances will use source labels"
                    );
                }
            }
        }
    }

    // Re-split the labelled vec back into per-source buckets,
    // preserving original source order so the dispatch order
    // matches the pre-#206 behaviour.
    let mut split: Vec<Vec<Utterance>> = (0..source_labels.len()).map(|_| Vec::new()).collect();
    for (idx, u) in bucket_indices.into_iter().zip(chronological) {
        split[idx].push(u);
    }

    for (label, utts) in source_labels.into_iter().zip(split) {
        dispatch_utterances(session_id, &label, utts, partials, repo, event_emitter).await;
    }
}

/// Route streaming-session output: finals land in the database,
/// partials land in the in-memory map. Falls back to the source-
/// derived `speaker_label` (`"mic"` / `"system"`) when the
/// diarizer hasn't already set one — so the panel always has a
/// label to render with.
///
/// Errors are logged + swallowed — a single bad utterance shouldn't
/// abort the session.
pub(super) async fn dispatch_utterances(
    session_id: i64,
    source_label: &str,
    utterances: Vec<Utterance>,
    partials: &Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    repo: &Arc<dyn MeetingSessionRepository>,
    event_emitter: &dyn crate::events::EventEmitter,
) {
    for mut u in utterances {
        // Source-derived speaker label is the fallback for any
        // utterance whose diarizer abstained (`NoopDiarizer`,
        // or `OnnxDiarizer` skipping a too-short utterance, or
        // the toggle-off branch of `FlagGatedDiarizer`).
        if u.speaker_label.is_none() {
            u.speaker_label = Some(source_label.to_owned());
        }

        if u.is_final {
            // Skip empty finals — the streaming session usually
            // filters them, but defence in depth (whitespace-only
            // text from a non-speech segment) keeps the panel
            // clean.
            let trimmed = u.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Clear the in-flight partial for this source first —
            // the user just saw the partial firm up into a final, so
            // the partial slot for this source belongs to whatever
            // segment comes next. Doing this BEFORE the DB append
            // means a concurrent IPC poll between the partial-clear
            // and the DB-append sees neither (better than seeing
            // both, which would briefly show the same text twice).
            if let Ok(mut guard) = partials.write() {
                if let Some(per_session) = guard.get_mut(&session_id) {
                    per_session.remove(source_label);
                }
            }

            if let Err(e) = repo
                .append_utterance(NewPersistedUtterance {
                    session_id,
                    started_at_ms: u.started_at_ms as i64,
                    ended_at_ms: u.ended_at_ms as i64,
                    speaker_label: u.speaker_label.clone(),
                    text: trimmed.to_owned(),
                })
                .await
            {
                tracing::warn!(
                    error = ?e,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: utterance append failed; final dropped"
                );
                // Restore the partial so the text is not silently lost — the
                // user can at least see it in the panel even if it wasn't
                // persisted. The brief window where neither partial nor DB row
                // is visible (between the clear above and this restore) is
                // preferable to permanent data loss.
                if let Ok(mut guard) = partials.write() {
                    if let Some(per_session) = guard.get_mut(&session_id) {
                        per_session.insert(source_label.to_owned(), u);
                    }
                }
                // Surface to the user via the same banner the dictation IPC
                // path uses (#790) — transcript went to clipboard but not to
                // history, which is worth a visible warning.
                emit_utterance_append_failed(event_emitter, session_id, &e.to_string());
            }
        } else {
            // Partial — replace the in-flight slot for this source.
            // The map is keyed by source label so mic + system don't
            // overwrite each other.
            if let Ok(mut guard) = partials.write() {
                guard
                    .entry(session_id)
                    .or_insert_with(HashMap::new)
                    .insert(source_label.to_owned(), u);
            }
        }
    }
}

/// Open one audio handle and (if a transcriber is available) a matching
/// streaming transcription session for it. Pure — no events emitted.
///
/// Used by the fallback and reconnect paths in [`run_pump`]. For the
/// startup path, [`super::lifecycle`] still uses the inline pre-warm
/// block so it can emit source-specific failure events with the right
/// session id and error context.
///
/// Sequence:
/// 1. Open the audio session handle.
/// 2. Pre-warm drain (to learn the device's capture format, which the
///    streaming session needs for its internal resampler).
/// 3. Call `start_stream` with that format.
///
/// On pre-warm failure the audio handle is still returned (it may be
/// usable for audio-only recording); `streaming_session` is `None`.
#[allow(clippy::type_complexity)]
pub(super) fn open_source_handle(
    audio: &Arc<dyn AudioCapture>,
    transcriber: Option<&Arc<dyn Transcribe>>,
    source: &AudioSource,
    vocab_prompt: &str,
) -> Result<(
    Box<dyn AudioSession>,
    Option<Box<dyn StreamingTranscribeSession>>,
)> {
    let handle = audio.start_session(source.clone())?;
    let streaming_session = match transcriber {
        Some(t) => {
            let mut scratch = Vec::new();
            match handle.drain_into(&mut scratch) {
                Ok(format) => match t.start_stream(format, vocab_prompt) {
                    Ok(mut sess) => {
                        // Replay pre-warm audio so the first inference
                        // window is not cold (#868). Treat feed failure
                        // as stream-setup failure — a broken session
                        // is worse than no session.
                        if !scratch.is_empty() {
                            if let Err(e) = sess.feed(&scratch) {
                                tracing::warn!(
                                    error = ?e,
                                    source_kind = source.kind_label(),
                                    "open_source_handle: pre-warm replay failed; audio-only"
                                );
                                scratch.zeroize(); // (#930)
                                return Ok((handle, None));
                            }
                        }
                        scratch.zeroize(); // (#930) after successful feed
                        Some(sess)
                    }
                    Err(e) => {
                        scratch.zeroize();
                        tracing::warn!(
                            error = ?e,
                            source_kind = source.kind_label(),
                            "open_source_handle: start_stream failed; audio-only"
                        );
                        None
                    }
                },
                Err(e) => {
                    scratch.zeroize();
                    tracing::warn!(
                        error = ?e,
                        source_kind = source.kind_label(),
                        "open_source_handle: pre-warm drain failed; audio-only"
                    );
                    None
                }
            }
        }
        None => None,
    };
    Ok((handle, streaming_session))
}

#[cfg(test)]
mod tests {
    use crate::meeting::test_support::build_tail_pump_context;
    use std::sync::Arc;

    /// No-tail-loss regression (#947 review Gap 1): the samples returned by
    /// `AudioSession::stop()` on release MUST be fed into the streaming
    /// session before `finish()` runs, or the tail of a meeting is lost.
    ///
    /// The stock `StubSession::stop()` returns `samples: vec![]`, so the
    /// `if samples.is_empty() { continue; }` guard short-circuits and the
    /// tail-feed line never executes under any existing test. This test
    /// uses `TailReleaseSession` (empty tick drains, NON-empty `stop()`)
    /// plus a `RecordingFeedSession` that logs every non-empty `feed()`,
    /// then drives the whole `run_pump` finalization path.
    ///
    /// Non-vacuous by construction: the tick-drain path appends nothing
    /// (drain_into returns no samples), so the ONLY route to `feed()` is
    /// `release_audio_handles`. If the tail-feed line is removed, `fed_lens`
    /// stays empty (first assert fails) AND `RecordingFeedSession::finish`
    /// emits no utterance, so no row is persisted (second assert fails).
    #[tokio::test]
    async fn release_feeds_captured_tail_into_streaming_session() {
        let tail = vec![0.25f32; 4_096];
        let (ctx, repo, fed_lens) = build_tail_pump_context(tail.clone()).await;
        let session_id = ctx.session_id;

        // cancel is pre-set inside the context, so run_pump skips the loop
        // body and goes straight to final-drain → release_audio_handles →
        // flush_sessions.
        super::run_pump(ctx).await;

        // 1. The captured tail reached the streaming session's feed().
        let fed = fed_lens.lock().unwrap().clone();
        assert!(
            !fed.is_empty(),
            "release_audio_handles must feed the captured tail into the streaming session; \
             fed_lens is empty, so the tail-feed line did not execute"
        );
        assert_eq!(
            fed.iter().sum::<usize>(),
            tail.len(),
            "the full captured tail must reach feed() exactly once"
        );

        // 2. Downstream effect: the tail produced a final utterance that
        // was persisted (finish() emits one only if it saw a feed).
        let utts = repo.list_utterances(session_id).await.unwrap();
        assert_eq!(
            utts.len(),
            1,
            "the tail final must be persisted; got {} utterances",
            utts.len()
        );
        assert_eq!(utts[0].text, "tail words");
    }

    /// #667 regression — the background finalization must read THIS
    /// session's real (non-empty) diarizer centroids, not an empty diarizer
    /// left by a racing rebuild.
    ///
    /// Background: the meeting-stop IPC (`stop_meeting_and_rebuild_transcriber`)
    /// once spawned a diarizer rebuild that swapped a FRESH empty
    /// `OnnxDiarizer` into the shared `DiarizeSlot` the moment Stop was
    /// pressed (~80 ms). The pump's `session_centroids()` read runs at the
    /// END of the background finalization, after the slow tail `finish()`
    /// (up to 60 s). The rebuild won that race, so the centroid read saw an
    /// EMPTY diarizer and #667 identity resolution silently no-op'd for every
    /// backgrounded meeting. The fix removes that rebuild so the slot's
    /// diarizer stays stable across the stop boundary.
    ///
    /// This test wires the pump's diarizer as a `FlagGatedDiarizer` reading
    /// through a `DiarizeSlot` (the production shape), and runs the full
    /// `run_pump` finalization. The store records every centroid that reaches
    /// the resolver.
    ///
    /// Non-vacuous by construction: the second half swaps the slot to an
    /// empty diarizer BEFORE finalization (exactly what the old rebuild did)
    /// and asserts resolution no-ops — so if a future change reintroduces a
    /// slot clobber before the centroid read, the first assertion fails.
    #[tokio::test]
    async fn background_finalization_resolves_against_real_centroids() {
        use crate::diarization::{Diarize, DiarizeSlot, FlagGatedDiarizer, NoopDiarizer};
        use crate::meeting::test_support::{
            build_tail_pump_context_with_diarize, CentroidDiarizer, RecordingSpeakerStore,
        };
        use std::sync::atomic::AtomicBool;
        use std::sync::RwLock;

        // --- Case 1: slot holds the session's real centroids (the fix) ---
        let slot: DiarizeSlot = Arc::new(RwLock::new(
            Arc::new(CentroidDiarizer::with_one_cluster()) as Arc<dyn Diarize>,
        ));
        let flag_gated: Arc<dyn Diarize> = Arc::new(FlagGatedDiarizer::new(
            Arc::new(AtomicBool::new(true)),
            Arc::clone(&slot),
            Arc::new(NoopDiarizer),
        ));
        let store = RecordingSpeakerStore::new();

        let (ctx, _repo) = build_tail_pump_context_with_diarize(
            vec![0.25f32; 4_096],
            flag_gated,
            Arc::clone(&store) as Arc<dyn crate::speakers::SpeakerStore>,
            true,
        )
        .await;

        super::run_pump(ctx).await;

        let resolved = store.created.lock().unwrap().clone();
        assert_eq!(
            resolved.len(),
            1,
            "background finalization must resolve the session's one real \
             centroid cluster; got {resolved:?} (an empty diarizer would yield none)"
        );
        assert_eq!(
            resolved[0], 256,
            "the resolver must receive the real 256-d centroid, not a stub"
        );

        // --- Case 2: slot clobbered to empty BEFORE the read (the bug) ---
        // Reproduces what the old post-stop rebuild did: swap a fresh, empty
        // diarizer into the shared slot. The pump reads through the slot, so
        // `session_centroids()` now returns empty and resolution must no-op.
        // This pins the regression: if the rebuild (or any pre-read swap)
        // comes back, Case 1 would see this empty result and fail.
        let slot2: DiarizeSlot = Arc::new(RwLock::new(
            Arc::new(CentroidDiarizer::with_one_cluster()) as Arc<dyn Diarize>,
        ));
        let flag_gated2: Arc<dyn Diarize> = Arc::new(FlagGatedDiarizer::new(
            Arc::new(AtomicBool::new(true)),
            Arc::clone(&slot2),
            Arc::new(NoopDiarizer),
        ));
        let store2 = RecordingSpeakerStore::new();
        let (ctx2, _repo2) = build_tail_pump_context_with_diarize(
            vec![0.25f32; 4_096],
            flag_gated2,
            Arc::clone(&store2) as Arc<dyn crate::speakers::SpeakerStore>,
            true,
        )
        .await;
        // Clobber the slot to empty, as the racing rebuild used to.
        *slot2.write().unwrap() = Arc::new(NoopDiarizer);

        super::run_pump(ctx2).await;

        assert!(
            store2.created.lock().unwrap().is_empty(),
            "control: an empty diarizer in the slot must yield NO resolution — \
             this is the failure mode the fix prevents"
        );
    }
}
