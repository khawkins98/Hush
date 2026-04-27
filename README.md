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
- 🎙️ **Configurable push-to-talk** — pick any combination of modifier / function / Caps Lock keys held simultaneously. Default is `Right ⌘` on macOS, `Right Ctrl` elsewhere. Edit in Settings → General → Hotkeys; combo + Enabled persist across launches.
- 🤫 100% local transcription — whisper.cpp on your machine; no audio ever leaves the device
- 📋 Transcription written to clipboard with a "Ready to paste" notification
- 🔴 Recording HUD overlay — transparent always-on-top pill with pulsing dot + live RMS level meter, draggable, with a dismiss button that hides without stopping the recording

**Meeting Mode**
- 🎤 Long-running multi-source capture (mic + macOS system-audio in parallel via ScreenCaptureKit) with You/Remote-tagged transcripts
- ⚡ Streaming Whisper sliding-window transcription with live partials + final utterances
- 📜 Searchable session history; in-app diagnostic for revoked permissions

**Library**
- 📝 SQLite-backed history with FTS5 full-text search, copy, delete, recording duration
- 📖 Personal Dictionary: vocabulary terms (Whisper prompt-bias) + literal find/replace rules
- ⚙️ Model picker — Whisper tiny → large-v3, with one-click auto-download (SHA-256 verified, host-restricted to Hugging Face) and hot-load on select

**Platform polish (macOS)**
- 🪟 Three-window architecture: main app + standalone Settings (⌘,) + transparent HUD
- ⌨️ Native macOS menu bar — Hush → Settings…, View → Dictation/Meetings/History (⌘1/⌘2/⌘3)
- 🟢 Live TCC permission detection (Microphone, Screen Recording, Input Monitoring) without triggering OS prompts; green "Permissions OK" pill on the Dictation surface when everything is granted
- ⚙️ Autostart toggle (Launch Hush at login)
- 👋 First-run welcome that explains Microphone + Input Monitoring permissions, with a "Show welcome on next launch" reset button

### Planned (v1.x)

- 🔊 Linux ([#106](https://github.com/khawkins98/Hush/issues/106)) and Windows ([#107](https://github.com/khawkins98/Hush/issues/107)) system-audio capture (macOS shipped via ScreenCaptureKit)
- 🗣️ Speaker diarization for meetings ([#111](https://github.com/khawkins98/Hush/issues/111))
- 🤖 Per-app classifier for auto-detecting meeting context ([#112](https://github.com/khawkins98/Hush/issues/112))
- 🔄 Auto-update channel via the Tauri updater plugin ([#10](https://github.com/khawkins98/Hush/issues/10))
- 🎯 Parakeet via ONNX as a second engine ([#32](https://github.com/khawkins98/Hush/issues/32))

---

## Platform support — honest version

| Platform | Status | Tested by maintainer |
|---|---|---|
| **macOS 26** | Primary target. Daily-driven. PTT is opt-in via Settings → General → Hotkeys (it triggers the OS Input Monitoring permission prompt; we don't fire that on first launch). | ✅ Yes |
| **macOS ≤ 15** | Not directly supported. Code may compile and run, but the maintainer does not test against older macOS, will not gate features on older-macOS APIs, and bug reports against older versions are best-effort. | ❌ Not supported |
| **Linux (X11)** | Theoretically supported. Code is cross-platform; CI builds + tests on `ubuntu-latest`. | ❌ Not hands-on tested |
| **Linux (Wayland)** | Toggle hotkey works through the desktop portal; PTT degrades gracefully (rdev requires X11). | ❌ Not hands-on tested |
| **Windows** | Theoretically supported. Was in the original CI matrix but dropped to keep CI fast (PRD §11 — Windows distribution lands at M6). | ❌ Not hands-on tested |

**Linux and Windows hands-on contributions are welcome.** If you run Hush on either and something is broken, file an issue with steps to reproduce + your platform version. Build prerequisites are in [`CONTRIBUTING.md`](./CONTRIBUTING.md). PRs that fix platform-specific gaps are exactly the right contribution shape — small, scoped, and address a real reported bug.

The maintainer's focus is macOS 26; older macOS is explicitly out of scope, and everything else is validated only at the "compiles cleanly, unit tests pass, frontend type-checks" CI level. That's a meaningful gap from "this app actually works on your machine."

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
npm run tauri:ui-only
```

For full setup including model placement, see the testing guide in [`STATUS.md`](./STATUS.md) §b.

---

## Testing

Hush has multiple test layers covering different regression classes:

```bash
# Rust unit tests (fast, no whisper feature needed)
cd src-tauri && cargo test --lib

# Same plus the whisper-gated paths (needs cmake)
cargo test --lib --features whisper

# Frontend type check
npm run check

# Frontend e2e (Playwright + mocked Tauri IPC)
npm run test:e2e
```

See [`CONTRIBUTING.md`](./CONTRIBUTING.md#testing) for the layered breakdown — what each suite catches, what it doesn't, and when to reach for which.

---

## Privacy posture

- **No audio leaves the device.** Transcription is whisper.cpp running locally; there is no cloud round-trip.
- **No telemetry.** The updater plugin is currently stubbed. If telemetry is ever added it will be opt-in with a separate privacy review.
- **One outbound network surface:** the Whisper model download from Hugging Face when you click Download in the model picker. The HTTP client redirects only within `huggingface.co` (host-restricted, hop-cap 4) and verifies SHA-256 on every download. Once the model is cached locally, transcription is fully offline.

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

Apache-2.0 (pending final licence decision before first public release — see §13.8 of the PRD).
