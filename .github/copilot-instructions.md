# Copilot Instructions — Hush

**Primary references:** [`CLAUDE.md`](../CLAUDE.md) (commands, gotchas, IPC rules, macOS TCC quirks) and [`ARCHITECTURE.md`](../ARCHITECTURE.md) (full stack diagram, module map, meeting-pump dataflow). Read both before non-trivial cross-module changes.

---

## Stack

Tauri 2 desktop app — Rust backend (`src-tauri/`) + SvelteKit / Svelte 5 frontend (`src/`).

- Frontend uses **Svelte 5 runes** (`$state`, `$derived`, `$effect`, `$props()`).
- IPC uses Tauri commands with `serde(rename_all = "camelCase")` — Rust snake_case serialises to camelCase on the wire.
- Persistence: SQLite via `sqlx` with FTS5; migrations in `src-tauri/migrations/`.
- Transcription: `whisper-rs` (whisper.cpp). Diarization: `tract-onnx` (wespeaker ResNet34-LM).
- **Primary target: macOS 26+ on Apple Silicon.** Linux/Windows compile in CI but are not hands-on tested.

---

## Commands

```bash
# Normal dev loop
npm run tauri dev

# UI-only (skips whisper/diarizer compile, fast startup)
cd src-tauri && cargo tauri dev --no-default-features

# TCC permission testing — builds a signed .app to ~/Applications/Hush.app
npm run tauri:bundle

# Rust unit tests
cd src-tauri && cargo test --lib
cd src-tauri && cargo test --lib --features whisper
cd src-tauri && cargo test --lib --features diarization-onnx

# Run a single Rust test
cd src-tauri && cargo test --lib audio::tests::name_of_test
cd src-tauri && cargo test --lib meeting::   # entire module

# Frontend type check (required clean before every PR)
npm run check

# Frontend e2e (Playwright + mocked IPC)
npm run test:e2e
npm run test:e2e:ui                          # interactive
npx playwright test tests/e2e/meeting-panel.spec.ts  # single spec

# Lint + format
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --all

# Cross-platform lint (run before any Rust PR — catches cfg-gated errors)
cd src-tauri && cargo clippy --lib --no-default-features -- -D warnings
```

---

## Architecture

### Windows

Four windows, each with its own capability file in `src-tauri/capabilities/`:

| Window | Route | Purpose |
|---|---|---|
| `main` | `/` | Sidebar nav (Dictation · History · Settings · About). Settings is an inline Svelte panel, not a separate window (since #479). |
| `hud` | `/hud` | Transparent always-on-top pill showing recording state + level meter. |
| `menu-bar` | `/menu-bar` | Compact popover from the macOS menu-bar icon. |
| `debug` | `/debug` | Dev-only floating log console. |

`main` hides instead of closing (`WindowEvent::CloseRequested`); `⌘Q` / tray Quit actually exits. Background launch (autostart) hides the main window and sets `Accessory` activation policy.

### Trait-seam pattern

Every OS-touching layer is a `trait` with a prod impl + hand-rolled mocks. `AppState` (composition root in `ipc/`) holds `Arc<dyn Trait>` so tests inject deterministic stubs.

Key seams:

| Trait | Prod impl | Notes |
|---|---|---|
| `audio::AudioCapture` | `CpalAudioCapture` | Two APIs: singleton (dictation) + handle-based (meeting pump). System audio via a Swift helper binary (`resources/macos-audio-tap.swift`), CoreAudio process tap. |
| `transcription::Transcribe` | `WhisperTranscribe` | **Two independent slots** in `AppState` — `transcribe` (dictation) and `transcribe_meeting` — to avoid mutex contention (#248). |
| `diarization::Diarize` | `FlagGatedDiarizer` → `OnnxDiarizer` | Online 1-NN streaming matcher, threshold 0.4. Pure `tract-onnx` — never reintroduce ORT (causes unbounded IOAccelerator growth on Apple Silicon, #641). |
| `history::HistoryRepository` | `SqliteHistoryRepository` | `Mem*` mocks for tests. |

Hot-swappable slots allow model/setting changes without restart: `TranscribeSlot` (`Arc<Mutex<Option<Arc<dyn Transcribe>>>>`), `DiarizeSlot` (`Arc<RwLock<Arc<dyn Diarize>>>`), `inference_threads` (`Arc<AtomicI32>`).

### Meeting pump

Runs as a tokio task. Every `CHUNK_DURATION` (10 s) it drains both the mic and system-audio handles, runs Whisper (`spawn_blocking`), passes utterances to `Diarize::label_utts`, emits an IPC event, and persists utterance rows. Session state is `Idle | Opening | Active(...)` behind a `Mutex`. Audio is held in RAM only (`AudioRollingBuffer`, ~30 s ring); never written to disk.

---

## Critical conventions

### The four-place IPC sync rule

Adding or changing a `#[tauri::command]` touches **four** places — CI can't catch shape mismatches between them:

1. **Rust struct + handler** in `src-tauri/src/ipc/commands/mod.rs` (or a domain submodule), with `#[serde(rename_all = "camelCase")]`.
2. **Register** in `src-tauri/src/lib.rs` inside `tauri::generate_handler![...]` using the **full module path** — `ipc::commands::my_command` or `ipc::commands::meeting::meeting_start_manual`. `pub use` re-exports do **not** carry the `__cmd__<name>` symbol (see `learnings.md` 2026-04-25).
3. **TypeScript type** in `src/lib/types.ts` (shared) or inline in the page, with `invoke<MyResult>("my_command", ...)`.
4. **Playwright mock** in `tests/e2e/_mock.ts` — field shape must mirror the Rust struct exactly. Mocks are serialised via `toString()` so they cannot capture closure variables; use `page.exposeFunction` for per-test counters.

A new `IpcError` variant also needs a case in `src/lib/errors.ts::formatErrorDisplay`.

Settings-window IPCs that call Tauri plugins need an explicit entry in `src-tauri/capabilities/settings.json`; custom `#[tauri::command]` functions don't.

### Dev-launch smoke test

CI does not run a real Tauri runtime. Run `npm run tauri dev` before opening a PR that touches `lib.rs`, `tauri.conf.json`, `Cargo.toml` (adding/removing plugins or unconditional deps), `capabilities/*.json`, `app_menu/`, or `settings_window/`.

### macOS TCC testing

`npm run tauri dev` produces an unsigned binary — TCC attributes it to the parent terminal. For Microphone / Screen Recording / Input Monitoring permission flows, use `npm run tauri:bundle` (builds a signed `.app`, installs to `~/Applications/Hush.app`). See `docs/macos-permissions.md` for troubleshooting.

### Svelte 5 state

Shared reactive state lives in `src/lib/state/*.svelte.ts` (runes-based stores). The main window owns `nav.svelte.ts`, `dictation.svelte.ts`, `history.svelte.ts`, `meeting-sessions.svelte.ts`, and `audio.svelte.ts`. Settings state is managed inline by the `SettingsPanel` component — not from `routes/+page.svelte`.

### Destructive UI actions

Use the click-to-confirm pattern (first click → "Are you sure?" state, auto-resets after ~5 s, second click commits) — no `window.confirm`. Track pending state with a `pendingConfirmId` `$state` + `setTimeout`.

### Logging

Use `tracing::debug!` (one-per-event) and `tracing::trace!` (per-poll) with named structured fields:
```rust
// ✓
tracing::debug!(raw_segments, non_empty_segments, window_ms, "inference ran");
```
Three sinks: stderr fmt, daily-rolling file (`~/Library/Logs/io.github.khawkins98.hush/hush.log.<date>`, disable with `HUSH_LOG_FILE=off`), and in-app `DebugLogLayer` ring.

### Commits and branches

- **Conventional Commits 1.0.0:** `<type>(<scope>): <subject>` — imperative mood, no full stop, ≤72 chars.
- **Types:** `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `style`, `perf`, `build`, `ci`, `security`.
- **Scopes:** `audio`, `transcription`, `hotkey`, `ui`, `ux`, `dictionary`, `history`, `db`, `ipc`, `tauri`, `updater`, `build`, `e2e`.
- **Branch names:** `<type>/<short-kebab-description>`. All changes land via squash-merge PR.
- **Untagged TODOs fail CI.** Use `// TODO(#NNN):` or `// FIXME(#NNN):`.

### Supply-chain / dependency policy

- `rdev` is pinned to [fufesou's fork](https://github.com/fufesou/rdev) — do not bump to upstream.
- Never reintroduce `ort`/`onnxruntime` for diarization (use `tract-onnx` only, #641).
- CI's `supply-chain-pins` job blocks new RC pins or git deps not on the allowlist (#327). See `learnings.md` "Supply-chain pins" before running `cargo update -p rdev`.

### Black-box reimplementation discipline

VoiceInk's source code must **never** be read by anyone working on Hush. Design comes only from VoiceInk's public README and observable runtime behaviour. If the discipline is broken, declare it immediately. See `CONTRIBUTING.md` and `hush-prd.md` §13.8.

---

## Key gotchas

- **`pub use` + `tauri::generate_handler![]`** — always use the full module path, not a re-export (see `learnings.md` 2026-04-25).
- **`whisper.cpp` WhisperState lifecycle** — the `Option<WhisperState>` in `transcription/whisper.rs` is lazily created, dropped on `state.full` Err, and periodically recreated every 30 inferences. Preserve the triplet when editing `WhisperInferer::infer`.
- **`tauri-plugin-single-instance` registration order** — must be first in the `tauri::Builder` chain (#326).
- **Tray icon** — must use `tray-icon@2x.png` (monochrome alpha silhouette) with `icon_as_template(true)`. Using the full-colour app icon produces a black blob (#275).
- **`cargo test --lib` Swift dylib error** — if you get a missing `libSwift_Concurrency` error: `DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx cargo test --lib`.
- **`learnings.md`** — append-only engineering decision log. Add an entry for any non-obvious architectural decision. Read it before re-deriving past choices.
