//! Rolling per-source audio buffer for the diarizer (#111 PR-F).
//!
//! The meeting pump needs to hand each finalised utterance's audio
//! to the diarizer (`OnnxDiarizer` runs an ONNX speaker-embedding
//! model on it). The streaming transcription session owns its own
//! sliding window internally but doesn't surface per-utterance
//! audio when finals come out — by the time `drain` returns
//! finals, the audio is still in the session but there's no API
//! to get it back out.
//!
//! Rather than extend the `StreamingTranscribeSession` trait
//! surface (which would force every backend + test mock to grow
//! the new method), we keep an independent rolling buffer in the
//! pump, parallel to the streaming session, in the **canonical
//! 16 kHz mono** format the diarizer expects. The pump appends to
//! the buffer every tick (right alongside feeding the streaming
//! session); when finals come out, the pump slices each
//! utterance's `[started_at_ms, ended_at_ms)` range out of the
//! buffer to hand to the diarizer.
//!
//! ## Why canonical format here, not raw
//!
//! The diarizer trait takes a single `CaptureFormat` for all
//! chunks. Each source can emit at a different rate / channel
//! count (mic at 48 kHz stereo, system at 44.1 kHz stereo, etc.).
//! Storing raw means we'd have to either pass per-chunk formats
//! (trait change) or resample at slice time (every diarize call).
//! Storing canonical means resampling once on append — the
//! amortised cost is the same, but the trait contract stays
//! clean.
//!
//! ## Bounded memory
//!
//! The buffer drops oldest samples once total length exceeds
//! [`MAX_BUFFER_MS`]. The dropped offset is tracked so absolute-
//! session-time slicing keeps working — utterances that started
//! before the dropped horizon return whatever fragment is still
//! in the buffer (or empty if the whole utterance is gone).
//! [`MAX_BUFFER_MS`] is 30 seconds: longer than the streaming
//! session's `window_max_ms` (30 s) so any utterance the session
//! could still be revising is still in the diarizer's reach.

use std::collections::VecDeque;

use zeroize::Zeroize;

use crate::audio::CaptureFormat;
use crate::transcription::resample::resample_to_mono;

/// Sample rate every utterance handed to the diarizer is at.
/// Matches the canonical format used everywhere else in the pipeline
/// (whisper preprocessing, OnnxDiarizer's MelExtractor input).
pub const CANONICAL_SAMPLE_RATE_HZ: u32 = 16_000;

/// Maximum buffer length in milliseconds. Sized to at least the
/// streaming session's `window_max_ms` (30 s) so any utterance the
/// session could revise is still slice-able. A few extra seconds
/// would be defensive; we trade the memory for the simpler bound.
pub const MAX_BUFFER_MS: u64 = 30_000;

/// Rolling 16 kHz mono buffer with absolute-session-time addressing.
///
/// **Privacy: scrubbed on Drop.** Like
/// [`crate::transcription::streaming::SlidingWindowState`] the
/// buffer holds raw PCM that's exactly the audio the user just
/// said. We zeroize the in-flight samples + reset the dropped-ms
/// counter when the buffer is destroyed (session end / pump
/// shutdown) so the bytes don't survive in process memory beyond
/// their useful lifetime. The `zeroize` crate uses a volatile
/// write + compiler fence the optimiser cannot elide; a hand-
/// rolled `iter_mut` zero-loop in `Drop` is legally elidable on
/// release builds. See `Cargo.toml` for the rationale already
/// recorded against the `zeroize` dep.
pub struct AudioRollingBuffer {
    /// Mono 16 kHz f32 samples. Front is oldest. `VecDeque` so
    /// front-dropping is O(1) amortised — `Vec::drain(0..n)` would
    /// be O(n).
    samples: VecDeque<f32>,
    /// Cumulative ms of audio dropped from the front so far. Adds
    /// to a slice-time offset to translate absolute session times
    /// into buffer-relative sample indices.
    dropped_ms: u64,
}

impl Default for AudioRollingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioRollingBuffer {
    fn drop(&mut self) {
        // VecDeque stores its elements in a ring buffer split
        // across (up to) two contiguous slices; `as_mut_slices`
        // exposes both. Zeroize each one explicitly. After the
        // zero-pass the deque's logical length and capacity are
        // unchanged, but the bytes the OS would dump on swap-out
        // / core-dump are scrubbed.
        let (head, tail) = self.samples.as_mut_slices();
        head.zeroize();
        tail.zeroize();
        self.dropped_ms = 0;
    }
}

impl AudioRollingBuffer {
    /// Construct an empty buffer.
    pub fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            dropped_ms: 0,
        }
    }

    /// Append `chunk` (in `chunk_format`) to the buffer, converting
    /// to canonical 16 kHz mono on the way in. Drops oldest samples
    /// if the buffer would exceed [`MAX_BUFFER_MS`], advancing
    /// `dropped_ms` so subsequent `slice` calls still resolve
    /// absolute session times correctly.
    pub fn append(&mut self, chunk: &[f32], chunk_format: CaptureFormat) {
        if chunk.is_empty() {
            return;
        }

        let mono: Vec<f32> = if chunk_format.channels > 1 {
            crate::audio::downmix_to_mono(chunk, chunk_format.channels)
        } else {
            chunk.to_vec()
        };
        let canonical: Vec<f32> = if chunk_format.sample_rate == CANONICAL_SAMPLE_RATE_HZ {
            mono
        } else {
            resample_to_mono(&mono, chunk_format.sample_rate, CANONICAL_SAMPLE_RATE_HZ)
        };

        self.samples.extend(canonical);

        let max_samples = ms_to_samples(MAX_BUFFER_MS);
        if self.samples.len() > max_samples {
            let drop_count = self.samples.len() - max_samples;
            self.samples.drain(..drop_count);
            self.dropped_ms = self.dropped_ms.saturating_add(samples_to_ms(drop_count));
        }
    }

    /// Slice samples in the half-open absolute session-time range
    /// `[start_ms, end_ms)`. Returns whatever portion is still in
    /// the buffer; an entirely-dropped utterance returns an empty
    /// vec, an entirely-future utterance also returns empty.
    ///
    /// `end_ms < start_ms` (caller passed reversed times) returns
    /// empty rather than panicking — the diarizer's `embed` will
    /// then short-circuit on the under-min-frames check.
    pub fn slice_ms(&self, start_ms: u64, end_ms: u64) -> Vec<f32> {
        if end_ms <= start_ms {
            return Vec::new();
        }
        // Translate absolute session ms → buffer-relative ms.
        // Saturating: if start is before the dropped horizon,
        // clamp to the buffer's front (relative ms = 0).
        let rel_start_ms = start_ms.saturating_sub(self.dropped_ms);
        let rel_end_ms = end_ms.saturating_sub(self.dropped_ms);
        if rel_end_ms <= rel_start_ms {
            // The whole [start, end) range is in the dropped
            // history.
            return Vec::new();
        }

        let start_sample = ms_to_samples(rel_start_ms);
        let end_sample = ms_to_samples(rel_end_ms).min(self.samples.len());
        if end_sample <= start_sample {
            return Vec::new();
        }
        self.samples
            .iter()
            .skip(start_sample)
            .take(end_sample - start_sample)
            .copied()
            .collect()
    }

    /// Total samples buffered today. Diagnostic / test hook.
    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }
}

fn ms_to_samples(ms: u64) -> usize {
    ((ms * CANONICAL_SAMPLE_RATE_HZ as u64) / 1000) as usize
}

fn samples_to_ms(samples: usize) -> u64 {
    (samples as u64 * 1000) / CANONICAL_SAMPLE_RATE_HZ as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_canonical() -> CaptureFormat {
        CaptureFormat {
            sample_rate: CANONICAL_SAMPLE_RATE_HZ,
            channels: 1,
        }
    }

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    #[test]
    fn empty_append_is_noop() {
        let mut b = AudioRollingBuffer::new();
        b.append(&[], fmt_canonical());
        assert_eq!(b.len(), 0);
        assert!(b.slice_ms(0, 1000).is_empty());
    }

    #[test]
    fn appends_canonical_format_passthrough() {
        let mut b = AudioRollingBuffer::new();
        // 1 s at 16 kHz mono = 16000 samples.
        b.append(&ramp(16_000), fmt_canonical());
        assert_eq!(b.len(), 16_000);
    }

    #[test]
    fn appends_resamples_48khz_to_16khz() {
        let mut b = AudioRollingBuffer::new();
        // 1 s at 48 kHz mono = 48000 samples; resample → ~16000.
        b.append(
            &ramp(48_000),
            CaptureFormat {
                sample_rate: 48_000,
                channels: 1,
            },
        );
        // Linear resample at 3:1 lands at exactly 16000 (or off
        // by one; the resample rounds toward zero on the index).
        assert!(
            (b.len() as i64 - 16_000).abs() <= 1,
            "expected ~16000, got {}",
            b.len()
        );
    }

    #[test]
    fn appends_downmixes_stereo() {
        let mut b = AudioRollingBuffer::new();
        // 4 stereo samples = 2 mono samples after downmix.
        b.append(
            &[1.0, -1.0, 0.5, 0.5],
            CaptureFormat {
                sample_rate: CANONICAL_SAMPLE_RATE_HZ,
                channels: 2,
            },
        );
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn slice_returns_correct_range() {
        let mut b = AudioRollingBuffer::new();
        // 2 s of audio (32_000 samples). Slice [500, 1500) ms →
        // samples [8_000, 24_000), 16_000 samples.
        b.append(&ramp(32_000), fmt_canonical());
        let s = b.slice_ms(500, 1500);
        assert_eq!(s.len(), 16_000);
        assert!((s[0] - 8000.0).abs() < 1e-6);
        assert!((s[s.len() - 1] - 23999.0).abs() < 1e-6);
    }

    #[test]
    fn slice_reversed_range_is_empty() {
        let mut b = AudioRollingBuffer::new();
        b.append(&ramp(16_000), fmt_canonical());
        assert!(b.slice_ms(1000, 500).is_empty());
        assert!(b.slice_ms(500, 500).is_empty());
    }

    #[test]
    fn slice_after_drop_uses_absolute_time() {
        // Append > MAX_BUFFER_MS so the front gets dropped, then
        // confirm a slice at an absolute-session-time that lives
        // *after* the dropped horizon still returns the right
        // samples. This is the load-bearing property — the pump
        // runs for hours; the buffer drops constantly, but
        // utterance timestamps are absolute.
        let mut b = AudioRollingBuffer::new();
        // 35 s of audio at 16 kHz = 560_000 samples; buffer caps
        // at 30 s = 480_000.
        b.append(&ramp(35 * 16_000), fmt_canonical());
        assert!(b.len() <= ms_to_samples(MAX_BUFFER_MS));
        // The first 5 s (0..5000 ms) should be entirely dropped.
        assert!(b.slice_ms(0, 5_000).is_empty());
        // A slice from 30_000..31_000 ms is in the buffer; the
        // ramp value at sample 480_000 corresponds to that point.
        let s = b.slice_ms(30_000, 31_000);
        assert_eq!(s.len(), 16_000);
        // Ramp value at the start of [30_000, 31_000) ms is the
        // sample at index 30_000 * 16 = 480_000.
        assert!((s[0] - 480_000.0).abs() < 1.0, "ramp head = {}", s[0]);
    }

    #[test]
    fn slice_partially_dropped_returns_remaining_tail() {
        // Buffer holds [t=5000ms, t=35000ms) after a drop. A slice
        // request [4000, 7000) overlaps the dropped horizon at the
        // front — should return only the in-buffer portion (5000
        // to 7000 ms = 2000 ms = 32000 samples), starting at the
        // ramp value for sample 80_000 (the head of the buffer).
        let mut b = AudioRollingBuffer::new();
        b.append(&ramp(35 * 16_000), fmt_canonical());
        let s = b.slice_ms(4_000, 7_000);
        assert_eq!(s.len(), 2 * 16_000);
        assert!((s[0] - 80_000.0).abs() < 1.0);
    }

    #[test]
    fn slice_entirely_in_future_returns_empty() {
        let mut b = AudioRollingBuffer::new();
        b.append(&ramp(16_000), fmt_canonical());
        // Buffer covers [0, 1000) ms; a slice request [2000, 3000)
        // is entirely after the head.
        assert!(b.slice_ms(2_000, 3_000).is_empty());
    }

    #[test]
    fn dropped_ms_advances_with_drops() {
        // Driving the public API only — `dropped_ms` is private,
        // but its behaviour leaks through the absolute-time slice.
        // After 35 s of audio we expect 5 s of drops.
        let mut b = AudioRollingBuffer::new();
        b.append(&ramp(35 * 16_000), fmt_canonical());
        // Verify the dropped horizon by slicing [4500, 5500) — we
        // expect ~500 ms of audio (the post-horizon half).
        let s = b.slice_ms(4_500, 5_500);
        assert!(
            (s.len() as i64 - 8_000).abs() < 100,
            "expected ~8000 samples (500 ms), got {}",
            s.len()
        );
    }

    #[test]
    fn ms_to_samples_round_trip() {
        for &ms in &[0_u64, 1, 100, 500, 1000, 9999, 30_000] {
            let samples = ms_to_samples(ms);
            let back = samples_to_ms(samples);
            // Conversion rounds toward zero — round-trip can lose
            // sub-millisecond precision but the absolute error is
            // bounded by 1 ms per direction.
            assert!(back <= ms);
            assert!(ms.saturating_sub(back) <= 1, "ms={ms} back={back}");
        }
    }

    #[test]
    fn drop_runs_without_panicking_when_buffer_has_data() {
        // Smoke: the Drop impl zeroizes both halves of the
        // VecDeque's ring buffer. That zeroization itself is
        // covered by the `zeroize` crate's own test suite — same
        // shape as `transcription::streaming::SlidingWindowState`
        // (which also doesn't unit-test the actual zeroization).
        // This test pins that the Drop wiring exists and doesn't
        // panic on a populated buffer.
        let mut b = AudioRollingBuffer::new();
        b.append(&ramp(16_000), fmt_canonical());
        assert_eq!(b.len(), 16_000);
        drop(b);
    }
}
