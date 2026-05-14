import { describe, it, expect } from "vitest";
import { formatMb, formatTimestamp, formatDuration } from "$lib/format";

describe("formatMb", () => {
  it("formats bytes to megabytes with one decimal", () => {
    expect(formatMb(1024 * 1024)).toBe("1.0 MB");
    expect(formatMb(1.5 * 1024 * 1024)).toBe("1.5 MB");
  });

  it("rounds to one decimal place", () => {
    expect(formatMb(10 * 1024 * 1024)).toBe("10.0 MB");
    expect(formatMb(0)).toBe("0.0 MB");
  });
});

describe("formatTimestamp", () => {
  it("returns the original string for an invalid date", () => {
    expect(formatTimestamp("not-a-date")).toBe("not-a-date");
    expect(formatTimestamp("")).toBe("");
  });

  it("returns a non-empty locale string for a valid ISO date", () => {
    const result = formatTimestamp("2024-01-15T14:30:00Z");
    expect(result).toBeTruthy();
    // Should NOT contain seconds (the function intentionally omits them)
    expect(result).not.toMatch(/:\d{2}:\d{2}/);
  });
});

describe("formatDuration", () => {
  it("returns null for null input", () => {
    expect(formatDuration(null)).toBeNull();
  });

  it("returns null for negative input", () => {
    expect(formatDuration(-1)).toBeNull();
  });

  it("formats sub-second clips with one decimal", () => {
    expect(formatDuration(400)).toBe("0.4s");
    expect(formatDuration(999)).toBe("1.0s");
  });

  it("formats whole seconds under a minute", () => {
    expect(formatDuration(1000)).toBe("1s");
    expect(formatDuration(59000)).toBe("59s");
  });

  it("formats minutes with zero-padded seconds", () => {
    expect(formatDuration(60000)).toBe("1:00");
    expect(formatDuration(90000)).toBe("1:30");
    expect(formatDuration(125000)).toBe("2:05");
  });
});
