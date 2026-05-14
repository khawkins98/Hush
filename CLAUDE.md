# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. **Human contributors:** see [`docs/developing.md`](./docs/developing.md) for the dev setup, command reference, and test guide.

## Project shape

Hush is a Tauri 2 desktop app: Rust backend (`src-tauri/`) + SvelteKit / Svelte 5 frontend (`src/`). Three windows (`main`, `settings`, `hud`) each with their own capability file. The full architecture — stack, three-window topology, trait-seam pattern, meeting pump dataflow, module map — lives in [`ARCHITECTURE.md`](./ARCHITECTURE.md). Read it before non-trivial cross-module changes.

Primary target: **macOS 26 only.** macOS 15 and older are explicitly out of scope — don't add backwards-compat shims or `@available`-style version guards. Linux and Windows compile cleanly via CI but are not hands-on tested.

## Common commands

```bash
# Run the full app. Default features are `whisper` (needs cmake on
# macOS) + `diarization-onnx` (pure-Rust ONNX inference via `tract-onnx`;
# no vendored binaries — compiles from source, no network needed). ScreenCaptureKit
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

# macOS-only: build a release DMG for local distribution testing.
# Automatically ejects any stale Hush DMG mounts left by previous
# failed builds (the root cause of "failed to run bundle_dmg.sh"
# errors — not a macOS 26 version-parsing bug as previously noted).
# DMG lands at src-tauri/target/release/bundle/dmg/*.dmg.
npm run tauri:dmg

# Regenerate the DMG installer background PNG from its SVG source.
# Requires rsvg-convert (brew install librsvg). Run after editing
# src-tauri/assets/dmg-background.svg, then commit the updated PNG.
bash scripts/generate-dmg-background.sh

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

# UX screenshot walkthrough — captures ~16 PNGs of every significant
# screen state to /tmp/hush-uxwalk-shots/. Use for visual review after
# UI changes. Script: tests/e2e/zz-uxwalk.spec.ts
npm run test:uxwalk

# Frontend e2e — Path B (tauri-driver + WebdriverIO, real binary)
# Prereq: `cargo install tauri-driver --locked` and a debug build:
#   npm run tauri build -- --debug
# See `tests/e2e-tauri/README.md` for full setup. CI integration is
# deferred until tauri-driver's macOS support stabilises.
npm run test:e2e:tauri

# Reset stale dev servers (kills tauri/vite processes only)
npm run dev-cleanup

# Full vanilla reset — kills processes AND wipes TCC grants, app database,
# preferences, and caches. Use before testing onboarding or new-user flows.
# Pass --nuke-models to also remove downloaded models; --user <name> for another account.
npm run dev-reset

# Lint + format
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --all

# Cross-platform lint (required before every PR): simulates the Linux CI path.
# macOS clippy misses errors in #[cfg(target_os = "macos")]-gated code, unused
# imports that are only used behind a cfg, and new_without_default on constructors
# whose signatures differ across platforms. Run this before committing Rust changes.
cd src-tauri && cargo clippy --lib --no-default-features -- -D warnings
```

ScreenCaptureKit is now an unconditional macOS dependency (no feature flag). The crate's build script links libSwift_Concurrency at runtime. On a dev machine where the rpaths the build script bakes in (`/usr/lib/swift`, `/Library/Developer/CommandLineTools/...swift-5.5/macosx`) don't resolve, `cargo test --lib` aborts with a missing-dylib error. Workaround: `DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx cargo test --lib`. Production app bundles inherit the Swift runtime from the dyld shared cache and need no override; CI on `macos-latest` has the CommandLineTools path populated and doesn't either.

## Architecture: trait-seam pattern

Every OS-touching layer is a trait (`AudioCapture`, `Transcribe`, `Diarize`, `HistoryRepository`, etc.) with a concrete impl + hand-rolled mocks. The IPC layer composes `Arc<dyn Trait>` into `AppState` so tests substitute deterministic stubs without real audio / SQLite / network. Hot-swap slots let model changes propagate to in-flight meeting pumps: `transcribe` (dictation) + `transcribe_meeting` (the pump's own `WhisperContext`, split per #248 to avoid mutex contention), `DiarizeSlot`, plus a shared `inference_threads: Arc<AtomicI32>` (#255) that lets the Settings → Performance slider take effect on the next inference call without a model reload.

Full seam table, the meeting-pump dataflow diagram, and the module map are in [`ARCHITECTURE.md`](./ARCHITECTURE.md). When you touch a seam (adding a new trait method, swapping a prod impl, threading a new dependency through `AppState`), update both the prod impl *and* the test mock in the same change — the mocks are how the IPC tests stay deterministic.

## IPC integration testing infrastructure

The trait-seam pattern enables deterministic round-trip tests without real audio/SQLite. Key utilities live in `src-tauri/src/ipc/tests.rs`:

**`AppStateBuilder`** (`src-tauri/src/ipc/builder.rs`) — explicit-builder for `AppState`. Each method sets one seam; `build()` panics if a required seam is missing. Used in production (`lib.rs::setup`) and all IPC tests.

**`MemHistory`** — in-memory `HistoryRepository` impl that stores entries in `Mutex<Vec>`. Unlike `NoopHistory` (which discards writes), `MemHistory` retains them so tests can assert side-effects. 18 integration tests use it to round-trip `history_create`, `history_search`, `history_delete`, `history_get_stats`. Other seams have `Noop*` variants that return empty/default values.

When adding a command that touches a seam:
1. Write the handler in `src-tauri/src/ipc/commands/{domain}.rs`.
2. Add an integration test in `src-tauri/src/ipc/tests.rs` using `AppStateBuilder` + appropriate `Mem*` mock.
3. Assert on the mock's state after the handler completes (not just on the return value).
4. The [four-place IPC sync rule](#the-four-place-ipc-sync-rule) still applies: handler → registration → TS type → Playwright mock.

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

**The one canonical workflow for TCC / permission testing:**

    npm run dev-reset    # REQUIRED when testing IM from scratch — see note below
    npm run tauri:bundle # build → re-sign → install to ~/Applications/Hush.app → launch

`tauri:bundle` builds a debug `.app`, re-signs it so TCC uses the stable bundle ID (`io.github.khawkins98.hush`), and installs it to `~/Applications/Hush.app` — a standard macOS app location that TCC treats identically to `/Applications`. This is as reliable as a DMG install without requiring a full release compile (which can take 5–10 min).

Use `npm run tauri dev` for fast UI/Rust iteration — it can't test TCC reliably. Use `npm run tauri:dmg` only when you need a distributable release artifact.

**`dev-reset` is required when testing Input Monitoring from a clean state.**  
`npm run dev-reset` runs a nuclear `tccutil reset ListenEvent` (no bundle ID) that clears all IM grants for the user — necessary because TCC entries created during quarantine or by ad-hoc cdhash don't match the plain bundle-ID-scoped reset. It also removes *both* `~/Applications/Hush.app` and `/Applications/Hush.app`. The `/Applications` removal is critical: old DMG installs from before the re-signing fix carry a linker-signed codesign identifier (`hush-<hash>`) instead of `io.github.khawkins98.hush`. TCC uses the **codesign identifier** (not `CFBundleIdentifier`) to key permission rows, so the two installs have completely separate TCC universes and running both produces a confusing split where one shows Denied and the other shows NotDetermined. See `learnings.md` 2026-05-13 "Linker-signed vs re-signed TCC identity".

**Why `cargo tauri dev` and the raw debug binary don't work for TCC:**  
`cargo tauri dev` produces an unsigned binary. TCC attributes it to the parent terminal, and Screen Recording in particular effectively requires a real `.app` bundle. Even `cargo tauri build --debug` leaves a linker-signed binary with a hash-based identifier (`hush-<hash>`), not `io.github.khawkins98.hush` — `tauri:bundle` fixes this automatically with `codesign --force --deep --sign -`. See `learnings.md` 2026-05-04 for the full investigation.

Stale `Hush.app` rows after rebuilds are recovered by manually removing them with `−` in System Settings → Privacy, then running `npm run dev-reset`, then `npm run tauri:bundle`.

The full troubleshooting guide lives in [`docs/macos-permissions.md`](./docs/macos-permissions.md).

## Conventions

- **Conventional Commits 1.0.0**: `<type>(<scope>): <subject>`. Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `style`, `perf`, `build`, `ci`, `security`. Scopes: `audio`, `transcription`, `hotkey`, `ui`, `ux`, `dictionary`, `history`, `db`, `ipc`, `tauri`, `updater`, `build`, `e2e`. Subject in imperative mood, no full stop, ≤72 chars.
- **Branch names**: `<type>/<short-kebab-description>` (e.g. `feat/whisper-streaming`, `fix/hotkey-release-edge`).
- All changes land via squash-merge PR — `main` is the only long-lived branch.
- **Untagged TODOs fail CI lint.** Use `// TODO(#NNN):` or `// FIXME(#NNN):`.
- **Comments explain *why*, not *what*.** Where a module's design was directly inspired by VoiceInk, the module header says so explicitly.
- **`learnings.md`** at the repo root is the durable design-decision log. Add an entry when a non-obvious architectural call gets made — future sessions read it before re-deriving.

## Black-box reimplementation discipline (legal — read before writing audio / dictation code)

Hush is a black-box reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk). **VoiceInk's source code must never be read** by anyone working on Hush — before, during, or after writing equivalent functionality. Design comes from VoiceInk's public README and observable runtime behaviour, plus general dictation-app knowledge. See `hush-prd.md` §13.8 for the full reasoning. If the discipline is broken accidentally, declare it; the affected module gets re-implemented by a clean contributor.

## Module gotchas

The high-level module map is in [`ARCHITECTURE.md`](./ARCHITECTURE.md). Below are the non-obvious things worth knowing before editing specific modules — most are calls that didn't survive simplification:

- **`transcription/` redirect handling.** `ipc/mod.rs::redirect_decision` allows a hop to any HTTPS host when the previous URL was on a Hugging Face host, so HF → signed-CDN chains work. Don't tighten this without testing the actual download path — HF redirects to a S3-style signed URL.
- **`diarization/` D1 vs D2 history.** Production is `FlagGatedDiarizer` → `OnnxDiarizer` (D2, wespeaker) using the streaming 1-NN matcher in `onnx::SessionClusterState`. The earlier D1 path (`EnergyDiarizer` silence-gap heuristic) and the offline agglomerative `cluster_with_threshold` were both removed in #310; `cluster.rs` retains only `cosine_distance` + `DEFAULT_DISTANCE_THRESHOLD`, both consumed by the streaming matcher. The default threshold is **0.4** as of #633 (down from 0.6 — empirically better on multi-person calls where system-audio sources from a single Zoom/Teams stream pull cosine distances tighter than the wespeaker eval-curve mid-range suggests). Tunable at runtime via `HUSH_DIARIZER_THRESHOLD` env var; per-`assign` cosine distance + decision is logged at INFO. The "1-NN chaining drift" risk in `assign` is the live design call — `learnings.md` and #316 cover the open research follow-up.
- **`diarization/onnx.rs` tract engine.** `OnnxDiarizer` uses `tract-onnx` (pure Rust, zero Metal/IOAccelerator) since #641. `TypedRunnableModel<TypedModel>` is `Send+Sync` — no mutex needed around the model. The `MelExtractor` lives in the struct to avoid per-call re-init cost. Don't reintroduce any ORT/ONNX Runtime dependency here — ORT's Apple Silicon prebuilts dispatch through Metal Performance Shaders even with the CPU EP, causing unbounded IOAccelerator growth (#641).
- **`transcription/whisper.rs` WhisperState lifecycle (#612).** The streaming session holds an `Option<WhisperState>` slot that is **lazily created** on the first `infer` call, **dropped on `state.full` Err** (so KV-cache junk from a failed decode doesn't bleed forward), and **periodically dropped** every `DEFAULT_STATE_RECREATE_INTERVAL = 30` inferences to bound whisper.cpp's per-call C-heap accumulation. The interval is tunable via `HUSH_WHISPER_STATE_RECREATE_INTERVAL` (0 disables recreation). When editing `WhisperInferer::infer`, preserve the lazy-init / drop-on-Err / periodic-recreate triplet — they're co-load-bearing for the memory profile in `learnings.md` "#612 not actually closed". The current state of #612 is "reduced from catastrophic to merely uncomfortable"; the deeper structural fix (drop+recreate `WhisperContext` on session boundaries) is tracked in #636. Note: the `OnnxDiarizer` half of #636 was resolved by #641 (ORT → tract-onnx eliminates IOAccelerator growth entirely).
- **`ipc/commands/` registration trap.** `pub use` re-exports do **not** carry the `#[tauri::command]` macro's hidden `__cmd__<name>` symbol. Always register submodule commands with their full module path (`ipc::commands::meeting::meeting_start_manual`) in `tauri::generate_handler![]`. See `learnings.md` 2026-04-25 for the lesson we already ate.
- **`hotkey/` rdev fork.** Pinned to [fufesou's rdev fork](https://github.com/fufesou/rdev) (the one RustDesk ships) because Narsil's upstream PR #147 only fixed the `send` path on macOS 26+; the `listen()` path needs the CGEventTap attached to `CFRunLoopGetMain()`. Don't bump to upstream rdev until that lands.
- **`tauri-plugin-single-instance` registration order (#326).** The plugin must be registered **first** in the `tauri::Builder` chain so a second-instance launch bails out before the side-effect-bearing plugins (autostart, global-shortcut, the SQLite pool in `setup`) initialise. If you add a new plugin that opens a system resource, register it **after** `tauri_plugin_single_instance::init` — otherwise a duplicate launch leaks two of whatever the new plugin owns.
- **Supply-chain pin policy.** `ort` and `ndarray` were removed when we migrated to `tract-onnx` (#641). `rdev` remains a git fork pin. `tract-onnx = "0.22.1"` uses a standard caret pin — no special bump policy. Bump-when policy for `rdev` lives in `learnings.md` "Supply-chain pins" — read it before any `cargo update -p rdev`. CI's `supply-chain-pins` job blocks new RC pins / git deps that aren't on the explicit allowlist (#327).
- **`tray/` icon-as-template.** Loads `src-tauri/icons/tray-icon@2x.png` — a monochrome alpha-extracted silhouette — and sets `icon_as_template(true)` so macOS adapts to dark/light menu bars. Feeding `default_window_icon()` (full colour) to the template mechanism produces a black blob on light menu bars (#275).
- **`updater/` is manual only.** Hits GitHub `/releases/latest`, returns a tagged `UpdateCheckResult`. Hush does not poll — every update check is user-initiated. Auto-update via `tauri-plugin-updater` (#10) pends a signing-key decision.
- **`routes/+page.svelte` ownership.** Does NOT own model picker, vocabulary, replacements, or TCC diagnostic state — those live in the Settings window. Cross-window invalidation is event-driven (`model:download-done` is broadcast; replacements/vocab refresh on the next `start_dictation`). **Owns global recording signals:** derives `meetingOnlyActive` / `anyRecordingActive` (mirroring `DictationSection`) and wires document title, tray `UiRecordingState`, sidebar recording dot, toggle hotkey, and command palette to `anyRecordingActive` so they respond to both dictation and auto-detected meeting sessions.
- **`permissions/` cross-platform split.** Renamed from `macos_perms/` in #597. `permissions/macos.rs` holds the AVFoundation / CoreGraphics / IOKit TCC reads behind `#[cfg(target_os = "macos")]`; `permissions/mod.rs` is the home for future Linux / Windows impls. The IPC layer at `ipc/commands/permissions.rs` (also renamed) wraps them; command names on the wire are unchanged.
- **Logging has three sinks.** stderr fmt + daily-rolling file appender at `~/Library/Logs/io.github.khawkins98.hush/hush.log.<date>` (#624) + the in-app `DebugLogLayer` ring buffer (#532). `init_tracing` in `lib.rs` composes them. Disable the file sink with `HUSH_LOG_FILE=off`. Settings → Debug surfaces the file path with reveal/copy controls (#627). Architecture detail: see `ARCHITECTURE.md` "Logging / observability".
- **`tests/e2e/` mock serialization.** Playwright mocks at `tests/e2e/_mock.ts` are serialized via `toString()` and rebuilt in the page context, so they can't capture closure variables — any per-test counters must go through `page.exposeFunction`.
- **`src-tauri/capabilities/`.** Per-window. Adding a permission to a window is deliberate; every grant widens that window's blast radius. Settings-window IPCs that hit Tauri plugins (autostart, clipboard) need explicit entries in `settings.json`; custom `#[tauri::command]` functions don't.
- **`.github/workflows/release.yml`.** macOS deployment target is 14.0 (the `macos-latest` runner's Xcode 16.4 ships the macOS 15 SDK — that's the ceiling); design target stays macOS 26. Maintainer recipe in [`docs/releases.md`](./docs/releases.md).
