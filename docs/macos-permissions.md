# macOS permissions troubleshooting

Hush needs two macOS permissions to work end-to-end:

- **Microphone** — for `cpal` to open the input stream when you press Start. The OS prompts the first time recording begins.
- **Input Monitoring** — for `rdev`'s low-level keyboard hook so push-to-talk works while a different app is focused. The OS prompts when the app first starts (because the rdev listener spawns at startup).

This doc covers what to do when either gets into a stuck state. If you're authoring an issue, copy the symptom you're hitting from below and include the OS version + Hush commit SHA.

---

## Why dev builds are flaky for permissions

macOS's TCC (Transparency, Consent, and Control) database keys permissions to a specific code-signing identity + bundle ID. A signed `Hush.app` (from `npm run tauri build`) registers under `com.khawkins.hush` and the grant survives rebuilds. The `cargo tauri dev` flow runs `target/debug/hush` — an unsigned binary — and TCC behaviour gets unpredictable:

- The grant may bind to the binary's hash, which changes on every Cargo rebuild. Result: you grant once, the next launch silently has no permission.
- The first prompt may attribute to *Terminal* (or whatever shell parent invoked `cargo tauri dev`) rather than to Hush itself. Granting Terminal does nothing for Hush.
- If the bundle ID didn't get applied (some unsigned dev builds register under a binary path instead), `tccutil reset … com.khawkins.hush` returns "No such bundle identifier" and you have to reset more broadly.

The signed-bundle path (`npm run tauri build`) is the most realistic test of "what users will see." Use the dev path for fast iteration, the bundle path before claiming a permission flow is shipped.

---

## Symptom: PTT silently does nothing

You hold `Right Control` (the default PTT key) but no recording starts. The toggle hotkey (`⌘+Shift+H`) works fine.

**Cause:** Input Monitoring not granted, or granted to a stale binary.

**Fix:**

1. System Settings → Privacy & Security → **Input Monitoring**. If Hush is listed and toggled on, but PTT still doesn't fire, toggle it off and on. If listed multiple times (multiple binary paths), remove all entries.
2. Reset and re-prompt:
   ```sh
   tccutil reset ListenEvent com.khawkins.hush
   ```
   (Yes, "Input Monitoring" is `ListenEvent` in the TCC vocabulary. macOS naming.)
3. Relaunch Hush — the prompt should reappear on first PTT press.

If the `tccutil reset` returns "No such bundle identifier," the dev binary isn't registered under `com.khawkins.hush`. Run `tccutil reset ListenEvent` (no bundle id) — this resets *every* app's Input Monitoring permission, so other apps will re-prompt too, but it clears Hush's stale entry.

---

## Symptom: clicking Start records but the transcript is empty / silence

You press Start, Stop after a few seconds, and the result is an empty string or only the model's filler text (e.g. `[BLANK_AUDIO]` from Whisper).

**Cause:** Microphone permission denied or revoked. The cpal stream opened successfully but is delivering all-zero samples (macOS gives this back instead of erroring).

**Fix:**

1. System Settings → Privacy & Security → **Microphone**. Confirm Hush is enabled.
2. Reset and re-prompt:
   ```sh
   tccutil reset Microphone com.khawkins.hush
   ```
3. Relaunch Hush. Start recording — the OS should prompt this time.

---

## Symptom: the prompt attributes the request to "Terminal" or another app

The first time you run `cargo tauri dev`, the macOS Microphone or Input Monitoring prompt shows up but the app icon and name in the prompt aren't Hush — they're Terminal, iTerm, your IDE, or sometimes something even less helpful.

**Cause:** the unsigned dev binary doesn't carry an identity macOS recognizes, so it falls back to attributing the request to the parent process that spawned it.

**Fix:**

1. Deny the misattributed prompt (don't grant Terminal mic access — that's a privilege you don't actually want).
2. Build a signed bundle once:
   ```sh
   npm run tauri build
   open src-tauri/target/release/bundle/macos/Hush.app
   ```
3. The bundled app will prompt under its own identity (`com.khawkins.hush`). Grant.
4. Subsequent `cargo tauri dev` sessions inherit the bundle-ID grant in many cases (TCC is forgiving when the bundle ID matches). When they don't, see the symptoms above for the reset recipe.

---

## Resetting all Hush permissions at once

```sh
tccutil reset Microphone com.khawkins.hush
tccutil reset ListenEvent com.khawkins.hush      # Input Monitoring
tccutil reset Accessibility com.khawkins.hush    # if the app ever asked
```

Followed by relaunch. Each permission re-prompts on the next trigger:

- Microphone — the next time you click Start.
- Input Monitoring — at app startup (the rdev listener spawns there).

---

## What about Hush's first-run welcome modal?

The welcome modal (the dismissible card on first launch) explains both permissions and links out to the right System Settings panes via the `open_macos_privacy_pane` IPC command. It does **not** trigger the prompts itself — it can't, macOS doesn't expose a programmatic "ask for X" API. The OS prompts already fire from the cpal/rdev call sites. The modal is an explainer, not a button to grant.

If a user dismisses the modal then later hits a stuck-permission symptom, this doc is the recovery path. We may add a "Run diagnostics" button in a future PR (tracking issue: see the project board) that wraps the `tccutil reset` recipe in-app.
