# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project shape

Hush is a Tauri 2 desktop app: Rust backend (`src-tauri/`) + SvelteKit / Svelte 5 frontend (`src/`). It records the microphone, transcribes locally via whisper.cpp (`whisper-rs`), and writes the text to the clipboard. Meeting Mode adds continuous capture from microphone + system audio with You/Remote-tagged transcripts.

Primary target: **macOS 26 only.** macOS 15 and older are explicitly out of scope — don't add backwards-compat shims or `@available`-style version guards. Linux and Windows compile cleanly via CI but are not hands-on tested.

### Three Tauri windows

Post-IA-redesign (#163–#167), the app runs three windows:

- **`main`** — the primary window. Sidebar nav with Dictation / Meetings / History sections. Loads `/`.
- **`settings`** — standalone Settings window opened via ⌘, on macOS or the sidebar's "Settings" footer button. Hosts the model picker, vocabulary, replacements, macOS permissions diagnostic, autostart toggle, hotkey display, and the PTT editor. Hidden by default; the only path to show is `crate::settings_window::show()`. Loads `/settings`.
- **`hud`** — borderless transparent always-on-top pill that shows during recording. No interactivity beyond drag + dismiss. Loads `/hud`.

Each window has its own capability file in `src-tauri/capabilities/` (`default.json` for main, `settings.json`, `hud.json`). On macOS the native menu bar lives in `src-tauri/src/app_menu/` — `Hush → Settings…` (⌘,), `View → Dictation/Meetings/History` (⌘1/⌘2/⌘3). Menu events emit `menu:goto-section` to the main window or call `settings_window::show` directly.

**Window lifecycle (#280):** main + Settings intercept `WindowEvent::CloseRequested` in `lib.rs::setup` and call `window.hide()` instead of letting Tauri destroy. Closing the red ✕ on either window hides it without quitting; the tray icon stays alive and ⌘, reopens Settings. The HUD has the standard show/hide pair. To actually quit Hush, use ⌘Q (macOS), `Hush → Quit Hush` from the menu, or `Quit Hush` from the tray.

**Background launch (#280):** the autostart plugin is registered with `Some(vec!["--background"])`, so the LaunchAgent fires Hush at login with that arg. The setup hook checks `std::env::args()` and, when `--background` is present, hides the main window and switches activation policy to `Accessory` (no Dock icon). User-initiated launches (Finder / Spotlight) don't pass the flag and show the main window normally.

## Common commands

```bash
# Run the full app. Default features are `whisper` (needs cmake on
# macOS) + `diarization-onnx` (pulls in the ~50 MB ORT vendored libs
# at build time via `ort`'s `download-binaries` feature; needs
# network during the first build to fetch them). ScreenCaptureKit
# is linked unconditionally on macOS, so system-audio capture works
# without an extra feature flag.
npm run tauri dev

# UI-only path: launches the app shell with no Whisper backend
# and no ONNX diarizer. Transcription returns
# IpcError::TranscriptionUnavailable; meetings get NoopDiarizer.
cd src-tauri && cargo tauri dev --no-default-features

# Diarizer-only (no whisper): rare but useful for iterating on the
# diarization stack without paying whisper.cpp compile cost.
cd src-tauri && cargo tauri dev --no-default-features --features diarization-onnx

# macOS-only: build a debug .app bundle and open it. Use this for
# smoke-testing anything that depends on macOS treating Hush as a
# proper app — Screen Recording / Microphone TCC prompts in
# particular. The bare `cargo tauri dev` binary has no .app wrapper
# and doesn't register reliably with TCC (see "macOS TCC dev-binary
# quirk" below). Slow: 30 s – 2 min, not a hot-iteration tool.
npm run tauri:bundle

# Rust unit tests — fast, no real audio device needed.
cd src-tauri && cargo test --lib
cd src-tauri && cargo test --lib --features whisper             # plus whisper-gated paths
cd src-tauri && cargo test --lib --features diarization-onnx    # plus diarizer-gated paths

# Run a single Rust test or module
cd src-tauri && cargo test --lib audio::tests::name_of_test
cd src-tauri && cargo test --lib meeting::                       # whole module

# Integration tests (#[ignore]'d by default, need external resources)
cd src-tauri && HUSH_TEST_AUDIO=/path/to/sample.wav cargo test --features whisper -- --ignored

# Frontend type check (svelte-check) — required clean for every PR
npm run check

# Frontend e2e — Path A (Playwright + mocked Tauri IPC)
npm run test:e2e
npm run test:e2e:ui                                              # interactive

# Run a single Path A spec
npx playwright test tests/e2e/meeting-panel.spec.ts

# Frontend e2e — Path B (tauri-driver + WebdriverIO, real binary)
# Prereq: `cargo install tauri-driver --locked` and a debug build:
#   npm run tauri build -- --debug
# See `tests/e2e-tauri/README.md` for full setup. CI integration is
# deferred until tauri-driver's macOS support stabilises.
npm run test:e2e:tauri

# Reset stale dev servers (kills tauri/vite processes)
npm run dev-cleanup

# Lint + format
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --all
```

ScreenCaptureKit is now an unconditional macOS dependency (no feature flag). The crate's build script links libSwift_Concurrency at runtime. On a dev machine where the rpaths the build script bakes in (`/usr/lib/swift`, `/Library/Developer/CommandLineTools/...swift-5.5/macosx`) don't resolve, `cargo test --lib` aborts with a missing-dylib error. Workaround: `DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx cargo test --lib`. Production app bundles inherit the Swift runtime from the dyld shared cache and need no override; CI on `macos-latest` has the CommandLineTools path populated and doesn't either.

## Architecture: trait-seam pattern

Every OS-touching layer is a trait, with a concrete impl + hand-rolled mocks at the boundary. The IPC layer holds `Arc<dyn Trait>` so tests can substitute deterministic stubs without spinning up real audio / sqlite / network. The traits are the load-bearing seams:

- **`audio::AudioCapture`** (`src-tauri/src/audio/mod.rs`) — capture lifecycle. Cpal-backed `CpalAudioCapture` is the prod impl. Two APIs:
  - Singleton `start_with_source(source) -> ()` + `stop() -> CapturedAudio` is the dictation hot path.
  - Handle-based `start_session(source) -> Box<dyn AudioSession>` returns a per-session handle. The meeting pump uses it to capture mic + SCK system-audio in parallel. Each handle's `stop()` consumes `Box<Self>` so a double-stop is a compile error.
  - `active_sessions: AtomicU32` is a refcount of in-flight captures; `is_recording()` returns `count > 0` so legacy + handle paths coexist. `MAX_BUFFER_FRAMES` defends against runaway buffer growth in callbacks.
- **`transcription::Transcribe`** (`src-tauri/src/transcription/mod.rs`) — inference. `WhisperTranscribe` is gated behind the `whisper` feature; the trait's `transcribe_chunks` default impl pretends to be streaming (one final utterance per call). #108 (sliding-window streaming) replaces that default for the meeting pump.
- **`history::HistoryRepository`** / **`meeting::MeetingSessionRepository`** / **`dictionary::*Repository`** / **`settings::SettingsRepository`** — persistence. Each has a `Sqlite*` impl + an in-memory test mock (search for `Noop*` / `Mem*` in `src-tauri/src/ipc/mod.rs` tests).

The IPC layer (`src-tauri/src/ipc/`) wires these into `AppState`, which is `manage`'d by Tauri at startup. `TranscribeSlot = Arc<Mutex<Option<Arc<dyn Transcribe>>>>` is shared between `AppState` and `meeting::SessionManager` so model hot-swap propagates to in-flight pumps.

## Meeting mode (post-#122)

The meeting pump runs continuously from `meeting::SessionManager::start_manual(sources, app_name)`:

1. Open one `Box<dyn AudioSession>` per source (default: mic + SystemAudio when supported).
2. Spawn a tokio task (`run_pump`) that, every `CHUNK_DURATION` (10 s):
   - Drains every handle (so siblings don't accumulate while one transcribes).
   - Runs whisper inference per chunk via `spawn_blocking`.
   - Hands utterances + per-utterance audio (sliced from `meeting::audio_buffer::AudioRollingBuffer`) to `Diarize::label_utterances`. Production diarizer: `FlagGatedDiarizer` routes to `OnnxDiarizer` when the wespeaker model is loaded + the Speakers toggle is on, else `NoopDiarizer`. Source-derived `"mic"` / `"system"` tags stand in when the diarizer doesn't override (the panel maps these to "You" / "Remote").
   - Restarts capture for the next window (or exits on cancel).
3. `stop_manual` sets the cancel flag, awaits the pump's final-chunk drain, writes `ended_at` on the session row.

State machine: `Mutex<SessionState>` where `SessionState` is `Idle | Opening | Active(...)`. The `Opening` sentinel is held across the async DB / handle-open work so concurrent `meeting_start_manual` IPC calls can't race past the precondition. `SessionManager::Drop` aborts the pump's `JoinHandle` on app shutdown; `CpalMicSessionHandle` and `SckSessionHandle` both have `Drop` impls that release their OS resources.

## The four-place IPC sync rule

A `#[tauri::command]` lives in **four** places that must stay aligned. CI catches Rust-only and TS-only breaks; it cannot catch shape mismatches between them — that's a hands-on responsibility. Any time you add or change a command:

1. **Rust struct + handler** in `src-tauri/src/ipc/commands/mod.rs` (or a domain submodule like `commands/meeting.rs`) with `#[serde(rename_all = "camelCase")]`.
2. **Register** in `src-tauri/src/lib.rs` inside `tauri::generate_handler![...]` using the **full module path**:
   - Top-level commands: `ipc::commands::my_command`.
   - Submodule commands: `ipc::commands::meeting::meeting_start_manual`.
   - `pub use` re-exports do **not** carry the macro's hidden `__cmd__<name>` symbol — see `learnings.md` 2026-04-25 for why we ate that lesson once already. The header of `commands/mod.rs` cites this so future contributors don't try.
3. **TypeScript type** in `src/lib/types.ts` (or inline in `+page.svelte` if scoped to the page), then `invoke<MyResult>("my_command", ...)`.
4. **Playwright mock** in `tests/e2e/_mock.ts` with a default handler whose field shape mirrors the Rust struct exactly. Mocks are serialized via `toString()` and rebuilt in the page context, so they can't capture closure variables — any per-test counters must go through `page.exposeFunction`.

A new `IpcError` variant also needs `formatErrorDisplay` in `src/lib/errors.ts` updated to map it to the structured `{ headline, hint?, details? }` shape that `ErrorDisplay.svelte` renders. Page-level surfaces wrap that in their own `ErrorDisplay` slot.

A new IPC the **settings window** needs to invoke isn't automatically allowed by the `default` capability — the settings window has its own `capabilities/settings.json`. Custom `#[tauri::command]` functions don't need permission entries, but Tauri plugin commands (autostart, clipboard, etc.) do. Add explicitly.

## Dev-launch smoke (required for startup-touching changes)

CI does not run a real Tauri runtime. A panic at app boot (plugin init, capability misconfig, `AppState::build_default` failure, `tauri.conf.json` issue, missing rpath for a transitively-linked dylib) is **invisible to CI** and only surfaces when someone pulls the branch. Run `npm run tauri dev` once before opening a PR that touches:

- `src-tauri/src/lib.rs` (especially the `tauri::Builder` chain or `setup` hook)
- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.toml` — adding/removing a Tauri plugin dep, **or making a transitive dep unconditional** (e.g. dropping a feature flag that gated a crate which links system frameworks; the crate's build-script-baked rpaths don't propagate from a transitive dep, see `learnings.md` 2026-04-27).
- `src-tauri/.cargo/config.toml` (link-arg / rpath changes)
- `src-tauri/capabilities/*.json`
- `src-tauri/src/app_menu/` (native macOS menu — a malformed `MenuBuilder` chain panics during `setup`).
- `src-tauri/src/settings_window/` (window-show path — referencing a label not in `tauri.conf.json` is a runtime error).
- Anything that adds or removes a `.plugin(...)` call

## macOS TCC dev-binary quirk

The short version: `cargo tauri dev` builds an unsigned binary that TCC attributes to the parent terminal, so Microphone / Input Monitoring leak through but **Screen Recording (SCK / system-audio) does not**. For anything that touches SCK, build the real bundle:

    npm run tauri:bundle

Stale `Hush.app` rows after rebuilds are recovered via Settings → Permissions → Reset permissions inside Hush, then `−` on the System Settings rows, then relaunch.

The full reasoning, symptom-by-symptom recovery recipes, and the "Dev-loop: stale Hush.app rows after a re-bundle" recipe live in [`docs/macos-permissions.md`](./docs/macos-permissions.md). `learnings.md` 2026-04-27 has the original investigation.

## Conventions

- **Conventional Commits 1.0.0**: `<type>(<scope>): <subject>`. Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `style`, `perf`, `build`, `ci`, `security`. Scopes: `audio`, `transcription`, `hotkey`, `ui`, `ux`, `dictionary`, `history`, `db`, `ipc`, `tauri`, `updater`, `build`, `e2e`. Subject in imperative mood, no full stop, ≤72 chars.
- **Branch names**: `<type>/<short-kebab-description>` (e.g. `feat/whisper-streaming`, `fix/hotkey-release-edge`).
- All changes land via squash-merge PR — `main` is the only long-lived branch.
- **Untagged TODOs fail CI lint.** Use `// TODO(#NNN):` or `// FIXME(#NNN):`.
- **Comments explain *why*, not *what*.** Where a module's design was directly inspired by VoiceInk, the module header says so explicitly.
- **`learnings.md`** at the repo root is the durable design-decision log. Add an entry when a non-obvious architectural call gets made — future sessions read it before re-deriving.

## Black-box reimplementation discipline (legal — read before writing audio / dictation code)

Hush is a black-box reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk). **VoiceInk's source code must never be read** by anyone working on Hush — before, during, or after writing equivalent functionality. Design comes from VoiceInk's public README and observable runtime behaviour, plus general dictation-app knowledge. See `hush-prd.md` §13.8 for the full reasoning. If the discipline is broken accidentally, declare it; the affected module gets re-implemented by a clean contributor.

## Where things live

### Backend (`src-tauri/src/`)

- `audio/` — cpal mic + SCK system-audio + the `AudioSession` handle trait.
- `transcription/` — `Transcribe` trait, whisper-rs backend, GGUF auto-download (origin-restricted to huggingface.co — `ipc/mod.rs::redirect_decision` allows a hop to any HTTPS host when the previous URL was on an HF host so HF→signed-CDN chains work; SHA-256 verified), resample helpers, model catalog.
- `meeting/` — `SessionManager` + chunking pump + `AppClassifier` for foreground-app detection. `app_overrides` submodule persists per-app classifier overrides (#112) consulted at every session start.
- `diarization/` — `Diarize` trait + impls. **Production wiring (#111):** `FlagGatedDiarizer` reads the `diarization_enabled` `AtomicBool` from `AppState` and routes utterances to either `OnnxDiarizer` (the wespeaker ResNet34-LM ONNX speaker-embedding model + online 1-NN-with-threshold clustering for session-stable IDs) or the `NoopDiarizer` fallback. `OnnxDiarizer` is constructed when the model file is present in `models_dir`; the IPC `download_diarizer_model` path hot-swaps via the shared `DiarizeSlot = Arc<RwLock<Arc<dyn Diarize>>>` so a fresh download takes effect on the next pump tick. Submodules: `cluster.rs` (offline agglomerative — kept for potential batch use; production uses the streaming matcher in `onnx::SessionClusterState`), `features.rs` (Mel-FB extraction matching `torchaudio.compliance.kaldi.fbank`), `onnx.rs` (the diarizer impl, gated behind the `diarization-onnx` Cargo feature), `catalog.rs` (single-entry metadata for the wespeaker model). `EnergyDiarizer` (D1 silence-gap heuristic) sits on disk for reference but isn't wired — the cross-source merge collapsed it to "Speaker A" everywhere; D2 superseded it.
- `ipc/` — `AppState`, `AppStateBuilder`, `IpcError`. `commands/` is now a directory: `mod.rs` holds dictation / history / replacements / vocabulary / models / app commands; `commands/meeting.rs` holds the meeting-mode commands + types + sanitiser (extracted under #82). New domain-cohesive command groups should follow the same submodule pattern.
- `hotkey/` — `tauri-plugin-global-shortcut` for the toggle hotkey + `rdev` for push-to-talk. PTT exposes a configurable combo (set of keys held simultaneously) via `PttCombo` and a `ComboMatcher` state machine; combo + Enabled persist to settings DB and are editable in Settings → General → Hotkeys. **Default-on across all platforms** as of #194 — the macOS Input Monitoring TCC prompt fires at first-listener-spawn (~boot time), but in exchange both the toggle hotkey and PTT work out of the box. Pre-#194 the macOS default was off because rdev `listen()` aborted on macOS 26+; that constraint is gone now that we pin to [fufesou's fork](https://github.com/fufesou/rdev) (the one RustDesk ships, which attaches the CGEventTap to `CFRunLoopGetMain()`). Narsil's upstream PR #147 was incomplete (only fixed the `send` path).
- `hud/` — borderless transparent always-on-top recording HUD with drag (`data-tauri-drag-region`) + dismiss button + level meter pump.
- `settings_window/` — `show()` / `hide()` helpers for the standalone Settings window. Symmetric with `hud/`. Window itself is declared in `tauri.conf.json`.
- `app_menu/` — native macOS menu bar. No-op on non-macOS. Menu events emit `menu:goto-section` to the main window or call `settings_window::show` directly.
- `tray/` — status-bar / system-tray icon (macOS menu-bar extra, Windows system tray, Linux notification area). Menu: Show Hush / Toggle Recording / Open Settings / Quit. "Toggle Recording" emits the existing `hotkey:toggle` Tauri event the frontend listens for; one source of truth for start/stop semantics. Behind the `tauri = { features = ["tray-icon", "image-png"] }` Cargo features. Loads `src-tauri/icons/tray-icon@2x.png` — a monochrome alpha-extracted silhouette of the brand mark — at compile time via `include_bytes!`, then sets `icon_as_template(true)` so macOS adapts it to dark/light menu bars (#275). Pre-#275 the builder fed the full-colour `default_window_icon()` to the template mechanism, producing a black blob on light menu bars.
- `macos_perms/` — programmatic TCC permission status reads via AVFoundation / CoreGraphics / IOKit. Used by `diagnose_macos_permissions` to surface granted/denied/not-determined per permission without triggering OS prompts.
- `updater/` — manual "Check for updates" probe (#223). Hits GitHub's `/releases/latest`, compares to `CARGO_PKG_VERSION` via `semver`, returns a tagged `UpdateCheckResult` (UpToDate / UpdateAvailable / CheckFailed). The full `tauri-plugin-updater` auto-update channel (#10) still pends a signing-key decision; the manual probe lives here in the meantime so users have an "am I current?" affordance. Hush does not poll — every update check is user-initiated.

### Frontend (`src/`)

- `lib/*.svelte` — Svelte 5 components (runes-based; `$state`, `$derived`, `$effect`, `$bindable`, `$props()`). `AppSidebar.svelte`, `PttHotkeyEditor.svelte`, `ErrorDisplay.svelte` (shared structured-error renderer), plus the panel components (`HistoryPanel`, `MeetingSessionsPanel`, `ModelPickerPanel`, `VocabularyPanel`, `ReplacementsPanel`, `MeetingAppOverridesPanel`, `MacosDiagnosticPanel`, `ControlsSection`, `ResultBlock`).
- `lib/format.ts`, `lib/types.ts` — shared format helpers and TS types mirroring backend serde shapes (camelCase).
- `lib/errors.ts` — `ErrorDisplay` shape + `formatErrorDisplay` / `formatErrorMessage` helpers. Single place to map an `IpcError` variant (or a thrown `Error`) into the headline / hint / details rendered by `ErrorDisplay.svelte`.
- `app.css` — global stylesheet imported by every route via `+layout.svelte`. Hosts the accent CSS custom properties (`--accent`, `--accent-hover`) used across components.
- `routes/+layout.svelte` — markup-free layout that imports `app.css`. SvelteKit requires a layout file to apply a global stylesheet to every route.
- `routes/+page.svelte` — main window; orchestrates Dictation / Meetings / History sections. ~1.2k LOC. Does NOT own model picker, vocabulary, replacements, or macOS-permissions diagnostic state — those live in the Settings window. Further extraction (meeting state into a composable) is the next natural step if a contributor finds navigation friction.
- `routes/settings/+page.svelte` — standalone Settings window. State-owner for the moved panels. Cross-window invalidation is event-driven where it matters (`model:download-done` is broadcast; replacements/vocab changes are picked up at the next `start_dictation`).
- `routes/hud/+page.svelte` — recording HUD pill. Loaded into the secondary `hud` window.
- `app.html` — page shell.
- `static/app-icon.png` / `app-icon@2x.png` — sourced from `src-tauri/icons/` and used by the sidebar brand chip.

### Other

- `tests/e2e/` — Path A. Playwright specs against `HUSH_E2E=1` mode (vite swaps `@tauri-apps/api/{core,event}` for in-tree stubs). Helper at `tests/e2e/_mock.ts`. Sidebar nav uses `gotoSection(page, "meetings" | "history")` to switch tabs in tests.
- `tests/e2e-tauri/` — Path B (#57 / #202). WebdriverIO + `tauri-driver` against the real built binary. Catches the flows Path A's IPC mocks can't (real `invoke` round-trips, `listen` events, HUD secondary-window lifecycle, real model download against `wiremock`). Scaffold + smoke spec ship today; CI integration deferred until tauri-driver's macOS path stabilises. Run locally per the README.
- `src-tauri/capabilities/` — per-window Tauri capability files: `default.json` (main), `settings.json`, `hud.json`. Adding a new permission to a window is deliberate; every grant widens that window's blast radius.
- `.github/workflows/release.yml` — tag-driven cross-platform builds via `tauri-action` (macOS Apple Silicon `.dmg` only — macOS 26+ is Apple-Silicon-only, so Intel was dropped from the matrix; Linux `.AppImage` + `.deb`; Windows `.msi` + `.exe`). Fires on `v*` tags or manual `workflow_dispatch`. macOS deployment target is 14.0 (the `macos-latest` runner's Xcode 16.4 ships the macOS 15 SDK — that's the ceiling); design target stays macOS 26. Maintainer recipe in [`docs/releases.md`](./docs/releases.md). Auto-update via `tauri-plugin-updater` (#10) is the natural follow-up — gated on a signing-key decision.
