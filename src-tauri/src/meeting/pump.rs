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
use std::time::Duration;

use anyhow::Result;

use crate::audio::{apply_mic_gain, AudioSession, AudioSource, CaptureFormat};
use crate::transcription::{StreamingTranscribeSession, Utterance};

use super::manager::{MeetingSourceFailedPayload, MEETING_SOURCE_FAILED_EVENT};
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
    pub handles: Vec<Box<dyn AudioSession>>,
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
    // Per-source scratch buffer reused across ticks. Sized at first
    // drain; subsequent drains amortize the capacity. Indexed
    // parallel to `handles` / `sources`.
    let mut drain_buffers: Vec<Vec<f32>> = (0..ctx.handles.len()).map(|_| Vec::new()).collect();

    // Per-source rolling audio buffer in canonical 16 kHz mono
    // (#111 PR-F). The diarizer needs each utterance's audio to
    // run its embedding model; the streaming session doesn't
    // surface that, so we keep an independent buffer here. Drained
    // tick samples are appended every iteration; when finals come
    // out, each utterance's `[started_at_ms, ended_at_ms)` is
    // sliced out of the buffer for the diarize call. Bounded at
    // 30 s (matches the streaming session's window).
    let mut audio_buffers: Vec<crate::meeting::audio_buffer::AudioRollingBuffer> =
        (0..ctx.handles.len())
            .map(|_| crate::meeting::audio_buffer::AudioRollingBuffer::new())
            .collect();

    // Per-tick scratch for the merge-sort-label-split pattern (#206).
    // Accumulates `(source_label, utterances)` pairs from each
    // source's inference, then `diarize_and_dispatch_merged` runs the
    // diarizer once over the chronologically-merged batch before
    // splitting back per source for dispatch. Pre-#206 this lived
    // inside the per-source loop, which meant the diarizer never saw
    // mic + system audio interleaved — its alternating-talker
    // heuristic produced "Speaker A/B" inside each source's stream
    // without coordination, so "Speaker A" meant different people
    // depending on which source the chunk came from.
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

        // Drain audio for every source first (cheap, no inference),
        // then run inference per source. The drain step takes
        // microseconds; the inference step takes milliseconds-to-
        // seconds inside the streaming session's `drain` if a new
        // inference window has matured. Splitting the loop bounds
        // each source's audio buffer to the tick window plus the
        // few-ms drain.
        let mut tick_formats: Vec<Option<CaptureFormat>> = vec![None; ctx.handles.len()];
        for (i, handle) in ctx.handles.iter().enumerate() {
            let buf = &mut drain_buffers[i];
            buf.clear();
            match handle.drain_into(buf) {
                Ok(format) => tick_formats[i] = Some(format),
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        source_kind = ctx.sources[i].kind_label(),
                        session_id = ctx.session_id,
                        "meeting pump: drain_into failed for tick"
                    );
                }
            }
        }

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
            // inference session. A single application here means neither
            // consumer needs its own gain path.
            let gain_db = f32::from_bits(ctx.mic_gain_db.load(Ordering::Relaxed));
            apply_mic_gain(&mut drain_buffers[i], gain_db);

            let samples = std::mem::take(&mut drain_buffers[i]);
            let source_label = ctx.sources[i].speaker_tag().to_owned();
            let session_id = ctx.session_id;

            // Mirror the drained samples into the diarizer's rolling
            // buffer (#111 PR-F). Done before the `samples` move so
            // we don't have to clone — `audio_buffer::append` does
            // its own resample/downmix copy. Skip if drain_into
            // failed and we don't know the format for this tick.
            if let Some(format) = tick_formats[i] {
                audio_buffers[i].append(&samples, format);
            }

            // Spawn-blocking: returns (session, samples_buf,
            // Result<Vec<Utterance>>). The buffer round-trips so we
            // can put it back into `drain_buffers[i]` to keep its
            // capacity warm for the next tick.
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
                    ctx.event_emitter.emit(
                        MEETING_SOURCE_FAILED_EVENT,
                        &MeetingSourceFailedPayload {
                            session_id,
                            source_kind: &source_label,
                            reason: "transcription task panicked",
                        },
                    );
                    continue;
                }
            };

            // Restore the session + buffer for the next tick.
            ctx.streaming_sessions[i] = Some(returned_session);
            drain_buffers[i] = returned_buf;

            let utterances = match drain_result {
                Ok(u) => u,
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
                    ctx.event_emitter.emit(
                        MEETING_SOURCE_FAILED_EVENT,
                        &MeetingSourceFailedPayload {
                            session_id,
                            source_kind: &source_label,
                            reason: &reason,
                        },
                    );
                    continue;
                }
            };

            // Slice each utterance's audio out of the rolling
            // buffer for the diarizer (#111 PR-F). Parallel to
            // `utterances`. Empty `Vec` if the utterance's audio
            // dropped past the buffer horizon (very rare — would
            // require a >30 s utterance + late drain).
            let audio: Vec<Vec<f32>> = utterances
                .iter()
                .map(|u| audio_buffers[i].slice_ms(u.started_at_ms, u.ended_at_ms))
                .collect();

            // Accumulate this source's utterances into the tick
            // bucket. The per-tick `diarize_and_dispatch_merged`
            // call below runs the diarizer once over the merged +
            // chronologically-sorted batch, then splits the labelled
            // result back per source for dispatch (#206).
            tick_buckets.push(TickBucket {
                source_label,
                utterances,
                audio,
            });
        }

        if !tick_buckets.is_empty() {
            diarize_and_dispatch_merged(
                ctx.session_id,
                std::mem::take(&mut tick_buckets),
                &ctx.diarize,
                &ctx.partials,
                &ctx.repo,
            )
            .await;
        }
    }

    // Cancel — flush each streaming session. `finish` drains
    // anything still in the rolling window as finals; we persist
    // those before returning so `stop_manual` sees the
    // tail-of-conversation utterances. Same merge-sort-label-split
    // shape as the per-tick path (#206) so the tail flush can't
    // re-introduce the per-source independent-A/B regression.
    let mut tail_buckets: Vec<TickBucket> = Vec::new();
    #[allow(clippy::needless_range_loop)] // see explanation in the tick loop above
    for i in 0..ctx.sources.len() {
        let Some(session) = ctx.streaming_sessions[i].take() else {
            continue;
        };
        let source_label = ctx.sources[i].speaker_tag().to_owned();
        let session_id = ctx.session_id;
        let join = tokio::task::spawn_blocking(move || session.finish()).await;
        let finals = match join {
            Ok(Ok(u)) => u,
            Ok(Err(e)) => {
                tracing::warn!(
                    error = ?e,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: streaming finish failed; tail dropped"
                );
                continue;
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    session_id,
                    "meeting pump: streaming finish task panicked"
                );
                continue;
            }
        };
        let tail_audio: Vec<Vec<f32>> = finals
            .iter()
            .map(|u| audio_buffers[i].slice_ms(u.started_at_ms, u.ended_at_ms))
            .collect();
        tail_buckets.push(TickBucket {
            source_label,
            utterances: finals,
            audio: tail_audio,
        });
    }

    if !tail_buckets.is_empty() {
        diarize_and_dispatch_merged(
            ctx.session_id,
            tail_buckets,
            &ctx.diarize,
            &ctx.partials,
            &ctx.repo,
        )
        .await;
    }

    // Belt-and-braces: clear partials for this session id. The
    // dispatch loop above removes per-source entries on each final
    // commit; this drops the (now-empty) per-session HashMap so the
    // partials store doesn't grow unbounded across many sessions.
    if let Ok(mut guard) = ctx.partials.write() {
        guard.remove(&ctx.session_id);
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
) {
    if buckets.is_empty() {
        return;
    }

    // Hold the source labels in original order — the dispatch loop
    // at the bottom needs them, but the merge step consumes the
    // bucket vec.
    let source_labels: Vec<String> = buckets.iter().map(|b| b.source_label.clone()).collect();

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
        diarize.label_utterances(&mut chronological, &chronological_audio, CANONICAL_FORMAT);
    }

    // Re-split the labelled vec back into per-source buckets,
    // preserving original source order so the dispatch order
    // matches the pre-#206 behaviour.
    let mut split: Vec<Vec<Utterance>> = (0..source_labels.len()).map(|_| Vec::new()).collect();
    for (idx, u) in bucket_indices.into_iter().zip(chronological) {
        split[idx].push(u);
    }

    for (label, utts) in source_labels.into_iter().zip(split) {
        dispatch_utterances(session_id, &label, utts, partials, repo).await;
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
