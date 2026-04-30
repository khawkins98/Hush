# Hush

**Offline voice-to-text for macOS, Windows, and Linux.**

Hush records your voice, transcribes it locally using [whisper.cpp](https://github.com/ggerganov/whisper.cpp) (via `whisper-rs`), and places the text on your clipboard — ready to paste wherever you need it. Transcription happens on-device — no audio leaves your machine. No telemetry by default. The only network traffic is the one-time download of the Whisper model from [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp) the first time you pick one; after that, transcription is fully offline.

> **Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk).** No source code was copied or referenced. See [Acknowledgements](#acknowledgements).

---

## Status

🚧 **Active development — usable on macOS 26 for early testers.** Dictation hot path, Meeting Mode, sidebar nav with Dictation/Meetings/History, standalone Settings window (⌘,) with model picker + vocabulary + replacements + macOS permissions diagnostic + autostart toggle, configurable PTT combo with on-demand listener, recording HUD with drag + dismiss + live level meter, history with FTS5 search. Auto-update and direct-text-insertion are deferred. Older macOS versions, Linux, and Windows are not hands-on tested by the maintainer; see the platform table below. See [STATUS.md](./STATUS.md) for the latest snapshot.

---

## Features

### Shipped

**Dictation**
- 🎙️ Toggle-record global hotkey (`Ctrl+⌥/Alt+H` by default; works on every platform)
- 🎙️ **Configurable push-to-talk, on by default** (#194) — pick any combination of modifier / function / Caps Lock keys held simultaneously. Default is `Right ⌘` on macOS, `Right Ctrl` elsewhere. Edit in Settings → General → Hotkeys; combo + Enabled persist across launches.
- 🤫 100% local transcription — whisper.cpp on your machine; no audio ever leaves the device
- 📋 Transcription written to clipboard with a "Ready to paste" notification
- 🔴 Recording HUD overlay — transparent always-on-top pill with pulsing dot + live RMS level meter, draggable, with a dismiss button that hides without stopping the recording

**Meeting Mode**
- 🎤 Long-running multi-source capture (mic + macOS system-audio in parallel via ScreenCaptureKit) with You/Remote-tagged transcripts
- ⚡ Streaming Whisper sliding-window transcription with live partials + final utterances
- 🗣️ Per-speaker labels — opt-in **Speakers** toggle (Settings → Meeting → Speakers) runs each utterance through a 26 MB ONNX speaker-embedding model (wespeaker ResNet34-LM) and labels transcripts as "Speaker 1, 2, …". Falls back to source-tagged You / Remote labels when off. The model auto-downloads on first enable, SHA-256 verified ([#111](https://github.com/khawkins98/Hush/issues/111))
- 🤖 Per-app classifier with user-editable overrides (Settings → Meeting tab; #112/#192)
- 📜 Searchable session history; in-app diagnostic for revoked permissions

**Library**
- 📝 SQLite-backed history with FTS5 full-text search, copy, delete, recording duration
- 📖 Personal Dictionary: vocabulary terms (Whisper prompt-bias) + literal find/replace rules
- ⚙️ Model picker — Whisper tiny → large-v3, with one-click auto-download (SHA-256 verified, host-restricted to Hugging Face) and hot-load on select

**Platform polish (macOS)**
- 🪟 Three-window architecture: main app + standalone Settings (⌘,) + transparent HUD
- ⌨️ Native macOS menu bar — Hush → Settings…, View → Dictation/Meetings/History (⌘1/⌘2/⌘3)
- 📍 Status-bar icon (Windows system tray / Linux notification area too) — Show Hush / Toggle Recording / Open Settings / Quit. Monochrome template glyph adapts to dark/light menu bars.
- 🟢 Live TCC permission detection (Microphone, Screen Recording, Input Monitoring) without triggering OS prompts; green "Permissions OK" pill on the Dictation surface when everything is granted. Settings → Permissions auto-refreshes when the user comes back from System Settings.
- ⚙️ Autostart toggle (Launch Hush at login). Launches silently into the menu bar — no main-window pop-up.
- 🪟 Closing the main / Settings windows hides them; tray stays alive. ⌘Q (or tray Quit) actually exits.
- 👋 First-run welcome that explains Microphone + Input Monitoring + Screen Recording permissions, with a "Show welcome on next launch" reset button

### Planned (v1.x)

- 🔊 Linux ([#106](https://github.com/khawkins98/Hush/issues/106)) and Windows ([#107](https://github.com/khawkins98/Hush/issues/107)) system-audio capture (macOS shipped via ScreenCaptureKit)
- 🔄 Auto-update channel via the Tauri updater plugin ([#10](https://github.com/khawkins98/Hush/issues/10)) — manual "Check for updates" ships today; the auto-channel is gated on a signing-key decision
- 🎯 Parakeet via ONNX as a second engine ([#32](https://github.com/khawkins98/Hush/issues/32))

---

## Platform support — honest version

| Platform | Status | Tested by maintainer |
|---|---|---|
| **macOS 26** | Primary target. Daily-driven. PTT is on by default (#194); the Input Monitoring prompt fires at first launch. Disable in Settings → General → Hotkeys if not wanted. | ✅ Yes |
| **macOS ≤ 15** | Not directly supported. Code may compile and run, but the maintainer does not test against older macOS, will not gate features on older-macOS APIs, and bug reports against older versions are best-effort. | ❌ Not supported |
| **Linux (X11)** | Theoretically supported. Code is cross-platform; CI builds + tests on `ubuntu-latest`. | ❌ Not hands-on tested |
| **Linux (Wayland)** | Toggle hotkey works through the desktop portal; PTT degrades gracefully (rdev requires X11). | ❌ Not hands-on tested |
| **Windows** | Built and published in the release pipeline (`.msi` + `.exe`). Code is cross-platform but the maintainer doesn't run it day-to-day; bug reports against the Windows build are best-effort. | ❌ Not hands-on tested |

**Linux and Windows hands-on contributions are welcome.** If you run Hush on either and something is broken, file an issue with steps to reproduce + your platform version. Build prerequisites are in [`CONTRIBUTING.md`](./CONTRIBUTING.md). PRs that fix platform-specific gaps are exactly the right contribution shape — small, scoped, and address a real reported bug.

The maintainer's focus is macOS 26; older macOS is explicitly out of scope, and everything else is validated only at the "compiles cleanly, unit tests pass, frontend type-checks" CI level. That's a meaningful gap from "this app actually works on your machine."

---

## Install

Pre-built binaries are published from the [GitHub Releases page](https://github.com/khawkins98/Hush/releases) — pick the latest `v*` tag, scroll to **Assets**, and download the file for your platform:

- **macOS** — `.dmg` (Apple Silicon only; macOS 26 / Tahoe is the supported target).
- **Linux** — `.AppImage` (works on any distro) or `.deb` (Debian / Ubuntu).
- **Windows** — `.msi` (recommended) or `.exe`.

### First-launch warnings

The first release wave is unsigned, so:

- **macOS** shows a Gatekeeper warning ("Hush can't be opened because Apple cannot check it for malicious software"). Right-click `Hush.app` → **Open** the first time; subsequent launches are silent.
- **Windows** shows a SmartScreen warning. Click **More info** → **Run anyway**.

Code-signing (Developer ID + notarisation on macOS, EV certificate on Windows) is on the roadmap — once those land, the warnings go away.

### Updates

Hush does **not** check for updates automatically. There is no background poll, no startup probe, no analytics ping that compares your version to anything. Auto-update is on the roadmap ([#10](https://github.com/khawkins98/Hush/issues/10)) and will ship as an opt-in.

To check manually:

- Open **Settings → About** and click **Check for updates**, or
- On macOS, **Hush → Check for Updates…** from the menu bar.

The check makes a single read-only request to `api.github.com/repos/khawkins98/Hush/releases/latest`, compares the tag to your installed version, and tells you one of: you're up to date, an update is available (with a link to the release page), or the check failed (offline / rate-limited). If an update is available, you download and install it the same way you did the first time.

---

## Quick start (development)

### Prerequisites

- Rust stable (`rustup update stable`)
- Node.js ≥ 20 (`nvm install 22`)
- **`cmake`** — required for whisper.cpp's bindings to compile. On macOS: `brew install cmake`. On Ubuntu: `sudo apt install cmake`. **The default build now includes the Whisper transcription backend, so cmake is mandatory unless you explicitly opt out (see UI-only path below).**
- Platform build deps: see [Tauri prerequisites](https://tauri.app/start/prerequisites/)

```bash
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install

# Full app with Whisper transcription (the default path; needs cmake)
npm run tauri dev

# UI-only path (no cmake needed, no transcription) for frontend
# work. The Models picker still renders but Start surfaces the
# "no transcription compiled in" error if you click it.
cd src-tauri && cargo tauri dev --no-default-features
```

---

## Testing

Hush has multiple test layers covering different regression classes:

```bash
# Rust unit tests — `whisper` is a default feature so this includes
# the whisper-gated paths. Use `--no-default-features` for the
# UI-only path on machines without cmake.
cd src-tauri && cargo test --lib

# Frontend type check
npm run check

# Frontend e2e (Playwright + mocked Tauri IPC)
npm run test:e2e
```

See [`CONTRIBUTING.md`](./CONTRIBUTING.md#testing) for the layered breakdown — what each suite catches, what it doesn't, and when to reach for which.

---

## Privacy posture

- **No audio leaves the device.** Transcription is whisper.cpp running locally; there is no cloud round-trip.
- **No telemetry, no analytics, no startup beacon.** Auto-update is not enabled — Hush does not phone home unprompted. If telemetry or auto-update ever ships, it will be opt-in with a separate privacy review.
- **Two outbound network surfaces, both user-initiated:**
  - **Whisper model download** from Hugging Face when you click Download in the model picker. The HTTP client only follows redirects originating from an HF host (`huggingface.co` / `*.hf.co`); HF's CDN can redirect to a signed object-storage URL on a third-party CDN, and that one signed-URL hop is allowed because the previous URL was on HF. HTTPS-only, hop-cap 4. SHA-256 verified on every download. Once cached, transcription is fully offline.
  - **Manual update check** when you click "Check for updates" in Settings → About (or `Hush → Check for Updates…` on macOS). Single read-only request to `api.github.com`. No identifying headers beyond the default reqwest user agent and the GitHub-recommended `Accept: application/vnd.github+json`.

---

## Documentation

| Document | Purpose |
|---|---|
| [`README.md`](./README.md) | This file — what Hush is, how to install, where to start. |
| [`hush-prd.md`](./hush-prd.md) | Product requirements doc — v1 scope, non-goals, milestone plan. |
| [`CHANGELOG.md`](./CHANGELOG.md) | Keep-a-Changelog record of what shipped. |
| [`STATUS.md`](./STATUS.md) | Point-in-time hand-off snapshot. Rots fast on purpose. |
| [`learnings.md`](./learnings.md) | Append-only engineering decision log. |
| [`CONTRIBUTING.md`](./CONTRIBUTING.md) | How to develop, test, and submit changes. |
| [`SECURITY.md`](./SECURITY.md) | Vulnerability reporting policy. |
| [`CODE_OF_CONDUCT.md`](./CODE_OF_CONDUCT.md) | Community standards. |
| [`docs/macos-permissions.md`](./docs/macos-permissions.md) | Troubleshooting macOS Microphone + Input Monitoring on dev builds. |

---

## Acknowledgements

Hush is inspired by [VoiceInk](https://github.com/Beingpax/VoiceInk) by [Pax](https://github.com/Beingpax), a fantastic macOS-native dictation app. Hush reimplements the same product concept for cross-platform use. No VoiceInk source code was copied or referenced at any point during development. Design was derived from VoiceInk's public README and observable runtime behaviour.

---

## License

[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).
