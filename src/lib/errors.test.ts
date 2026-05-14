import { describe, it, expect } from "vitest";
import {
  formatErrorDisplay,
  formatErrorMessage,
  isPermissionShapedError,
} from "$lib/errors";

// ── isPermissionShapedError ──────────────────────────────────────────────────

describe("isPermissionShapedError", () => {
  it("returns false for non-IPC throwables", () => {
    expect(isPermissionShapedError(new Error("oops"))).toBe(false);
    expect(isPermissionShapedError("string error")).toBe(false);
    expect(isPermissionShapedError(null)).toBe(false);
  });

  it("returns true for the typed permission-denied kind", () => {
    expect(
      isPermissionShapedError({ kind: "permission-denied", message: "screen-recording" }),
    ).toBe(true);
  });

  it("returns true for screen-recording substring in message", () => {
    expect(
      isPermissionShapedError({ kind: "audio", message: "Screen Recording permission required" }),
    ).toBe(true);
  });

  it("returns true for declined TCCs substring", () => {
    expect(
      isPermissionShapedError({ kind: "audio", message: "user declined TCCs for application" }),
    ).toBe(true);
  });

  it("returns true for microphone not authorized substring", () => {
    expect(
      isPermissionShapedError({ kind: "audio", message: "microphone access not authorized" }),
    ).toBe(true);
  });

  it("returns true for input monitoring substring", () => {
    expect(
      isPermissionShapedError({ kind: "audio", message: "Input Monitoring denied" }),
    ).toBe(true);
  });

  it("returns false for an IPC error with no permission-related message", () => {
    expect(
      isPermissionShapedError({ kind: "audio", message: "device not found" }),
    ).toBe(false);
  });
});

// ── formatErrorDisplay ───────────────────────────────────────────────────────

describe("formatErrorDisplay", () => {
  it("handles non-IPC Error instances", () => {
    const result = formatErrorDisplay(new Error("boom"));
    expect(result.headline).toBe("Something went wrong");
    expect(result.details).toBe("boom");
  });

  it("handles non-IPC plain strings", () => {
    const result = formatErrorDisplay("plain string error");
    expect(result.headline).toBe("Something went wrong");
    expect(result.details).toBe("plain string error");
  });

  it("handles transcription-unavailable", () => {
    const result = formatErrorDisplay({ kind: "transcription-unavailable" });
    expect(result.headline).toBe("No transcription model loaded");
    expect(result.actionKey).toBe("open-model-settings");
    expect(result.hint).toBeTruthy();
  });

  it("handles audio-device-lost with device name in hint", () => {
    const result = formatErrorDisplay({
      kind: "audio-device-lost",
      message: "My USB Mic",
    });
    expect(result.headline).toBe("Microphone disconnected");
    expect(result.hint).toContain("My USB Mic");
  });

  it("handles permission-denied kind directly (typed path)", () => {
    const result = formatErrorDisplay({
      kind: "permission-denied",
      message: "screen-recording",
    });
    expect(result.headline).toBe("System Audio permission needed");
    expect(result.hint).toContain("Screen Recording");
    expect(result.details).toBeUndefined();
  });

  it("handles screen-recording substring fallback with details", () => {
    const msg = "Screen Recording permission required: NSEvent tap failed";
    const result = formatErrorDisplay({ kind: "audio", message: msg });
    expect(result.headline).toBe("System Audio permission needed");
    expect(result.details).toBe(msg);
  });

  it("handles microphone not-authorized substring fallback", () => {
    const msg = "microphone access not authorized by user";
    const result = formatErrorDisplay({ kind: "audio", message: msg });
    expect(result.headline).toBe("Microphone permission needed");
    expect(result.details).toBe(msg);
  });

  it("handles unknown IPC kind gracefully", () => {
    const result = formatErrorDisplay({ kind: "future-variant", message: "detail" });
    expect(result.headline).toBe("future-variant");
    expect(result.details).toBe("detail");
  });

  it("returns generic headline for unknown kind with no message", () => {
    const result = formatErrorDisplay({ kind: "" });
    expect(result.headline).toBe("Something went wrong");
  });
});

// ── formatErrorMessage ───────────────────────────────────────────────────────

describe("formatErrorMessage", () => {
  it("returns just the headline when there are no details", () => {
    const result = formatErrorMessage({ kind: "transcription-unavailable" });
    expect(result).toBe("No transcription model loaded");
  });

  it("returns headline + colon + details when details are present", () => {
    const result = formatErrorMessage({ kind: "history", message: "db locked" });
    expect(result).toBe("History update failed: db locked");
  });
});
