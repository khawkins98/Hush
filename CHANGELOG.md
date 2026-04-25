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
- IPC layer (`ipc` module): three Tauri commands —
  `list_input_devices`, `start_dictation`, `stop_dictation` — wiring the
  audio capture and Whisper transcription pipelines together. Captures the
  foreground app at recording start via `active-win-pos-rs`, writes the
  transcribed text to the system clipboard, and fires a "Ready to paste"
  notification on stop. Production transcriber loaded from
  `HUSH_MODEL_PATH` (M1/M2 spike; replaced by the model picker in M3).
  Tagged-enum error type so the frontend can dispatch on `kind` instead of
  parsing free-form strings.
- Dictation UI (`src/routes/+page.svelte`): minimal device dropdown +
  start/stop buttons + result display, replacing the Tauri starter
  template's "greet" placeholder. Drives the M2 end-to-end loop from a
  button rather than a hotkey (hotkey lands in #5).
- Toggle-record global hotkey (`hotkey` module): registers
  `CmdOrCtrl+Shift+Space` (overridable via `HUSH_TOGGLE_HOTKEY`) on
  startup and emits a `hotkey:toggle` event to the frontend on each
  press. The frontend dispatches start vs. stop against its existing
  `recording` flag, keeping a single source of truth for UI state and
  one orchestration path for the pipeline. Push-to-talk via `rdev` is
  the open second half of #5.
- SQLite persistence (`db` module): `SqliteDatabase` wrapper around
  `sqlx::SqlitePool` that opens the database at a caller-provided
  path, creates the parent directory if missing, sets WAL journal
  mode + `synchronous=NORMAL` + foreign-key enforcement, and runs the
  embedded migrations from `src-tauri/migrations/` via
  `sqlx::migrate!`. Plus an `open_in_memory` helper for tests that
  need a real SQLite without touching the filesystem. Not yet wired
  into `AppState` — that lands with the first downstream consumer
  (#7 history or #6 dictionary).
- Push-to-talk global hotkey (`hotkey::ptt`): an `rdev`-based listener
  on a dedicated thread emits `hotkey:ptt-press` and `hotkey:ptt-release`
  events to the frontend on key-down and key-up of the configured key.
  Default is `RightControl` (overridable via `HUSH_PTT_HOTKEY`; accepts
  modifier keys, F1–F12, and CapsLock with case-insensitive aliases).
  Frontend starts dictation on press and stops on release, sharing the
  existing `recording`/`busy` source of truth with the toggle hotkey.
  Closes the PTT half of #5. Caveats: macOS first-run prompt for Input
  Monitoring; Linux requires X11 (Wayland support is compositor-dependent
  and out of scope for this release per PRD §10).

- History persistence (`history` module): each successful transcription
  is auto-inserted into a SQLite-backed history table via the
  `HistoryRepository` trait (sqlx pool from #18). New Tauri commands
  (`history_list`, `history_search`, `history_delete`, `history_count`)
  back a frontend history view with debounced FTS5 search, per-row copy
  / delete, and newest-first ordering. The `Transcribe` trait gained a
  `model_label()` method so the row records which model produced each
  transcript (whisper-rs returns the GGUF file's basename).

### Changed

- **M2 polish.** Visible recording and transcribing states (pulsing red
  dot + status text + window-title indicator), spinner during the
  Whisper inference window, and an in-app shortcuts hint card so the
  default hotkeys are discoverable without reading the README.
- **Friendlier error copy.** IPC errors are now mapped to recovery-
  oriented strings in the frontend rather than shown as raw `kind:
  message` pairs. The `transcription-unavailable` case in particular
  gives an actionable hint about `HUSH_MODEL_PATH` and the `whisper`
  feature.
- **Empty input-device list** now surfaces a platform-aware
  troubleshooting hint instead of silently disabling the start button.
- **Dark-mode error contrast** raised so the warning text passes WCAG
  AA on a dark background (was `#ffa0a0` on `#3a1a1a`, flagged as
  borderline by the UX review).
- `prefers-reduced-motion` honoured by the new pulse / spin animations.

### Fixed

- IPC `start_dictation` no longer overwrites the foreground-app slot
  when the underlying audio backend fails to start. Previously a
  failed start could leave a stale foreground snapshot visible to a
  subsequent `stop_dictation` call.
- IPC `stop_dictation` no longer routes errors via substring matching
  on a merged anyhow message (which could send a Whisper error
  mentioning "device" to the `audio` variant). Audio and
  transcription failures are now classified structurally at the call
  site.
- Internal mutex acquisition uses `?` with a typed
  `IpcError::Internal` variant instead of `.expect("…mutex")`, so a
  poisoned lock no longer panics a Tauri command (which can
  destabilise the renderer).

---

*First entry: Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk). No source code copied or referenced.*
