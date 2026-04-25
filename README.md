# Hush

**Offline voice-to-text for macOS, Windows, and Linux.**

Hush records your voice, transcribes it locally using [whisper.cpp](https://github.com/ggerganov/whisper.cpp) (via `whisper-rs`), and places the text on your clipboard — ready to paste wherever you need it. Transcription happens on-device — no audio leaves your machine. No telemetry by default. The only network traffic is the one-time download of the Whisper model from [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp) the first time you pick one; after that, transcription is fully offline.

> **Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk).** No source code was copied or referenced. See [Acknowledgements](#acknowledgements).

---

## Status

🚧 **Active development — usable on macOS for early testers.** M3 persistence shipped: history search, settings, model picker with auto-download, replacements, vocabulary, first-run welcome, recording HUD. Auto-update and direct-text-insertion are deferred. See [STATUS.md](./STATUS.md) for the latest snapshot.

---

## Features (v1 roadmap)

- 🎙️ Push-to-talk and toggle-record global hotkeys
- 🤫 100 % local transcription — whisper.cpp on your machine; no audio ever leaves the device. (One-time model download from Hugging Face on first use; offline after that.)
- 📋 Transcription written to clipboard with a "Ready to paste" notification
- 📝 History view with search, copy, delete, and CSV export
- 📖 Personal Dictionary: custom vocabulary + find/replace pairs
- ⚙️ Model picker: tiny → large-v3, with download progress
- 🔄 Auto-update channel

---

## Quick start (development)

### Prerequisites

- Rust stable (`rustup update stable`)
- Node.js ≥ 20
- Platform build deps: see [Tauri prerequisites](https://tauri.app/start/prerequisites/)

```bash
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install
npm run tauri dev
```

---

## Acknowledgements

Hush is inspired by [VoiceInk](https://github.com/Beingpax/VoiceInk) by [Pax](https://github.com/Beingpax), a fantastic macOS-native dictation app. Hush reimplements the same product concept for cross-platform use. No VoiceInk source code was copied or referenced at any point during development. Design was derived from VoiceInk's public README and observable runtime behaviour.

---

## License

Apache-2.0 (pending final licence decision before first public release — see §13.8 of the PRD).
