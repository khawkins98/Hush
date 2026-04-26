//! macOS system-audio capture via ScreenCaptureKit (#105).
//!
//! Compiled only on macOS with the `screencapturekit` feature on.
//! When present, this module gives [`super::CpalAudioCapture`] a parallel
//! capture path that pulls system audio (whatever the OS mixer is
//! routing to the speakers — Zoom calls, browser audio, music) into the
//! same `Vec<f32>` shape that the cpal mic path produces, so the rest
//! of the transcription pipeline does not need to know which source
//! it came from.
//!
//! ## Why ScreenCaptureKit
//!
//! Apple deprecated CoreAudio's HAL plug-in path for application audio
//! capture in macOS 14, and the new Tap APIs (`AudioHardwareCreateProcessTap`)
//! require entitlements only available to MAS-distributed apps. SCK is
//! the only sanctioned, non-entitled route to system-audio on consumer
//! macOS as of 2026, and the `screencapturekit` crate exposes the
//! Swift-side API through stable FFI so we can stay in pure Rust.
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
//! handler closes over `Arc<Mutex<Vec<f32>>>` and `Arc<AtomicU32>` so
//! the public API stays synchronous from the caller's perspective —
//! same shape as the cpal worker-thread plumbing.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use screencapturekit::{
    cm::CMSampleBuffer,
    shareable_content::SCShareableContent,
    stream::{
        configuration::SCStreamConfiguration, content_filter::SCContentFilter,
        output_trait::SCStreamOutputTrait, output_type::SCStreamOutputType, SCStream,
    },
};

use super::CaptureFormat;

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
/// of one recording and the shared buffer the callback writes into.
pub struct ScreenCaptureKitSession {
    /// Held alive for the duration of capture. Dropping it stops the
    /// stream; we also call `stop_capture()` explicitly in [`Self::stop`]
    /// so any final in-flight callbacks have settled before we drain.
    stream: SCStream,
    /// Format the buffer was captured in, surfaced back through the
    /// trait's [`super::CapturedAudio`] alongside the samples.
    pub format: CaptureFormat,
    /// Shared with the SCK callback. The callback locks briefly per
    /// sample buffer; the worker drains the whole thing on stop. Same
    /// discipline as the cpal path.
    buffer: Arc<Mutex<Vec<f32>>>,
}

/// Sample-buffer handler installed on the SCStream. Holds the buffer +
/// level Arcs the cpal path also writes through, so the HUD level pump
/// and the eventual drain see the same atomic / mutex shape regardless
/// of which capture source is active.
struct AudioHandler {
    buffer: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(
        &self,
        sample: CMSampleBuffer,
        of_type: SCStreamOutputType,
    ) {
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

        // Locking the mutex briefly is fine — the only other holder is
        // the worker thread on stop, by which point capture has been
        // halted via `stop_capture()` and no more callbacks land.
        match self.buffer.lock() {
            Ok(mut guard) => guard.extend_from_slice(&samples),
            Err(poisoned) => poisoned.into_inner().extend_from_slice(&samples),
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

        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(
            AudioHandler {
                buffer: Arc::clone(&buffer),
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
            buffer,
        })
    }

    /// Stop capture and return the accumulated samples.
    pub fn stop(self) -> Result<Vec<f32>> {
        // Stop first so any final in-flight sample callback writes
        // its bytes before we take the buffer.
        self.stream
            .stop_capture()
            .context("ScreenCaptureKit: stop capture")?;
        let mut guard = self
            .buffer
            .lock()
            .map_err(|_| anyhow!("audio buffer mutex poisoned"))?;
        Ok(std::mem::take(&mut *guard))
    }

    /// Format the in-flight buffer is being captured in. Used by
    /// the public `stop()` to package the drained samples back into
    /// a [`super::CapturedAudio`].
    pub fn format(&self) -> CaptureFormat {
        self.format
    }
}
