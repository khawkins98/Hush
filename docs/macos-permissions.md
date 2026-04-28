# macOS permissions troubleshooting

Hush needs three macOS permissions to work end-to-end:

- **Microphone** — for `cpal` to open the input stream when you press Start. The OS prompts the first time recording begins.
- **Input Monitoring** — for `rdev`'s low-level keyboard hook so push-to-talk works while a different app is focused. The OS prompts on first launch (PTT is on by default; the listener spawns at startup unless you disable it in Settings).
- **Screen Recording** — for ScreenCaptureKit to capture system audio in Meeting Mode. The OS prompts when a meeting session asks for the system-audio source for the first time.

This doc covers what to do when any of them gets into a stuck state. If you're authoring an issue, copy the symptom you're hitting from below and include the OS version + Hush commit SHA. Settings → Permissions inside Hush also surfaces a per-permission status snapshot (granted / denied / not-determined) and a one-click "reset Hush's TCC entries" button.

---

## PTT default-on as of #194

Push-to-talk (the hold-to-record key — `Right ⌘` by default on macOS, `Right Ctrl` elsewhere) is **on by default everywhere** as of #194. The macOS Input Monitoring prompt fires on first launch.

Pre-#194 PTT was opt-in on macOS because `rdev::listen` aborted at the OS level on macOS 26+ (`dispatch_assert_queue_fail` from a non-main-thread TSM call). That's resolved — we pin to fufesou's rdev fork (the one RustDesk ships) which attaches the CGEventTap to `CFRunLoopGetMain()`. See `learnings.md` 2026-04-27 for the diff-against-Narsil details.

Don't want PTT? Settings → General → Hotkeys → toggle "Push-to-talk enabled" off. The setting persists across launches; the listener stays unspawned the next time the app boots.

The toggle hotkey (`⌃⌥H` by default) is independent — it goes through Tauri's global-shortcut plugin, not rdev, and isn't affected by the PTT toggle.

---

## Why dev builds are flaky for permissions

macOS's TCC (Transparency, Consent, and Control) database keys permissions to a specific code-signing identity + bundle ID. A signed `Hush.app` (from `npm run tauri:bundle`) registers under `com.khawkins.hush` and the grant survives rebuilds. The `cargo tauri dev` flow runs `target/debug/hush` — an unsigned binary — and TCC behaviour gets unpredictable:

- The grant may bind to the binary's hash, which changes on every Cargo rebuild. Result: you grant once, the next launch silently has no permission.
- The first prompt may attribute to *Terminal* (or whatever shell parent invoked `cargo tauri dev`) rather than to Hush itself. Granting Terminal does nothing for Hush. Microphone and Input Monitoring fall through this parent-attribution path and work fine; **Screen Recording is stricter** and effectively requires a real `.app` bundle.
- If the bundle ID didn't get applied (some unsigned dev builds register under a binary path instead), `tccutil reset … com.khawkins.hush` returns "No such bundle identifier" and you have to reset more broadly.

The signed-bundle path (`npm run tauri:bundle`) is the most realistic test of "what users will see." Use the dev path for fast iteration, the bundle path before claiming a permission flow is shipped or before testing system-audio capture.

---

## Symptom: PTT silently does nothing

You hold `Right ⌘` (the default PTT key on macOS) but no recording starts. The toggle hotkey (`⌃⌥H`) works fine.

**Cause:** Input Monitoring not granted, or granted to a stale binary, or the persisted PTT toggle is off.

**Fix:**

1. Settings → General → Hotkeys → confirm "Push-to-talk enabled" is checked.
2. System Settings → Privacy & Security → **Input Monitoring**. If Hush is listed and toggled on, but PTT still doesn't fire, toggle it off and on. If listed multiple times (multiple binary paths), remove all entries.
3. Reset and re-prompt:
   ```sh
   tccutil reset ListenEvent com.khawkins.hush
   ```
   (Yes, "Input Monitoring" is `ListenEvent` in the TCC vocabulary. macOS naming.)
4. Relaunch Hush — the prompt should reappear on first PTT press.

If the `tccutil reset` returns "No such bundle identifier," the dev binary isn't registered under `com.khawkins.hush`. Run `tccutil reset ListenEvent` (no bundle id) — this resets *every* app's Input Monitoring permission, so other apps will re-prompt too, but it clears Hush's stale entry. Settings → Permissions in Hush wraps this same call.

---

## Symptom: clicking Start records but the transcript is empty / silence

You press Start, Stop after a few seconds, and the result is the friendly "No audio detected" copy (post-#196) or — on older builds — `[BLANK_AUDIO]` leaking through.

**Cause:** Microphone permission denied or revoked. The cpal stream opened successfully but is delivering all-zero samples (macOS gives this back instead of erroring).

**Fix:**

1. System Settings → Privacy & Security → **Microphone**. Confirm Hush is enabled.
2. Reset and re-prompt:
   ```sh
   tccutil reset Microphone com.khawkins.hush
   ```
3. Relaunch Hush. Start recording — the OS should prompt this time.

---

## Symptom: meeting session start fails with a Screen Recording error

You start a meeting with system audio enabled and immediately see a "Screen Recording permission needed" card.

**Cause:** ScreenCaptureKit needs Screen Recording permission to enumerate shareable content; until granted, the system-audio source can't open.

**Fix:**

1. System Settings → Privacy & Security → **Screen Recording**. Confirm Hush is enabled.
2. Reset and re-prompt:
   ```sh
   tccutil reset ScreenCapture com.khawkins.hush
   ```
3. Relaunch Hush. Start a meeting with system audio — the OS should prompt this time. Until it's granted, microphone-only meetings still work.

If you're running `cargo tauri dev` (an unsigned binary), Screen Recording typically can't be granted at all — the OS attributes the request to a parent process that's not the `.app` bundle. Use `npm run tauri:bundle` to validate the system-audio path.

---

## Symptom: the prompt attributes the request to "Terminal" or another app

The first time you run `cargo tauri dev`, the macOS Microphone or Input Monitoring prompt shows up but the app icon and name in the prompt aren't Hush — they're Terminal, iTerm, your IDE, or sometimes something even less helpful.

**Cause:** the unsigned dev binary doesn't carry an identity macOS recognizes, so it falls back to attributing the request to the parent process that spawned it.

**Fix:**

1. Deny the misattributed prompt (don't grant Terminal mic access — that's a privilege you don't actually want).
2. Build a signed bundle once:
   ```sh
   npm run tauri:bundle
   ```
3. The bundled app will prompt under its own identity (`com.khawkins.hush`). Grant.
4. Subsequent `cargo tauri dev` sessions inherit the bundle-ID grant in many cases (TCC is forgiving when the bundle ID matches). When they don't, see the symptoms above for the reset recipe.

---

## Resetting all Hush permissions at once

```sh
tccutil reset Microphone com.khawkins.hush
tccutil reset ListenEvent com.khawkins.hush      # Input Monitoring
tccutil reset ScreenCapture com.khawkins.hush    # Screen Recording
tccutil reset Accessibility com.khawkins.hush    # if the app ever asked
```

Followed by relaunch. Each permission re-prompts on the next trigger:

- Microphone — the next time you click Start.
- Input Monitoring — at app startup (the rdev listener spawns there when PTT is enabled).
- Screen Recording — when a meeting session opens the system-audio source.

The same recipe is wrapped behind a button in Settings → Permissions inside Hush.

---

## What about Hush's first-run welcome modal?

The welcome modal (the dismissible card on first launch) explains the permissions and links out to the right System Settings panes via the `open_macos_privacy_pane` IPC command. It does **not** trigger the prompts itself — it can't, macOS doesn't expose a programmatic "ask for X" API. The OS prompts already fire from the cpal / rdev / SCK call sites. The modal is an explainer, not a button to grant.

If you dismissed the modal and want it back: Settings → General → "Show welcome on next launch."
