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
- Cross-platform audio capture via `cpal`, behind an `AudioCapture`
  trait so OS-touching code can be mocked at the test seam. Captures
  at the device's native format and surfaces it alongside the
  samples; downmix and 16 kHz resampling happen at the transcription
  stage where format-mismatches are recoverable.
- Local Whisper transcription via `whisper-rs`, behind a `Transcribe`
  trait at the heavy-dep boundary. Gated behind the `whisper` Cargo
  feature because whisper.cpp needs cmake. Pure-logic linear
  resampler converts any captured sample rate to the 16 kHz mono
  Whisper expects before inference. Constructor takes a caller-
  provided GGUF model path; auto-download lands in #30.
- Three Tauri commands wire the dictation pipeline end-to-end:
  `list_input_devices`, `start_dictation`, `stop_dictation`. The stop
  command captures the foreground app at recording start (via
  `active-win-pos-rs`), writes the transcript to the system
  clipboard, and fires a "Ready to paste" notification. Errors are a
  tagged enum (`{ kind, message? }`) so the frontend dispatches
  recovery copy on `kind` rather than parsing free-form strings.
- Minimal Svelte dictation UI replaces the Tauri starter's "greet"
  placeholder. M2 ships button-driven recording first; the hotkey
  layer adds keyboard control on top.
- Toggle-record global hotkey via `tauri-plugin-global-shortcut`,
  default `CmdOrCtrl+Shift+Space` (overridable via
  `HUSH_TOGGLE_HOTKEY`). The handler emits a `hotkey:toggle` event
  and the frontend dispatches start vs. stop against its existing
  `recording` flag, keeping one source of truth for UI state.
- SQLite persistence via `sqlx`, wrapped in a `SqliteDatabase` that
  opens the database at a caller-provided path with WAL journal
  mode, `synchronous=NORMAL`, and per-connection foreign-key
  enforcement, then runs the embedded migrations from
  `src-tauri/migrations/`. An `open_in_memory` helper backs tests
  that need a real SQLite without touching the filesystem.
- Push-to-talk global hotkey via `rdev`, default `RightControl`
  (overridable via `HUSH_PTT_HOTKEY`). A dedicated thread runs the
  blocking `listen` loop and forwards key-down / key-up as
  `hotkey:ptt-press` / `hotkey:ptt-release` events. Closes the PTT
  half of #5. macOS prompts for Input Monitoring on first press;
  Linux requires X11 (Wayland support is compositor-dependent per
  PRD §10).
- History persistence: every successful transcription auto-inserts
  into a SQLite-backed history table via the `HistoryRepository`
  trait (sharing the sqlx pool). Tauri commands (`history_list`,
  `history_search`, `history_delete`, `history_count`) back a
  frontend history view with debounced FTS5 search, newest-first
  ordering, and per-row copy / delete. The `Transcribe` trait gained
  a `model_label()` so each row records which model produced its
  transcript.
- Post-transcription find/replace pipeline: pure-logic
  `apply_replacements()` plus a SQLite-backed `ReplacementRepository`.
  Rules are literal substrings, applied in `(sort_order, id)` order
  before the text reaches the clipboard. Tauri commands
  (`replacements_list`, `_create`, `_update`, `_delete`) back a
  frontend "Replacements" panel.
- Vocabulary prompt-biasing: user-managed terms are joined into the
  initial prompt Whisper's decoder sees, biasing recognition toward
  proper nouns and jargon. Backed by `VocabularyRepository` and four
  new IPC commands. The `Transcribe` trait gained a default-impl
  `transcribe_with_prompt` so non-Whisper backends can ignore the
  prompt without forcing every callsite to branch. Closes #6.
- Generic key-value settings persistence: `SettingsRepository` trait +
  SQLite impl backing the `settings` table. First consumer: the
  model picker's `selected_model_id`.
- Whisper model picker: static catalog of
  the five Whisper variants (tiny / base / small / medium / large-v3)
  with size, speed/accuracy ratings, and descriptions. Frontend
  card-grid section adopts the layout the user shared as the design
  reference (per-card name + size + bar-rated speed/accuracy +
  description + Default badge on the active card). Two new IPC
  commands: `model_list` and `model_select`. The transcriber
  resolution at startup now reads `selected_model_id` from settings
  and looks for the file in `<app-data>/models/<filename>`; falls
  back to the legacy `HUSH_MODEL_PATH` env var for the existing dev
  workflow. Hot-swap is intentionally not in this PR — selecting a
  new model writes the setting and prompts the user to restart.
  Auto-download is a follow-up.

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
