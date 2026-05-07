// External-URL opener (#322, #648).
//
// Plain `<a target="_blank">` links do nothing in a Tauri 2 WebView
// — outbound navigation is intercepted and dropped silently. This
// helper invokes the `open_url` IPC command which spawns `open`/
// `xdg-open`/`rundll32` directly via `posix_spawn()` — unlike the
// previous `tauri-plugin-shell` route which used `fork()` in the
// multithreaded Tokio process and caused SIGSEGV on macOS (#648).
//
// Use from a click handler:
//
//     <a
//       href={url}
//       onclick={(e) => { e.preventDefault(); openExternal(url); }}
//       rel="noopener noreferrer"
//     >link text</a>
//
// `href` stays for accessibility (right-click → Copy link, screen
// readers announce the URL); `onclick` prevents the dead WebView
// navigation and routes through the IPC instead.

import { invoke } from "@tauri-apps/api/core";

export async function openExternal(url: string): Promise<void> {
  try {
    await invoke("open_url", { url });
  } catch (err) {
    // Failures are exotic — the IPC only errors if the URL scheme is
    // rejected or the OS handler is missing. Log for support and
    // continue; the alternative is silently failing twice (the WebView
    // already won't navigate).
    console.warn("[hush] openExternal failed", { url, err });
  }
}
