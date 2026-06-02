// Unit tests for the recording-timer helpers (#990).
//
// The bug these pin: navigating Settings/History → Transcribe during an
// active recording remounted RecordPanel, which re-stamped its local
// start time to Date.now() and reset the visible timer to 00:00. The
// fix derives the start from remount-safe sources; these tests encode
// the resolution priority and the elapsed formatting (including the
// over-one-hour case from #989's screenshot).

import { describe, it, expect } from "vitest";
import { resolveRecordingStartMs, formatElapsed } from "./recording-time";

describe("resolveRecordingStartMs", () => {
  it("prefers the dictation phase start when present", () => {
    expect(resolveRecordingStartMs(1_000_000, "2026-06-02T07:00:00Z")).toBe(1_000_000);
  });

  it("falls back to the meeting session's ISO startedAt", () => {
    const iso = "2026-06-02T07:00:00Z";
    expect(resolveRecordingStartMs(null, iso)).toBe(Date.parse(iso));
  });

  it("returns null when neither source is available", () => {
    expect(resolveRecordingStartMs(null, null)).toBeNull();
    expect(resolveRecordingStartMs(null, undefined)).toBeNull();
  });

  it("returns null for an unparseable meeting timestamp rather than NaN", () => {
    expect(resolveRecordingStartMs(null, "not-a-date")).toBeNull();
  });

  it("treats 0 as a valid dictation start (epoch edge case)", () => {
    // 0 is falsy but is a legitimate ms timestamp; the guard must be a
    // null check, not a truthiness check.
    expect(resolveRecordingStartMs(0, null)).toBe(0);
  });
});

describe("formatElapsed", () => {
  // House format (#481, unified across HUD + Transcribe panel in the
  // multi-agent review follow-up): unpadded minutes under an hour.
  it("formats sub-minute durations as M:SS", () => {
    expect(formatElapsed(5_000)).toBe("0:05");
  });

  it("formats sub-hour durations as M:SS", () => {
    expect(formatElapsed(45 * 60_000 + 7_000)).toBe("45:07");
    expect(formatElapsed(5 * 60_000 + 7_000)).toBe("5:07");
  });

  it("switches to H:MM:SS past one hour", () => {
    // 1h 28m 16s — the duration from the #989 report screenshot.
    expect(formatElapsed(((1 * 60 + 28) * 60 + 16) * 1000)).toBe("1:28:16");
    // Minutes ARE padded once hours are present.
    expect(formatElapsed((60 * 60 + 5 * 60 + 7) * 1000)).toBe("1:05:07");
  });

  it("clamps negative durations to zero (clock skew)", () => {
    expect(formatElapsed(-3_000)).toBe("0:00");
  });

  it("floors partial seconds", () => {
    expect(formatElapsed(1_999)).toBe("0:01");
  });
});
