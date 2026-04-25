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

---

*First entry: Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk). No source code copied or referenced.*
