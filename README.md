# Hush

**Offline voice-to-text for macOS, Windows, and Linux.**

Hush records your voice, transcribes it locally using [whisper.cpp](https://github.com/ggerganov/whisper.cpp) (via `whisper-rs`), and places the text on your clipboard — ready to paste wherever you need it. Transcription happens on-device — no audio leaves your machine. No telemetry by default. The only network traffic is the one-time download of the Whisper model from [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp) the first time you pick one; after that, transcription is fully offline.

> **Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk).** No source code was copied or referenced. See [Acknowledgements](#acknowledgements).

---

## Status

🚧 **Active development — usable on macOS for early testers.** M3 persistence shipped: history search, settings, model picker with auto-download, replacements, vocabulary, first-run welcome, recording HUD with live level meter. Auto-update and direct-text-insertion are deferred. See [STATUS.md](./STATUS.md) for the latest snapshot.

---

## Features

### Shipped

- 🎙️ Push-to-talk (`RightControl` by default) and toggle-record (`⌘/Ctrl+Shift+Space`) global hotkeys
- 🤫 100 % local transcription — whisper.cpp on your machine; no audio ever leaves the device
- 📋 Transcription written to clipboard with a "Ready to paste" notification
- 🔴 Recording HUD overlay — borderless transparent always-on-top window with a pulsing dot and a live RMS-driven level meter
- 📝 SQLite-backed history with FTS5 full-text search, copy, delete
- 📖 Personal Dictionary: vocabulary terms (Whisper prompt-bias) + literal find/replace rules
- ⚙️ Model picker: Whisper tiny → large-v3, with one-click auto-download and SHA-256 verification
- 👋 macOS first-run welcome that explains Microphone + Input Monitoring permissions

### Planned (v1)

- 🔄 Auto-update channel via the Tauri updater plugin (#10)
- 🎯 Parakeet via ONNX as a second engine (#32)
- 🔊 System-audio loopback capture (#33)

---

## Quick start (development)

### Prerequisites

- Rust stable (`rustup update stable`)
- Node.js ≥ 20 (`nvm install 22`)
- `cmake` (whisper.cpp's bindings need it; macOS: `brew install cmake`)
- Platform build deps: see [Tauri prerequisites](https://tauri.app/start/prerequisites/)

```bash
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install

# UI shell (no transcription — useful for frontend work)
npm run tauri dev

# Full app with whisper transcription
cd src-tauri && cargo tauri dev --features whisper
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

---

## Acknowledgements

Hush is inspired by [VoiceInk](https://github.com/Beingpax/VoiceInk) by [Pax](https://github.com/Beingpax), a fantastic macOS-native dictation app. Hush reimplements the same product concept for cross-platform use. No VoiceInk source code was copied or referenced at any point during development. Design was derived from VoiceInk's public README and observable runtime behaviour.

---

## License

Apache-2.0 (pending final licence decision before first public release — see §13.8 of the PRD).
