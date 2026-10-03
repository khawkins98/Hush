//! Learned wall-clock estimate for a dictation transcription.
//!
//! The HUD used to show whisper.cpp's own progress percentage, but
//! whisper.cpp only reports progress once per 30 s seek window, so any
//! dictation shorter than 30 s went straight from 0 % to 100 %. Instead
//! the backend emits one `transcription:estimate` event per dictation
//! carrying an *expected* duration, and the HUD animates locally from it
//! (no per-frame emits — every emit leaks WKWebView memory, #986).
//!
//! The estimate is `fixed_ms + per_audio_s_ms * audio_seconds`, with
//! both coefficients learned on the user's machine: after each
//! successful dictation the (audio seconds, elapsed ms) pair is folded
//! into exponentially-decayed least-squares sums, keyed by model file.
//! Decayed sums rather than a single EMA of "ms per second" because the
//! two coefficients genuinely differ by model and by machine, and
//! dictation on small/small-q8 sizes its encoder window to the clip
//! (#1023, `DICTATION_AUDIO_CTX_MODELS`), which makes short clips much
//! cheaper than a pure rate would predict. A line plus a floor is close
//! enough for a progress bar; anything fancier would be fitting noise.
//!
//! Everything here is pure so it can be unit-tested without a model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Fixed cost assumed before any history exists for a model. Covers
/// the encoder pass and decoder warm-up that every run pays.
pub const DEFAULT_FIXED_MS: f64 = 400.0;
/// Cost per second of audio assumed before any history exists.
/// Deliberately on the slow side: a bar that finishes early (snaps to
/// done) reads better than one that stalls near the end.
pub const DEFAULT_PER_AUDIO_S_MS: f64 = 80.0;
/// No estimate is ever shorter than this. Tiny clips still pay for
/// model dispatch and the clipboard write.
pub const FLOOR_MS: u64 = 250;
/// Weight kept by older observations each time a new one arrives.
/// 0.85 gives an effective memory of roughly six dictations, so a
/// model or thread-count change is absorbed within a few runs.
pub const DECAY: f64 = 0.85;
/// Minimum variance of clip lengths (seconds²) before the slope is
/// fitted. Below it every sample is roughly the same length and a
/// fitted slope would be noise, so only the rate is learned.
pub const MIN_SPREAD_S2: f64 = 4.0;
/// Clamp bounds for the learned coefficients; protect the bar from a
/// single outlier (e.g. a run that queued behind another inference).
pub const MIN_PER_AUDIO_S_MS: f64 = 5.0;
pub const MAX_PER_AUDIO_S_MS: f64 = 3_000.0;
pub const MAX_FIXED_MS: f64 = 10_000.0;
/// Clips shorter than this are skipped when learning: the fixed cost
/// dominates and the rate they imply is meaningless.
pub const MIN_LEARN_AUDIO_S: f64 = 0.5;

/// A learning sample is clamped to within this factor of the current
/// prediction (see [`CostStats::observe`]).
pub const OUTLIER_FACTOR: f64 = 3.0;

/// Exponentially-decayed sufficient statistics for a least-squares
/// line through (audio seconds, elapsed ms). Persisted as-is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostStats {
    pub w: f64,
    pub sx: f64,
    pub sy: f64,
    pub sxx: f64,
    pub sxy: f64,
}

/// The two coefficients of the cost line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    pub fixed_ms: f64,
    pub per_audio_s_ms: f64,
}

impl Coefficients {
    pub const DEFAULT: Self = Self {
        fixed_ms: DEFAULT_FIXED_MS,
        per_audio_s_ms: DEFAULT_PER_AUDIO_S_MS,
    };

    /// Expected wall-clock time for `audio_ms` of audio, floored.
    pub fn expected_ms(&self, audio_ms: u64) -> u64 {
        let secs = audio_ms as f64 / 1000.0;
        let ms = self.fixed_ms + self.per_audio_s_ms * secs;
        if ms.is_finite() {
            (ms.round() as u64).max(FLOOR_MS)
        } else {
            FLOOR_MS
        }
    }
}

impl CostStats {
    /// Fold one finished run into the stats. Unusable samples (too
    /// short, non-finite, zero time) leave the stats unchanged.
    ///
    /// Once there is history, a sample is clamped to within
    /// [`OUTLIER_FACTOR`]× of the current prediction first. Wall time
    /// includes waiting on the shared WhisperContext (#1018), so one
    /// dictation stalled behind a meeting tick could otherwise read as a
    /// 300 s run and skew the estimate for ~25 presses (#1025 red-team).
    #[must_use]
    pub fn observe(self, audio_ms: u64, elapsed_ms: u64) -> Self {
        let x = audio_ms as f64 / 1000.0;
        let mut y = elapsed_ms as f64;
        if x < MIN_LEARN_AUDIO_S || y <= 0.0 || !x.is_finite() || !y.is_finite() {
            return self;
        }
        if self.w > f64::EPSILON {
            let predicted = self.coefficients().expected_ms(audio_ms) as f64;
            y = y.clamp(predicted / OUTLIER_FACTOR, predicted * OUTLIER_FACTOR);
        }
        Self {
            w: self.w * DECAY + 1.0,
            sx: self.sx * DECAY + x,
            sy: self.sy * DECAY + y,
            sxx: self.sxx * DECAY + x * x,
            sxy: self.sxy * DECAY + x * y,
        }
    }

    /// Fit the cost line. Falls back to defaults with no history, and
    /// to a rate-only fit when the clips seen so far are all about the
    /// same length.
    pub fn coefficients(&self) -> Coefficients {
        if self.w <= f64::EPSILON {
            return Coefficients::DEFAULT;
        }
        let mx = self.sx / self.w;
        let my = self.sy / self.w;
        if mx <= 0.0 || !mx.is_finite() || !my.is_finite() {
            return Coefficients::DEFAULT;
        }
        let var = self.sxx / self.w - mx * mx;
        if var < MIN_SPREAD_S2 {
            // Keep the default fixed cost, but never let it eat more
            // than half the observed time on a fast machine.
            let fixed = DEFAULT_FIXED_MS.min(my / 2.0);
            let rate = ((my - fixed) / mx).clamp(MIN_PER_AUDIO_S_MS, MAX_PER_AUDIO_S_MS);
            return Coefficients {
                fixed_ms: fixed,
                per_audio_s_ms: rate,
            };
        }
        let cov = self.sxy / self.w - mx * my;
        let rate = (cov / var).clamp(MIN_PER_AUDIO_S_MS, MAX_PER_AUDIO_S_MS);
        let fixed = (my - rate * mx).clamp(0.0, MAX_FIXED_MS);
        Coefficients {
            fixed_ms: fixed,
            per_audio_s_ms: rate,
        }
    }
}

/// Wire payload of the one-per-dictation `transcription:estimate`
/// event. Mirrors `TranscriptionEstimatePayload` in `src/lib/types.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimatePayload {
    /// Length of the clip actually handed to whisper (after VAD trim
    /// and the trailing-silence pad).
    pub audio_ms: u64,
    /// Expected wall-clock transcription time.
    pub expected_ms: u64,
}

/// Learned stats for every model seen, keyed by model file name.
pub type CostModels = BTreeMap<String, CostStats>;

/// Parse the persisted settings value. A missing or unreadable value
/// is an empty map: the estimate falls back to defaults and relearns.
pub fn parse_cost_models(raw: Option<&str>) -> CostModels {
    raw.and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// Expected duration for `audio_ms` on `model`, from the learned map.
pub fn expected_ms_for(models: &CostModels, model: &str, audio_ms: u64) -> u64 {
    models
        .get(model)
        .map_or(Coefficients::DEFAULT, CostStats::coefficients)
        .expected_ms(audio_ms)
}

/// Record one finished run for `model` and return the updated map.
#[must_use]
pub fn record_run(
    mut models: CostModels,
    model: &str,
    audio_ms: u64,
    elapsed_ms: u64,
) -> CostModels {
    let stats = models.get(model).copied().unwrap_or_default();
    models.insert(model.to_owned(), stats.observe(audio_ms, elapsed_ms));
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learn(samples: &[(u64, u64)]) -> CostStats {
        samples
            .iter()
            .fold(CostStats::default(), |s, &(a, e)| s.observe(a, e))
    }

    #[test]
    fn no_history_uses_defaults() {
        let c = CostStats::default().coefficients();
        assert_eq!(c, Coefficients::DEFAULT);
        // 10 s at the defaults: 400 + 80 * 10.
        assert_eq!(c.expected_ms(10_000), 1_200);
    }

    #[test]
    fn floor_applies_to_tiny_clips() {
        let c = Coefficients {
            fixed_ms: 10.0,
            per_audio_s_ms: 10.0,
        };
        assert_eq!(c.expected_ms(1_000), FLOOR_MS);
    }

    #[test]
    fn single_sample_learns_a_rate_keeping_default_fixed() {
        // 10 s took 2.4 s → (2400 - 400) / 10 = 200 ms per second.
        let c = learn(&[(10_000, 2_400)]).coefficients();
        assert_eq!(c.fixed_ms, DEFAULT_FIXED_MS);
        assert!((c.per_audio_s_ms - 200.0).abs() < 1e-9);
        assert_eq!(c.expected_ms(10_000), 2_400);
    }

    #[test]
    fn fast_machine_does_not_get_a_fixed_cost_larger_than_observed() {
        // 5 s in 300 ms: the default 400 ms fixed would imply a negative
        // rate; fixed is capped at half the observed time instead.
        let c = learn(&[(5_000, 300)]).coefficients();
        assert!((c.fixed_ms - 150.0).abs() < 1e-9);
        assert!((c.per_audio_s_ms - 30.0).abs() < 1e-9);
    }

    #[test]
    fn spread_samples_recover_the_true_line() {
        // Exact line: 300 ms + 50 ms/s.
        let samples: Vec<(u64, u64)> = [2u64, 5, 10, 20, 40, 3, 15]
            .iter()
            .map(|&s| (s * 1_000, 300 + 50 * s))
            .collect();
        let c = learn(&samples).coefficients();
        assert!((c.per_audio_s_ms - 50.0).abs() < 1e-6, "{c:?}");
        assert!((c.fixed_ms - 300.0).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn recent_runs_outweigh_old_ones() {
        // Ten slow runs, then twenty fast ones (e.g. more inference
        // threads). The estimate follows the recent behaviour instead of
        // averaging the two (a plain mean would say 2.3 s).
        let mut samples = vec![(10_000, 5_000); 10];
        samples.extend(vec![(10_000, 1_000); 20]);
        let est = learn(&samples).coefficients().expected_ms(10_000);
        assert!((1_000..1_300).contains(&est), "est = {est}");
    }

    #[test]
    fn outlier_rate_is_clamped() {
        let c = learn(&[(1_000, 60_000)]).coefficients();
        assert_eq!(c.per_audio_s_ms, MAX_PER_AUDIO_S_MS);
    }

    #[test]
    fn negative_slope_is_clamped_to_minimum() {
        // Longer clips came back faster (noise) — never predict a
        // negative rate.
        // Sums built directly: `observe` would clamp the second sample
        // as an outlier, and this test is about the fit's own guard.
        let (x1, y1, x2, y2) = (2.0, 3_000.0, 30.0, 1_000.0);
        let c = CostStats {
            w: 2.0,
            sx: x1 + x2,
            sy: y1 + y2,
            sxx: x1 * x1 + x2 * x2,
            sxy: x1 * y1 + x2 * y2,
        }
        .coefficients();
        assert_eq!(c.per_audio_s_ms, MIN_PER_AUDIO_S_MS);
        assert!(c.fixed_ms >= 0.0);
    }

    #[test]
    fn unusable_samples_are_ignored() {
        let s = CostStats::default().observe(200, 500).observe(5_000, 0);
        assert_eq!(s, CostStats::default());
    }

    #[test]
    fn models_are_keyed_independently_and_round_trip() {
        let models = record_run(CostModels::new(), "ggml-small.bin", 10_000, 2_400);
        let models = record_run(models, "ggml-base.bin", 10_000, 900);
        let raw = serde_json::to_string(&models).unwrap();
        let back = parse_cost_models(Some(&raw));
        assert_eq!(back, models);
        assert_eq!(expected_ms_for(&back, "ggml-small.bin", 10_000), 2_400);
        assert!(expected_ms_for(&back, "ggml-base.bin", 10_000) < 1_000);
        // Unknown model → defaults.
        assert_eq!(expected_ms_for(&back, "ggml-large.bin", 10_000), 1_200);
    }

    #[test]
    fn garbage_settings_value_parses_as_empty() {
        assert!(parse_cost_models(Some("not json")).is_empty());
        assert!(parse_cost_models(None).is_empty());
    }

    #[test]
    fn one_stalled_run_does_not_poison_the_estimate() {
        let mut stats = CostStats::default();
        for _ in 0..12 {
            stats = stats.observe(10_000, 800);
        }
        let before = stats.coefficients().expected_ms(10_000);
        // A dictation that waited 300 s behind a meeting tick.
        stats = stats.observe(10_000, 300_000);
        let after = stats.coefficients().expected_ms(10_000);
        assert!(after <= before * 2, "before {before} after {after}");
        // And it recovers within a few normal runs.
        for _ in 0..3 {
            stats = stats.observe(10_000, 800);
        }
        let recovered = stats.coefficients().expected_ms(10_000);
        assert!(
            recovered <= before + before / 2,
            "before {before} recovered {recovered}"
        );
    }
}
