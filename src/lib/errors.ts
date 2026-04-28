// Unified error-display shape (#199).
//
// Pre-#199 each error in the UI was rendered as a single string —
// fine for "no model loaded", confusing for the deep error chains
// the meeting-mode and audio paths can produce. Example from the
// user's hands-on testing of #197:
//
//   meeting-sessions: meeting-sessions: start_manual: open audio
//   session for system-audio source: ScreenCaptureKit: query
//   shareable content: No shareable content available: Content
//   unavailable: The user declined TCCs for application, window,
//   display capture — grant Screen Recording permission in System
//   Settings → Privacy & Security to capture system audio
//
// That's a wall of internal context that buries the actionable
// recovery hint at the very end. The new shape splits each error
// into a friendly **headline** (what happened, in plain language)
// + an optional **hint** (what to do about it) + the raw
// **details** for users who want to dig deeper. The `ErrorDisplay`
// Svelte component renders the details collapsed by default.

import type { IpcError } from "./types";

export type ErrorDisplay = {
  /// Plain-language summary, ~5 words. Always present.
  headline: string;
  /// Action-oriented recovery hint. Optional — some errors don't
  /// have a clear next step.
  hint?: string;
  /// Raw technical message — surfaced in a collapsed `<details>`
  /// so power users can debug, but not in the user's face.
  details?: string;
};

/// Map an unknown thrown value into the rich display shape. The
/// frontend's catch blocks pass `unknown`; the function inspects
/// the IpcError tag + message to pick the friendliest copy.
export function formatErrorDisplay(e: unknown): ErrorDisplay {
  // Plain string / Error fallback — most often a frontend-side
  // failure (clipboard, navigation) that didn't go through Tauri.
  if (typeof e !== "object" || e === null || !("kind" in e)) {
    return {
      headline: "Something went wrong",
      details: e instanceof Error ? e.message : String(e),
    };
  }

  const ipc = e as IpcError;
  const message = ipc.message ?? "";

  // Permission-shaped errors deserve a tailored hint regardless of
  // which IPC kind they came back as. Detect on the message text
  // (the backend chains context strings rather than emitting a
  // dedicated variant).
  const lower = message.toLowerCase();
  if (lower.includes("screen recording") || lower.includes("declined tccs")) {
    return {
      headline: "Screen Recording permission needed",
      hint:
        "Grant Hush Screen Recording access in System Settings → " +
        "Privacy & Security → Screen Recording, then try again. " +
        "Until then, microphone-only recording still works.",
      details: message,
    };
  }
  if (lower.includes("microphone") && lower.includes("not authorized")) {
    return {
      headline: "Microphone permission needed",
      hint:
        "Grant Hush microphone access in System Settings → Privacy " +
        "& Security → Microphone, then try again.",
      details: message,
    };
  }
  if (lower.includes("input monitoring")) {
    return {
      headline: "Input Monitoring permission needed",
      hint:
        "Grant Hush Input Monitoring access in System Settings → " +
        "Privacy & Security → Input Monitoring. PTT keystrokes " +
        "won't reach the listener until then.",
      details: message,
    };
  }

  // Per-kind defaults. Each branch picks a friendly headline and a
  // recovery hint where one is obvious; falls through to surfacing
  // the raw message if neither is.
  switch (ipc.kind) {
    case "transcription-unavailable":
      return {
        headline: "No transcription model loaded",
        hint:
          "Open Settings → Model and pick one. Hush will fetch and " +
          "verify it, then load it without a restart.",
      };
    case "audio":
      return {
        headline: "Microphone access failed",
        hint:
          "Check your selected input source and that the mic is " +
          "plugged in. On macOS, also check System Settings → " +
          "Privacy & Security → Microphone for Hush.",
        details: message,
      };
    case "transcription":
      return {
        headline: "Transcription failed",
        hint:
          "The selected model may be incompatible — try a smaller / " +
          "different one in Settings → Model.",
        details: message,
      };
    case "clipboard":
      return {
        headline: "Couldn't copy to clipboard",
        hint:
          "The transcript was generated but the clipboard write " +
          "failed. Open History to copy it manually.",
        details: message,
      };
    case "settings":
      return {
        headline: "Settings update failed",
        details: message,
      };
    case "history":
      return {
        headline: "History update failed",
        hint: "The action didn't go through. Try again in a moment.",
        details: message,
      };
    case "replacements":
      return {
        headline: "Replacements update failed",
        hint: "The change wasn't saved. Try again in a moment.",
        details: message,
      };
    case "meeting-sessions":
      return {
        headline: "Meeting session error",
        hint:
          "Try again, or fall back to a single-source recording " +
          "(microphone only).",
        details: message,
      };
    case "internal":
      return {
        headline: "Internal error",
        hint: "Restart Hush. If it keeps happening, file an issue.",
        details: message,
      };
    default:
      // Unknown kind — surface what we have so a future variant
      // doesn't render as a confusingly-empty box.
      return {
        headline: ipc.kind || "Something went wrong",
        details: message || undefined,
      };
  }
}

/// String flavour of [`formatErrorDisplay`] for surfaces that
/// haven't migrated to the rich `ErrorDisplay` shape — autostart's
/// inline status text, the per-card model-download failure map,
/// and the shared `firstRunResetMessage` / `macosResetMessage`
/// status lines that double as success copy. Routes both windows
/// through one source of truth so per-window `formatError` shadows
/// can't drift.
///
/// Renders as `"headline: details"` (or just `"headline"` when no
/// details). Hint is dropped — the surfaces using this format show
/// the message inline next to the action that failed, where a
/// multi-line hint reads as noise.
export function formatErrorMessage(e: unknown): string {
  const display = formatErrorDisplay(e);
  if (display.details && display.details.length > 0) {
    return `${display.headline}: ${display.details}`;
  }
  return display.headline;
}
