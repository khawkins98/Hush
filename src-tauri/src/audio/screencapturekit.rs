//! macOS system-audio capture via ScreenCaptureKit (#105).
//!
//! Compiled on macOS only via `cfg(target_os = "macos")`. The
//! `screencapturekit` crate is linked unconditionally on macOS — the
//! feature flag was dropped once SCK became load-bearing for Meeting
//! Mode. This module gives [`super::CpalAudioCapture`] a parallel
//! capture path that pulls system audio (whatever the OS mixer is
//! routing to the speakers — Zoom calls, browser audio, music) into
//! the same `Vec<f32>` shape that the cpal mic path produces, so the
//! rest of the transcription pipeline does not need to know which
//! source it came from.
//!
//! ## Why ScreenCaptureKit
//!
//! Apple deprecated CoreAudio's HAL plug-in path for application audio
//! capture in macOS 14, and the Tap APIs (`AudioHardwareCreateProcessTap`)
//! require entitlements only available to MAS-distributed apps. SCK is
//! the only sanctioned, non-entitled route to system-audio on consumer
//! macOS, and the `screencapturekit` crate exposes the Swift-side API
//! through stable FFI so we can stay in pure Rust.
//!
//! Hush targets macOS 26+ only — older macOS is out of scope (see
//! `learnings.md` and the README's platform-support table). No version
//! guards or older-macOS fallbacks live in this module.
//!
//! ## Permission model
//!
//! SCK is gated behind the **Screen Recording** TCC bucket — confusing,
//! since we capture no pixels, but Apple bundles audio-from-display under
//! the same prompt. The first call to `SCShareableContent::get()` triggers
//! the prompt; if denied, calls fail with a clear error that the IPC layer
//! surfaces verbatim. The diagnostic / reset panel (`MacosDiagnosticPanel`)
//! already covers Screen Recording in its TCC sweep — see
//! `src-tauri/src/ipc/macos_perms.rs`.
//!
//! ## Threading
//!
//! `SCStream` is `Send + Sync` (the underlying Swift object is
//! retain/release-safe and the crate's per-stream context lives behind
//! a heap allocation referenced by FFI callbacks). Sample-buffer
//! callbacks fire on a libdispatch queue owned by the framework; our
//! handler holds the producer end of an `rtrb` SPSC ring (#251),
//! mirroring the cpal mic path. The framework dispatches callbacks
//! serially per output handler, so the producer side is wait-free
//! and never blocks waiting on the consumer (the meeting pump's
//! drain tick) — even if the pump stalls on a SQLite write or a
//! long Whisper inference, the SCK callback continues to push into
//! the ring without waiting on a mutex.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use rtrb::{Consumer, Producer, RingBuffer};
use screencapturekit::{
    cm::CMSampleBuffer,
    shareable_content::SCShareableContent,
    stream::{
        configuration::SCStreamConfiguration, content_filter::SCContentFilter,
        output_trait::SCStreamOutputTrait, output_type::SCStreamOutputType, SCStream,
    },
};

use super::{drain_consumer, log_overflow_if_set, CaptureFormat, MAX_BUFFER_FRAMES};

/// `Sync` wrapper around `rtrb::Producer<f32>` for use inside an
/// SCK output handler.
///
/// `rtrb::Producer` is `Send + !Sync` — that's the correct shape for
/// a single-producer ring (two threads concurrently calling
/// `Producer::push` would race on the head pointer). The SCStream
/// `did_output_sample_buffer` callback signature takes `&self`,
/// which would normally force us to wrap the producer in a `Mutex`.
/// We don't, because **ScreenCaptureKit dispatches callbacks
/// serially per output handler** (libdispatch serial queue), so the
/// producer is in fact never accessed concurrently. An `UnsafeCell`
/// + manual `unsafe impl Sync` captures that invariant directly.
///
/// SAFETY: every call to [`Self::with_inner`] runs on the SCK
/// libdispatch callback thread for one output handler. Two outstanding
/// `&mut` references to the inner `Producer` would only arise if SCK
/// re-entered the callback before the previous call returned, which
/// the framework's serial-dispatch contract forbids.
struct SerialProducer(UnsafeCell<Producer<f32>>);

unsafe impl Sync for SerialProducer {}

impl SerialProducer {
    fn new(inner: Producer<f32>) -> Self {
        Self(UnsafeCell::new(inner))
    }

    /// Run a closure with mutable access to the inner producer.
    /// SAFETY: see struct-level comment.
    fn with_inner<R>(&self, f: impl FnOnce(&mut Producer<f32>) -> R) -> R {
        // SAFETY: SCK callback dispatch is serial; no other thread
        // holds a reference to the inner Producer at any time.
        let inner = unsafe { &mut *self.0.get() };
        f(inner)
    }
}

/// Sample rate we ask ScreenCaptureKit to deliver. SCK supports a fixed
/// set (8000 / 16000 / 24000 / 48000); 48 kHz matches the rate the OS
/// mixer is running at internally on every modern Mac, so we avoid an
/// extra resample pass at capture time. The transcription stack
/// resamples to 16 kHz before whisper, same path the cpal mic input
/// already takes.
const SAMPLE_RATE: u32 = 48_000;

/// Stereo. Most system audio sources are stereo; the existing downmix
/// helper (`audio::format::downmix_to_mono`) collapses to mono before
/// whisper sees the buffer.
const CHANNELS: u16 = 2;

/// Active SCK system-audio capture. Owns the stream for the duration
/// of one recording and the consumer end of the SPSC ring the
/// callback writes into.
pub struct ScreenCaptureKitSession {
    /// Held alive for the duration of capture. Dropping it stops the
    /// stream; we also call `stop_capture()` explicitly in [`Self::stop`]
    /// so any final in-flight callbacks have settled before we drain.
    stream: SCStream,
    /// Format the buffer was captured in, surfaced back through the
    /// trait's [`super::CapturedAudio`] alongside the samples.
    pub format: CaptureFormat,
    /// Consumer end of the SPSC ring (#251). Wrapped in `Mutex` only
    /// to give `&self`-callable methods (`drain_buffer`, `stop`)
    /// interior mutability; the lock is uncontended in practice
    /// because the consumer side is single-threaded. `rtrb` itself
    /// never blocks the consumer either — `Consumer::read_chunk` is
    /// wait-free.
    consumer: Mutex<Consumer<f32>>,
    /// Set by the SCK callback when `Producer::push` returns
    /// `PushError::Full`. The worker logs once per drain cycle when
    /// it sees this set, then resets it.
    overflow_flag: Arc<AtomicBool>,
}

/// Sample-buffer handler installed on the SCStream.
///
/// Owns the producer end of the SPSC ring + shared atomics for the
/// overflow flag and the level meter. Producer push is wait-free,
/// so the framework callback thread never blocks waiting on the
/// consumer — even if the meeting pump's drain tick is stalled.
struct AudioHandler {
    producer: SerialProducer,
    overflow_flag: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        // The same handler is registered only for the Audio output type,
        // but the framework can in principle deliver other types; guard
        // anyway to keep the cast below sound.
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let Some(list) = sample.audio_buffer_list() else {
            return;
        };
        let num = list.num_buffers();
        if num == 0 {
            return;
        }

        // SCK delivers either:
        //   - one buffer with channel-interleaved f32 PCM, or
        //   - N buffers (one per channel) in planar layout.
        // The convention is "1 buffer = interleaved", and we fold the
        // planar case into the same interleaved Vec<f32> the rest of
        // the pipeline expects.
        //
        // We still build the interleaved `Vec<f32>` because the
        // planar→interleaved transform isn't expressible in a single
        // `Producer::push_iter` pass (different source slices per
        // sample), and the alloc cost is amortised by the caller's
        // typical buffer-size of a few thousand frames per callback.
        // The ring push happens immediately after — see below.
        let mut samples: Vec<f32> = Vec::new();
        let mut sum_sq = 0.0_f32;

        let count = if num == 1 {
            let Some(buf) = list.get(0) else { return };
            let bytes = buf.data();
            let n = bytes.len() / 4;
            samples.reserve(n);
            for i in 0..n {
                let off = i * 4;
                let s = f32::from_le_bytes([
                    bytes[off],
                    bytes[off + 1],
                    bytes[off + 2],
                    bytes[off + 3],
                ]);
                sum_sq += s * s;
                samples.push(s);
            }
            n
        } else {
            // Planar. Each buffer is one channel. Interleave so the
            // downstream `downmix_to_mono` (which assumes interleaved
            // layout governed by `CaptureFormat::channels`) works
            // unchanged.
            let chans: Vec<&[u8]> = (0..num)
                .filter_map(|i| list.get(i).map(AsByteSlice::as_byte_slice))
                .collect();
            if chans.is_empty() {
                return;
            }
            let frames_per_chan = chans[0].len() / 4;
            // Defensive: a malformed AudioBufferList where channel
            // buffers disagree on length would corrupt the interleaved
            // layout. Drop the buffer rather than guess.
            if !chans.iter().all(|c| c.len() / 4 == frames_per_chan) {
                tracing::warn!(
                    num_buffers = num,
                    "SCK audio buffer list channel sizes disagree; dropping sample"
                );
                return;
            }
            samples.reserve(frames_per_chan * chans.len());
            for f in 0..frames_per_chan {
                for chan_bytes in &chans {
                    let off = f * 4;
                    let s = f32::from_le_bytes([
                        chan_bytes[off],
                        chan_bytes[off + 1],
                        chan_bytes[off + 2],
                        chan_bytes[off + 3],
                    ]);
                    sum_sq += s * s;
                    samples.push(s);
                }
            }
            frames_per_chan * chans.len()
        };

        // Push into the wait-free ring. `Producer::push` is wait-free
        // and allocation-free; on a full ring it returns
        // `PushError::Full` and we drop the oldest-pushed-first
        // newer-sample-first — same overflow shape as the cpal path
        // (drop newer, not older). Setting the overflow flag lets
        // the worker log once per drain cycle so chronic overflow is
        // visible without flooding the log on a single transient.
        //
        // The ring's pre-allocated capacity is `MAX_BUFFER_FRAMES`,
        // matching the cpal path's defensive ceiling. The pre-#251
        // path used a `Mutex<Vec<f32>>` that locked the framework
        // callback thread while the worker drained — under a wedged
        // worker (long whisper inference, SQLite write, etc.) the
        // callback would stall, risking OS-level frame drops.
        let mut overflowed = false;
        self.producer.with_inner(|producer| {
            for s in &samples {
                if producer.push(*s).is_err() {
                    overflowed = true;
                    // Continue the loop so RMS still reflects the
                    // full callback worth of audio even when the
                    // ring is full — the level meter shouldn't go
                    // silent during overflow.
                }
            }
        });
        if overflowed {
            // `Relaxed`: the worker reads on a human-paced drain
            // tick; the message is purely informational.
            self.overflow_flag.store(true, Ordering::Relaxed);
        }

        if count > 0 {
            let rms = (sum_sq / count as f32).sqrt();
            // `Relaxed`: the level field is independent and a one-tick
            // stale read is invisible. Same reasoning as the cpal path
            // — see the long comment on `CpalAudioCapture::level`.
            self.level.store(rms.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Tiny helper so the planar branch can map `Option<&AudioBuffer>` to
/// `&[u8]` without a closure body that mentions a private type.
trait AsByteSlice {
    fn as_byte_slice(&self) -> &[u8];
}
impl AsByteSlice for screencapturekit::cm::AudioBuffer {
    fn as_byte_slice(&self) -> &[u8] {
        self.data()
    }
}

/// Touch SCK's `SCShareableContent::get()` so macOS adds Hush to
/// the Screen Recording permission list and (if not yet granted)
/// fires the standard TCC prompt. The result is discarded — we
/// only care about the side effect of macOS noticing that this
/// process wants Screen Recording.
///
/// Why this exists: macOS only shows an app under "Screen &
/// System Audio Recording" once the app actively requests the
/// permission. A user who hasn't started a Meeting Mode session
/// yet doesn't have a Hush row to toggle on; the per-row "Grant
/// in Settings…" button on the Permissions tab deep-links them
/// to a list that doesn't include Hush. Calling this function
/// before deep-linking guarantees the row is there.
///
/// Lightweight (single-millisecond range) and idempotent —
/// calling it on a process that already has the permission is a
/// no-op as far as the user's concerned. Only ever invoked from
/// the explicit "Grant in Settings…" click; we deliberately do
/// **not** auto-call this on app launch (that would prompt every
/// fresh-install user, even those who'll never use Meeting Mode).
///
/// Returns `Ok(())` on either "permission granted" or
/// "permission denied / not-determined" (both are valid outcomes
/// of the priming call). Returns `Err` only when the SCK
/// framework itself errored — which on a healthy system shouldn't
/// happen.
pub fn prime_screen_recording_permission() -> Result<()> {
    // We don't need the content; we just need macOS to register
    // that this process tried to query it. The error variant from
    // `SCShareableContent::get()` typically means "user hasn't
    // granted Screen Recording" — that's the very state we're
    // priming, so swallow it.
    let _ = SCShareableContent::get();
    Ok(())
}

impl ScreenCaptureKitSession {
    /// Start an SCK capture session against the system's first display
    /// (which is what the audio mixer is bound to). The first call on
    /// a fresh launch triggers the Screen Recording TCC prompt.
    ///
    /// `level` is the same `Arc<AtomicU32>` the cpal path writes to,
    /// so the HUD level meter works without any per-source branching
    /// on the consumer side.
    pub fn start(level: Arc<AtomicU32>) -> Result<Self> {
        // SCShareableContent::get() blocks the calling thread until
        // the framework returns. The cost is small (single millisecond
        // range) and only paid on start_dictation, not in the hot
        // path. Doing it inline keeps the start API synchronous —
        // same shape as cpal's `default_input_config`.
        let content = SCShareableContent::get().map_err(|e| {
            anyhow!(
                "ScreenCaptureKit: query shareable content: {e} — \
                 grant Screen Recording permission in System Settings → \
                 Privacy & Security to capture system audio"
            )
        })?;
        let displays = content.displays();
        let display = displays.first().ok_or_else(|| {
            anyhow!(
                "ScreenCaptureKit: no displays available — \
                 system audio capture requires Screen Recording permission \
                 (System Settings → Privacy & Security → Screen Recording)"
            )
        })?;

        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build();

        let config = SCStreamConfiguration::new()
            .with_captures_audio(true)
            .with_sample_rate(SAMPLE_RATE as i32)
            .with_channel_count(CHANNELS as i32)
            // Hush itself never plays audio today (no TTS, no sound
            // effects), but excluding the current process's audio is
            // free insurance against future feedback if we add either.
            .with_excludes_current_process_audio(true);

        // Pre-allocate the SPSC ring at the cpal path's defensive
        // cap so the overflow behaviour matches across sources. The
        // whole capacity is allocated once at session start — no
        // realloc inside the realtime callback.
        let (producer, consumer) = RingBuffer::<f32>::new(MAX_BUFFER_FRAMES);
        let overflow_flag = Arc::new(AtomicBool::new(false));
        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(
            AudioHandler {
                producer: SerialProducer::new(producer),
                overflow_flag: Arc::clone(&overflow_flag),
                level: Arc::clone(&level),
            },
            SCStreamOutputType::Audio,
        );
        stream.start_capture().map_err(|e| {
            anyhow!(
                "ScreenCaptureKit: start capture: {e} — \
                 grant Screen Recording permission in System Settings → \
                 Privacy & Security and try again"
            )
        })?;

        Ok(Self {
            stream,
            format: CaptureFormat {
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS,
            },
            consumer: Mutex::new(consumer),
            overflow_flag,
        })
    }

    /// Stop capture and return the accumulated samples.
    pub fn stop(self) -> Result<Vec<f32>> {
        // Stop first so any final in-flight sample callback writes
        // its bytes before we drain the ring.
        self.stream
            .stop_capture()
            .context("ScreenCaptureKit: stop capture")?;
        let mut guard = self
            .consumer
            .lock()
            .map_err(|_| anyhow!("sck consumer mutex poisoned"))?;
        let samples = drain_consumer(&mut guard);
        log_overflow_if_set(&self.overflow_flag);
        Ok(samples)
    }

    /// Drain whatever samples have accumulated **without stopping**
    /// the SCStream. The streaming pump (#108 PR3) uses this on a
    /// tight tick to feed a `WhisperStreamingSession` between
    /// stop()-style chunk boundaries.
    ///
    /// The SCK callback continues writing into the producer end of
    /// the ring after the drain — only the consumer half is touched
    /// here.
    ///
    /// Returns an empty Vec if the callback hasn't written anything
    /// since the previous drain — that's a normal "tick fired
    /// faster than the audio callback" condition, not an error.
    pub fn drain_buffer(&self) -> Result<Vec<f32>> {
        let mut guard = self
            .consumer
            .lock()
            .map_err(|_| anyhow!("sck consumer mutex poisoned"))?;
        let samples = drain_consumer(&mut guard);
        log_overflow_if_set(&self.overflow_flag);
        Ok(samples)
    }

    /// Format the in-flight buffer is being captured in. Used by
    /// the public `stop()` to package the drained samples back into
    /// a [`super::CapturedAudio`].
    pub fn format(&self) -> CaptureFormat {
        self.format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that the wrapper actually upgrades
    /// `Producer<f32>` to `Sync`. If a future refactor accidentally
    /// removes the unsafe impl, the SCStreamOutputTrait bound on
    /// `AudioHandler` would no longer hold and the build would
    /// regress to a less-helpful error elsewhere — pin the contract
    /// here directly.
    #[test]
    fn serial_producer_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SerialProducer>();
    }

    /// Round-trip a few samples through a real `SerialProducer` to
    /// prove `with_inner` actually exposes mutable access. rtrb's
    /// own crate covers the ring semantics; this test pins the
    /// wrapper's contract.
    #[test]
    fn serial_producer_pushes_through_with_inner() {
        let (producer, mut consumer) = RingBuffer::<f32>::new(4);
        let serial = SerialProducer::new(producer);

        serial.with_inner(|p| {
            for v in [0.1, 0.2, 0.3] {
                p.push(v).expect("ring has space");
            }
        });

        let drained = drain_consumer(&mut consumer);
        assert_eq!(drained, vec![0.1, 0.2, 0.3]);
    }

    /// On a full ring, push returns `Err`. The handler treats this
    /// as overflow and sets the flag; the worker logs once on its
    /// next drain. This pins the contract that the wrapper does
    /// not silently drop the error.
    #[test]
    fn serial_producer_surfaces_overflow_when_ring_is_full() {
        let (producer, _consumer) = RingBuffer::<f32>::new(2);
        let serial = SerialProducer::new(producer);

        let mut overflowed = false;
        serial.with_inner(|p| {
            for v in [0.1, 0.2, 0.3, 0.4] {
                if p.push(v).is_err() {
                    overflowed = true;
                }
            }
        });
        assert!(
            overflowed,
            "pushing past capacity should surface PushError::Full to the caller"
        );
    }
}
