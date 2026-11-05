# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. **Human contributors:** see [`docs/developing.md`](./docs/developing.md) for the dev setup, command reference, and test guide.

## Project shape

Hush is a Tauri 2 desktop app: Rust backend (`src-tauri/`) + SvelteKit / Svelte 5 frontend (`src/`). Four windows are configured: three production surfaces (`main`, `hud`, `menu-bar`) plus the developer-only `debug` console, each with its own capability file. Settings is inline in the main window, not a separate Tauri window. The full architecture — stack, window topology, trait-seam pattern, meeting pump dataflow, module map — lives in [`ARCHITECTURE.md`](./ARCHITECTURE.md). Read it before non-trivial cross-module changes.

Primary target: **macOS 26 only.** macOS 15 and older are explicitly out of scope — don't add backwards-compat shims or `@available`-style version guards. Linux and Windows compile cleanly via CI but are not hands-on tested.

## Common commands

Use [Developing Hush](./docs/developing.md) for setup, command variants and the
validation matrix. `npm run dev` is a browser with fake backend data;
`npm run tauri dev` launches the real app. Native permission testing uses the
installed DMG path, not the unsigned dev binary. Reset commands can alter macOS
permission grants and remove app installs; read the documented effects first.

## Architecture: trait-seam pattern

Every OS-touching layer is a trait (`AudioCapture`, `Transcribe`, `Diarize`, `HistoryRepository`, etc.) with a concrete impl + hand-rolled mocks. The IPC layer composes `Arc<dyn Trait>` into `AppState` so tests substitute deterministic stubs without real audio / SQLite / network. Hot-swap slots let model changes propagate to in-flight meeting pumps: `transcribe` (dictation) + `transcribe_meeting` (the pump's transcriber, split per #248 so dictation never queues behind a meeting tick; both share ONE loaded `WhisperContext` with separate inference gates — whisper.cpp copies weights into RAM rather than mmapping, so two contexts meant two full copies), `DiarizeSlot`, plus a shared `inference_threads: Arc<AtomicI32>` (#255) that lets the Settings → Performance slider take effect on the next inference call without a model reload.

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

Follow [Add or change an IPC command](./docs/ipc-commands.md): Rust handler and
wire types, full-path registration, TypeScript types/call site and Playwright
mock must stay aligned. New error variants also need their display mapper;
plugin commands need permissions for the window that invokes them. Keep the
mock serialization constraint and real-backend verification in that recipe.

## Dev-launch smoke (required for startup-touching changes)

CI does not boot a real Tauri runtime. Changes to startup, plugins, capabilities,
window configuration, menu construction or linking require the
[dev-launch smoke](./docs/developing.md#dev-launch-smoke). Preserve its trigger
list and run the app before claiming startup works.

## macOS TCC testing

Follow [the native permission workflow](./docs/developing.md#macos-tcc-quirks-and-dev-reset)
and [recovery guide](./docs/macos-permissions.md). Install by dragging from the
DMG to `~/Applications`: the quarantine-strip/exec-restart path is part of the
real permission flow. An unsigned `tauri dev` launch does not validate it.

For a deliberately clean Input Monitoring test, the documented reset is required:
its unscoped `ListenEvent` reset affects **all apps**, and it removes Hush installs
from both Applications locations to clear conflicting signing identities. Choose
the documented reset mode before running it; vocabulary/history are preserved by
default unless the corresponding deletion flags are supplied.

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
- **`diarization/` pipeline (#1013).** Production is `FlagGatedDiarizer` → `OnnxDiarizer` (wespeaker ResNet34-LM) → `diarization/session.rs::SessionClusterState`. Three load-bearing calls: (1) **features must match wespeaker's training front end** — `features.rs` is kaldi-compatible (Hamming, int16 scale, per-frame DC/pre-emphasis, mel-domain triangles; matches `torchaudio.compliance.kaldi.fbank` to 5e-4) and `onnx.rs::embed` applies **per-utterance CMN**. Dropping CMN takes LibriSpeech EER from 0.6 % back to 10.9 % — it was the real cause of #633's "speakers merge at 0.6", so don't touch the front end without re-running the `diarization::onnx::eval_tests` harness (`#[ignore]`d; needs the model + labelled speech, see its header). (2) **`DEFAULT_DISTANCE_THRESHOLD` is 0.6** (back from #633's 0.4, now evidence-backed; learnings.md 2026-10-01 has the tables) and is also the re-cluster's average-linkage cut. (3) **The online matcher is duration-gated** (< 1.0 s assign-only, ≥ 1.5 s to found a speaker), centroids are normalised sums of unit vectors, and a **session-end re-cluster** (`Diarize::finalize_session`, run in background finalization *before* identity resolution) relabels persisted rows via `MeetingSessionRepository::relabel_utterances`. Any embedding-space change must bump `speakers::CURRENT_EMBEDDING_VERSION` (stored voiceprints of other versions are excluded from auto-matching; merging a new identity into an old one via `speaker_merge` re-enrols it). `cluster.rs` holds only `cosine_distance` + the threshold; the D1 `EnergyDiarizer` and offline `cluster_with_threshold` were removed in #310. Env tunables: `HUSH_DIARIZER_THRESHOLD` (0.6; a Settings-slider value wins), `HUSH_DIARIZER_RECLUSTER` (on; `0` keeps live labels), `HUSH_DIARIZER_OVERLAP_GUARD` (on; remote chunks overlapping mic speech may assign but not update/create; self-disables per session above 60 % overlap, i.e. speakerphone), `HUSH_DIARIZER_LOCAL_SEPARATION` (off; `1` diarizes the mic in its own cannot-link namespace so in-room voices separate while the dominant one stays "You"). Per-`assign` distance + decision is logged at INFO.
- **`diarization/onnx.rs` tract engine.** `OnnxDiarizer` uses `tract-onnx` (pure Rust, zero Metal/IOAccelerator) since #641. `TypedRunnableModel<TypedModel>` is `Send+Sync` — no mutex needed around the model. The `MelExtractor` lives in the struct to avoid per-call re-init cost. Don't reintroduce any ORT/ONNX Runtime dependency here — ORT's Apple Silicon prebuilts dispatch through Metal Performance Shaders even with the CPU EP, causing unbounded IOAccelerator growth (#641).
- **`transcription/whisper.rs` WhisperState lifecycle (#612).** The streaming session holds an `Option<WhisperState>` slot that is **lazily created** on the first `infer` call, **dropped on `state.full` Err** (so KV-cache junk from a failed decode doesn't bleed forward), and **periodically dropped** every `DEFAULT_STATE_RECREATE_INTERVAL = 30` inferences to bound whisper.cpp's per-call C-heap accumulation. The interval is tunable via `HUSH_WHISPER_STATE_RECREATE_INTERVAL` (0 disables recreation). When editing `WhisperInferer::infer`, preserve the lazy-init / drop-on-Err / periodic-recreate triplet — they're co-load-bearing for the memory profile in `learnings.md` "#612 not actually closed". The context is loaded `_no_state` and holds only immutable weights, so all per-inference growth lives in `WhisperState`; the #636 per-meeting `WhisperContext` rebuild was therefore removed when the context became shared (learnings.md 2026-10-01). The `OnnxDiarizer` half of #636 was resolved by #641 (ORT → tract-onnx).
- **`ipc/commands/` registration trap.** `pub use` re-exports do **not** carry the `#[tauri::command]` macro's hidden `__cmd__<name>` symbol. Always register submodule commands with their full module path (`ipc::commands::meeting::meeting_start_manual`) in `tauri::generate_handler![]`. See `learnings.md` 2026-04-25 for the lesson we already ate.
- **`hotkey/` rdev fork.** Pinned to [fufesou's rdev fork](https://github.com/fufesou/rdev) (the one RustDesk ships) because Narsil's upstream PR #147 only fixed the `send` path on macOS 26+; the `listen()` path needs the CGEventTap attached to `CFRunLoopGetMain()`. Don't bump to upstream rdev until that lands.
- **`tauri-plugin-single-instance` registration order (#326).** The plugin must be registered **first** in the `tauri::Builder` chain so a second-instance launch bails out before the side-effect-bearing plugins (autostart, global-shortcut, the SQLite pool in `setup`) initialise. If you add a new plugin that opens a system resource, register it **after** `tauri_plugin_single_instance::init` — otherwise a duplicate launch leaks two of whatever the new plugin owns.
- **Supply-chain pin policy.** `ort` was removed from the **diarizer** in #641 (`diarization/onnx.rs` uses `tract-onnx` and must stay that way). It returned in #521 for the **Parakeet engine only**, behind the off-by-default `parakeet` feature, because tract cannot load those graphs at all — exact-pinned `ort = "=2.0.0-rc.13"` and allowlisted. `rdev` remains a git fork pin. `tract-onnx = "0.22.1"` uses a standard caret pin — no special bump policy. Bump-when policy for `rdev` and `ort` lives in `learnings.md` "Supply-chain pins" — read it before any `cargo update -p rdev` or `-p ort`; an ORT bump invalidates the memory measurement that justified its return, so re-run the soak. CI's `supply-chain-pins` job blocks new RC pins / git deps in `Cargo.toml` **and** unallowlisted pre-releases in `Cargo.lock` (#327, extended #521 after a transitive RC slipped through the manifest-only check).
- **`routes/hud/+page.svelte` transparent-window contract.** The HUD window is configured with `transparent: true` + `decorations: false` in `tauri.conf.json`. This means the WebView body has **no default background** — if the `<style>` block is stripped or left empty (e.g. by a mass dark-mode refactor that touches every Svelte file), the browser renders a plain white rectangle over the screen instead of the intended dark pill. Any edit to `hud/+page.svelte` must preserve `body { background-color: transparent !important; }` and the pill container's own background. Verified empirically: stripping the style block wipes the HUD appearance entirely.
- **`tray/` icon-as-template.** Loads `src-tauri/icons/tray-icon@2x.png` — a monochrome alpha-extracted silhouette — and sets `icon_as_template(true)` so macOS adapts to dark/light menu bars. Feeding `default_window_icon()` (full colour) to the template mechanism produces a black blob on light menu bars (#275).
- **`updater/` is manual only.** Hits GitHub `/releases/latest`, returns a tagged `UpdateCheckResult`. Hush does not poll — every update check is user-initiated. Auto-update via `tauri-plugin-updater` (#10) pends a signing-key decision.
- **`routes/+page.svelte` ownership.** Does NOT own model picker, vocabulary, replacements, or TCC diagnostic state — those live in the `src/lib/state/*.svelte.ts` modules rendered by `SettingsPanel.svelte` inside the main window. `+page.svelte` owns the global recording signals: it derives `meetingOnlyActive` / `anyRecordingActive` (mirroring `DictationSection`) and wires document title, tray `UiRecordingState`, sidebar recording dot, toggle hotkey, and command palette to `anyRecordingActive` so they respond to both dictation and auto-detected meeting sessions.
- **`permissions/` cross-platform split.** Renamed from `macos_perms/` in #597. `permissions/macos.rs` holds the AVFoundation / CoreGraphics / IOKit TCC reads behind `#[cfg(target_os = "macos")]`; `permissions/mod.rs` is the home for future Linux / Windows impls. The IPC layer at `ipc/commands/permissions.rs` (also renamed) wraps them; command names on the wire are unchanged.
- **Logging has three sinks.** stderr fmt + daily-rolling file appender at `~/Library/Logs/io.github.khawkins98.hush/hush.log.<date>` (#624) + the in-app `DebugLogLayer` ring buffer (#532). `init_tracing` in `lib.rs` composes them. Disable the file sink with `HUSH_LOG_FILE=off`. Settings → Debug surfaces the file path with reveal/copy controls (#627). Architecture detail: see `ARCHITECTURE.md` "Logging / observability". **All three layers must keep their `.with_filter(env_filter())`** — an unfiltered layer force-enables `trace!`/`debug!` crate-wide — and the `log:event` emit must stay gated on debug-console visibility: every emit is a WKWebView `evaluateJavaScript` call that leaks host-process memory per call on macOS (#986; ~160 MB/min during meetings when ungated).
- **`alloc_tuning/` mimalloc purge tuning.** mimalloc (the global allocator, `override` feature) retains freed pages as committed-dirty by default; whisper.cpp's per-inference scratch churn then accumulates as compressed dirty pages — *physical footprint* grows ~1 GB/min during meetings while RSS stays flat. `alloc_tuning::init()` (called in `run()`) sets `purge_delay=0`, and `whisper.rs` calls `alloc_tuning::force_collect()` after each streaming inference. `HUSH_ALLOC_PURGE=0` disables both (A/B baseline knob). Don't remove mimalloc itself — system malloc is worse (23.5 GB MALLOC_LARGE, see learnings.md 2026-05-07). When touching memory-related code, validate with `npm run memwatch` and read `docs/memory-debugging.md` first.
- **`tests/e2e/` mock serialization.** Playwright mocks at `tests/e2e/_mock.ts` are serialized via `toString()` and rebuilt in the page context, so they can't capture closure variables — any per-test counters must go through `page.exposeFunction`.
- **`src-tauri/capabilities/`.** Per-window. Adding a permission to a window is deliberate; every grant widens that window's blast radius. Settings UI actions live in the main window, so plugin permissions they need belong in `default.json`; the `hud`, `menu-bar`, and `debug` windows must be granted separately if their scope grows. Custom `#[tauri::command]` functions don't need permission entries; Tauri plugin commands do.
- **`meeting/call_end_detector.rs` scope and cold-start behaviour.** The detector only activates for **manually-started sessions** (`session_is_auto == false`); auto-started sessions already have `AutoStop` via `evaluate_mic_state`. The `WhisperSilenceSignal` reads `whisper_consecutive_empty_ticks` (an `Arc<AtomicU32>` written by the pump). This atomic initialises to **0**, and `ticks == 0` is treated as a **reversal** ("real speech just arrived") — so the signal keeps the state machine in `Monitoring` until the pump's first inference tick increments the counter above 0. This is intentional (prevents false-positive on session start) but can be surprising when debugging. Idle false-positives are handled separately and explicitly: the detector loop skips (and resets to `Monitoring`) whenever `meeting_manager.has_active_session()` is false — don't rely on the `ticks == 0` quirk for that. Six env-var tunables: `HUSH_CE_CONFIRM_SECS` (30), `HUSH_CE_SOLO_SECS` (120), `HUSH_CE_MIC_SECS` (10), `HUSH_CE_AUDIO_SECS` (60), `HUSH_CE_AUDIO_THRESHOLD` (0.005 ≈ -46 dBFS), `HUSH_CE_WHISPER_TICKS` (20 ≈ 10 s at 500 ms pump tick). Poll interval is `POLL_INTERVAL_SECS = 5` (constant, not env-tunable). Adding a fourth signal: implement `CallEndSignal`, extend the `match signal.name()` dispatch in `run_call_end_detector_task`, and wire a new `CallEndInputs` field. (`learnings.md` 2026-06-15 documents why the weighted-scorer approach was rejected in favour of this state machine.)
- **`.github/workflows/release.yml`.** macOS deployment target is 14.0 (the `macos-latest` runner's Xcode 16.4 ships the macOS 15 SDK — that's the ceiling); design target stays macOS 26. The Homebrew tap was retired (learnings.md 2026-05-28); releases ship DMG/installers from GitHub Releases only. Maintainer recipe in [`docs/releases.md`](./docs/releases.md).
