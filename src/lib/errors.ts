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

import type { IpcError, KnownIpcError, KnownIpcErrorKind } from "./types";

/// Discriminated tag the parent screen can map to a concrete
/// callback when rendering an actionable error. Keeping it a string
/// keeps `ErrorDisplay` serialisable and avoids closing over UI
/// callbacks inside `lib/errors.ts` (which has no view dependencies).
/// Add new keys here when an error class can offer a one-click recovery.
export type ErrorActionKey = "open-model-settings";

export type ErrorDisplay = {
  /// Plain-language summary, ~5 words. Always present.
  headline: string;
  /// Action-oriented recovery hint. Optional — some errors don't
  /// have a clear next step.
  hint?: string;
  /// Raw technical message — surfaced in a collapsed `<details>`
  /// so power users can debug, but not in the user's face.
  details?: string;
  /// Optional one-click recovery. The component renders a button
  /// labelled `actionLabel` that calls the parent's `onAction`
  /// callback with `actionKey`; the parent maps the key to a
  /// concrete handler (e.g. `"open-model-settings"` → opens the
  /// Model tab in Settings). Both fields must be set for a button
  /// to render.
  actionKey?: ErrorActionKey;
  actionLabel?: string;
};

const KNOWN_IPC_ERROR_KINDS = new Set<KnownIpcErrorKind>([
  "audio",
  "audio-device-lost",
  "transcription",
  "transcription-unavailable",
  "clipboard",
  "settings",
  "history",
  "replacements",
  "meeting-sessions",
  "permission-denied",
  "updater-unavailable",
  "internal",
]);

function isIpcError(e: unknown): e is IpcError {
  return typeof e === "object" && e !== null && "kind" in e;
}

function isKnownIpcError(ipc: IpcError): ipc is KnownIpcError {
  return KNOWN_IPC_ERROR_KINDS.has(ipc.kind as KnownIpcErrorKind);
}

function ipcMessage(ipc: IpcError): string {
  return "message" in ipc ? (ipc.message ?? "") : "";
}

function assertNever(value: never): never {
  throw new Error(`Unhandled IPC error kind: ${String(value)}`);
}

/// Check whether a thrown value is a permission-shaped IPC error
/// (#232, refined in #386).
///
/// Primary check: \`kind === "permission-denied"\` — the typed
/// variant the backend started emitting in #386 from
/// \`meeting_start_manual\`'s error path. Once every IPC that
/// could throw a permission-shaped error is updated to use the
/// classifier, the substring fallback below can be retired.
///
/// Fallback: substring match against the chained message string
/// for any IPC variant that hasn't been updated yet. Same patterns
/// the Rust \`classify_permission_error\` helper uses, so the two
/// surfaces stay coherent.
///
/// Callers use this to decide whether to pop the PermissionsDialog
/// (#232) alongside the error chip — putting the next click on a
/// button that opens System Settings rather than buried in the
/// hint copy. Returns false for non-IPC throwables.
export function isPermissionShapedError(e: unknown): boolean {
  if (!isIpcError(e)) {
    return false;
  }
  if (e.kind === "permission-denied") {
    return true;
  }
  const lower = ipcMessage(e).toLowerCase();
  return (
    lower.includes("screen recording") ||
    lower.includes("declined tccs") ||
    (lower.includes("microphone") && lower.includes("not authorized")) ||
    lower.includes("input monitoring")
  );
}

/// Tailored copy for each known permission name. Used by both
/// the typed-variant path (#386) and the substring fallback so
/// the message a user sees is identical regardless of how the
/// classification happened. `details` carries the original
/// message for the collapsed `<details>` debug view; pass
/// `undefined` when called from the typed path (the IPC's
/// `message` field already IS the permission name, no chain
/// content to surface).
function formatPermissionDenied(
  permission: string,
  details?: string,
): ErrorDisplay {
  switch (permission) {
    case "screen-recording":
      return {
        headline: "System Audio permission needed",
        hint:
          "Grant Hush System Audio access in System Settings → " +
          "Privacy & Security → Screen Recording, then relaunch " +
          "Hush to enable system-audio capture. " +
          "Microphone-only recording still works without it.",
        details,
      };
    case "microphone":
      return {
        headline: "Microphone permission needed",
        hint:
          "Grant Hush microphone access in System Settings → Privacy " +
          "& Security → Microphone, then try again.",
        details,
      };
    case "input-monitoring":
      return {
        headline: "Input Monitoring permission needed",
        hint:
          "Grant Hush Input Monitoring access in System Settings → " +
          "Privacy & Security → Input Monitoring. PTT keystrokes " +
          "won't reach the listener until then.",
        details,
      };
    default:
      return {
        headline: "Permission needed",
        hint: `Hush couldn't access ${permission}. Open System Settings → Privacy & Security to grant access.`,
        details,
      };
  }
}

function formatKnownIpcError(ipc: KnownIpcError): ErrorDisplay {
  switch (ipc.kind) {
    case "permission-denied":
      return formatPermissionDenied(ipc.message);
    case "transcription-unavailable":
      return {
        headline: "No transcription model loaded",
        hint:
          "Pick one in Settings → Model. Hush will fetch and verify " +
          "it, then load it without a restart.",
        actionKey: "open-model-settings",
        actionLabel: "Open Settings → Model",
      };
    case "audio":
      return {
        headline: "Microphone access failed",
        hint:
          "Check your selected input source and that the mic is " +
          "plugged in. On macOS, also check System Settings → " +
          "Privacy & Security → Microphone for Hush.",
        details: ipc.message,
      };
    case "audio-device-lost":
      // The selected device disconnected mid-session — distinct
      // from the generic "audio" bucket so the user gets a concrete
      // explanation (USB unplugged, AirPods walked away, webcam
      // disabled) and a clear next step. Auto-fallback to a
      // different source is gated on the policy decision tracked
      // in PR 2 of #587.
      return {
        headline: "Microphone disconnected",
        hint: `The selected input source ("${ipc.message}") is no longer available. Pick a different source and try again.`,
      };
    case "transcription":
      return {
        headline: "Transcription failed",
        hint:
          "The selected model may be incompatible — try a smaller / " +
          "different one in Settings → Model.",
        details: ipc.message,
      };
    case "clipboard":
      return {
        headline: "Couldn't copy to clipboard",
        hint:
          "The transcript was generated but the clipboard write " +
          "failed. Open History to copy it manually.",
        details: ipc.message,
      };
    case "settings":
      return {
        headline: "Settings update failed",
        details: ipc.message,
      };
    case "history":
      return {
        headline: "History update failed",
        hint: "The action didn't go through. Try again in a moment.",
        details: ipc.message,
      };
    case "replacements":
      return {
        headline: "Replacements update failed",
        hint: "The change wasn't saved. Try again in a moment.",
        details: ipc.message,
      };
    case "meeting-sessions":
      return {
        headline: "Meeting session error",
        hint:
          "Try again, or fall back to a single-source recording " +
          "(microphone only).",
        details: ipc.message,
      };
    case "updater-unavailable":
      return {
        headline: "Automatic install unavailable",
        hint:
          "Check the latest release notes instead. This build can still download updates manually.",
      };
    case "internal":
      return {
        headline: "Internal error",
        hint: "Restart Hush. If it keeps happening, file an issue.",
        details: ipc.message,
      };
    default:
      return assertNever(ipc);
  }
}

/// Map an unknown thrown value into the rich display shape. The
/// frontend's catch blocks pass `unknown`; the function inspects
/// the IpcError tag + message to pick the friendliest copy.
export function formatErrorDisplay(e: unknown): ErrorDisplay {
  if (!isIpcError(e)) {
    return {
      headline: "Something went wrong",
      details: e instanceof Error ? e.message : String(e),
    };
  }

  const message = ipcMessage(e);

  // Substring fallback. Any IPC variant that hasn't been updated
  // to use `classify_permission_error` still surfaces the chain
  // message verbatim, so we keep this branch as a safety net.
  const lower = message.toLowerCase();
  if (lower.includes("screen recording") || lower.includes("declined tccs")) {
    return formatPermissionDenied("screen-recording", message);
  }
  if (lower.includes("microphone") && lower.includes("not authorized")) {
    return formatPermissionDenied("microphone", message);
  }
  if (lower.includes("input monitoring")) {
    return formatPermissionDenied("input-monitoring", message);
  }

  if (isKnownIpcError(e)) {
    return formatKnownIpcError(e);
  }

  // Unknown kind — surface what we have so a future variant
  // doesn't render as a confusingly-empty box.
  return {
    headline: e.kind || "Something went wrong",
    details: message || undefined,
  };
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
