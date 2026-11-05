# Developing Hush

Practical reference for setting up, running, and testing Hush locally.

This guide owns setup, commands and validation. [CONTRIBUTING](../CONTRIBUTING.md)
owns contribution rules; the [IPC recipe](./ipc-commands.md) owns command changes;
[ARCHITECTURE](../ARCHITECTURE.md) explains the runtime. For installing a release
instead of building, use [getting started](./getting-started.md).

---

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | stable | `rustup update stable` |
| Node.js | 24 | Matches the frontend CI jobs; with nvm: `nvm install 24` |
| cmake | any | **macOS only** — required by `whisper-rs` to build whisper.cpp bindings. `brew install cmake` |

> Linux and Windows compile cleanly via CI but are not hands-on tested. macOS 26 is the primary target.

---

## First-time setup

```bash
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install
```

`npm install` runs the `prepare` script, which sets `core.hooksPath = .githooks`.
This activates a pre-push hook that runs Rust formatting,
`cargo clippy --lib --no-default-features -- -D warnings` and `npm run check`
before every push. These are local checks, not the complete CI matrix. The
[hook](../.githooks/pre-push) owns its exact commands and failure hints.

The hook prints the failing step and a short remediation hint when something
breaks, so you don't have to scroll back through the full output to find what
went wrong. Set `HUSH_SLOW_HOOKS=1` in your shell to additionally run
`cargo test --lib --no-default-features` before each push (off by default to
keep the hook interactive-friendly during rapid iteration; a good idea before
the final push to a PR).

**Toolchain differences.** Use the [CI workflow](../.github/workflows/ci.yml)
as the authority for Rust setup. Formatting can differ across toolchains; if CI
and a local check disagree, inspect the failing diff and toolchain versions
rather than assuming local `cargo fmt` reproduces the CI output. Dated findings
remain in `learnings.md`.

The first `npm run tauri dev` will compile whisper.cpp via `whisper-rs` (several minutes on a clean machine) and build the default-on `tract-onnx` diarizer from source. There are **no** vendored ORT binaries or one-time network downloads in the current path.

Subsequent runs are incremental.

---

## Which command to run

Run these from the repository root. Cleanup/reset utilities stop processes or
change installed-app/permission state; read the [reset effects](#macos-tcc-quirks-and-dev-reset)
before choosing them. The table is a task index, not a script to execute top-to-bottom.

| Task | Command / next step |
|---|---|
| Browser/UI iteration with fake data, no native backend | `npm run dev` → http://localhost:1420 |
| Normal development with the real backend | `npm run tauri dev` |
| Native shell without Whisper or the ONNX diarizer | `npm run tauri -- dev -- --no-default-features` |
| Native shell with only the diarization feature | `npm run tauri -- dev --features diarization-onnx -- --no-default-features` |
| Installed-app permission or DMG smoke | `npm run tauri:dmg` on macOS, then drag to `~/Applications` |
| Frontend type/accessibility check (required for each PR) | `npm run check` |
| Frontend logic/state tests | `npm run test:unit`; watch: `npm run test:unit:watch` |
| Browser e2e using mocked IPC | `npm run test:e2e`; interactive: `npm run test:e2e:ui` |
| One browser spec | `npm run test:e2e -- tests/e2e/meeting-panel.spec.ts` |
| Native-binary e2e | `npm run test:e2e:tauri`; [additional setup](../tests/e2e-tauri/README.md) |
| Rust unit tests | `(cd src-tauri && cargo test --lib)`; [feature variants](#rust-unit-tests-cargo-test---lib) |
| Rust formatting and lint | `(cd src-tauri && cargo fmt --all -- --check)`; `(cd src-tauri && cargo clippy --lib --no-default-features -- -D warnings)`; use `(cd src-tauri && cargo clippy --all-targets -- -D warnings)` for the default-feature paths |
| Stop stale dev processes | `npm run dev-cleanup`; inspect [the process cleanup script](../scripts/dev-cleanup.sh) first |
| Reset onboarding state, preserving vocabulary/history by default | `npm run dev-reset`; deletion flags and permission scope [below](#macos-tcc-quirks-and-dev-reset) |
| Reset permissions/install identity while keeping app state | `npm run dev-reset:keep`; [effects below](#macos-tcc-quirks-and-dev-reset) |
| Memory sampling | `npm run memwatch`; [methodology](./memory-debugging.md) |
| Visual review exports | [Walkthrough](#ux-screenshot-walkthrough-npm-run-testuxwalk) and [screenshot export](#ui-reference-sheets-npm-run-testscreenshots) |

### Browser playground versus native app

`npm run dev` sets `HUSH_MOCK=1`. The same mock IPC stubs used by Playwright seed
fake history, permissions, models and settings; this does not exercise real audio,
transcription or permission prompts. Backend events do not arrive as they would
in the app. If a changed seed appears stale, stop the server, clear the local
`node_modules/.vite` cache and restart. See [Path A](#frontend-e2e--path-a-npm-run-teste2e)
for the test/playground seed distinction.

`npm run dev:tauri` is the mock-free Vite server used by Tauri's
`beforeDevCommand`; running that server alone does not launch the Rust app.
The feature variants use the npm-installed Tauri CLI, so no separate `cargo tauri`
installation is assumed. The second `--` forwards `--no-default-features` to
Cargo; see [Tauri CLI runner arguments](https://v2.tauri.app/reference/cli/#dev).
Default app builds include `whisper` and
`diarization-onnx`; the latter builds from source with tract-onnx. The shell with both backends disabled returns `IpcError::TranscriptionUnavailable` for transcription and
uses `NoopDiarizer` for meetings. It still needs the native platform toolchain;
`--no-default-features` is not a browser-only build.

System-audio capture uses the macOS CoreAudio process-tap backend without an
extra capture feature flag. ScreenCaptureKit remains linked for the permission diagnostic path; that is distinct from needing a Screen Recording grant. Check
[the permissions guide](./macos-permissions.md) before diagnosing capture failures.

### Installer assets

After editing the installer background SVG, regenerate its committed PNG with:

```sh
bash scripts/generate-dmg-background.sh
```

This requires `rsvg-convert` (`brew install librsvg`). The installed-DMG path is
for native permission/installer claims, not every hot-reload iteration.

---

## macOS TCC quirks and dev-reset

`cargo tauri dev` produces an **unsigned** binary. macOS TCC attributes it to the parent terminal process, so Microphone and Input Monitoring permissions work inconsistently and may not persist across rebuilds. **Screen Recording is no longer required** (system audio uses the CoreAudio process-tap backend as of v0.5.0, not ScreenCaptureKit).

For anything that requires a real signed `.app` bundle — permission prompts, TCC identity testing, first-run onboarding — build the release DMG and install from it:

```bash
npm run tauri:dmg
# then drag Hush.app from the mounted DMG → ~/Applications, and open it
```

Dragging from the DMG sets the `com.apple.quarantine` xattr; on first launch the app strips it and exec-restarts to establish a clean TCC identity (the same flow real users get). It's a full release compile (5–10 min), so use it deliberately rather than as your default loop. (The former `tauri:bundle` shortcut was retired 2026-06-05 — it `cp`'d the app in place, never setting quarantine, so it skipped that exec-restart and tested a codepath users never hit. See `learnings.md` 2026-06-05.)

If macOS shows stale "Hush" rows in System Settings → Privacy & Security after rebuilding: Settings → Permissions → Reset permissions inside Hush, remove the stale row in System Settings, then relaunch.

To get back to a completely clean state for testing onboarding or first-run permission prompts, run:

```bash
npm run dev-reset
```

This stops Hush/dev processes, removes Hush app installs, resets the permission
grants listed in the script, and clears Hush settings, preferences and caches.
In particular, the unscoped `ListenEvent` reset clears Input Monitoring grants
for **all apps**, which may then prompt again. Your dictionary terms, text replacements, enabled vocabulary packs, transcription history, and meeting sessions are **preserved** by default. Pass `--nuke-dictionary` to also wipe the vocabulary, or `--nuke-db` to wipe history (deletes the whole database, vocabulary included). Permission rows from previous builds may still appear in System Settings — remove any stale "Hush" entries there manually before testing onboarding. See [`scripts/dev-reset.sh`](../scripts/dev-reset.sh) for exactly what is deleted.

Additional flags: `--nuke-models` removes downloaded models; `--user <name>`
targets another account. Forward flags with `npm run dev-reset -- <flags>` and
review the script before using them.

If you only need to re-test the permission flow itself and want to keep your dictionary, text replacements, window layout, and other settings intact, use the softer mode instead:

```bash
npm run dev-reset:keep
```

This still kills processes, resets TCC grants, and removes app installs (so a re-installed bundle doesn't carry a stale codesign-identity TCC row — see `learnings.md` 2026-05-13). But it preserves everything else, so you can run `npm run tauri:dmg` (drag to ~/Applications) after and test the permission flow on top of your real working state.

Full recovery recipes: [`docs/macos-permissions.md`](./macos-permissions.md).

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
- Anything that adds or removes a `.plugin(...)` call

The check is cheap: launch, wait for the "starting Hush" trace log, confirm no panic, kill it (~30 seconds).

---

## Testing layers

### Rust unit tests (`cargo test --lib`)

Pure-logic tests at the trait + module boundaries. No real audio device needed. The default build (features `whisper` + `diarization-onnx`) needs cmake and builds tract-onnx from source; for a fast no-cmake pass use `--no-default-features`.

- **`--no-default-features`** — no cmake required; covers most paths. Test execution is fast on a warm build; cold native dependency compilation can take much longer.
- **Default features** — same tests, but also exercises feature-gated code. Needs cmake + tract-onnx.
- **`--no-default-features --features whisper`** — Whisper without the default ONNX feature; needs cmake.
- **`--no-default-features --features diarization-onnx`** — ONNX diarizer without Whisper. Cargo features are additive unless defaults are disabled.
- **Hand-rolled mocks** at every trait seam (`Noop*`, `Mem*` impls in `src-tauri/src/ipc/tests.rs`) — preferred over `mockall` for clearer test failure messages. `MemHistory` enables round-trip assertions; `Noop*` variants return defaults. Compose test `AppState` instances via `AppStateBuilder`.
- **Async tests** use `#[tokio::test]`. SQLite-backed tests use `SqliteDatabase::open_in_memory()` — no disk, no shared state.

### Integration tests (`src-tauri/tests/`)

Two patterns:

- **`wiremock`-driven HTTP tests** for the model-download path. The orchestrator is pure logic; the wiremock server stands in for Hugging Face. See `src-tauri/src/transcription/download.rs`.
- **`#[ignore]`'d env-var fixtures** for things that need a binary the repo can't ship. `audio_fixture` / `streaming_fixture` / `meeting_fixture` require `HUSH_TEST_MODEL`; `HUSH_TEST_AUDIO` is optional and defaults to the bundled `jfk.wav`. The diarization fixture reads `HUSH_DIARIZATION_MODEL_PATH` plus the speaker WAV env vars documented in `src-tauri/tests/fixtures/README.md`.

For an ignored audio/streaming/meeting fixture, set `HUSH_TEST_MODEL` to an
existing compatible model, then select the test explicitly:

```sh
(cd src-tauri && HUSH_TEST_MODEL=/path/to/ggml-base.bin cargo test --features whisper --test audio_fixture -- --ignored)
(cd src-tauri && HUSH_TEST_MODEL=/path/to/ggml-base.bin cargo test --features whisper --test streaming_fixture -- --ignored --nocapture)
(cd src-tauri && HUSH_TEST_MODEL=/path/to/ggml-base.bin cargo test --features whisper,test-utils --test meeting_fixture -- --ignored --nocapture)
```

No model path means those ignored tests have not exercised real inference.
Use the [fixture README](../src-tauri/tests/fixtures/README.md) for the distinct
ONNX model and labelled-audio requirements. Run a focused Rust unit with, for
example, `(cd src-tauri && cargo test --lib --no-default-features meeting::)`.

When adding an integration test that needs an external resource, prefer `#[ignore]` + an env-var pointer over committing the resource — keeps the repo small and lets contributors opt in.

### Frontend e2e — Path A (`npm run test:e2e`)

Playwright + Chromium drives the SvelteKit dev server in `HUSH_E2E=1` mode, which swaps `@tauri-apps/api/{core,event,app}` for in-tree stubs (`tests/e2e/setup/*-stub.ts`). Tests configure per-spec `invoke` handlers and fire backend-emitted events. See `tests/e2e/README.md`.

The browser playground (`npm run dev`, `HUSH_MOCK=1`) reuses the **same stubs** — the only difference is the seed: Playwright sets a minimal, deterministic, throw-on-unmocked set per test; `npm run dev` falls back to `tests/e2e/setup/mock-defaults.ts`, a populated, forgiving seed (unknown commands return `[]`/`undefined` instead of throwing). `mock-defaults.ts` no-ops under Playwright, which sets `window.__hush_e2e` first.

**Catches:** UI regressions, modal a11y, error-copy drift, retry-race UX, aria-attribute bugs.  
**Doesn't catch:** real IPC, HUD lifecycle, hotkey registration, real audio, real model download. Those are Path B.

### Frontend e2e — Path B (`npm run test:e2e:tauri`)

`tauri-driver` + WebdriverIO drives a real built Hush binary. Catches real `invoke` round-trips, real `listen` events, HUD secondary-window lifecycle, real model download against `wiremock`. Scaffold landed under #202; CI integration is deferred until `tauri-driver`'s macOS support stabilises. Run locally per `tests/e2e-tauri/README.md` — `cargo install tauri-driver --locked`, then `npm run tauri build -- --debug`, then the test command.

### Manual smoke

Before merging anything that touches the dictation hot path, run through the manual checklist in [`STATUS.md`](../STATUS.md) §c. Requires a real microphone and optionally a Whisper model — neither of which CI has access to.

### UX screenshot walkthrough (`npm run test:uxwalk`)

Exports the states defined in the walkthrough spec into `/tmp/hush-uxwalk-shots/`.
Read the passed/skipped test summary before claiming a state was exercised. Use it to spot visual regressions, layout gaps, or stale copy across a branch or after a large refactor.

```bash
npm run test:uxwalk
# Captured PNGs are under /tmp/hush-uxwalk-shots/
# On macOS: open /tmp/hush-uxwalk-shots/
```

The script is `tests/e2e/zz-uxwalk.spec.ts`. It runs as part of the normal CI suite (all tests pass/fail the spec, but no screenshot diffing is enforced — screenshots are for human review only). Meeting panel shots are `test.skip`'d pending real meeting-state mocking.

### UI reference sheets (`npm run test:screenshots`)

The current [screenshot spec](../tests/e2e/zz-screenshots.spec.ts) exports the
single `brand` palette to `tmp/uxwalk/brand/*.png`. It uses mocked IPC; inspect the
run's passed/skipped summary before describing which states were captured.
The package script still prints an old light/dark output hint; the spec owns
the actual output location.

```sh
npm run test:screenshots
```

View the individual PNGs for design review. They are not committed (`tmp/` is
ignored), and they do not establish native audio or permission behavior.

**Legacy compositor:** [`scripts/make-ui-reference.py`](../scripts/make-ui-reference.py)
expects `tmp/uxwalk/light/` and `dark/` and writes two theme sheets. It does not
consume the current `brand` export. Running it after the current screenshot task
is not a maintained end-to-end recipe; use the PNGs directly until the compositor
is updated. Its Pillow dependency does not change that input mismatch.

### Type check (`npm run check`)

Runs `svelte-check` across the full frontend including `vite.config.js`. Required clean for every PR; CI runs the same command.

---

## Where logs live

Hush writes tracing events to **three** sinks. Pick the one that fits the task:

| Sink                                                            | Use it for                                                                                                                                                                                                                                |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **stderr** (`cargo tauri dev` console)                          | Live tailing while iterating. Filtered by `RUST_LOG`. Lost when the process exits.                                                                                                                                                        |
| **In-app Debug Console** (Settings → Debug)                     | Browsing recent events from inside the running app. In-memory ring buffer; lost on quit. Has a "Copy to issue report" button.                                                                                                             |
| **`~/Library/Logs/io.github.khawkins98.hush/hush.log.YYYY-MM-DD`** (macOS only) | Post-hoc grepping after the app exits, sharing logs in bug reports, or correlating events across multiple sessions. Daily-rotating, plain-text (no ANSI). Filtered by the same `RUST_LOG` as stderr. Files accumulate; clean up manually. |

The file sink defaults on for any non-CI run. Disable with `HUSH_LOG_FILE=off` (e.g. for a one-off binary that shouldn't litter Logs). The first stderr line at startup prints the resolved path so you don't have to guess.

```bash
# Tail today's file while the app runs:
tail -f ~/Library/Logs/io.github.khawkins98.hush/hush.log.$(date +%F)

# Grep across all recent days:
grep -h "recreating WhisperState" ~/Library/Logs/io.github.khawkins98.hush/hush.log.*
```

## Diagnosing meeting mode (0 utterances)

When meeting mode transcribes nothing, the logs distinguish three failure modes. First, enable debug logging:

```bash
# Build + install via the DMG (drag to ~/Applications), then launch with
# debug logging. RUST_LOG is read by the app at startup regardless of how
# it's launched; set it for the GUI launch via launchctl so it survives
# the quarantine-strip exec-restart:
launchctl setenv RUST_LOG hush=debug
open ~/Applications/Hush.app
```

Then start a meeting session and watch the console output (Tauri dev console, in-app Debug tab, or `tail -f ~/Library/Logs/io.github.khawkins98.hush/hush.log.$(date +%F)`). You should see lines like:

```
meeting pump: inference tick  session_id=1 source_kind=microphone utterances=0 elapsed_ms=47
streaming tick: inference ran  raw_segments=2 non_empty_segments=0 window_ms=3000
whisper: inference complete  n_segments=2 window_samples=48000
```

### Failure mode 1 — Audio not flowing (`samples = 0`)

```
meeting pump: inference tick  utterances=0 elapsed_ms=1
```

... and every tick shows `elapsed_ms` near 0 with no `"streaming tick: inference ran"` lines from `streaming.rs`.

**Means:** The ring buffer is empty. The audio capture source isn't pushing samples. Check the selected audio source and microphone TCC grants on an installed build.
Follow [macOS permission troubleshooting](./macos-permissions.md): system audio
does not need a separate Screen Recording grant.

### Failure mode 2 — Whisper no-speech filtering

```
streaming tick: inference ran  raw_segments=2 non_empty_segments=0
whisper: inference complete  n_segments=2 window_samples=48000
```

**Means:** Whisper ran and produced segments, but they were all suppressed by `no_speech_thold` (0.6). Common with compressed call audio (Opus/AAC artefacts raise the no-speech token probability). The fix is not to lower the threshold without evidence — see `learnings.md` "2026-05-06" — but to verify the input is actually human speech at an expected level.

### Failure mode 3 — Inference gate never opened

```
meeting pump: inference tick  utterances=0 elapsed_ms=47
```

... and there are **no** `"streaming tick: inference ran"` lines at all (only `"interval gate not open"` or `"waiting for min-first audio threshold"` at `trace!` level, visible with `RUST_LOG=hush=trace`).

**Means:** Audio is flowing but the streaming policy never opens the gate. Check whether `total_samples_fed` is growing (add a temporary `RUST_LOG=hush=trace` session to see the trace-level ticks) and that `infer_interval_ms` / `min_first_inference_ms` are configured as expected.

### Reading the log cross-layer

| Log line | Location | Signal |
|----------|----------|--------|
| `"streaming tick: inference ran"` | `streaming.rs` `tick()` | Gate opened; `raw_segments` vs `non_empty_segments` distinguishes filter vs speech |
| `"streaming finish: tail flush inference ran"` | `streaming.rs` `finish()` | Session-end flush |
| `"whisper: inference complete"` | `whisper.rs` `infer()` | What Whisper saw before text post-processing |
| `"meeting pump: inference tick"` | `pump.rs` | Top-level utterance count; `elapsed_ms` distinguishes slow inference from empty-gate |
| `"transcription slot is None"` (WARN) | `pump.rs` | Model not loaded |

## Diagnosing memory growth

When Hush's memory climbs during a meeting (or any long session), don't reach
for `top` — Activity Monitor's "Memory" column is *physical footprint*, which
includes compressed/swapped dirty pages that RSS-based tools miss entirely.
Leaks in this codebase have repeatedly hidden in exactly that gap.

The full methodology — the RSS-vs-footprint distinction, `npm run memwatch`
usage, the vmmap region-to-owner attribution table, A/B knobs
(`HUSH_ALLOC_PURGE=0`), and the history of every leak found so far — lives in
[`docs/memory-debugging.md`](./memory-debugging.md).

Quick start:

```bash
npm run memwatch          # start sampling, then reproduce (e.g. hold a meeting)
# → /tmp/hush-memwatch-<timestamp>/memwatch.csv + per-sample vmmap snapshots
```
