/**
 * Appearance / theme override (#411 phase A).
 *
 * The default behaviour follows the OS — `app.css` has a
 * `@media (prefers-color-scheme: dark)` block that swaps the dark
 * tokens in. This module adds an explicit user override on top:
 * "system" (no override), "light" (force light), or "dark"
 * (force dark). The override beats the media query via attribute-
 * selector specificity (`:root[data-theme="…"]` > `:root` inside
 * `@media`), so a user on a dark OS who picks Light still gets
 * the light tokens.
 *
 * Persistence is `localStorage` — Tauri webviews on macOS share a
 * data store across windows, so settings + main + HUD all read the
 * same value at boot. To propagate a *change* across already-open
 * windows we emit a Tauri event; every window listens and re-
 * applies. Going through Tauri rather than the browser's `storage`
 * event keeps the path identical on platforms where webview
 * isolation is stricter.
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ThemePref = "system" | "light" | "dark";

const STORAGE_KEY = "hush.theme";
export const THEME_EVENT = "hush:theme";

export function readStoredTheme(): ThemePref {
  if (typeof localStorage === "undefined") return "system";
  const raw = localStorage.getItem(STORAGE_KEY);
  return raw === "light" || raw === "dark" ? raw : "system";
}

export function applyThemeAttribute(pref: ThemePref): void {
  if (typeof document === "undefined") return;
  if (pref === "system") {
    document.documentElement.removeAttribute("data-theme");
  } else {
    document.documentElement.setAttribute("data-theme", pref);
  }
}

export async function setTheme(pref: ThemePref): Promise<void> {
  if (typeof localStorage !== "undefined") {
    if (pref === "system") {
      localStorage.removeItem(STORAGE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, pref);
    }
  }
  applyThemeAttribute(pref);
  try {
    await emit(THEME_EVENT, pref);
  } catch {
    // Emit is best-effort cross-window sync; the local apply
    // above already covered the current window.
  }
}

export function listenForThemeChanges(
  onChange: (pref: ThemePref) => void,
): Promise<UnlistenFn> {
  return listen<ThemePref>(THEME_EVENT, (event) => {
    const next = event.payload;
    if (next === "system" || next === "light" || next === "dark") {
      onChange(next);
    }
  });
}
