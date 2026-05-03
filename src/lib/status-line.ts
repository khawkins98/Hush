/**
 * Technical status line toggle (#411 phase F5).
 *
 * The status line under the main-window waveform reads
 * `🎤 device · model`. It's an opt-in display — power users want
 * the at-a-glance reassurance that the right device + model are
 * loaded; first-time users would just be asking "what does that
 * mean?" Toggle lives in Settings → General → Advanced.
 *
 * Persistence is `localStorage`, mirroring `theme.ts`. Tauri
 * webviews on macOS share the data store across windows, so
 * boot-time reads are coherent. To propagate a *change* across
 * already-open windows (Settings on, main on) we emit a Tauri
 * event; the main window's ControlsSection listens.
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Events } from "./events";

const STORAGE_KEY = "hush.statusLine";

export function readStatusLineEnabled(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(STORAGE_KEY) === "1";
}

export async function setStatusLineEnabled(enabled: boolean): Promise<void> {
  if (typeof localStorage !== "undefined") {
    if (enabled) {
      localStorage.setItem(STORAGE_KEY, "1");
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
  }
  try {
    await emit(Events.StatusLine, enabled);
  } catch {
    // Best-effort cross-window sync; the local apply already
    // covered the current window via the storage write.
  }
}

export function listenForStatusLineChanges(
  onChange: (enabled: boolean) => void,
): Promise<UnlistenFn> {
  return listen<boolean>(Events.StatusLine, (event) => {
    onChange(event.payload === true);
  });
}
