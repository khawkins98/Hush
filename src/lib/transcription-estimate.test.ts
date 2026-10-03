import { describe, expect, it } from "vitest";
import {
  ESTIMATE_TARGET,
  OVERRUN_CEILING,
  REAL_PROGRESS_CEILING,
  SHORT_RUN_MS,
  displayedFraction,
  estimatedFraction,
  formatAudioLength,
  isShortRun,
  transcribingLabel,
} from "./transcription-estimate";

describe("estimatedFraction", () => {
  it("starts at zero", () => {
    expect(estimatedFraction(0, 4000)).toBe(0);
  });

  it("reaches the target exactly at the expected time", () => {
    expect(estimatedFraction(4000, 4000)).toBeCloseTo(ESTIMATE_TARGET, 10);
  });

  it("eases out: the first half covers more than half the target", () => {
    expect(estimatedFraction(2000, 4000)).toBeGreaterThan(ESTIMATE_TARGET / 2);
  });

  it("is monotonic and never reaches 1, even far past the estimate", () => {
    let prev = -1;
    for (let t = 0; t <= 200_000; t += 250) {
      const f = estimatedFraction(t, 4000);
      expect(f).toBeGreaterThanOrEqual(prev);
      expect(f).toBeLessThan(1);
      prev = f;
    }
    expect(prev).toBeLessThanOrEqual(OVERRUN_CEILING);
  });

  it("keeps creeping after an overrun instead of freezing", () => {
    const atTwice = estimatedFraction(8000, 4000);
    const atThrice = estimatedFraction(12000, 4000);
    expect(atTwice).toBeGreaterThan(ESTIMATE_TARGET);
    expect(atThrice).toBeGreaterThan(atTwice);
  });

  it("is continuous at the expected time", () => {
    const before = estimatedFraction(3999, 4000);
    const after = estimatedFraction(4001, 4000);
    expect(after - before).toBeLessThan(0.001);
  });

  it("treats a degenerate expectation as no progress", () => {
    expect(estimatedFraction(1000, 0)).toBe(0);
    expect(estimatedFraction(1000, Number.NaN)).toBe(0);
  });
});

describe("displayedFraction", () => {
  it("uses whisper's progress as a floor", () => {
    expect(displayedFraction(100, 60_000, 66)).toBeCloseTo(0.66, 10);
  });

  it("ignores a real value below the estimate", () => {
    expect(displayedFraction(4000, 4000, 10)).toBeCloseTo(ESTIMATE_TARGET, 10);
  });

  it("never lets whisper's 100 fill the bar before the text is ready", () => {
    expect(displayedFraction(10, 4000, 100)).toBe(REAL_PROGRESS_CEILING);
  });

  it("handles no real progress yet", () => {
    expect(displayedFraction(0, 4000, null)).toBe(0);
  });
});

describe("short runs and copy", () => {
  it("treats sub-threshold and missing estimates as short", () => {
    expect(isShortRun(null)).toBe(true);
    expect(isShortRun({ audioMs: 3000, expectedMs: SHORT_RUN_MS - 1 })).toBe(true);
    expect(isShortRun({ audioMs: 3000, expectedMs: SHORT_RUN_MS })).toBe(false);
  });

  it("formats audio length briefly", () => {
    expect(formatAudioLength(400)).toBe("1 s");
    expect(formatAudioLength(42_300)).toBe("42 s");
    expect(formatAudioLength(59_400)).toBe("59 s");
    expect(formatAudioLength(125_000)).toBe("2 min");
  });

  it("labels long runs with the audio length, short runs plainly", () => {
    expect(transcribingLabel({ audioMs: 42_000, expectedMs: 5000 })).toBe(
      "Transcribing 42 s of audio…",
    );
    expect(transcribingLabel({ audioMs: 4000, expectedMs: 800 })).toBe("Transcribing…");
    expect(transcribingLabel(null)).toBe("Transcribing…");
  });
});
