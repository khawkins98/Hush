// Estimated dictation-transcription progress.
//
// whisper.cpp reports progress once per 30 s window, so a typical
// dictation jumped 0 % → 100 % with nothing in between. The backend now
// emits a single `transcription:estimate` event (`{ audioMs, expectedMs }`,
// learned from past runs on this machine) and the HUD / Transcribe panel
// animate locally from it. These helpers are pure so the curve and copy
// are unit-tested without a browser.

import type { TranscriptionEstimatePayload } from "./types";

/// Runs expected to finish faster than this get an indeterminate
/// shimmer and no bar: a bar that appears and fills in under a second
/// and a half is just flicker.
export const SHORT_RUN_MS = 1500;

/// Where the bar sits when the expected time is reached. The rest of
/// the track is reserved for overruns and for the real "done" snap.
export const ESTIMATE_TARGET = 0.9;

/// Asymptote of the overrun creep. Never reaches 1: only the real
/// completion signal fills the bar.
export const OVERRUN_CEILING = 0.99;

/// Cap on the whisper-reported floor, for the same reason.
export const REAL_PROGRESS_CEILING = 0.95;

/// Tick period for the components' animation interval. 10 Hz with a
/// matching CSS width transition looks continuous and costs a fraction
/// of a 60 Hz rAF loop.
export const ESTIMATE_TICK_MS = 100;

// Shape of the ease-out before the expected time: larger = faster
// start, flatter finish.
const EASE_K = 3;
const EASE_NORM = 1 - Math.exp(-EASE_K);

/// Bar fraction [0, OVERRUN_CEILING) for `elapsedMs` into a run expected
/// to take `expectedMs`. Eases out to ESTIMATE_TARGET at the expected
/// time, then keeps creeping toward OVERRUN_CEILING so an overrunning
/// run never looks frozen.
export function estimatedFraction(elapsedMs: number, expectedMs: number): number {
  if (!(expectedMs > 0) || !(elapsedMs > 0)) return 0;
  if (elapsedMs <= expectedMs) {
    const t = elapsedMs / expectedMs;
    return (ESTIMATE_TARGET * (1 - Math.exp(-EASE_K * t))) / EASE_NORM;
  }
  const over = (elapsedMs - expectedMs) / expectedMs;
  return ESTIMATE_TARGET + (OVERRUN_CEILING - ESTIMATE_TARGET) * (1 - Math.exp(-over));
}

/// Bar fraction to render: the estimate, floored by whisper's own
/// progress (0–100, or null before any tick). Long clips span several
/// 30 s windows, so the real value can run ahead of a pessimistic
/// estimate.
export function displayedFraction(
  elapsedMs: number,
  expectedMs: number,
  realPercent: number | null,
): number {
  const est = estimatedFraction(elapsedMs, expectedMs);
  const real =
    realPercent === null || !Number.isFinite(realPercent)
      ? 0
      : Math.min(REAL_PROGRESS_CEILING, Math.max(0, realPercent / 100));
  return Math.max(est, real);
}

/// Whether the run is short enough to show only a shimmer.
export function isShortRun(est: TranscriptionEstimatePayload | null): boolean {
  return est === null || est.expectedMs < SHORT_RUN_MS;
}

/// "42 s" under a minute, "2 min" above it — short enough for the HUD
/// pill. Minutes round to the nearest whole minute.
export function formatAudioLength(ms: number): string {
  const secs = Math.max(1, Math.round(ms / 1000));
  if (secs < 60) return `${secs} s`;
  return `${Math.round(secs / 60)} min`;
}

/// Label shown while transcribing. Short runs (and runs with no
/// estimate yet) just say "Transcribing…".
export function transcribingLabel(est: TranscriptionEstimatePayload | null): string {
  if (isShortRun(est) || est === null) return "Transcribing…";
  return `Transcribing ${formatAudioLength(est.audioMs)} of audio…`;
}
