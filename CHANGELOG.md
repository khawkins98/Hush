# Changelog

All notable changes to Hush will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial project scaffold: Tauri 2 + Svelte + TypeScript frontend, Rust backend.
- Rust module stubs: audio, transcription, hotkey, dictionary, history, db, ipc, updater.
- SQLite schema with FTS5 history index (migration 0001).
- Repository meta-files: README, CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, learnings.md.
- CI workflow: cargo clippy, rustfmt check, cargo test on every push and PR.
- GitHub PR template and bug/feature issue templates.
- Audio capture (`audio` module): cross-platform input via `cpal` behind an
  `AudioCapture` trait so OS-touching code can be mocked at the test seam.
  Surfaces input-device enumeration, start/stop session, and a channel
  downmix utility. Captures at the device's native format and surfaces the
  format alongside the samples; downmix and resampling to whisper's 16 kHz
  happen at the transcription stage.
- Local Whisper transcription (`transcription` module): `Transcribe` trait
  at the OS / heavy-dep boundary, plus a `whisper-rs` backed implementation
  gated behind the `whisper` Cargo feature. Includes a pure-logic linear
  resampler (`resample_to_mono`) so any captured sample rate is converted
  to whisper's 16 kHz before inference. Constructor takes a caller-provided
  GGUF model path; auto-download is deferred to M3.

---

*First entry: Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk). No source code copied or referenced.*
