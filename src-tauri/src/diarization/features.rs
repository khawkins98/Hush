//! Mel-Filterbank feature extraction for the D2 speaker-embedding
//! diarizer (#111).
//!
//! The wespeaker ResNet34-LM ONNX model takes **80-dim Mel-FB
//! features** as input — pre-extracted from the audio rather than
//! raw waveform. PR-D wires the model; this module is the
//! preprocessing step that bridges raw 16 kHz mono PCM to the
//! `(num_frames, 80)` tensor the model expects.
//!
//! ## Pipeline
//!
//! 1. **Pre-emphasis**: `y[n] = x[n] - 0.97 * x[n-1]`. Boosts
//!    high-frequency energy that the rest of the pipeline tends to
//!    underweight.
//! 2. **Frame**: 25 ms windows (400 samples @ 16 kHz) at 10 ms hop
//!    (160 samples). Frame `i` covers samples `[i*160, i*160+400)`.
//! 3. **Window**: Povey window `(0.5 - 0.5*cos(2π·n/(N-1)))^0.85`.
//!    Kaldi's default; matches what wespeaker's training-time
//!    feature extractor uses (`torchaudio.compliance.kaldi.fbank`,
//!    `window_type="povey"`).
//! 4. **FFT**: real-input FFT, size 512 (next power of 2 above the
//!    400-sample frame; the frame is zero-padded). Output is 257
//!    complex bins.
//! 5. **Power spectrum**: `|X[k]|² = re² + im²` for each bin.
//! 6. **Mel filterbank**: 80 triangular filters spanning 20 Hz to
//!    Nyquist (8000 Hz @ 16 kHz). Mel-scale uses the HTK formula
//!    `mel = 1127 * ln(1 + hz/700)`, which matches Kaldi's default
//!    (`htk_compat=true`).
//! 7. **Log**: `log(max(filter_energy, ε))` with `ε = 1e-10` as
//!    the floor — silent frames need a floor so the model doesn't
//!    see `-∞`.
//!
//! ## Numerical fidelity to torchaudio.kaldi.fbank
//!
//! Unit tests in this module verify the pipeline is **structurally
//! correct**: frame-count math, sine waves peaking in the expected
//! mel bins, mel-scale monotonicity, log-floor on silence. They do
//! **not** verify bit-exact match to `torchaudio.compliance.kaldi.
//! fbank` — which is what wespeaker uses at training time. The
//! end-to-end correctness check (does the embedding model produce
//! sane embeddings on real audio?) lives in PR-D's hands-on
//! validation against a recorded meeting fixture.
//!
//! Known minor differences vs the reference:
//! - **Dither**: kaldi's default `dither=1.0` adds Gaussian noise
//!   to avoid `log(0)` on silent frames. We skip dither (use a
//!   constant log-floor instead) — matches torchaudio's
//!   `dither=0.0` setting that production model deployments
//!   typically use for determinism.
//! - **Edges**: kaldi's `snip_edges=true` skips frames that don't
//!   fully fit. We do the same.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

/// Sample rate the diarizer's Mel-FB extractor expects. Wespeaker
/// is trained at 16 kHz; PR-D will resample upstream of this
/// module's entry point. Hard-coded as a `const` rather than a
/// parameter because the mel filter centres + the FFT size are
/// computed at construction time relative to it; making the rate
/// configurable would mean recomputing those, with no use case
/// the diarizer has today.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Frame length — 25 ms at 16 kHz = 400 samples. Matches kaldi's
/// `frame_length=25.0` default.
pub const FRAME_SIZE: usize = 400;

/// Frame shift / hop — 10 ms at 16 kHz = 160 samples. Matches
/// kaldi's `frame_shift=10.0` default.
pub const FRAME_HOP: usize = 160;

/// FFT size. The next power of 2 above [`FRAME_SIZE`]; the 400-
/// sample frame is zero-padded to 512. Matches kaldi's
/// `round_to_power_of_two=true` default.
pub const FFT_SIZE: usize = 512;

/// Number of mel-filterbank bins. Matches the wespeaker config
/// (`fbank_args.num_mel_bins=80`).
pub const NUM_MEL_BINS: usize = 80;

/// Pre-emphasis coefficient. Standard kaldi default.
pub const PREEMPH_COEFF: f32 = 0.97;

/// Lower edge of the mel filterbank, in Hz. Kaldi default.
pub const LOW_FREQ_HZ: f32 = 20.0;

/// Power-spectrum floor passed to `log()`. Silent frames otherwise
/// produce `-∞`, which crashes downstream tensor ops.
const LOG_FLOOR: f32 = 1e-10;

/// Compute Mel-Filterbank features from a 16 kHz mono PCM signal.
///
/// Returns a `(num_frames, NUM_MEL_BINS)` row-major matrix as a
/// flat `Vec<f32>` — element `(frame, bin)` is at index
/// `frame * NUM_MEL_BINS + bin`. Empty input → empty vector;
/// signals shorter than [`FRAME_SIZE`] also return empty (kaldi
/// `snip_edges=true` semantics).
///
/// The returned features are in the shape the wespeaker ONNX
/// model expects (modulo a transpose / batch-dim that the model
/// caller adds). The model wants `(batch, num_frames, num_mels)`
/// in NCHW-style — the `(num_frames, num_mels)` layout produced
/// here lets PR-D stack features into a 3-d ndarray without
/// reshaping.
pub fn mel_filterbank(samples: &[f32]) -> Vec<f32> {
    MelExtractor::new().extract(samples)
}

/// Reusable Mel-FB extractor that holds the FFT plan + filterbank
/// matrix so consecutive calls don't re-plan or re-compute. PR-D's
/// `OnnxDiarizer` will own one of these for the session lifetime
/// to amortise the planning cost across utterances.
pub struct MelExtractor {
    /// Planned 512-pt real FFT. `Arc` so the extractor can be
    /// cheaply cloned across `Send` boundaries (the meeting pump
    /// hands one of these to the inference task per chunk).
    fft: Arc<dyn RealToComplex<f32>>,
    /// Pre-computed Povey window, `FRAME_SIZE` long.
    window: Vec<f32>,
    /// Mel filterbank matrix, row-major `(NUM_MEL_BINS,
    /// FFT_SIZE/2 + 1)`. Each row is one triangular filter's
    /// gains across the FFT bins.
    filterbank: Vec<f32>,
}

impl Default for MelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MelExtractor {
    /// Construct a fresh extractor. Plans the 512-pt real FFT and
    /// computes the Povey window + 80-bin mel filterbank up front.
    /// All allocations happen here; `extract` is allocation-light
    /// (one scratch buffer + the output).
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window = povey_window(FRAME_SIZE);
        let filterbank = mel_filterbank_matrix(
            NUM_MEL_BINS,
            FFT_SIZE / 2 + 1,
            SAMPLE_RATE_HZ as f32,
            LOW_FREQ_HZ,
            (SAMPLE_RATE_HZ as f32) / 2.0,
        );
        Self {
            fft,
            window,
            filterbank,
        }
    }

    /// Run the full pipeline on `samples`. See [`mel_filterbank`]
    /// for the output shape contract.
    pub fn extract(&self, samples: &[f32]) -> Vec<f32> {
        if samples.len() < FRAME_SIZE {
            return Vec::new();
        }

        let pre_emphasised = preemphasise(samples, PREEMPH_COEFF);
        let num_frames = ((pre_emphasised.len() - FRAME_SIZE) / FRAME_HOP) + 1;
        let mut output = Vec::with_capacity(num_frames * NUM_MEL_BINS);

        // realfft buffers — sized via make_*_vec so process() never
        // returns an error from a length mismatch. Allocated per
        // call so `extract` stays Send + thread-friendly without
        // requiring &mut self.
        let mut frame_buf = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();

        for frame_idx in 0..num_frames {
            let start = frame_idx * FRAME_HOP;

            // Copy the frame, apply window, zero-pad to FFT_SIZE.
            for (i, slot) in frame_buf.iter_mut().enumerate() {
                if i < FRAME_SIZE {
                    *slot = pre_emphasised[start + i] * self.window[i];
                } else {
                    *slot = 0.0;
                }
            }

            self.fft
                .process(&mut frame_buf, &mut spectrum)
                .expect("realfft buffers were sized via make_*_vec; cannot fail");

            // Power spectrum: |X[k]|² = re² + im².
            // We collect into a small stack-style buffer then apply
            // the filterbank — keeps the inner loop tight.
            let mut power = [0.0_f32; FFT_SIZE / 2 + 1];
            for (i, c) in spectrum.iter().enumerate() {
                power[i] = c.re * c.re + c.im * c.im;
            }

            // Apply each triangular mel filter and log the result.
            let row_len = FFT_SIZE / 2 + 1;
            for mel_idx in 0..NUM_MEL_BINS {
                let row = &self.filterbank[mel_idx * row_len..(mel_idx + 1) * row_len];
                let energy: f32 = row.iter().zip(power.iter()).map(|(g, p)| g * p).sum();
                output.push(energy.max(LOG_FLOOR).ln());
            }
        }

        output
    }
}

/// Apply a first-order pre-emphasis filter to `samples`.
///
/// `y[0] = x[0]`; `y[n] = x[n] - coeff * x[n-1]` for `n >= 1`. The
/// returned vector is the same length as the input. Kaldi defaults
/// to `coeff = 0.97`.
fn preemphasise(samples: &[f32], coeff: f32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(samples.len());
    out.push(samples[0]);
    for i in 1..samples.len() {
        out.push(samples[i] - coeff * samples[i - 1]);
    }
    out
}

/// Construct the Povey window of length `n`.
///
/// Povey is kaldi's default and what `torchaudio.compliance.kaldi.
/// fbank(window_type="povey")` produces. It's a Hann window raised
/// to the 0.85 power; flatter top + slightly steeper roll-off than
/// Hann, which is what kaldi's authors found gave the best speech-
/// recognition results.
///
/// Formula: `w[n] = (0.5 - 0.5*cos(2π·n / (N-1)))^0.85` for
/// `n in [0, N-1]`.
fn povey_window(n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }
    let mut w = Vec::with_capacity(n);
    let denom = (n - 1) as f32;
    for i in 0..n {
        let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (i as f32) / denom).cos();
        w.push(hann.powf(0.85));
    }
    w
}

/// Convert a frequency in Hz to the mel scale. Uses the HTK
/// formula `mel = 1127 * ln(1 + hz/700)` — this matches kaldi's
/// `htk_compat=true` default.
fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Inverse of [`hz_to_mel`].
fn mel_to_hz(mel: f32) -> f32 {
    700.0 * ((mel / 1127.0).exp() - 1.0)
}

/// Build the mel-filterbank matrix.
///
/// `num_mels` triangular filters span `low_hz` → `high_hz`, with
/// centres equally spaced on the mel scale. Each filter's response
/// across the `num_fft_bins` linear-frequency FFT bins is one row
/// of the returned `(num_mels, num_fft_bins)` row-major matrix.
///
/// The returned values are filter gains in `[0, 1]`, peaking at 1
/// on the centre frequency and ramping linearly to 0 at the
/// adjacent centres — the standard triangular shape.
fn mel_filterbank_matrix(
    num_mels: usize,
    num_fft_bins: usize,
    sample_rate: f32,
    low_hz: f32,
    high_hz: f32,
) -> Vec<f32> {
    // Mel-spaced centre frequencies. We need `num_mels + 2` points
    // (left-edge, num_mels centres, right-edge) so each triangle
    // can find its left + right neighbours.
    let mel_low = hz_to_mel(low_hz);
    let mel_high = hz_to_mel(high_hz);
    let mut mel_points = Vec::with_capacity(num_mels + 2);
    for i in 0..(num_mels + 2) {
        let fraction = (i as f32) / ((num_mels + 1) as f32);
        mel_points.push(mel_low + fraction * (mel_high - mel_low));
    }
    let hz_points: Vec<f32> = mel_points.iter().copied().map(mel_to_hz).collect();

    let mut matrix = vec![0.0_f32; num_mels * num_fft_bins];
    let bin_hz = sample_rate / ((num_fft_bins - 1) as f32 * 2.0);

    for m in 0..num_mels {
        let left = hz_points[m];
        let centre = hz_points[m + 1];
        let right = hz_points[m + 2];

        for k in 0..num_fft_bins {
            let f = (k as f32) * bin_hz;
            let gain = if f < left || f > right {
                0.0
            } else if f <= centre {
                (f - left) / (centre - left).max(f32::EPSILON)
            } else {
                (right - f) / (right - centre).max(f32::EPSILON)
            };
            matrix[m * num_fft_bins + k] = gain;
        }
    }

    matrix
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 kHz, two-second sine wave at `freq_hz` Hz.
    fn sine(freq_hz: f32, duration_s: f32) -> Vec<f32> {
        let n = (duration_s * SAMPLE_RATE_HZ as f32) as usize;
        let mut s = Vec::with_capacity(n);
        let omega = 2.0 * std::f32::consts::PI * freq_hz / (SAMPLE_RATE_HZ as f32);
        for i in 0..n {
            s.push((omega * i as f32).sin());
        }
        s
    }

    #[test]
    fn preemphasis_first_sample_unchanged() {
        let s = vec![1.0, 0.5, -0.25];
        let out = preemphasise(&s, 0.97);
        assert!((out[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn preemphasis_subsequent_samples_apply_coefficient() {
        // y[1] = x[1] - 0.97*x[0] = 0.5 - 0.97*1.0 = -0.47
        // y[2] = x[2] - 0.97*x[1] = -0.25 - 0.97*0.5 = -0.735
        let s = vec![1.0, 0.5, -0.25];
        let out = preemphasise(&s, 0.97);
        assert!((out[1] - (-0.47)).abs() < 1e-5, "got {}", out[1]);
        assert!((out[2] - (-0.735)).abs() < 1e-5, "got {}", out[2]);
    }

    #[test]
    fn preemphasis_empty_input_returns_empty() {
        assert!(preemphasise(&[], 0.97).is_empty());
    }

    #[test]
    fn povey_window_endpoints_are_zero() {
        // (0.5 - 0.5*cos(0))^0.85 = 0^0.85 = 0
        // (0.5 - 0.5*cos(2π))^0.85 = 0^0.85 = 0
        let w = povey_window(400);
        assert!(w[0].abs() < 1e-6, "left endpoint: {}", w[0]);
        assert!(w[399].abs() < 1e-6, "right endpoint: {}", w[399]);
    }

    #[test]
    fn povey_window_peaks_at_middle() {
        // Centre of the window has cos(π) = -1, so Hann = 1.0,
        // and 1.0^0.85 = 1.0.
        let w = povey_window(401);
        let centre = w[200];
        assert!((centre - 1.0).abs() < 1e-4, "centre: {centre}");
    }

    #[test]
    fn povey_window_short_inputs() {
        assert!(povey_window(0).is_empty());
        assert_eq!(povey_window(1), vec![1.0]);
    }

    #[test]
    fn hz_to_mel_to_hz_round_trips() {
        for &hz in &[100.0_f32, 500.0, 1000.0, 4000.0, 8000.0] {
            let round = mel_to_hz(hz_to_mel(hz));
            assert!((round - hz).abs() < 1e-3, "hz={hz} round={round}");
        }
    }

    #[test]
    fn hz_to_mel_is_monotonic() {
        // Mel scale must be strictly increasing in hz — otherwise
        // the filterbank's centre frequencies would overlap.
        let mut prev = hz_to_mel(0.0);
        for hz_int in 1..=8000_u32 {
            let m = hz_to_mel(hz_int as f32);
            assert!(m > prev, "non-monotonic at {hz_int} Hz");
            prev = m;
        }
    }

    #[test]
    fn mel_filterbank_matrix_rows_are_non_empty_and_bounded() {
        // Every mel filter must touch at least one FFT bin (no
        // empty rows) and gain must never exceed 1.0 (the
        // triangle's analytic peak). The lowest few mel bins are
        // narrower than the FFT-bin spacing (~31 Hz) so their
        // *discrete* peak gain can be well below 1.0 — that is
        // expected, not a bug. The model expects this exact
        // discretisation since training-time features are computed
        // the same way.
        let m = mel_filterbank_matrix(80, 257, 16000.0, 20.0, 8000.0);
        for mel_idx in 0..80 {
            let row = &m[mel_idx * 257..(mel_idx + 1) * 257];
            let max = row.iter().cloned().fold(0.0_f32, f32::max);
            assert!(max > 0.0, "mel {mel_idx} has zero gain everywhere");
            assert!(max <= 1.0 + 1e-6, "mel {mel_idx} exceeds 1.0: {max}");
        }
    }

    #[test]
    fn mel_filterbank_matrix_centres_are_increasing() {
        // The bin where each filter peaks should advance as the
        // mel index increases. This catches a class of bugs where
        // the mel-spacing calculation breaks (e.g. wrong scale,
        // off-by-one).
        let m = mel_filterbank_matrix(80, 257, 16000.0, 20.0, 8000.0);
        let mut prev_peak_bin = 0_usize;
        for mel_idx in 0..80 {
            let row = &m[mel_idx * 257..(mel_idx + 1) * 257];
            let peak = row
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            assert!(
                peak >= prev_peak_bin,
                "mel {mel_idx} peak {peak} regressed from {prev_peak_bin}"
            );
            prev_peak_bin = peak;
        }
    }

    #[test]
    fn extract_short_input_returns_empty() {
        let s = sine(1000.0, 0.01); // 160 samples — < 400-sample frame
        let out = mel_filterbank(&s);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_frame_count_matches_formula() {
        // 1 second @ 16 kHz = 16000 samples. With 400-sample frame
        // and 160-sample hop:
        //   num_frames = (16000 - 400) / 160 + 1 = 98
        let s = sine(1000.0, 1.0);
        let out = mel_filterbank(&s);
        let num_frames = out.len() / NUM_MEL_BINS;
        assert_eq!(num_frames, 98, "got {num_frames} frames");
        assert_eq!(out.len() % NUM_MEL_BINS, 0, "output not row-major aligned");
    }

    #[test]
    fn extract_silent_input_returns_log_floor() {
        // All-zeros input → every bin lands at the log floor.
        // (Pre-emphasis of zeros is still zeros; window of zeros is
        // zeros; FFT of zeros is zeros; power is zeros; filterbank
        // of zeros is zeros; log(LOG_FLOOR) is finite.)
        let s = vec![0.0_f32; 16000];
        let out = mel_filterbank(&s);
        let expected = LOG_FLOOR.ln();
        for (i, &v) in out.iter().enumerate() {
            assert!(
                (v - expected).abs() < 1e-3,
                "frame_bin {i}: expected log-floor ≈ {expected}, got {v}"
            );
        }
    }

    #[test]
    fn extract_sine_at_1khz_peaks_in_low_mel_range() {
        // A clean 1 kHz sine has all its energy at one frequency.
        // 1 kHz on the mel scale is mid-low — falls roughly around
        // mel bin 30/80 with our 20 Hz - 8 kHz range. The peak bin
        // must be in the lower half of the filterbank.
        let s = sine(1000.0, 1.0);
        let out = mel_filterbank(&s);
        let num_frames = out.len() / NUM_MEL_BINS;

        // Average energy per mel bin across all frames — peaks
        // should align even with windowing artefacts.
        let mut avg = vec![0.0_f32; NUM_MEL_BINS];
        for frame in 0..num_frames {
            for bin in 0..NUM_MEL_BINS {
                avg[bin] += out[frame * NUM_MEL_BINS + bin];
            }
        }

        let peak_bin = avg
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            peak_bin < NUM_MEL_BINS / 2,
            "1 kHz sine should peak in lower half, got bin {peak_bin}"
        );
    }

    #[test]
    fn extract_sine_at_4khz_peaks_higher_than_1khz() {
        // Sanity check: a higher-frequency sine peaks in a higher
        // mel bin. Catches a class of bugs where the mel scale or
        // the FFT-bin-to-Hz mapping is inverted.
        let bin_for = |freq: f32| {
            let s = sine(freq, 1.0);
            let out = mel_filterbank(&s);
            let num_frames = out.len() / NUM_MEL_BINS;
            let mut avg = vec![0.0_f32; NUM_MEL_BINS];
            for frame in 0..num_frames {
                for bin in 0..NUM_MEL_BINS {
                    avg[bin] += out[frame * NUM_MEL_BINS + bin];
                }
            }
            avg.iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap()
        };
        let bin_1k = bin_for(1000.0);
        let bin_4k = bin_for(4000.0);
        assert!(
            bin_4k > bin_1k,
            "4 kHz peak (bin {bin_4k}) should be higher than 1 kHz peak (bin {bin_1k})"
        );
    }

    #[test]
    fn extract_is_deterministic() {
        // Same input → same output across calls. The MelExtractor
        // takes itself by value in `extract` (consumed) so we use
        // the free function `mel_filterbank` for both runs.
        let s = sine(1000.0, 0.5);
        let a = mel_filterbank(&s);
        let b = mel_filterbank(&s);
        assert_eq!(a.len(), b.len());
        for (i, (&x, &y)) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                (x - y).abs() < 1e-6,
                "deterministic mismatch at {i}: {x} vs {y}"
            );
        }
    }
}
