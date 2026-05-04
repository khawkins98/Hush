/**
 * Developer console toggle (#532).
 *
 * When enabled, the Settings window exposes a "Debug" tab with a
 * live view of the Rust backend's `tracing` log stream. The toggle
 * lives in Settings → General → Advanced.
 *
 * Persistence is `localStorage` (same as status-line.ts). No
 * cross-window event is needed: the debug tab only lives in the
 * settings window, and the toggle is read once on mount.
 */

const STORAGE_KEY = "hush.debugConsole";

export function readDebugConsoleEnabled(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(STORAGE_KEY) === "1";
}

export function setDebugConsoleEnabled(enabled: boolean): void {
  if (typeof localStorage === "undefined") return;
  if (enabled) {
    localStorage.setItem(STORAGE_KEY, "1");
  } else {
    localStorage.removeItem(STORAGE_KEY);
  }
}
