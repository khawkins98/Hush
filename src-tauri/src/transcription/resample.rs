//! Pure-logic linear-interpolation resampler.
//!
//! Whisper.cpp expects 16 kHz mono f32 PCM, but consumer microphones almost
//! always capture at 44.1 or 48 kHz. This module bridges the gap.
//!
//! ## Why linear interpolation (not `rubato` or windowed-sinc)
//!
//! For M1 we picked the simplest correct algorithm — linear interpolation
//! between adjacent input samples — over a windowed-sinc resampler such as
//! the one in the `rubato` crate. The reasoning, in order:
//!
//! 1. **Whisper is robust.** The model's first stage is a mel spectrogram
//!    with 25 ms windows and 10 ms hops. Linear-interpolation aliasing
//!    artifacts above ~4 kHz are smoothed away by the mel filterbank long
//!    before they reach the encoder. Empirically, dictation accuracy with
//!    a linear resampler is within noise of a sinc resampler for the 44.1
//!    or 48 kHz → 16 kHz downsample case that 99% of users will hit.
//!
//! 2. **Zero new dependencies on the default-feature build.** The audio →
//!    transcription pipeline becomes a single Cargo feature flag (`whisper`)
//!    rather than `whisper` plus a transitive resampler. CI is faster,
//!    audit surface is smaller, the build works for contributors who do
//!    not have cmake locally.
//!
//! 3. **Easy to swap.** The public API is `resample_to_mono(samples,
//!    in_rate, out_rate) -> Vec<f32>`. If a future quality regression test
//!    shows linear interpolation is the bottleneck, we can replace the body
//!    with a `rubato::FftFixedIn` call without touching any caller.
//!
//! See `learnings.md` (2026-04-25) for the longer write-up.
//!
//! ## What this module does *not* do
//!
//! - Pre-filter for downsampling. With a 48 kHz → 16 kHz target the Nyquist
//!   shifts from 24 kHz to 8 kHz; energy in the 8–24 kHz band aliases
//!   downward. For dictation this is benign because human speech has
//!   essentially no useful information above 8 kHz. If we ever target
//!   non-speech audio this assumption breaks and we need a proper anti-alias
//!   filter, which is reason enough on its own to swap in `rubato`.
//! - Anything other than mono. Channel downmix is a separate concern in
//!   [`crate::audio::downmix_to_mono`] and runs before us.

/// Resample mono `samples` from `in_rate` to `out_rate`.
///
/// `samples` must be mono (single-channel). Pass channel-interleaved buffers
/// through [`crate::audio::downmix_to_mono`] first.
///
/// Returns a freshly allocated buffer of length
/// `ceil(samples.len() * out_rate / in_rate)` (give or take one sample at
/// the boundary). When `in_rate == out_rate` the input is returned as-is.
///
/// # Panics
///
/// Panics if `in_rate` or `out_rate` is zero. Both are caught at the
/// pipeline boundary because they would indicate a corrupted
/// [`crate::audio::CaptureFormat`] from the audio module.
pub fn resample_to_mono(samples: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    assert!(in_rate > 0, "in_rate must be > 0");
    assert!(out_rate > 0, "out_rate must be > 0");

    // Identity fast-path. The common "device already at 16 kHz" case (rare on
    // consumer hardware but real on certain conference-mic firmware) skips
    // allocation arithmetic entirely.
    if in_rate == out_rate {
        return samples.to_vec();
    }
    if samples.is_empty() {
        return Vec::new();
    }
    if samples.len() == 1 {
        // A single-sample input has no neighbour to interpolate against;
        // returning a single-sample output is the only sensible behaviour
        // and avoids a divide-by-zero in the index arithmetic below.
        return vec![samples[0]];
    }

    // Output length: round up so we don't silently drop a fractional sample
    // at the tail of a short buffer. Using f64 here keeps the arithmetic
    // exact for any realistic capture length (>10 hours at 192 kHz).
    let in_len = samples.len();
    let ratio = out_rate as f64 / in_rate as f64;
    let out_len = ((in_len as f64) * ratio).ceil() as usize;

    let mut out = Vec::with_capacity(out_len);
    let step = in_rate as f64 / out_rate as f64;

    // We walk the output index space and project each output position back
    // onto a fractional input index, then linearly interpolate between the
    // two nearest input samples. This is the standard "polyphase-style"
    // formulation written long-form for clarity.
    for i in 0..out_len {
        let src_pos = i as f64 * step;
        let src_idx = src_pos as usize;

        // Clamp at the right edge: when out_len was rounded up, the final
        // src_pos may sit just past the last valid pair index. Snapping to
        // the last sample is correct (a constant tail) and keeps us out of
        // bounds.
        if src_idx >= in_len - 1 {
            out.push(samples[in_len - 1]);
            continue;
        }

        let frac = (src_pos - src_idx as f64) as f32;
        let a = samples[src_idx];
        let b = samples[src_idx + 1];
        out.push(a + (b - a) * frac);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_rates_match() {
        let input = vec![0.1, -0.2, 0.3, -0.4, 0.5];
        assert_eq!(resample_to_mono(&input, 16_000, 16_000), input);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(resample_to_mono(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn single_sample_input_passes_through() {
        // A single sample has no neighbour to interpolate against; returning
        // it unchanged is the only reasonable behaviour. This is the
        // smallest possible "undefined" input and guarding it keeps the main
        // loop's index arithmetic safe.
        assert_eq!(resample_to_mono(&[0.42], 48_000, 16_000), vec![0.42]);
    }

    #[test]
    fn upsample_2x_inserts_midpoints() {
        // 4 samples at 1 Hz → 8 samples at 2 Hz. Each new sample sits at the
        // midpoint between two inputs (with a clamp at the tail).
        let input = vec![0.0, 1.0, 0.0, 1.0];
        let out = resample_to_mono(&input, 1, 2);
        assert_eq!(out.len(), 8);
        // Midpoint interpolation: 0.0, 0.5, 1.0, 0.5, 0.0, 0.5, 1.0, 1.0
        let expected = [0.0, 0.5, 1.0, 0.5, 0.0, 0.5, 1.0, 1.0];
        for (got, want) in out.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    #[test]
    fn downsample_2x_picks_alternate_samples() {
        // 4 samples at 2 Hz → 2 samples at 1 Hz. With our integer-step path
        // the output should land exactly on input indices 0 and 2.
        let input = vec![0.1, 0.2, 0.3, 0.4];
        let out = resample_to_mono(&input, 2, 1);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.1).abs() < 1e-6);
        assert!((out[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn downsample_48k_to_16k_length_correct() {
        // Common production case: 48 kHz capture → 16 kHz whisper input.
        // 1 second of input (48000 samples) should produce ~16000 samples.
        let input = vec![0.0_f32; 48_000];
        let out = resample_to_mono(&input, 48_000, 16_000);
        // Allow one-sample slack from the ceil() rounding at the tail.
        assert!(
            (16_000..=16_001).contains(&out.len()),
            "unexpected length {}",
            out.len()
        );
    }

    #[test]
    fn downsample_44k_to_16k_length_correct() {
        // The other common production case: 44.1 kHz capture. The ratio is
        // not an integer so we exercise the fractional-index path properly.
        let input = vec![0.0_f32; 44_100];
        let out = resample_to_mono(&input, 44_100, 16_000);
        // 44100 * 16000 / 44100 = 16000 exactly; ceil() may add 1.
        assert!(
            (16_000..=16_001).contains(&out.len()),
            "unexpected length {}",
            out.len()
        );
    }

    #[test]
    fn linear_interpolation_is_exact_on_a_ramp() {
        // A perfectly linear input should resample to a perfectly linear
        // output; this is the strongest invariant a linear interpolator
        // must hold. Pinning it down catches any off-by-one in src_idx.
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_to_mono(&input, 100, 50);
        // 50 output samples at step=2 should pick 0, 2, 4, ... 98.
        assert_eq!(out.len(), 50);
        for (i, &v) in out.iter().enumerate() {
            let expected = (i as f32) * 2.0;
            assert!(
                (v - expected).abs() < 1e-4,
                "out[{i}] = {v}, want {expected}"
            );
        }
    }

    #[test]
    fn sine_wave_preserves_amplitude_envelope() {
        // Generate a 1 kHz sine at 48 kHz, downsample to 16 kHz, and check
        // the peak amplitude is preserved within reasonable bounds. This is
        // a sanity check on the whole pipeline rather than a strict spectral
        // test — linear interpolation is not expected to be transparent, but
        // it must not blow up or attenuate the signal.
        let in_rate = 48_000_u32;
        let out_rate = 16_000_u32;
        let freq = 1000.0_f32;
        let input: Vec<f32> = (0..in_rate)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / in_rate as f32).sin())
            .collect();

        let out = resample_to_mono(&input, in_rate, out_rate);

        let peak = out.iter().fold(0.0_f32, |m, &v| m.max(v.abs()));
        // For a 1 kHz sine well below the new Nyquist (8 kHz), peak should
        // remain near 1.0. Linear interpolation introduces a tiny ripple,
        // but a 5% tolerance is plenty.
        assert!(
            peak > 0.95 && peak <= 1.0,
            "expected peak near 1.0, got {peak}"
        );
    }

    #[test]
    #[should_panic(expected = "in_rate must be > 0")]
    fn zero_in_rate_panics() {
        // A zero rate could only come from a corrupted CaptureFormat, which
        // is itself a bug. We panic loudly rather than emit a divide-by-zero
        // or a silent empty buffer.
        resample_to_mono(&[0.0], 0, 16_000);
    }

    #[test]
    #[should_panic(expected = "out_rate must be > 0")]
    fn zero_out_rate_panics() {
        resample_to_mono(&[0.0], 48_000, 0);
    }
}
