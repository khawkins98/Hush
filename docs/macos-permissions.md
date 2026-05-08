# macOS permissions troubleshooting

Hush currently needs **two** macOS permissions to work end-to-end:

- **Microphone** — for dictation capture and meeting audio capture. Since v0.5.0, system audio uses the CoreAudio process tap backend, so there is **no separate Screen Recording grant** anymore.
- **Input Monitoring** — for the low-level keyboard hook that powers push-to-talk while another app is focused.

This doc covers what to do when either permission gets into a stuck state. If you file an issue, copy the symptom heading you hit below and include the macOS version plus the Hush version / commit SHA. Settings → Permissions in Hush shows the current status snapshot and includes a one-click reset shortcut.

> **System audio note:** current builds do **not** need Screen Recording / “Screen & System Audio Recording” for Meeting Mode. If system audio is missing on v0.5.x+, treat it as an audio-path or source-selection bug — not a third TCC grant you still need to flip.

---

## PTT default-on as of #194 (with a safer prompt flow in v0.5.1)

Push-to-talk (hold-to-record — `Right ⌘` by default on macOS, `Right Ctrl` elsewhere) is still **on by default**.

What changed in v0.5.1 is *when* Input Monitoring prompts. Hush no longer starts the keyboard listener unconditionally at app boot. On a fresh macOS install, the Input Monitoring prompt should appear when you first enable / re-enable PTT from Settings → General → Hotkeys, rather than firing at startup before you've even reached the relevant setting.

Don't want PTT? Turn **Push-to-talk enabled** off in Settings → General → Hotkeys. The toggle hotkey (`⌃⌥H` by default) is separate — it uses Tauri's global-shortcut plugin, not the Input Monitoring-gated rdev listener.

---

## Why dev builds are flaky for permissions

macOS's TCC (Transparency, Consent, and Control) database keys grants to a specific code-signing identity + bundle ID. A signed `Hush.app` from `npm run tauri:bundle` registers under `io.github.khawkins98.hush` and gives you the most realistic permission flow. The `cargo tauri dev` / `npm run tauri dev` path runs an unsigned debug binary, and TCC behaviour gets unpredictable:

- The grant may bind to a binary hash that changes on the next rebuild.
- The first prompt may attribute to **Terminal**, iTerm, your IDE, or another parent process instead of Hush.
- Microphone and Input Monitoring can both look "granted" in System Settings while the current dev binary still fails the OS preflight.

Use the dev path for fast iteration. Use the bundled path before claiming a macOS permission flow is fixed.

---

## Symptom: PTT silently does nothing

You hold `Right ⌘` (the default macOS PTT key), but recording never starts. The toggle hotkey still works.

**Cause:** Input Monitoring is not granted, is bound to a stale build identity, or the persisted PTT toggle is off.

**Fix:**

1. In Hush: Settings → General → Hotkeys → confirm **Push-to-talk enabled** is checked.
2. In macOS: System Settings → Privacy & Security → **Input Monitoring**. If Hush is listed and enabled but PTT still does nothing, toggle it off and back on. If you see multiple Hush rows, remove them all.
3. Reset and re-prompt:
   ```sh
   tccutil reset ListenEvent io.github.khawkins98.hush
   ```
   (`ListenEvent` is the TCC name for Input Monitoring.)
4. Relaunch Hush, then visit Settings → General → Hotkeys and toggle **Push-to-talk enabled** off and back on so Hush attempts to arm the listener again.

If `tccutil reset` says `No such bundle identifier`, the dev binary is not registered under `io.github.khawkins98.hush`. Run `tccutil reset ListenEvent` with **no** bundle ID instead. That resets Input Monitoring for every app on the machine, so other apps will re-prompt too, but it clears Hush's stale row.

---

## Symptom: clicking Start records but the transcript is blank / silence

You press Start, stop after a few seconds, and get the friendly "No audio detected" result (or, on an older build, `[BLANK_AUDIO]`).

**Cause:** Microphone permission was denied, revoked, or granted to a stale build identity. macOS lets the stream open but delivers silence.

**Fix:**

1. System Settings → Privacy & Security → **Microphone**. Confirm Hush is enabled.
2. Reset and re-prompt:
   ```sh
   tccutil reset Microphone io.github.khawkins98.hush
   ```
3. Relaunch Hush and start a short recording. The OS should prompt again.

This is the same permission bucket Hush now uses for Meeting Mode audio capture too. There is no longer a separate Screen Recording reset step for system audio.

---

## Symptom: the prompt attributes the request to "Terminal" or another app

The first time you run `cargo tauri dev`, the macOS prompt shows up — but the app name/icon in the dialog is Terminal, iTerm, your IDE, or something equally unhelpful.

**Cause:** the unsigned dev binary does not carry an identity macOS recognizes, so TCC falls back to the parent process that launched it.

**Fix:**

1. Deny the misattributed prompt. Don't give Terminal microphone or Input Monitoring access just to work around a dev-binary quirk.
2. Build a signed bundle once:
   ```sh
   npm run tauri:bundle
   ```
3. Launch the bundled app and grant the prompt there under the real Hush identity (`io.github.khawkins98.hush`).
4. If later dev runs drift back into a stale state, use the reset recipes above.

---

## Resetting all Hush permissions at once

```sh
tccutil reset Microphone io.github.khawkins98.hush
tccutil reset ListenEvent io.github.khawkins98.hush      # Input Monitoring
tccutil reset Accessibility io.github.khawkins98.hush    # only if Hush ever asked
```

After relaunch, each permission re-prompts on the next trigger:

- **Microphone** — the next time you start dictation or a meeting capture.
- **Input Monitoring** — when you enable / re-enable PTT in Settings.

The same recipe is wrapped behind the reset button in Settings → Permissions.

---

## Dev-loop: stale Hush.app rows after a re-bundle

Even with the correct bundle ID, ad-hoc signing is still imperfect across rebuilds. macOS can accumulate **multiple Hush.app rows** in System Settings → Privacy & Security, one per signing identity it has seen.

When the active build's identity differs from the row that's currently enabled, you get the classic stale-grant behaviour: the row looks on, but the running build still fails the OS permission check.

**Recovery:**

1. Open System Settings → Privacy & Security → **Microphone** and **Input Monitoring**.
2. Remove every `Hush.app` row with the **`−`** button.
3. Run:
   ```sh
   npm run dev-reset
   npm run tauri:bundle
   ```
4. Grant Microphone when the bundled app first records.
5. If you use PTT, go to Settings → General → Hotkeys and enable / re-enable it so Input Monitoring prompts under the freshly bundled app identity.

If the rows refuse to disappear from the UI even after `tccutil reset`, manual `−` removal in System Settings is the authoritative fix.

---

## Traffic-light permission health in Settings → Permissions

Each permission row shows a coloured status dot:

- 🟢 **Green (Confirmed)** — the OS preflight succeeds right now.
- 🟡 **Yellow (Stale)** — Hush saw this permission granted before, but the current preflight now fails. Most often this means a rebuilt dev/bundled binary no longer matches the TCC row.
- 🔴 **Red (Not granted)** — there is no current grant.
- ⚫ **Grey (Not applicable)** — platform where the macOS TCC category does not apply.

**Stale** is the tricky one. The OS preflight APIs are blunt booleans; they do not tell Hush whether the user explicitly denied the prompt or whether the code-signing identity simply changed. Hush resolves that by remembering the last confirmed success timestamp. "Was granted before, false now" becomes **Stale**.

If a yellow row flips back immediately after you grant it, you almost certainly have the stale-rows problem above.

---

## What about Hush's first-run welcome modal?

The welcome / permissions flow now focuses on the two permissions that still matter:

- **Microphone**
- **Input Monitoring**

There is no separate Screen Recording step on current builds. If you dismissed the welcome flow and want it back, use Settings → General → **Show welcome on next launch**.
