/**
 * Developer console toggle (#532).
 *
 * When enabled, the Settings → Debug tab shows an "Open Debug Console"
 * button that launches a floating always-on-top palette window
 * (`"debug"` label in tauri.conf.json). The live log stream runs in
 * that window so the user can watch events while clicking around the
 * app.
 *
 * The toggle lives in Settings → General → Advanced.
 *
 * Persistence is `localStorage` (same as status-line.ts). No
 * cross-window event is needed: the debug tab only lives in the
 * Settings panel, and the toggle is read once on mount.
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
