// External-URL opener (#322).
//
// Plain `<a target="_blank">` links do nothing in a Tauri 2 WebView
// — outbound navigation is intercepted and dropped silently. This
// helper delegates to `tauri-plugin-shell`'s `open()` which hands
// the URL to the OS browser via the standard URL-handler chain
// (`open` on macOS, `xdg-open` / portal on Linux, `ShellExecute`
// on Windows).
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
// navigation and routes through the plugin instead. The Rust crate
// `tauri-plugin-shell` must be registered in `lib.rs` and
// `shell:allow-open` granted in the window's capability JSON for
// the call to succeed.

import { open } from "@tauri-apps/plugin-shell";

export async function openExternal(url: string): Promise<void> {
  try {
    await open(url);
  } catch (err) {
    // Failures are exotic — the plugin only errors if the
    // capability isn't granted or the OS handler is missing. Log
    // for support and continue; the alternative is silently
    // failing twice (the WebView already won't navigate).
    console.warn("[hush] openExternal failed", { url, err });
  }
}
