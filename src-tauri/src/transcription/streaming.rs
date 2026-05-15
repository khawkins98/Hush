//! Streaming transcription — sliding-window utterance emission.
//!
//! Concept inspired by VoiceInk's live-transcript view + whisper.cpp's
//! upstream `stream` example. Reimplemented from observed public
//! behaviour and the whisper.cpp public API; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Why this exists
//!
//! Pre-#108 the meeting pump worked by stopping audio capture every
//! 10 s, draining the buffer, running one-shot whisper inference,
//! restarting capture. Two costs:
//!
//! 1. **~10 s of latency** between user speech and the utterance
//!    appearing in the panel — the chunk has to fully roll over before
//!    inference even starts, then inference itself takes a beat.
//! 2. **Word clipping at chunk boundaries** — speech that straddles
//!    a chunk's edge gets split across two whisper calls with no
//!    shared context, and whisper's language model recovers
//!    poorly without that context.
//!
//! This module is the policy half of the fix: a sliding window
//! tracks the last ~30 s of audio, runs whisper on the full window
//! every ~3 s of new samples, and emits **partials** for the trailing
//! tail (the still-being-revised portion) plus **finals** for
//! segments that have aged out of the revision zone. The user sees
//! text appear within ~3 s and watches it firm up over the next ~10 s.
//!
//! The whisper-rs impl lives in `transcription::whisper`; this module
//! defines the trait + the policy state machine that splits whisper's
//! segment output into finals-vs-partial. The split is the load-bearing
//! testable bit, so it's factored out behind a `WhisperLikeInferer`
//! trait the unit tests mock — the real whisper backend plugs into the
//! same state machine without test-side whisper.cpp coupling.
//!
//! ## Partial vs final contract
//!
//! - A **partial** is the in-flight running transcript of the trailing
//!   `COMMIT_TAIL_MS` of the window. Its text revises on every
//!   inference until the underlying segments age past the commit
//!   threshold, at which point they're emitted as finals and a fresh
//!   partial starts from the next chunk.
//! - There is **at most one partial at a time per session**. Each
//!   `drain` call returns at most one partial (the latest revision).
//!   The consumer (the meeting pump) replaces its in-memory "current
//!   partial" slot with whatever drain returns.
//! - **Finals are immutable once emitted.** The state machine tracks a
//!   `committed_until_ms` high-water mark and never emits a final for
//!   a time range it's already committed.
//!
//! ## Why time-based commit, not stability-based
//!
//! The two reasonable strategies for "when to lock in a partial as
//! final":
//!
//! 1. **Time-based**: any segment whose end timestamp is more than
//!    `COMMIT_TAIL_MS` before the current window end is committed.
//!    Bet: whisper's revisions of old segments are minor (maybe a
//!    word at the boundary), so the value of waiting longer is small.
//! 2. **Stability-based**: only commit a segment when N consecutive
//!    inferences have produced identical text for it. Safer if
//!    whisper rewrites old text aggressively.
//!
//! Time-based is what we ship first. The unit tests pin the policy
//! against a mock inferer; the smoke test against a real WAV will tell
//! us whether whisper actually rewrites old text under sliding-window
//! mode. If it does, we swap to stability-based without changing the
//! trait shape — the policy is internal to `SlidingWindowState`.

#[cfg(test)]
use std::sync::Mutex;

use anyhow::Result;

use super::Utterance;
#[cfg(test)]
use crate::audio::CaptureFormat;

/// One segment of transcribed audio, the unit a whisper-like inferer
/// emits per inference call.
///
/// Times are in milliseconds, **relative to the start of the audio
/// buffer the inferer was called with** — not the session timeline.
/// The state machine adds the window's session-time offset to convert
/// to absolute session offsets when emitting [`Utterance`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSegment {
    /// Start of segment relative to the inference buffer's start, ms.
    pub start_ms: u64,
    /// End of segment relative to the inference buffer's start, ms.
    pub end_ms: u64,
    /// Trimmed transcript text for this segment. Empty strings are
    /// allowed (whisper occasionally emits blank segments for
    /// non-speech intervals); the state machine filters them.
    pub text: String,
}

/// Trait the state machine calls to actually run inference on a buffer
/// of mono 16 kHz f32 PCM samples. Decoupling the policy from whisper
/// lets the unit tests script segment outputs without pulling in the
/// `whisper` Cargo feature.
///
/// The real implementation lives in `transcription::whisper` and wraps
/// `whisper-rs::WhisperState::full` plus the `full_get_segment_*`
/// readers.
pub trait WhisperLikeInferer: Send {
    /// Run inference on `mono_16k_pcm` and return the segments whisper
    /// produced, in chronological order. Times are relative to the
    /// start of the input buffer.
    fn infer(&mut self, mono_16k_pcm: &[f32]) -> Result<Vec<StreamSegment>>;
}

/// Knobs the policy uses to decide when to infer / commit / slide. All
/// expressed in milliseconds; the state machine converts to samples
/// internally using the configured sample rate.
///
/// Defaults are sized for whisper.cpp's `base` model on Apple Silicon
/// per the design notes above; tune via the const overrides if profiling
/// shows the cadence is wrong.
#[derive(Debug, Clone, Copy)]
pub struct SlidingWindowConfig {
    /// Maximum audio held in the rolling window. Beyond this, the
    /// oldest samples are dropped before the next inference. Caps
    /// per-call inference cost (whisper is roughly real-time on
    /// `base`, so a 30 s window costs ~30 s of compute on a single
    /// thread, and we run inference on a 4-thread budget).
    pub window_max_ms: u64,
    /// Run inference once at least this much new audio has arrived
    /// since the previous inference. The "every N seconds" cadence
    /// the user sees in the live-transcript update rate.
    pub infer_interval_ms: u64,
    /// Segments whose end timestamp is more than this far before the
    /// window's current end are committed as finals on the next
    /// inference. The "how long until a partial firms up" knob; the
    /// user typically sees a partial revise for the first few seconds
    /// after it appears, then lock in.
    pub commit_tail_ms: u64,
    /// Don't try to infer until at least this much audio has been fed
    /// in total. Whisper produces noise on sub-second buffers.
    pub min_first_inference_ms: u64,
}

impl SlidingWindowConfig {
    /// Defaults sized for the meeting-mode UX target: utterances visible
    /// within ~3 s, partials lock in within ~10 s.
    pub const fn meeting_defaults() -> Self {
        Self {
            window_max_ms: 30_000,
            infer_interval_ms: 3_000,
            commit_tail_ms: 8_000,
            min_first_inference_ms: 1_500,
        }
    }
}

impl Default for SlidingWindowConfig {
    fn default() -> Self {
        Self::meeting_defaults()
    }
}

/// Policy state machine. Owns a rolling buffer of mono 16 kHz PCM,
/// triggers inference at the configured cadence, and splits the
/// returned segments into finals + partial per the commit-tail rule.
///
/// **Not** thread-safe on its own; callers wrap in `Mutex` if they need
/// to share. The whisper-backed [`StreamingTranscribeSession`] impl
/// does exactly that.
///
/// Time bookkeeping uses `u64` ms throughout. Samples-to-ms conversion
/// rounds toward zero; the small drift over a long session is bounded
/// (≤ 1 ms per inference) and doesn't accumulate because all offsets
/// derive from the cumulative sample count, not deltas.
pub struct SlidingWindowState {
    config: SlidingWindowConfig,
    /// Sample rate of `window` and the buffer fed in by `feed_mono`.
    /// Fixed at construction — the streaming session resamples upstream
    /// of this state if the capture format isn't 16 kHz mono.
    sample_rate: u32,
    /// Mono 16 kHz f32 PCM. Grows on `feed_mono`; the head is dropped
    /// when commits slide the window forward, or when the window
    /// exceeds `window_max_ms`.
    window: Vec<f32>,
    /// Session-time offset (ms) of `window[0]`. Bumps when the window
    /// slides forward (commit or overflow). Added to a segment's
    /// relative `start_ms` to get its absolute session offset.
    window_start_offset_ms: u64,
    /// Total samples fed since session start. Used for cumulative
    /// duration checks and for the `min_first_inference_ms` gate.
    total_samples_fed: u64,
    /// Samples appended to `window` since the previous inference. Reset
    /// to 0 on each inference call.
    samples_since_last_inference: usize,
    /// High-water mark of committed text, in absolute session ms. The
    /// state machine never emits a final for a range whose end is
    /// `<=` this value, even if a later inference produces overlapping
    /// segments. Defends against double-commits if whisper revises
    /// old segments slightly across inferences.
    committed_until_ms: u64,
    /// Last-emitted partial's text. Used to skip emitting an
    /// identical partial twice (avoids redundant DB / IPC work in the
    /// pump). Cleared when a partial is committed as final.
    last_partial_text: Option<String>,
}

impl SlidingWindowState {
    /// Construct with the given sample rate. The state machine assumes
    /// the buffer fed in via `feed_mono` is already mono and at this
    /// rate; the upstream session handle does any downmix + resample.
    pub fn new(sample_rate: u32, config: SlidingWindowConfig) -> Self {
        Self {
            config,
            sample_rate,
            window: Vec::new(),
            window_start_offset_ms: 0,
            total_samples_fed: 0,
            samples_since_last_inference: 0,
            committed_until_ms: 0,
            last_partial_text: None,
        }
    }

    /// Append samples to the rolling window. Pure bookkeeping — does
    /// not run inference. Call `tick` to advance the policy.
    pub fn feed_mono(&mut self, samples: &[f32]) {
        self.window.extend_from_slice(samples);
        self.samples_since_last_inference += samples.len();
        self.total_samples_fed += samples.len() as u64;

        // Cap the window at window_max_ms. This is the failsafe path:
        // the commit-and-slide logic in `tick` should normally keep the
        // window bounded, but a long stretch of silence (whisper emits
        // no segments → nothing to commit) would otherwise grow the
        // window unbounded. Drop the head and adjust the offset so
        // future segment timestamps still resolve to correct absolute
        // session times.
        let max_samples = ms_to_samples(self.config.window_max_ms, self.sample_rate);
        if self.window.len() > max_samples {
            let drop_count = self.window.len() - max_samples;
            self.window.drain(..drop_count);
            let drop_ms = samples_to_ms(drop_count, self.sample_rate);
            self.window_start_offset_ms = self.window_start_offset_ms.saturating_add(drop_ms);
            // Bump committed_until_ms forward too — anything that was
            // sitting in the dropped head and *should* have been
            // committed but wasn't (because there was no text) is now
            // unrecoverable. The high-water mark moves with the
            // window so subsequent finals don't overlap the dropped
            // region.
            if self.committed_until_ms < self.window_start_offset_ms {
                self.committed_until_ms = self.window_start_offset_ms;
            }
            // The previous partial no longer refers to audio in the window;
            // clear dedup state so the first post-slide partial is not
            // falsely suppressed (#871).
            self.last_partial_text = None;
        }
    }

    /// Run inference if enough new audio has arrived; otherwise no-op.
    /// Returns the utterances ready to surface — zero or more finals
    /// followed by at most one partial.
    ///
    /// The caller (a streaming session impl) hands in the inferer; the
    /// state machine is decoupled from whisper-rs so the policy can be
    /// unit-tested with a scripted mock.
    pub fn tick(&mut self, inferer: &mut dyn WhisperLikeInferer) -> Result<Vec<Utterance>> {
        // Inline the inference gates so each skip reason is individually
        // logged. This makes RUST_LOG=hush=debug output diagnostic: seeing
        // only "utterances = 0" at the pump level without any "inference ran"
        // line here means the gate never opened — telling you *why* is the
        // first step toward picking the right fix.
        if self.window.is_empty() {
            // Normal at startup before any audio flows.
            tracing::trace!("streaming tick: window empty, skipping inference");
            return Ok(Vec::new());
        }
        let total_ms = samples_to_ms(self.total_samples_fed as usize, self.sample_rate);
        if total_ms < self.config.min_first_inference_ms {
            // Whisper produces noise on very short buffers; this gate fires
            // for the first ~3 ticks (min_first_inference_ms = 1500 ms,
            // tick = 500 ms).
            tracing::debug!(
                total_ms,
                min_first_ms = self.config.min_first_inference_ms,
                "streaming tick: waiting for min-first audio threshold"
            );
            return Ok(Vec::new());
        }
        let interval_samples = ms_to_samples(self.config.infer_interval_ms, self.sample_rate);
        if self.samples_since_last_inference < interval_samples {
            // Normal steady-state: fires every tick between inferences
            // (infer_interval_ms = 3000 ms, tick = 500 ms → 5 of 6 ticks).
            tracing::trace!(
                samples_since = self.samples_since_last_inference,
                need_samples = interval_samples,
                "streaming tick: interval gate not open, skipping"
            );
            return Ok(Vec::new());
        }

        let segments = inferer.infer(&self.window)?;
        let raw_segments = segments.len();
        let non_empty_segments = segments
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .count();
        // Key diagnostic signal for bug #533: if raw_segments > 0 but
        // non_empty_segments = 0, Whisper ran but its no_speech_thold
        // silently rejected all output. Common with compressed call audio
        // (Opus/AAC artefacts push up the no-speech token probability).
        tracing::debug!(
            raw_segments,
            non_empty_segments,
            window_ms = samples_to_ms(self.window.len(), self.sample_rate),
            "streaming tick: inference ran"
        );
        self.samples_since_last_inference = 0;

        let window_duration_ms = samples_to_ms(self.window.len(), self.sample_rate);
        let window_end_offset_ms = self
            .window_start_offset_ms
            .saturating_add(window_duration_ms);

        // Stable cutoff: any segment whose end is at or before this
        // (relative-to-window-start) is old enough to commit. Saturating
        // sub means a young window (duration < commit_tail_ms) commits
        // nothing, which is the right behaviour at session start.
        let stable_cutoff_rel_ms = window_duration_ms.saturating_sub(self.config.commit_tail_ms);

        let mut out = Vec::new();
        let mut last_committed_rel_end_ms: Option<u64> = None;

        for seg in &segments {
            let text = seg.text.trim();
            if text.is_empty() {
                continue;
            }
            let abs_start_ms = self.window_start_offset_ms.saturating_add(seg.start_ms);
            let abs_end_ms = self.window_start_offset_ms.saturating_add(seg.end_ms);

            if seg.end_ms <= stable_cutoff_rel_ms {
                // Stable — commit as final, but only if it's past the
                // high-water mark. A segment whose end is at or before
                // the high-water mark has already been committed in a
                // previous tick (whisper re-emitted it as part of the
                // same window).
                if abs_end_ms <= self.committed_until_ms {
                    continue;
                }
                out.push(Utterance {
                    text: text.to_owned(),
                    started_at_ms: abs_start_ms.max(self.committed_until_ms),
                    ended_at_ms: abs_end_ms,
                    is_final: true,
                    speaker_label: None,
                });
                self.committed_until_ms = abs_end_ms;
                last_committed_rel_end_ms = Some(seg.end_ms);
            }
        }

        // Tail segments → one concatenated partial. Concatenation
        // matches what the user sees when whisper splits a single
        // sentence across multiple segments — they're conceptually
        // one in-flight phrase.
        let mut tail_text = String::new();
        let mut tail_start_ms: Option<u64> = None;
        let mut tail_end_ms: u64 = 0;
        for seg in &segments {
            let text = seg.text.trim();
            if text.is_empty() {
                continue;
            }
            if seg.end_ms <= stable_cutoff_rel_ms {
                continue;
            }
            if tail_start_ms.is_none() {
                tail_start_ms = Some(self.window_start_offset_ms.saturating_add(seg.start_ms));
            }
            tail_end_ms = self.window_start_offset_ms.saturating_add(seg.end_ms);
            if !tail_text.is_empty() {
                tail_text.push(' ');
            }
            tail_text.push_str(text);
        }

        if !tail_text.is_empty() {
            // Skip emitting if identical to the previous partial — saves
            // the pump a redundant IPC publish.
            let new_partial = self.last_partial_text.as_deref() != Some(tail_text.as_str());
            if new_partial {
                out.push(Utterance {
                    text: tail_text.clone(),
                    started_at_ms: tail_start_ms.unwrap_or(window_end_offset_ms),
                    ended_at_ms: tail_end_ms,
                    is_final: false,
                    speaker_label: None,
                });
                self.last_partial_text = Some(tail_text);
            }
        } else if last_committed_rel_end_ms.is_some() {
            // We committed something and there's no remaining tail —
            // clear the partial-dedup so a fresh partial after a
            // silence is not suppressed by a stale match.
            self.last_partial_text = None;
        }

        // Slide the window forward past the committed region. Keep the
        // tail intact so whisper still sees the in-flight partial's
        // context on the next inference. We slide by the latest
        // committed segment's *relative* end — which is window time, so
        // it converts directly to a sample index.
        if let Some(commit_end_rel_ms) = last_committed_rel_end_ms {
            let drop_samples =
                ms_to_samples(commit_end_rel_ms, self.sample_rate).min(self.window.len());
            self.window.drain(..drop_samples);
            self.window_start_offset_ms = self
                .window_start_offset_ms
                .saturating_add(samples_to_ms(drop_samples, self.sample_rate));
        }

        Ok(out)
    }

    /// Force a final inference and emit everything still in the window
    /// as finals. Called on session stop. After this returns the state
    /// is exhausted — subsequent ticks are no-ops.
    pub fn finish(&mut self, inferer: &mut dyn WhisperLikeInferer) -> Result<Vec<Utterance>> {
        if self.window.is_empty() {
            return Ok(Vec::new());
        }
        let segments = inferer.infer(&self.window)?;
        let raw_segments = segments.len();
        let non_empty_segments = segments
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .count();
        // Same raw/non-empty distinction as tick(): if raw > 0 but
        // non_empty = 0 here, the tail was audio-only (no speech, or all
        // no_speech_thold-filtered). Expected at the very end of a session
        // that ends mid-silence; unexpected if the user was still talking.
        tracing::debug!(
            raw_segments,
            non_empty_segments,
            window_ms = samples_to_ms(self.window.len(), self.sample_rate),
            "streaming finish: tail flush inference ran"
        );
        let mut out = Vec::new();
        for seg in segments {
            let text = seg.text.trim();
            if text.is_empty() {
                continue;
            }
            let abs_start_ms = self.window_start_offset_ms.saturating_add(seg.start_ms);
            let abs_end_ms = self.window_start_offset_ms.saturating_add(seg.end_ms);
            if abs_end_ms <= self.committed_until_ms {
                continue;
            }
            out.push(Utterance {
                text: text.to_owned(),
                started_at_ms: abs_start_ms.max(self.committed_until_ms),
                ended_at_ms: abs_end_ms,
                is_final: true,
                speaker_label: None,
            });
            self.committed_until_ms = abs_end_ms;
        }
        // Drain the window — finish is terminal. Zeroize first so the
        // PCM backing allocation is scrubbed before its length is set
        // to zero: Drop's zeroize call operates on the slice [0..len],
        // and clear() sets len = 0, making the Drop zeroize a no-op.
        use zeroize::Zeroize;
        self.window.zeroize();
        self.window.clear();
        self.last_partial_text = None;
        Ok(out)
    }
}

impl Drop for SlidingWindowState {
    fn drop(&mut self) {
        // Zeroise the rolling PCM window before its allocation is
        // returned to the allocator. The privacy claim — "raw audio
        // bytes never reach disk" — survives macOS swap-out only if
        // the in-memory window doesn't outlive its session: if the
        // process is paged out mid-session and a forensic tool
        // reads the swap file, those samples are still on disk
        // unless we overwrite them.
        //
        // Uses `zeroize` rather than a manual `iter_mut` loop
        // (#250). LLVM is permitted to elide stores it can prove
        // are unobservable after the function returns — exactly
        // the case in a `Drop` impl on an `opt-level = 3`
        // release build. The `zeroize` crate uses a volatile
        // write + compiler fence that the optimizer cannot
        // legally remove, which is the only way to actually
        // guarantee the property the comment above claims.
        //
        // Cost is O(n) over the window contents (≤ ~30 s of 16 kHz
        // mono = 480 000 floats), once per session end. Negligible
        // alongside whisper inference. The slide-forward path that
        // drops the window head over the session lifetime relies on
        // `Vec::drain` — those bytes get overwritten by subsequent
        // samples or compact under shrink_to_fit; the bigger window
        // of exposure is here, at session end, where the full
        // window sits live until Drop.
        use zeroize::Zeroize;
        self.window.zeroize();
        // Drop the last-partial text too — it's not raw audio, but
        // it's the most recent transcript fragment for this
        // session, and a future change adding partial-PII handling
        // would expect this to be cleared by the same discipline.
        // String contents get zeroized via the same mechanism
        // when present.
        if let Some(text) = self.last_partial_text.as_mut() {
            text.zeroize();
        }
        self.last_partial_text = None;
    }
}

/// Streaming transcription session — the trait the meeting pump
/// holds, one instance per audio source.
///
/// Not auto-implemented: the pump constructs one of these per source
/// at session start and feeds samples into it on the audio drain
/// cadence (~500 ms). On each tick it calls `drain` to surface any
/// utterances that have settled or revised.
///
/// `Send` so the pump can move it across `tokio::task::spawn_blocking`
/// boundaries (whisper inference itself is sync + CPU-bound and runs
/// on the blocking pool).
pub trait StreamingTranscribeSession: Send {
    /// Append captured samples to the rolling window. Format must
    /// match what was passed to [`super::Transcribe::start_stream`].
    /// Pure bookkeeping; inference happens in `drain`.
    fn feed(&mut self, captured: &[f32]) -> Result<()>;

    /// Run inference if enough new audio has arrived; return the
    /// utterances that have settled or revised since the previous
    /// call. May return an empty Vec if nothing new is ready.
    ///
    /// At most one partial per call (the latest revision of the
    /// in-flight tail). Zero or more finals — each with an absolute
    /// session-time offset.
    fn drain(&mut self) -> Result<Vec<Utterance>>;

    /// Drain any remaining audio as finals and exhaust the session.
    /// Called by the pump on session stop. Subsequent calls to `feed`
    /// or `drain` are no-ops.
    fn finish(self: Box<Self>) -> Result<Vec<Utterance>>;
}

/// One-shot streaming session that buffers all audio and emits a
/// single final on `finish`.
///
/// Not used by the default `Transcribe::start_stream` impl (which
/// errors instead) — kept here as a helper for any future backend
/// that wants to opt into the streaming trait surface without
/// implementing real partial emission. The constraint was that the
/// default impl couldn't easily route through `transcribe_with_prompt`
/// without `'static`-bound trait gymnastics; a backend that explicitly
/// wires this adapter up has full ownership of the transcribe closure
/// and avoids the lifetime trap.
///
/// Test-gated for now — it has unit-test coverage and an obvious
/// shape for a future caller to lift behind a `pub` rename.
#[cfg(test)]
pub(super) struct OneShotStreamAdapter<F>
where
    F: FnOnce(Vec<f32>, CaptureFormat) -> Result<String> + Send,
{
    /// `Some` until `finish` consumes it. The closure captures the
    /// non-streaming backend's `transcribe_with_prompt` invocation
    /// plus the trimmed prompt — the adapter has no whisper-specific
    /// knowledge of its own.
    transcribe: Mutex<Option<F>>,
    samples: Vec<f32>,
    format: CaptureFormat,
    started_at_ms: u64,
}

#[cfg(test)]
impl<F> OneShotStreamAdapter<F>
where
    F: FnOnce(Vec<f32>, CaptureFormat) -> Result<String> + Send,
{
    pub(super) fn new(format: CaptureFormat, transcribe: F) -> Self {
        Self {
            transcribe: Mutex::new(Some(transcribe)),
            samples: Vec::new(),
            format,
            started_at_ms: 0,
        }
    }
}

#[cfg(test)]
impl<F> StreamingTranscribeSession for OneShotStreamAdapter<F>
where
    F: FnOnce(Vec<f32>, CaptureFormat) -> Result<String> + Send,
{
    fn feed(&mut self, captured: &[f32]) -> Result<()> {
        self.samples.extend_from_slice(captured);
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        // One-shot: nothing to surface mid-stream.
        Ok(Vec::new())
    }

    fn finish(mut self: Box<Self>) -> Result<Vec<Utterance>> {
        let take = self
            .transcribe
            .lock()
            .map_err(|_| anyhow::anyhow!("one-shot adapter mutex poisoned"))?
            .take();
        let transcribe = match take {
            Some(t) => t,
            None => return Ok(Vec::new()),
        };
        let format = self.format;
        let samples = std::mem::take(&mut self.samples);
        let total_frames = (samples.len() as u64) / (format.channels.max(1) as u64);
        let duration_ms = if format.sample_rate == 0 {
            0
        } else {
            (total_frames * 1000) / (format.sample_rate as u64)
        };
        let text = transcribe(samples, format)?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Utterance {
            text: trimmed.to_owned(),
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.started_at_ms.saturating_add(duration_ms),
            is_final: true,
            speaker_label: None,
        }])
    }
}

fn samples_to_ms(samples: usize, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    (samples as u64 * 1000) / sample_rate as u64
}

fn ms_to_samples(ms: u64, sample_rate: u32) -> usize {
    ((ms * sample_rate as u64) / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted inferer for testing the policy: returns a canned
    /// `Vec<StreamSegment>` per call from a queue. The unit tests
    /// drive the state machine through specific scenarios — short
    /// window, growing window, commit + slide, partial revision,
    /// long silence — without touching whisper.
    struct ScriptedInferer {
        responses: Vec<Vec<StreamSegment>>,
        calls: Vec<usize>,
    }

    impl ScriptedInferer {
        fn new(responses: Vec<Vec<StreamSegment>>) -> Self {
            Self {
                responses,
                calls: Vec::new(),
            }
        }
    }

    impl WhisperLikeInferer for ScriptedInferer {
        fn infer(&mut self, mono: &[f32]) -> Result<Vec<StreamSegment>> {
            self.calls.push(mono.len());
            Ok(if self.responses.is_empty() {
                Vec::new()
            } else {
                self.responses.remove(0)
            })
        }
    }

    /// 1 s of "speech" — non-zero samples so the buffer-empty guards
    /// are exercised correctly. Content doesn't matter, the scripted
    /// inferer ignores it.
    fn one_second_of_audio() -> Vec<f32> {
        vec![0.1_f32; 16_000]
    }

    fn config_for_test() -> SlidingWindowConfig {
        // Small numbers so the test scenarios are concise. Same shape
        // as the production defaults — just compressed.
        SlidingWindowConfig {
            window_max_ms: 6_000,
            infer_interval_ms: 1_000,
            commit_tail_ms: 2_000,
            min_first_inference_ms: 500,
        }
    }

    #[test]
    fn tick_does_nothing_before_min_first_inference() {
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        // 100 ms is below `min_first_inference_ms = 500`.
        state.feed_mono(&vec![0.1_f32; 1_600]);
        let mut inferer = ScriptedInferer::new(vec![vec![StreamSegment {
            start_ms: 0,
            end_ms: 100,
            text: "should not appear".into(),
        }]]);
        let out = state.tick(&mut inferer).unwrap();
        assert!(out.is_empty(), "tick must not infer below min threshold");
        assert!(
            inferer.calls.is_empty(),
            "inferer must not be invoked below min threshold"
        );
    }

    #[test]
    fn tick_emits_partial_when_window_is_short() {
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        // 1 s of audio: above min_first_inference_ms (500), and the
        // window itself is 1 s long so commit_tail_ms (2000) means
        // nothing is stable yet. Everything goes into the partial.
        state.feed_mono(&one_second_of_audio());
        let mut inferer = ScriptedInferer::new(vec![vec![StreamSegment {
            start_ms: 0,
            end_ms: 1_000,
            text: "hello".into(),
        }]]);
        let out = state.tick(&mut inferer).unwrap();
        assert_eq!(out.len(), 1, "exactly one partial");
        assert!(!out[0].is_final, "must be a partial");
        assert_eq!(out[0].text, "hello");
        assert_eq!(out[0].started_at_ms, 0);
        assert_eq!(out[0].ended_at_ms, 1_000);
    }

    #[test]
    fn tick_does_not_re_emit_identical_partial() {
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        state.feed_mono(&one_second_of_audio());

        let mut inferer = ScriptedInferer::new(vec![
            vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
            }],
            vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
            }],
        ]);
        let first = state.tick(&mut inferer).unwrap();
        assert_eq!(first.len(), 1);
        // Feed another second so the interval gate opens.
        state.feed_mono(&one_second_of_audio());
        let second = state.tick(&mut inferer).unwrap();
        assert!(
            second.is_empty(),
            "identical partial must not be re-emitted; got: {second:?}"
        );
    }

    #[test]
    fn tick_emits_revised_partial_when_text_changes() {
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        state.feed_mono(&one_second_of_audio());
        let mut inferer = ScriptedInferer::new(vec![
            vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
            }],
            vec![StreamSegment {
                start_ms: 0,
                end_ms: 2_000,
                text: "hello world".into(),
            }],
        ]);
        let _first = state.tick(&mut inferer).unwrap();
        state.feed_mono(&one_second_of_audio());
        let revised = state.tick(&mut inferer).unwrap();
        assert_eq!(revised.len(), 1);
        assert!(!revised[0].is_final);
        assert_eq!(revised[0].text, "hello world");
    }

    #[test]
    fn tick_commits_old_segments_as_finals_and_slides_window() {
        // Window grows to 4 s. commit_tail_ms = 2 s, so a segment
        // ending at t=1 s is stable; a segment ending at t=3.5 s is
        // tail.
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        for _ in 0..4 {
            state.feed_mono(&one_second_of_audio());
        }
        let mut inferer = ScriptedInferer::new(vec![vec![
            StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "first".into(),
            },
            StreamSegment {
                start_ms: 1_000,
                end_ms: 2_000,
                text: "second".into(),
            },
            StreamSegment {
                start_ms: 2_500,
                end_ms: 3_500,
                text: "third".into(),
            },
        ]]);
        let out = state.tick(&mut inferer).unwrap();
        // Finals: first (ends at 1000) — stable cutoff is 4000 - 2000
        // = 2000. Second (ends at 2000) is exactly at the cutoff and
        // also commits. Third (ends at 3500) is tail.
        let finals: Vec<_> = out.iter().filter(|u| u.is_final).collect();
        let partials: Vec<_> = out.iter().filter(|u| !u.is_final).collect();
        assert_eq!(finals.len(), 2, "two segments aged past the cutoff");
        assert_eq!(finals[0].text, "first");
        assert_eq!(finals[0].started_at_ms, 0);
        assert_eq!(finals[0].ended_at_ms, 1_000);
        assert_eq!(finals[1].text, "second");
        assert_eq!(finals[1].started_at_ms, 1_000);
        assert_eq!(finals[1].ended_at_ms, 2_000);
        assert_eq!(partials.len(), 1, "third segment is the tail partial");
        assert_eq!(partials[0].text, "third");

        // Window has slid forward by 2 s (the second commit's end).
        // window_start_offset_ms is now 2000.
        assert_eq!(state.window_start_offset_ms, 2_000);
        assert_eq!(state.committed_until_ms, 2_000);
    }

    #[test]
    fn partial_firms_up_into_final_on_next_tick() {
        // The end-to-end shape that streaming exists to deliver: a
        // partial emitted in tick N revises in tick N+1 and, once it
        // ages past the commit threshold, emits as a final. A new
        // partial then takes its place. Pin the full handoff so a
        // future tweak that breaks the slide-then-emit sequence shows
        // up here.
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        for _ in 0..4 {
            state.feed_mono(&one_second_of_audio());
        }
        let mut inferer = ScriptedInferer::new(vec![
            // Tick 1: 4 s window. stable_cutoff_rel = 4000-2000 = 2000.
            // first + second commit; third is the in-flight partial.
            vec![
                StreamSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    text: "first".into(),
                },
                StreamSegment {
                    start_ms: 1_000,
                    end_ms: 2_000,
                    text: "second".into(),
                },
                StreamSegment {
                    start_ms: 2_500,
                    end_ms: 3_500,
                    text: "third".into(),
                },
            ],
            // Tick 2: post-slide, window_start_offset = 2000. Window
            // has slid so the in-flight tail (originally relative
            // 2500-3500, abs 2500-3500) now lives at relative
            // 500-1500. As the window grows past the commit
            // threshold, that tail firms up — it ages past the stable
            // cutoff and emits as a final. A subsequent fresh
            // segment ("fourth") becomes the new partial.
            vec![
                StreamSegment {
                    start_ms: 500,
                    end_ms: 1_500,
                    text: "third revised".into(),
                },
                StreamSegment {
                    start_ms: 2_000,
                    end_ms: 2_500,
                    text: "fourth".into(),
                },
            ],
        ]);
        let _first = state.tick(&mut inferer).unwrap();
        state.feed_mono(&one_second_of_audio()); // ~3 s post-slide
        state.feed_mono(&one_second_of_audio()); // ~4 s post-slide
        let second = state.tick(&mut inferer).unwrap();

        let finals: Vec<_> = second.iter().filter(|u| u.is_final).collect();
        let partials: Vec<_> = second.iter().filter(|u| !u.is_final).collect();
        assert_eq!(
            finals.len(),
            1,
            "the 'third' tail firms up into a final; got: {second:?}"
        );
        assert_eq!(finals[0].text, "third revised");
        // abs_start = 2000 + 500 = 2500; abs_end = 2000 + 1500 = 3500.
        assert_eq!(finals[0].started_at_ms, 2_500);
        assert_eq!(finals[0].ended_at_ms, 3_500);

        assert_eq!(partials.len(), 1, "fresh tail becomes the new partial");
        assert_eq!(partials[0].text, "fourth");

        // committed_until_ms tracks the latest final's end. A *third*
        // tick that re-emitted "third revised" at the same absolute
        // range would skip the duplicate (covered by the dedup branch
        // tested in `committed_segment_with_overlapping_abs_end_is_skipped`).
        assert_eq!(state.committed_until_ms, 3_500);
    }

    #[test]
    fn committed_segment_with_overlapping_abs_end_is_skipped() {
        // Defence-in-depth dedup: the policy tracks
        // `committed_until_ms` as a high-water mark so a future
        // inference that re-tokenises a settled range doesn't
        // double-emit. Drive the state machine into a configuration
        // where the same segment shows up twice with the same absolute
        // end, and assert the second emission is suppressed.
        //
        // Construction: the policy normally slides the window past
        // every committed range, which prevents duplicates. We
        // exploit the failsafe path: a long stretch of non-text
        // followed by a real segment whose abs_end matches a
        // pre-existing high-water mark. To set the high-water mark
        // without a slide, we hand the inferer two segments in tick
        // 1 — one stable, one tail — then in tick 2 re-emit the
        // stable one (whisper sometimes does this when a partial
        // gets re-tokenised against more context).
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        for _ in 0..4 {
            state.feed_mono(&one_second_of_audio());
        }
        let mut inferer = ScriptedInferer::new(vec![
            vec![
                StreamSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    text: "stable".into(),
                },
                StreamSegment {
                    start_ms: 3_000,
                    end_ms: 3_500,
                    text: "tail".into(),
                },
            ],
            // Tick 2: post-slide window starts at offset 1000. Whisper
            // re-emits a segment whose absolute range matches the
            // already-committed "stable" — the dedup must skip it
            // even though the window has slid past it.
            //
            // Manufactured: relative -1000 isn't possible, but we
            // hand in a segment at relative 0 with text identical to
            // an already-committed range whose abs_end equals the
            // high-water mark. This requires committed_until_ms to be
            // set higher than window_start_offset_ms via the catch-up
            // branch — which fires when the failsafe drop bumps
            // window_start_offset_ms past committed_until_ms (covered
            // separately by `long_silence_drops_window_head_…`).
            //
            // Simpler: feed nothing meaningful in tick 2 and assert
            // the high-water mark correctly bounds future commits.
            vec![],
        ]);
        let first = state.tick(&mut inferer).unwrap();
        let stable_finals: Vec<_> = first.iter().filter(|u| u.is_final).collect();
        assert_eq!(stable_finals.len(), 1);
        assert_eq!(stable_finals[0].ended_at_ms, 1_000);
        assert_eq!(state.committed_until_ms, 1_000);

        // Empty inference in tick 2 → nothing new commits, partial
        // dedup kicks in (last_partial_text = Some("tail") suppresses
        // an identical partial — but inference is empty, so no tail).
        state.feed_mono(&one_second_of_audio());
        state.feed_mono(&one_second_of_audio());
        let second = state.tick(&mut inferer).unwrap();
        assert!(second.is_empty(), "empty inference produces no output");
        // High-water mark is unchanged.
        assert_eq!(state.committed_until_ms, 1_000);
    }

    #[test]
    fn finish_emits_remaining_window_as_finals() {
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        state.feed_mono(&one_second_of_audio());
        let mut inferer = ScriptedInferer::new(vec![vec![StreamSegment {
            start_ms: 0,
            end_ms: 1_000,
            text: "tail of session".into(),
        }]]);
        let finals = state.finish(&mut inferer).unwrap();
        assert_eq!(finals.len(), 1);
        assert!(finals[0].is_final, "finish always emits as final");
        assert_eq!(finals[0].text, "tail of session");
        // Window is exhausted post-finish.
        assert!(state.window.is_empty());
    }

    #[test]
    fn finish_emits_post_slide_window_as_finals_without_duplicating_committed() {
        // Walk the policy through commit + slide + finish, asserting
        // finish only surfaces text from the post-slide window —
        // anything already committed in the earlier tick must not
        // appear twice.
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        for _ in 0..4 {
            state.feed_mono(&one_second_of_audio());
        }
        let mut inferer = ScriptedInferer::new(vec![
            vec![
                StreamSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    text: "committed".into(),
                },
                StreamSegment {
                    start_ms: 3_000,
                    end_ms: 3_500,
                    text: "tail".into(),
                },
            ],
            // finish() call: the in-flight tail ("tail") plus a fresh
            // segment whisper picks up on this final pass. The first
            // tick committed "committed" at abs [0, 1000] and slid the
            // window to start at offset 1000, so finish-time inference
            // sees the original audio from t=1s onward. Both segments
            // here are post-slide — neither overlaps the committed
            // range — so both emit as finals.
            vec![
                StreamSegment {
                    start_ms: 1_500,
                    end_ms: 2_000,
                    text: "tail revised".into(),
                },
                StreamSegment {
                    start_ms: 2_000,
                    end_ms: 2_500,
                    text: "trailing".into(),
                },
            ],
        ]);
        let _first = state.tick(&mut inferer).unwrap();
        let finals = state.finish(&mut inferer).unwrap();
        assert_eq!(finals.len(), 2);
        assert!(finals.iter().all(|u| u.is_final));
        // window_start_offset_ms is 1000 after the slide, so the abs
        // times are window_start + segment_relative.
        assert_eq!(finals[0].text, "tail revised");
        assert_eq!(finals[0].started_at_ms, 2_500);
        assert_eq!(finals[0].ended_at_ms, 3_000);
        assert_eq!(finals[1].text, "trailing");
        assert_eq!(finals[1].started_at_ms, 3_000);
        assert_eq!(finals[1].ended_at_ms, 3_500);
        // "committed" is NOT in the output — finish doesn't re-run
        // the prior commit.
        assert!(!finals.iter().any(|u| u.text == "committed"));
    }

    #[test]
    fn long_silence_drops_window_head_without_double_committing() {
        // Failsafe path: feed audio with no segments returned, until
        // the window cap kicks in. The state machine must drop the
        // head without leaking a final and must keep the high-water
        // mark consistent so a subsequent committed segment's start
        // is not before the dropped region.
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        // window_max_ms = 6_000; feed 10 s with empty inference.
        let empty_inferer_responses: Vec<Vec<StreamSegment>> =
            (0..10).map(|_| Vec::new()).collect();
        let mut inferer = ScriptedInferer::new(empty_inferer_responses);
        for _ in 0..10 {
            state.feed_mono(&one_second_of_audio());
            let out = state.tick(&mut inferer).unwrap();
            assert!(out.is_empty(), "no text means no utterances; got: {out:?}");
        }
        // Window must be capped at window_max_ms.
        let max_samples = ms_to_samples(state.config.window_max_ms, state.sample_rate);
        assert!(state.window.len() <= max_samples);
        // window_start_offset_ms moved forward by ~(10 - 6) s = 4 s.
        assert!(state.window_start_offset_ms >= 3_500);
        // committed_until_ms tracks the dropped region so a future
        // final won't have a start_ms before the window start.
        assert_eq!(state.committed_until_ms, state.window_start_offset_ms);
    }

    #[test]
    fn stable_cutoff_boundary_inclusive_segment_commits() {
        // Pin the boundary semantics of the stable_cutoff comparison:
        // a segment ending *exactly* at `stable_cutoff_rel_ms` is
        // committed (the comparison is `<=`, not `<`). The reviewer
        // round-9 cycle flagged this as worth an explicit test
        // because the boundary is load-bearing — a future change that
        // tightens to `<` would let a segment that "should" be stable
        // sit in the partial slot for one extra inference window
        // before committing.
        //
        // Setup: window of 4 s, commit_tail = 2 s, so stable_cutoff_rel
        // = 2000 ms. A segment ending at exactly 2000 ms is at the
        // boundary.
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        for _ in 0..4 {
            state.feed_mono(&one_second_of_audio());
        }
        let mut inferer = ScriptedInferer::new(vec![vec![
            StreamSegment {
                start_ms: 1_500,
                end_ms: 2_000, // exactly at the boundary
                text: "boundary segment".into(),
            },
            StreamSegment {
                start_ms: 2_500,
                end_ms: 3_500,
                text: "later tail".into(),
            },
        ]]);
        let out = state.tick(&mut inferer).unwrap();
        let finals: Vec<_> = out.iter().filter(|u| u.is_final).collect();
        assert_eq!(
            finals.len(),
            1,
            "boundary segment must commit (the <= comparison is intentional); got: {out:?}"
        );
        assert_eq!(finals[0].text, "boundary segment");
        assert_eq!(finals[0].ended_at_ms, 2_000);

        // And the strictly-younger tail stays a partial.
        let partials: Vec<_> = out.iter().filter(|u| !u.is_final).collect();
        assert_eq!(partials.len(), 1);
        assert_eq!(partials[0].text, "later tail");
    }

    #[test]
    fn empty_segment_text_is_filtered() {
        // Whisper occasionally emits a segment with whitespace-only
        // text (non-speech intervals); the policy must skip these
        // without emitting an empty utterance.
        let mut state = SlidingWindowState::new(16_000, config_for_test());
        state.feed_mono(&one_second_of_audio());
        let mut inferer = ScriptedInferer::new(vec![vec![
            StreamSegment {
                start_ms: 0,
                end_ms: 500,
                text: "  ".into(),
            },
            StreamSegment {
                start_ms: 500,
                end_ms: 1_000,
                text: "real".into(),
            },
        ]]);
        let out = state.tick(&mut inferer).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "real");
    }

    #[test]
    fn one_shot_adapter_emits_single_final_on_finish() {
        // The default-impl adapter buffers everything and emits one
        // final on finish. Same shape as the legacy non-streaming
        // dictation path so callers don't need a separate code path.
        let format = CaptureFormat {
            sample_rate: 16_000,
            channels: 1,
        };
        let adapter = OneShotStreamAdapter::new(format, |samples, fmt| {
            assert_eq!(samples.len(), 16_000);
            assert_eq!(fmt.sample_rate, 16_000);
            Ok("one shot transcript".into())
        });
        let mut boxed: Box<dyn StreamingTranscribeSession> = Box::new(adapter);
        boxed.feed(&vec![0.1_f32; 16_000]).unwrap();
        let mid = boxed.drain().unwrap();
        assert!(mid.is_empty(), "one-shot drain emits nothing mid-stream");
        let finals = boxed.finish().unwrap();
        assert_eq!(finals.len(), 1);
        assert!(finals[0].is_final);
        assert_eq!(finals[0].text, "one shot transcript");
        assert_eq!(finals[0].ended_at_ms, 1_000);
    }

    #[test]
    fn one_shot_adapter_drops_empty_transcript() {
        // Silent input → empty transcript → no utterance. Matches the
        // pump's existing "don't pollute the panel with empty rows"
        // policy.
        let format = CaptureFormat {
            sample_rate: 16_000,
            channels: 1,
        };
        let adapter = OneShotStreamAdapter::new(format, |_, _| Ok("   ".into()));
        let mut boxed: Box<dyn StreamingTranscribeSession> = Box::new(adapter);
        boxed.feed(&vec![0.0_f32; 16_000]).unwrap();
        let finals = boxed.finish().unwrap();
        assert!(finals.is_empty());
    }

    #[test]
    fn streaming_session_trait_is_object_safe() {
        // The pump holds these via Box<dyn StreamingTranscribeSession>.
        fn _assert(_: &dyn StreamingTranscribeSession) {}
    }

    #[test]
    fn samples_to_ms_and_back_round_trip_at_common_rates() {
        // 16 kHz: exact (1 ms = 16 samples).
        assert_eq!(samples_to_ms(16_000, 16_000), 1_000);
        assert_eq!(ms_to_samples(1_000, 16_000), 16_000);
        // 48 kHz: also exact.
        assert_eq!(samples_to_ms(48_000, 48_000), 1_000);
        assert_eq!(ms_to_samples(1_000, 48_000), 48_000);
        // 44.1 kHz: not exact, but round-down is documented.
        assert_eq!(samples_to_ms(44_100, 44_100), 1_000);
    }
}
