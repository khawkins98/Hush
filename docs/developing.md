# Developing Hush

Practical reference for setting up, running, and testing Hush locally.

For contribution rules, branch naming, commit format, and IPC recipes see [CONTRIBUTING.md](../CONTRIBUTING.md). For the full architecture see [ARCHITECTURE.md](../ARCHITECTURE.md).

---

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | stable | `rustup update stable` |
| Node.js | ≥ 20 | nvm recommended — `nvm install 22` |
| cmake | any | **macOS only** — required by `whisper-rs` to build whisper.cpp bindings. `brew install cmake` |

> Linux and Windows compile cleanly via CI but are not hands-on tested. macOS 26 is the primary target.

---

## First-time setup

```bash
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install
```

The first `npm run tauri dev` will:

1. Download the ONNX Runtime vendored binaries (~50 MB) via the `ort` `download-binaries` feature — needs network access once.
2. Compile whisper.cpp via `whisper-rs`. Takes several minutes on a clean machine.

Subsequent runs are incremental.

---

## Which command to run

| What you're trying to do | Command |
|---|---|
| Iterate on UI or Rust logic — the normal dev loop | `npm run tauri dev` |
| Frontend-only work, no cmake needed | `cd src-tauri && cargo tauri dev --no-default-features` |
| Diarizer only (no whisper compile cost) | `cd src-tauri && cargo tauri dev --no-default-features --features diarization-onnx` |
| Test Screen Recording / Microphone TCC permission prompts | `npm run tauri:bundle` (macOS only) |
| Build a release `.dmg` to smoke-test the installer | `npm run tauri:dmg` (macOS only) |
| Run Rust unit tests | `cd src-tauri && cargo test --lib` |
| Run frontend type check | `npm run check` |
| Run frontend e2e tests | `npm run test:e2e` |
| Kill stale dev server processes | `npm run dev-cleanup` |

---

## Full annotated command reference

```bash
# Run the full app. Default features are `whisper` (needs cmake on macOS) +
# `diarization-onnx` (pulls ~50 MB ORT binaries on first build; needs
# network). ScreenCaptureKit is linked unconditionally on macOS so
# system-audio capture works without an extra feature flag.
npm run tauri dev

# UI-only path: app shell with no Whisper backend and no ONNX diarizer.
# Transcription returns IpcError::TranscriptionUnavailable; meetings get
# NoopDiarizer. No cmake required — good for pure frontend work.
cd src-tauri && cargo tauri dev --no-default-features

# Diarizer-only (no whisper): useful for iterating on the diarization
# stack without paying the whisper.cpp compile cost.
cd src-tauri && cargo tauri dev --no-default-features --features diarization-onnx

# macOS-only: build a debug .app bundle and open it. Use this for
# smoke-testing anything that depends on macOS treating Hush as a proper
# app — Screen Recording / Microphone TCC prompts in particular. The bare
# `cargo tauri dev` binary doesn't register reliably with TCC (see below).
# Slow: 30 s – 2 min. Not a hot-iteration tool.
npm run tauri:bundle

# macOS-only: clean up stale DMG volumes then build the release .app + .dmg.
# Use when you want to test the installer experience (drag-to-Applications,
# Gatekeeper prompt). Not needed for normal feature work.
npm run tauri:dmg

# Rust unit tests — fast (~100 ms total), no real audio device needed.
cd src-tauri && cargo test --lib
cd src-tauri && cargo test --lib --features whisper            # plus whisper-gated paths
cd src-tauri && cargo test --lib --features diarization-onnx   # plus diarizer-gated paths

# Run a single Rust test or a whole module
cd src-tauri && cargo test --lib audio::tests::name_of_test
cd src-tauri && cargo test --lib meeting::

# Integration tests (#[ignore]'d by default — need external resources)
cd src-tauri && HUSH_TEST_AUDIO=/path/to/sample.wav cargo test --features whisper -- --ignored

# Frontend type check (svelte-check) — required clean for every PR
npm run check

# Frontend e2e — Path A (Playwright + mocked Tauri IPC)
npm run test:e2e
npm run test:e2e:ui                                             # interactive UI

# Run a single Path A spec
npx playwright test tests/e2e/meeting-panel.spec.ts

# Frontend e2e — Path B (tauri-driver + WebdriverIO, real binary)
# Prereq: `cargo install tauri-driver --locked` and a debug build:
#   npm run tauri build -- --debug
# See tests/e2e-tauri/README.md for full setup.
npm run test:e2e:tauri

# Kill stale tauri/vite processes from a previous dev run
npm run dev-cleanup

# Lint + format
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --all
```

---

## macOS TCC quirk (Screen Recording)

`cargo tauri dev` produces an **unsigned** binary. macOS TCC attributes it to the parent terminal process, so Microphone and Input Monitoring permissions work fine — but **Screen Recording (ScreenCaptureKit / system audio)** does not.

For anything that touches SCK, build the real `.app` bundle:

```bash
npm run tauri:bundle
```

This produces a proper `.app` that TCC treats like a user-installed app. It's slow (30 s – 2 min), so use it deliberately rather than as your default loop.

If macOS shows stale "Hush" rows in System Settings → Privacy & Security after rebuilding: Settings → Permissions → Reset permissions inside Hush, remove the stale row in System Settings, then relaunch.

Full recovery recipes: [`docs/macos-permissions.md`](./macos-permissions.md).

---

## ScreenCaptureKit / Swift dylib workaround

ScreenCaptureKit is an unconditional macOS dependency. The crate's build script links `libSwift_Concurrency` at runtime using baked-in rpaths (`/usr/lib/swift`, `/Library/Developer/CommandLineTools/.../swift-5.5/macosx`). On a dev machine where those paths don't resolve, `cargo test --lib` aborts with a missing-dylib error.

Workaround:

```bash
DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx \
  cargo test --lib
```

Production app bundles and CI (`macos-latest`) aren't affected — the shared dyld cache or the CommandLineTools path resolves the library automatically.

---

## Dev-launch smoke

CI does not run a real Tauri runtime. A panic at app boot — plugin init, capability misconfiguration, `AppState::build_default` failure, a `tauri.conf.json` issue, or a missing rpath for a transitively-linked dylib — is **invisible to CI** and only surfaces when someone pulls the branch.

**Run `npm run tauri dev` once before opening a PR that touches:**

- `src-tauri/src/lib.rs` (the `tauri::Builder` chain or `setup` hook)
- `src-tauri/tauri.conf.json` (window config, plugin config blocks)
- `src-tauri/Cargo.toml` — adding/removing a Tauri plugin dep, or making a transitive dep unconditional (see `learnings.md` 2026-04-27)
- `src-tauri/.cargo/config.toml` (link-arg / rpath changes)
- `src-tauri/capabilities/*.json`
- `src-tauri/src/app_menu/` — a malformed `MenuBuilder` chain panics during `setup`
- `src-tauri/src/settings_window/` — referencing a window label not in `tauri.conf.json` is a runtime error
- Anything that adds or removes a `.plugin(...)` call

The check is cheap: launch, wait for the "starting Hush" trace log, confirm no panic, kill it (~30 seconds).

---

## Testing layers

### Rust unit tests (`cargo test --lib`)

Pure-logic tests at the trait + module boundaries. Fast (~100 ms total), run on every PR via CI on Linux + macOS.

- **Default features** — no cmake required; covers most paths.
- **`--features whisper`** — adds whisper-gated paths. Needs cmake.
- **`--features diarization-onnx`** — adds diarizer-gated paths.
- **Hand-rolled mocks** at every trait seam (`Noop*`, `Mem*` impls in `src-tauri/src/ipc/mod.rs`) — preferred over `mockall` for clearer test failure messages.
- **Async tests** use `#[tokio::test]`. SQLite-backed tests use `SqliteDatabase::open_in_memory()` — no disk, no shared state.

### Integration tests (`src-tauri/tests/`)

Two patterns:

- **`wiremock`-driven HTTP tests** for the model-download path. The orchestrator is pure logic; the wiremock server stands in for Hugging Face. See `src-tauri/src/transcription/download.rs`.
- **`#[ignore]`'d env-var fixtures** for things that need a binary the repo can't ship. The audio fixture reads `HUSH_TEST_AUDIO` and runs a known WAV through the full transcription stack. See `src-tauri/tests/fixtures/README.md`.

When adding an integration test that needs an external resource, prefer `#[ignore]` + an env-var pointer over committing the resource — keeps the repo small and lets contributors opt in.

### Frontend e2e — Path A (`npm run test:e2e`)

Playwright + Chromium drives the SvelteKit dev server in `HUSH_E2E=1` mode, which swaps `@tauri-apps/api/{core,event}` for in-tree stubs. Tests configure per-spec `invoke` handlers and fire backend-emitted events. See `tests/e2e/README.md`.

**Catches:** UI regressions, modal a11y, error-copy drift, retry-race UX, aria-attribute bugs.  
**Doesn't catch:** real IPC, HUD lifecycle, hotkey registration, real audio, real model download. Those are Path B.

### Frontend e2e — Path B (`npm run test:e2e:tauri`)

`tauri-driver` + WebdriverIO drives a real built Hush binary. Catches real `invoke` round-trips, real `listen` events, HUD secondary-window lifecycle, real model download against `wiremock`. Scaffold landed under #202; CI integration is deferred until `tauri-driver`'s macOS support stabilises. Run locally per `tests/e2e-tauri/README.md` — `cargo install tauri-driver --locked`, then `npm run tauri build -- --debug`, then the test command.

### Manual smoke

Before merging anything that touches the dictation hot path, run through the manual checklist in [`STATUS.md`](../STATUS.md) §c. Requires a real microphone and optionally a Whisper model — neither of which CI has access to.

### Type check (`npm run check`)

Runs `svelte-check` across the full frontend including `vite.config.js`. Required clean for every PR; CI runs the same command.
