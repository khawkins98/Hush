//! Mel-Filterbank feature extraction for the D2 speaker-embedding
//! diarizer (#111).
//!
//! The wespeaker ResNet34-LM ONNX model takes **80-dim Mel-FB
//! features** as input — pre-extracted from the audio rather than
//! raw waveform. This module bridges raw 16 kHz mono PCM to the
//! `(num_frames, 80)` tensor the model expects.
//!
//! ## Matching wespeaker's training-time front end (#1013)
//!
//! An embedding model only behaves as evaluated when it sees features
//! drawn from the distribution it was trained on. wespeaker (Apache-2.0)
//! computes features with `torchaudio.compliance.kaldi.fbank` and then
//! applies per-utterance cepstral mean normalisation. Read from
//! `wespeaker/dataset/processor.py::compute_fbank` / `apply_cmvn` and
//! `wespeaker/cli/speaker.py::compute_features` (the inference path):
//!
//! | step | wespeaker | pre-#1013 Hush | now |
//! |---|---|---|---|
//! | waveform scale | ×32768 (int16 range; `wavform_norm=False`) | [-1, 1] | ×32768 |
//! | window | `hamming` | Povey | Hamming |
//! | DC removal | kaldi default, per frame | none | per frame |
//! | pre-emphasis | kaldi, per frame (`x[0] -= 0.97·x[0]`) | whole signal | per frame |
//! | mel triangles | built in the mel domain | built in Hz | mel domain |
//! | log floor | `f32::EPSILON` | `1e-10` | `f32::EPSILON` |
//! | dither | 0.0 at inference (torchaudio default) | none | none |
//! | **CMN** | `feat - mean(feat, dim=0)` | **missing** | [`apply_cmn`] |
//!
//! CMN was the material gap: without it every utterance carries a
//! per-bin offset (channel colouring plus the constant `ln(32768²)`
//! from the scale mismatch) that the network never saw in training.
//! CMN cancels a constant log-domain offset exactly, so the scale
//! change alone is nearly a no-op once CMN is on — it only moves the
//! log floor relative to signal, which we keep for fidelity. The window
//! and per-frame details are small, but they are a few lines each and
//! make the extractor numerically comparable to the reference (checked
//! against torchaudio's `kaldi.fbank` during #1013, see learnings.md).
//!
//! Implemented in our own words from those descriptions; no wespeaker
//! code was copied.
//!
//! ## Pipeline (per 25 ms frame, 10 ms hop, `snip_edges=true`)
//!
//! 1. Scale to int16 range, subtract the frame's mean (DC offset).
//! 2. Pre-emphasis within the frame: `y[n] = x[n] - 0.97·x[n-1]`,
//!    `y[0] = x[0] - 0.97·x[0]` (kaldi replicates the first sample).
//! 3. Hamming window, zero-pad to 512, real FFT → 257 bins, power.
//! 4. 80 triangular mel filters (20 Hz → Nyquist, HTK mel scale
//!    `1127·ln(1 + f/700)`), triangles linear in mel, Nyquist bin
//!    weight 0.
//! 5. `ln(max(energy, f32::EPSILON))`.
//!
//! [`apply_cmn`] then runs over the whole utterance in the embedding
//! path (`onnx.rs::embed`), not here, so the structural tests below can
//! inspect absolute log energies.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

/// Sample rate the diarizer's Mel-FB extractor expects. Wespeaker
/// is trained at 16 kHz; the diarizer resamples upstream of this
/// module's entry point.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Frame length — 25 ms at 16 kHz = 400 samples.
pub const FRAME_SIZE: usize = 400;

/// Frame shift / hop — 10 ms at 16 kHz = 160 samples.
pub const FRAME_HOP: usize = 160;

/// FFT size: next power of two above [`FRAME_SIZE`].
pub const FFT_SIZE: usize = 512;

/// Number of mel-filterbank bins (`fbank_args.num_mel_bins=80`).
pub const NUM_MEL_BINS: usize = 80;

/// Pre-emphasis coefficient. Standard kaldi default.
pub const PREEMPH_COEFF: f32 = 0.97;

/// Lower edge of the mel filterbank, in Hz. Kaldi default.
pub const LOW_FREQ_HZ: f32 = 20.0;

/// Scale applied to `[-1, 1]` float PCM so it matches the int16-range
/// waveform wespeaker feeds `kaldi.fbank`.
const INT16_SCALE: f32 = 32_768.0;

/// Log floor of the kaldi-compatible path: torchaudio clamps mel
/// energies at `torch.finfo(float32).eps` before the log.
const KALDI_LOG_FLOOR: f32 = f32::EPSILON;

/// Log floor of the pre-#1013 path, retained for the A/B evaluation.
const LEGACY_LOG_FLOOR: f32 = 1e-10;

/// Analysis window shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// wespeaker's choice for the ResNet34-LM recipe.
    Hamming,
    /// Kaldi's own default, which the pre-#1013 extractor used. Only
    /// reachable from the A/B evaluation harness.
    #[cfg_attr(not(test), allow(dead_code))]
    Povey,
}

/// Front-end configuration. Production always uses
/// [`FbankConfig::WESPEAKER`]; the pre-#1013 variant survives only so
/// the ignored evaluation tests can measure what the change bought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbankConfig {
    pub window: WindowKind,
    /// int16 scale + per-frame DC removal + per-frame pre-emphasis +
    /// mel-domain triangles + `f32::EPSILON` floor. `false` reproduces
    /// the pre-#1013 extractor exactly.
    pub kaldi_compat: bool,
}

impl FbankConfig {
    /// Matches wespeaker's `compute_fbank` (minus CMN, see [`apply_cmn`]).
    pub const WESPEAKER: Self = Self {
        window: WindowKind::Hamming,
        kaldi_compat: true,
    };

    /// The extractor as it shipped before #1013.
    #[cfg(test)]
    pub const LEGACY: Self = Self {
        window: WindowKind::Povey,
        kaldi_compat: false,
    };
}

/// Compute Mel-Filterbank features from a 16 kHz mono PCM signal with
/// the production configuration.
///
/// Returns a `(num_frames, NUM_MEL_BINS)` row-major matrix as a
/// flat `Vec<f32>` — element `(frame, bin)` is at index
/// `frame * NUM_MEL_BINS + bin`. Signals shorter than [`FRAME_SIZE`]
/// return empty (kaldi `snip_edges=true` semantics).
pub fn mel_filterbank(samples: &[f32]) -> Vec<f32> {
    MelExtractor::new().extract(samples)
}

/// Per-utterance cepstral mean normalisation: subtract each mel bin's
/// mean over all frames, in place. Mirrors wespeaker's
/// `feat - torch.mean(feat, dim=0)` (Apache-2.0, `cli/speaker.py`),
/// which the embedding model saw on every training and inference
/// utterance. No variance normalisation — wespeaker uses
/// `norm_var=False`.
///
/// `features` is the row-major `(num_frames, NUM_MEL_BINS)` matrix
/// [`MelExtractor::extract`] returns. Empty input is a no-op.
pub fn apply_cmn(features: &mut [f32]) {
    let num_frames = features.len() / NUM_MEL_BINS;
    if num_frames == 0 {
        return;
    }
    // f64 accumulators: a 30 s utterance is 3000 frames of values
    // around 10–20, where f32 summation starts losing the low digits
    // the mean subtraction is supposed to preserve.
    let mut sums = [0.0_f64; NUM_MEL_BINS];
    for frame in features.chunks_exact(NUM_MEL_BINS) {
        for (s, &v) in sums.iter_mut().zip(frame) {
            *s += f64::from(v);
        }
    }
    let n = num_frames as f64;
    let means: Vec<f32> = sums.iter().map(|s| (s / n) as f32).collect();
    for frame in features.chunks_exact_mut(NUM_MEL_BINS) {
        for (v, m) in frame.iter_mut().zip(&means) {
            *v -= m;
        }
    }
}

/// Reusable Mel-FB extractor that holds the FFT plan + filterbank
/// matrix so consecutive calls don't re-plan or re-compute.
/// `OnnxDiarizer` owns one for its lifetime.
pub struct MelExtractor {
    config: FbankConfig,
    /// Planned 512-pt real FFT.
    fft: Arc<dyn RealToComplex<f32>>,
    /// Pre-computed analysis window, `FRAME_SIZE` long.
    window: Vec<f32>,
    /// Mel filterbank matrix, row-major `(NUM_MEL_BINS,
    /// FFT_SIZE/2 + 1)`.
    filterbank: Vec<f32>,
}

impl Default for MelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MelExtractor {
    /// Production extractor ([`FbankConfig::WESPEAKER`]).
    pub fn new() -> Self {
        Self::with_config(FbankConfig::WESPEAKER)
    }

    /// Extractor with an explicit front-end configuration.
    pub fn with_config(config: FbankConfig) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window = match config.window {
            WindowKind::Hamming => hamming_window(FRAME_SIZE),
            WindowKind::Povey => povey_window(FRAME_SIZE),
        };
        let nyquist = (SAMPLE_RATE_HZ as f32) / 2.0;
        let filterbank = if config.kaldi_compat {
            kaldi_mel_banks(NUM_MEL_BINS, SAMPLE_RATE_HZ as f32, LOW_FREQ_HZ, nyquist)
        } else {
            mel_filterbank_matrix(
                NUM_MEL_BINS,
                FFT_SIZE / 2 + 1,
                SAMPLE_RATE_HZ as f32,
                LOW_FREQ_HZ,
                nyquist,
            )
        };
        Self {
            config,
            fft,
            window,
            filterbank,
        }
    }

    /// The `ln` floor silent bins land on for this configuration.
    pub fn log_floor(&self) -> f32 {
        self.floor().ln()
    }

    fn floor(&self) -> f32 {
        if self.config.kaldi_compat {
            KALDI_LOG_FLOOR
        } else {
            LEGACY_LOG_FLOOR
        }
    }

    /// Run the full pipeline on `samples`. See [`mel_filterbank`]
    /// for the output shape contract.
    pub fn extract(&self, samples: &[f32]) -> Vec<f32> {
        if samples.len() < FRAME_SIZE {
            return Vec::new();
        }

        // Legacy path pre-emphasises the whole signal once; the kaldi
        // path does it per frame below.
        let legacy_preemph;
        let source: &[f32] = if self.config.kaldi_compat {
            samples
        } else {
            legacy_preemph = preemphasise(samples, PREEMPH_COEFF);
            &legacy_preemph
        };
        let floor = self.floor();

        let num_frames = ((source.len() - FRAME_SIZE) / FRAME_HOP) + 1;
        let mut output = Vec::with_capacity(num_frames * NUM_MEL_BINS);

        // realfft buffers — sized via make_*_vec so process() never
        // returns an error from a length mismatch. Allocated per
        // call so `extract` stays `&self`.
        let mut frame_buf = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();
        let mut raw = [0.0_f32; FRAME_SIZE];
        let row_len = FFT_SIZE / 2 + 1;
        let mut power = [0.0_f32; FFT_SIZE / 2 + 1];

        for frame_idx in 0..num_frames {
            let start = frame_idx * FRAME_HOP;
            raw.copy_from_slice(&source[start..start + FRAME_SIZE]);

            if self.config.kaldi_compat {
                let mut mean = 0.0_f32;
                for v in raw.iter_mut() {
                    *v *= INT16_SCALE;
                    mean += *v;
                }
                mean /= FRAME_SIZE as f32;
                for v in raw.iter_mut() {
                    *v -= mean;
                }
                // Per-frame pre-emphasis, walking backwards so each
                // step reads the not-yet-modified previous sample.
                for i in (1..FRAME_SIZE).rev() {
                    raw[i] -= PREEMPH_COEFF * raw[i - 1];
                }
                raw[0] -= PREEMPH_COEFF * raw[0];
            }

            for (i, slot) in frame_buf.iter_mut().enumerate() {
                *slot = if i < FRAME_SIZE {
                    raw[i] * self.window[i]
                } else {
                    0.0
                };
            }

            self.fft
                .process(&mut frame_buf, &mut spectrum)
                .expect("realfft buffers were sized via make_*_vec; cannot fail");

            for (p, c) in power.iter_mut().zip(spectrum.iter()) {
                *p = c.re * c.re + c.im * c.im;
            }

            for mel_idx in 0..NUM_MEL_BINS {
                let row = &self.filterbank[mel_idx * row_len..(mel_idx + 1) * row_len];
                let energy: f32 = row.iter().zip(power.iter()).map(|(g, p)| g * p).sum();
                output.push(energy.max(floor).ln());
            }
        }

        output
    }
}

/// Apply a first-order pre-emphasis filter to the whole signal (the
/// pre-#1013 behaviour). `y[0] = x[0]`; `y[n] = x[n] - coeff·x[n-1]`.
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

/// Hamming window of length `n`, symmetric (kaldi / torchaudio
/// `hamming_window(periodic=False)`): `0.54 - 0.46·cos(2π·i/(N-1))`.
fn hamming_window(n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }
    let denom = (n - 1) as f32;
    (0..n)
        .map(|i| 0.54 - 0.46 * (2.0 * std::f32::consts::PI * (i as f32) / denom).cos())
        .collect()
}

/// Povey window (`(0.5 - 0.5·cos(2π·n/(N-1)))^0.85`), kaldi's default.
/// Used by the pre-#1013 front end only.
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

/// Convert a frequency in Hz to the HTK mel scale
/// (`mel = 1127·ln(1 + hz/700)`), kaldi's default.
fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Inverse of [`hz_to_mel`].
fn mel_to_hz(mel: f32) -> f32 {
    700.0 * ((mel / 1127.0).exp() - 1.0)
}

/// Kaldi-style mel filterbank, row-major `(num_mels, FFT_SIZE/2 + 1)`.
///
/// Triangle edges are equally spaced in mel and each FFT bin's weight
/// is computed from the bin's own mel value (`min(up_slope,
/// down_slope)`, clamped at 0) — so the triangles are linear in mel,
/// not in Hz. The Nyquist bin gets weight 0, matching kaldi's
/// `num_fft_bins = padded/2` loop plus torchaudio's zero-column pad.
fn kaldi_mel_banks(num_mels: usize, sample_rate: f32, low_hz: f32, high_hz: f32) -> Vec<f32> {
    let num_fft_bins = FFT_SIZE / 2;
    let row_len = num_fft_bins + 1;
    let bin_width = sample_rate / FFT_SIZE as f32;
    let mel_low = hz_to_mel(low_hz);
    let mel_high = hz_to_mel(high_hz);
    let delta = (mel_high - mel_low) / (num_mels as f32 + 1.0);
    let mut matrix = vec![0.0_f32; num_mels * row_len];
    for m in 0..num_mels {
        let left = mel_low + m as f32 * delta;
        let centre = mel_low + (m as f32 + 1.0) * delta;
        let right = mel_low + (m as f32 + 2.0) * delta;
        for k in 0..num_fft_bins {
            let mel = hz_to_mel(bin_width * k as f32);
            let up = (mel - left) / (centre - left);
            let down = (right - mel) / (right - centre);
            matrix[m * row_len + k] = up.min(down).max(0.0);
        }
    }
    matrix
}

/// Pre-#1013 mel filterbank (triangles linear in Hz between
/// mel-spaced edges). Used by the legacy front end only.
fn mel_filterbank_matrix(
    num_mels: usize,
    num_fft_bins: usize,
    sample_rate: f32,
    low_hz: f32,
    high_hz: f32,
) -> Vec<f32> {
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
        for config in [FbankConfig::WESPEAKER, FbankConfig::LEGACY] {
            let ex = MelExtractor::with_config(config);
            let out = ex.extract(&s);
            let expected = ex.log_floor();
            for (i, &v) in out.iter().enumerate() {
                assert!(
                    (v - expected).abs() < 1e-3,
                    "{config:?} frame_bin {i}: expected log-floor ≈ {expected}, got {v}"
                );
            }
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

    #[test]
    fn hamming_window_matches_formula() {
        let w = hamming_window(400);
        assert!((w[0] - 0.08).abs() < 1e-6, "left endpoint {}", w[0]);
        assert!((w[399] - 0.08).abs() < 1e-6, "right endpoint {}", w[399]);
        let w = hamming_window(401);
        assert!((w[200] - 1.0).abs() < 1e-5, "centre {}", w[200]);
        assert!(hamming_window(0).is_empty());
        assert_eq!(hamming_window(1), vec![1.0]);
    }

    #[test]
    fn kaldi_mel_banks_rows_are_non_empty_bounded_and_ordered() {
        let m = kaldi_mel_banks(80, 16000.0, 20.0, 8000.0);
        let row_len = FFT_SIZE / 2 + 1;
        let mut prev_peak = 0_usize;
        for mel_idx in 0..80 {
            let row = &m[mel_idx * row_len..(mel_idx + 1) * row_len];
            let (peak, max) =
                row.iter().enumerate().fold(
                    (0, 0.0_f32),
                    |acc, (i, &g)| if g > acc.1 { (i, g) } else { acc },
                );
            assert!(max > 0.0, "mel {mel_idx} has zero gain everywhere");
            assert!(max <= 1.0 + 1e-6, "mel {mel_idx} exceeds 1.0: {max}");
            assert!(peak >= prev_peak, "mel {mel_idx} peak regressed");
            prev_peak = peak;
            // Kaldi never weights the Nyquist bin.
            assert_eq!(row[row_len - 1], 0.0, "mel {mel_idx} touches Nyquist");
        }
    }

    #[test]
    fn kaldi_front_end_is_invariant_to_dc_offset() {
        // Per-frame DC removal: adding a constant to the waveform must
        // not move the features (beyond float noise).
        let s = sine(700.0, 0.5);
        let shifted: Vec<f32> = s.iter().map(|v| v * 0.5 + 0.2).collect();
        let halved: Vec<f32> = s.iter().map(|v| v * 0.5).collect();
        let ex = MelExtractor::new();
        let a = ex.extract(&halved);
        let b = ex.extract(&shifted);
        assert_eq!(a.len(), b.len());
        let max_diff = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max);
        // Bins far from the tone sit near the floor where relative
        // error is larger; compare only bins with real energy.
        let max_diff_energetic = a
            .iter()
            .zip(&b)
            .filter(|(x, _)| **x > 5.0)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_diff_energetic < 1e-2,
            "DC offset leaked into features: {max_diff_energetic} (all bins {max_diff})"
        );
    }

    #[test]
    fn apply_cmn_zeroes_per_bin_means() {
        // A chirp-ish signal so every bin varies over time.
        let mut s = sine(300.0, 1.0);
        s.extend(sine(2500.0, 1.0));
        let mut feats = mel_filterbank(&s);
        let frames = feats.len() / NUM_MEL_BINS;
        assert!(frames > 100);
        apply_cmn(&mut feats);
        for bin in 0..NUM_MEL_BINS {
            let mean: f64 = (0..frames)
                .map(|f| f64::from(feats[f * NUM_MEL_BINS + bin]))
                .sum::<f64>()
                / frames as f64;
            assert!(mean.abs() < 1e-4, "bin {bin} mean after CMN = {mean}");
        }
    }

    #[test]
    fn apply_cmn_cancels_a_constant_gain() {
        // A gain change is a constant offset in log-mel, which CMN
        // removes: the reason the int16 scale barely matters once CMN
        // is applied.
        let s = sine(1000.0, 1.0);
        let louder: Vec<f32> = s.iter().map(|v| v * 0.25).collect();
        let quieter: Vec<f32> = s.iter().map(|v| v * 0.05).collect();
        let mut a = mel_filterbank(&louder);
        let mut b = mel_filterbank(&quieter);
        apply_cmn(&mut a);
        apply_cmn(&mut b);
        let max_diff = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_diff < 1e-2, "gain survived CMN: {max_diff}");
    }

    #[test]
    fn apply_cmn_on_empty_is_noop() {
        let mut empty: Vec<f32> = Vec::new();
        apply_cmn(&mut empty);
        assert!(empty.is_empty());
    }

    /// Dump production features (pre-CMN) for a WAV so they can be
    /// diffed against `torchaudio.compliance.kaldi.fbank` (#1013).
    /// `HUSH_FBANK_WAV=<16 kHz mono wav> HUSH_FBANK_OUT=<file>` writes
    /// little-endian f32 row-major `(frames, 80)`.
    #[test]
    #[ignore]
    fn dump_fbank_for_reference_comparison() {
        let (Ok(wav), Ok(out)) = (
            std::env::var("HUSH_FBANK_WAV"),
            std::env::var("HUSH_FBANK_OUT"),
        ) else {
            eprintln!("skipping: set HUSH_FBANK_WAV and HUSH_FBANK_OUT");
            return;
        };
        let mut reader = hound::WavReader::open(&wav).expect("open wav");
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| f32::from(s.expect("sample")) / 32768.0)
            .collect();
        let feats = mel_filterbank(&samples);
        let bytes: Vec<u8> = feats.iter().flat_map(|f| f.to_le_bytes()).collect();
        std::fs::write(&out, bytes).expect("write");
        eprintln!("wrote {} frames to {out}", feats.len() / NUM_MEL_BINS);
    }
}
