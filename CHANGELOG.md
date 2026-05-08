# Changelog

All notable changes to Hush will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **About tab version and Tauri runtime now display correctly** (#649 follow-up). Version and "Tauri runtime" rows were silently hidden because the main window capability intentionally excludes `app:default`, causing `getVersion()`/`getTauriVersion()` to fail without error. These values are now read from the existing `get_build_info` IPC command (no capability required); `tauri_version` is baked in at compile time by `build.rs` parsing `Cargo.lock`.

### Changed

- **History timestamps no longer show seconds.** History row dates now render as "Apr 29, 2026, 1:00 PM" instead of "4/29/2026, 1:00:00 PM". Consistent with the format already used in meeting session rows.
- **Model picker: speed/accuracy numeric labels removed.** The "5.0" / "1.0" values next to each rating bar were redundant — the bar graph already encodes the ranking. Removed to reduce visual clutter.
- **Settings section headings: sentence case instead of ALL CAPS.** Section labels such as "STARTUP", "INTERFACE", "APPEARANCE", "HOTKEYS", "AUTO-START", and "SPEAKERS" now render in the casing they are authored in (e.g. "Startup", "Interface"). Applies to all six Settings tabs.
- **UX polish: reduced copy clutter across multiple panels.** Removed decorative panel sub-labels ("biases the recognition", "rewrites the output", "teaches Meeting Mode about your apps") — the hint paragraphs below each heading explain the same thing more clearly. History search placeholder shortened to "Search history…". Setup banner button renamed from "Open Settings → Model" to "Choose a model". Model picker intro simplified: technical detail (SHA-256, raw app-data path) moved to secondary copy; key action stays front-and-centre. Keyboard shortcut hint on the Dictation page now uses native Mac glyphs (⌃⌥H) instead of cross-platform "Ctrl/⌥/Alt" copy.

## [0.5.1] - 2026-05-08

### Fixed

- **About tab links no longer crash the app** (#648). Clicking any external link in the About tab (Apache licence, GitHub repo, bug report, whisper.cpp, etc.) previously caused a SIGSEGV via `fork()` in the multithreaded Tauri/Tokio process (`_malloc_fork_child + 184` in the crash log). The `tauri-plugin-shell` open path has been replaced with a custom `open_url` IPC command that uses `posix_spawn()` via `std::process::Command` — safe from multithreaded callers. All update-result "Open release notes" links are also fixed. The now-unnecessary `shell:allow-open` capability grant has been removed.
- **Build timestamp now shows in About tab** (#649). The compile-time timestamp embedded by `build.rs` was silently suppressed by an `.unwrap_or(0)` fallback and a silent `catch {}` block. `build.rs` now uses `.expect()` (no silent zero on broken CI clock), and the catch block logs a `console.warn` so stale-build issues surface in dev tools.
- **Input Monitoring permission prompt no longer fires at startup** (#647). The PTT keyboard listener (rdev) was started unconditionally at launch, triggering the macOS "Input Monitoring" TCC dialog before the user reached the Settings → PTT section. The listener now only starts if Input Monitoring is already granted (or not applicable on non-macOS). On a fresh install the prompt fires when the user first enables PTT in Settings, which is the appropriate moment.

### Added

- **Changelog link in About tab.** The About pane now includes a "Changelog → Release notes" row linking to the GitHub Releases page.

## [0.5.0] - 2026-05-07

### Added

#### System audio backend: CoreAudio process tap replaces ScreenCaptureKit (#588, #595)

- **No more "Screen Recording" prompt for system audio capture.** Hush previously required the user to grant Screen Recording in macOS Privacy settings — an alarming permission for an app that only captures audio. The system audio backend now uses `AudioHardwareCreateProcessTap` (CoreAudio's process-level audio tap, available on macOS 14.2+) which only requires the standard microphone-style audio consent dialog (`NSAudioCaptureUsageDescription`).
- Implemented as a small Swift helper binary at `resources/macos-audio-tap.swift`, compiled by `build.rs` and bundled as a Tauri resource. The Rust side spawns the helper, reads a 12-byte `HUSH` magic + sample-rate + channel-count header, then streams f32 LE interleaved PCM via stdout into an `rtrb` ring buffer that the meeting pump drains per tick.
- Resolves the silent-system-audio failure mode #533 was investigating: SCK's audio path applied codec processing internally and the post-codec PCM tripped Whisper's `no_speech_thold` gate. The direct CoreAudio tap delivers raw f32 PCM with zero codec round-trip.
- IOProc callback uses a pre-allocated scratch-buffer pool (#595) to avoid heap allocation on the realtime audio thread.

#### Build timestamp in the About pane (#589)

- The About pane now displays "Built DD/MM/YYYY HH:MM" below the version, sourced from the `HUSH_BUILD_TIMESTAMP` env var stamped by `build.rs` at compile time. Helps users (and bug reports) confirm which build is running without scrolling backend logs.
- Shared `formatBuildTimestamp` helper in `src/lib/utils/format.ts` consolidates the formatter that was previously duplicated across the Debug Console and issue-report generator.

#### Startup phase timings in the Debug tab (#584 Angle 1)

- New `get_startup_timings` IPC returns a per-phase trace of `AppState::build_default` (database / whisper-contexts / diarizer init / settings wiring) with absolute and per-phase durations. The Debug tab renders a collapsible "⏱ Startup" section listing each phase and its duration; total ≥ 2 s renders in amber as a regression flag.
- Captures startup-time regressions without needing Instruments. The existing `tracing::info!` checkpoints feed the same `Vec<StartupPhase>` so the IPC and the log line share one source of truth.

#### System Audio permission: auto-detect grant + relaunch prompt (#579)

- **"System Audio" label throughout UI** — all user-visible copy in the Permissions tab, first-run wizard, and error messages now says "System Audio" instead of "Screen Recording". The underlying TCC category is still `ScreenCapture`; this is a framing change only, matching how apps like OpenWhispr present the same permission.
- **Auto-detect grant and prompt relaunch** — after the user clicks "Grant in Settings" on the System Audio row (or the wizard equivalent), a background Rust watcher polls `CGPreflightScreenCaptureAccess()` every second (up to 60 s). When preflight flips true, the watcher validates the grant with a real `SCShareableContent::get()` probe before emitting `permission:screen-recording-granted`. The main window listens for this event and shows a green "System Audio permission granted — relaunch Hush to enable system audio in meetings" banner with a "Relaunch Now" button.
- **`relaunch_app` IPC command** — thin wrapper around Tauri's `AppHandle::restart()`. Called by the relaunch banner. The relaunch is necessary because macOS caches the TCC deny in `mediaserverd`/`coreaudiod` for the lifetime of the current process; only a fresh process sees the grant take effect.

#### Sound cues and dictation stats bar e2e test coverage (#292, #293)

- Added four Playwright specs to `tests/e2e/settings-window.spec.ts` covering:
  - Sound cues master toggle mounts unchecked (off by default) with sub-toggles disabled.
  - Enabling the master toggle un-disables the start and complete sub-toggles.
  - Dictation stats bar renders tiles (sessions, words, keystrokes, time-saved) when `sessionCount > 0`.
  - Dictation stats bar is absent when `sessionCount === 0` (empty-state contract).

#### Record-mode-badge e2e test coverage

- Added three Playwright specs to `tests/e2e/recording-phase.spec.ts` covering:
  - Badge absent when `screenRecording` health is `not-applicable` (default state).
  - Badge visible with `data-health="not-granted"` when Screen Recording permission is not granted.
  - Badge visible with `data-health="stale"` (expired) copy when Screen Recording permission is stale.

#### Microphone Boost slider e2e test coverage (#535)

- Added two Playwright specs to `tests/e2e/settings-window.spec.ts` covering the Microphone Boost slider under Settings → General → Advanced:
  - `mic-gain-db slider mounts at 0 and shows 'Off (0 dB)' label` — verifies default state and label copy when gain is 0.
  - `mic-gain-db slider mounts at persisted non-zero value and shows '+N dB' label` — verifies that a persisted value (e.g. 6 dB) is reflected in both slider position and the inline label.

#### Diarization pipeline integration test scaffold (`diarization-onnx` feature, #314)

- Added `tests/diarization_fixture.rs`: two `#[ignore]`'d integration tests that exercise the full `AudioRollingBuffer → OnnxDiarizer → speaker_label` pipeline — the exact path the meeting pump uses in production.
  - `two_speakers_get_distinct_labels` — asserts that two WAVs with distinct voices land in different `speaker_label` clusters, and that a third utterance from speaker 1 maps back to the same cluster (1-NN stability test for #316).
  - `short_audio_leaves_speaker_label_unchanged` — asserts that audio below the `MIN_FRAMES_FOR_EMBEDDING` floor (synthesized 100 ms silence) leaves `speaker_label` as `None` rather than panicking or assigning a spurious ID.
- Updated `docs/developing.md` and `tests/fixtures/README.md` with env var documentation, download instructions for the wespeaker ONNX model, and run commands.

#### Meeting pump `AudioCapture` seam integration test (`test-utils` feature, #559)

- Added `WavFileAudioCapture` / `WavFileAudioSession` in `src/audio/file_source.rs` (compiled under `--features test-utils`) — a deterministic file-backed `AudioCapture` / `AudioSession` implementation that serves pre-loaded WAV samples to the meeting pump in configurable-size chunks.
- Added `tests/meeting_fixture.rs`: an `#[ignore]`d integration test that exercises the full `SessionManager → pump → WhisperTranscription → DB` path through the seam boundary, using an in-memory SQLite database. Run with `HUSH_TEST_MODEL=... cargo test --features whisper,test-utils --test meeting_fixture -- --ignored --nocapture`.
- Updated `docs/developing.md`, `src-tauri/tests/fixtures/README.md`, and `src-tauri/Cargo.toml` with `test-utils` feature documentation and updated fixture test run commands.

### Fixed

#### Long-meeting memory leak: WhisperState reused + periodically recreated (#612)

- **Symptom:** a 35-minute meeting could grow RSS to 53 GB and not reclaim after stop, eventually grinding the host to swap. Reproduced reliably with the meeting-mode pump.
- **First-pass root cause (#615):** `WhisperInferer::infer` called `ctx.create_state()` on every inference cycle (~once per 3 s of speech). Each state init allocates ~76 MB of C-heap inside whisper.cpp that doesn't return cleanly on free.
- **First-pass fix:** a single `WhisperState` is now lazily created on the first inference call and reused for the lifetime of the streaming session. Brought 5-min meeting RSS from 53 GB → 3.5 GB.
- **Second-pass root cause:** real-session profiling on a long two-source meeting showed RSS still climbing at ~2 GB/min — ~46 MB allocated and not returned per `whisper_full` call even with the state held long. whisper.cpp's pure-CPU code path appears to do scratch allocations within `whisper_full` that don't return to the heap; not surfaced upstream because heavy users typically run Metal/CoreML paths and we ship `features = []`.
- **Second-pass fix:** every `DEFAULT_STATE_RECREATE_INTERVAL = 30` inferences (~90 s of speech per source), the streaming session drops its `WhisperState` so the next call lazy-recreates a fresh one. Pays the ~76 MB recreate cost once per ~90 s instead of leaking ~46 MB every ~3 s. Tunable via `HUSH_WHISPER_STATE_RECREATE_INTERVAL` env var.
- **Third-pass root cause (the actual dominant source):** with the second-pass fix in place, RSS still climbed at ~1.25 GB/min and held at peak for 4+ minutes after meeting stop — leak survived session end. Differential A/B (toggling Diarization OFF) cut the rate to ~250 MB/min. The OnnxDiarizer's ORT `Session` was responsible for ~80% of the leak: ORT's default CPU execution provider uses an arena allocator that never returns pages to the OS, plus a memory-pattern cache keyed on input shape. The wespeaker model takes variable-length log-Mel features (one per utterance), which is the textbook trigger for both — every new sequence length grows the arena and adds a cache entry, neither reclaimed. Documented ORT behaviour, not an `ort` Rust crate bug — see microsoft/onnxruntime#11627 / #22271.
- **Third-pass fix:** at `OnnxDiarizer` session build, register the CPU execution provider with `with_arena_allocator(false)` and call `with_memory_pattern(false)`. Allocations route through plain malloc/free (deallocations return to the OS at end of `run`); the per-shape plan cache is disabled. Perf cost is the documented 2–10% latency hit per `session.run`, invisible at the once-per-utterance cadence.
- **Bonus diagnostics:** the CoreAudio tap logs sample rate / channel count / ring footprint at init; the periodic-recreation block now logs `rss_before_mb` / `rss_after_mb` / `delta_mb` so further regressions can be diagnosed without speculation.

#### Microphone disconnect mid-session now surfaces a clear message (#587)

- When the cpal backend reports `StreamError::DeviceNotAvailable` (USB unplug, AirPods walked out of range, webcam disabled), Hush now propagates a typed `audio::DeviceLost` error with the captured device name through `Cmd::Stop` and `Cmd::DrainBuffer`.
- **Dictation:** the IPC layer downcasts to `IpcError::AudioDeviceLost(deviceName)`. The frontend renders `Microphone disconnected — the selected input source ("Foo") is no longer available. Pick a different source and try again.` with the device name in the hint.
- **Meeting mode:** the pump downcasts the same error and emits a typed `meeting:source-failed` event carrying a `deviceLost: true` flag (#617) so the frontend branches banner copy without substring-matching. The amber banner reads "Microphone disconnected — recording stopped" on single-source sessions, or "Microphone disconnected — system audio still recording" on multi-source ones (#618). Subsequent ticks skip the dead handle; other sources in the meeting keep going independently.
- Auto-fallback, reconnect-on-replug, and the `audio:device-lost` / `audio:device-restored` events are implemented in #611 (entry below).

#### Meeting mode: mic auto-fallback and reconnect on replug (#611)

- **Auto-fallback on disconnect:** when a mic source reports `DeviceLost` mid-meeting, the pump now `take()`s the old cpal handle (which triggers `Cmd::Stop` on the FIFO mpsc channel, releasing the singleton session slot) and immediately attempts to reopen the same `AudioSource` spec. `Microphone(None)` = "system default", so if AirPods disconnect and macOS has already switched the default to the built-in mic, reopening succeeds automatically.
- **SystemAudio disconnect is final:** a `DeviceLost` on the system-audio source transitions to `Dead` — the pump never tries to fall back to a microphone.
- **Reconnect watcher:** every ~5 s the pump lists available devices once (reused for all sources) and checks whether the original device has reappeared. On success it opens a fresh handle and streaming session, marks the source `Active`, and emits `audio:device-restored`.
- **State machine:** `Active` → `Fallback{original_source, original_device_name}` (capturing from fallback while watching for original) or `LostAwaitingReconnect{…}` (no fallback, watching for replug) → `Active` on reconnect, or `Dead` on permanent failure.
- **Frontend:** `audio:device-lost` (with optional `newDevice` name) shows an amber banner ("switched to Built-in Mic" or "recording stopped"); `audio:device-restored` dismisses it. New event constants in `events.ts`.
- **Stream epoch offsets:** when a streaming session is recreated mid-meeting its internal timestamps restart from 0. The pump now tracks `stream_epoch_offsets_ms[i]` (elapsed meeting time at each swap point) and adds the offset to utterance timestamps before persistence — audio slicing still uses the pre-offset stream-local timestamps so the rolling buffer index stays aligned.

#### Main window can be reopened after being closed/hidden (#590)

- Clicking the Dock icon while the main window is hidden (red ✕ or ⌘W) now brings it back to the front. Tauri's `RunEvent::Reopen` is now handled and routes through the same `tray::show_main_window` helper as the tray's "Show Hush" item.
- The Window menu gains a "Show Hush" entry. macOS's auto-managed NSWindowsMenu only lists *visible* windows, so a hidden main never appeared via the standard mechanism — this gives a discoverable, keyboard-accessible recovery path that doesn't require finding the tray icon.

#### Meeting mode system audio was always silent — IOProc replaces AVAudioEngine in CoreAudio tap (#593, #594)

- `CATapDescription.isExclusive` was `false`. With `processes = []`, `false` means "tap no processes" and delivers silence; `true` means "exclude no one" and captures all system audio. Changed to `true`.
- The readout path used AVAudioEngine pointed at an output-only aggregate sub-device, which returns silence from its non-existent input channels. Replaced with `AudioDeviceCreateIOProcIDWithBlock` — the IOProc receives the tap's PCM directly from the aggregate input bus.
- Added a device-alive wait (20 × 10 ms) before format query and IOProc start to avoid degenerate formats from HAL's async registration.
- HUSH wire-protocol header is now written before `AudioDeviceStart` so the Rust reader always sees the header before any PCM.
- Cleanup order is now correct: `AudioDeviceStop` → `AudioDeviceDestroyIOProcID` → destroy aggregate → destroy tap.
- Removed unused `AVFAudio` framework link from `build.rs`.

#### Meeting mode diagnostic counters distinguish real speech from silence (#591)

- `pump.rs` now tracks `blank_counts` alongside `final_counts` per source. At session end, the summary log emits `real_finals` (non-blank finals) and `blank_finals` (finals where Whisper returned `[BLANK_AUDIO]` or empty text) for each source, making a silent system-audio source immediately diagnostic without reading individual segment logs.
- Debug log ring buffer bumped from 200 → 500 entries (both Rust `debug_log/mod.rs` and the frontend `DebugConsole.svelte`).

#### Diarizer timeline drift on transient drain failure now corrected (#553)

- When a `drain_into` call fails for a tick, the pump now zero-fills the diarizer's rolling audio buffer for the expected tick duration, keeping its timeline aligned with the transcription session's internal clock. Previously, a failed drain left a gap in the buffer, causing `slice_ms()` to return stale or misaligned audio for subsequent utterances and degrading speaker-labelling quality for the rest of the session.

#### Toggle hotkey and command palette stop now apply trailing-silence buffer (#560)

- The toggle hotkey ("press to start, press to stop") and command palette "Stop dictation" entry previously called `dictation.stop()` with no trailing-silence delay, so the last word was silently clipped. All four stop paths (PTT key-up, record button, toggle hotkey, command palette) now consistently apply the 500 ms buffer.

#### Frontend recording lifecycle replaced with discriminated-union state machine (#558, #560)

- Replaced 7 interdependent flat `$state` variables with a single `RecordingPhase` discriminated union (`idle | starting | recording | stopping | transcribing`). Illegal state combinations are structurally impossible.
- Fixed the `transcribing` spinner: the previous `$derived` expression was always `false` because `busy` cleared in `finally` before the `setTimeout` fired. The spinner now correctly shows during result hydration after meeting stop.
- Eliminated 300 ms / 350 ms `setTimeout` delays in the stop path: `meeting_stop_manual` awaits pump drain before returning, so the result can be hydrated with a direct `await`.
- Stop-failure recovery now queries `meeting_active_session` directly rather than relying on `meeting.activeId` which could be stale after a failed `refresh()`.
- `meeting_session_get` is now called once on stop, with the result shared for both clipboard copy and the result block (previously called twice).

#### Meeting mode zero-utterance diagnosis: enhanced debug logging (#533)

- Added structured debug logs that distinguish the three root causes of "0 utterances" in meeting mode: model not loaded, audio not flowing, and Whisper no-speech filtering silently rejecting all segments.
- `streaming tick: inference ran` now logs `raw_segments` and `non_empty_segments` so Whisper no-speech suppression (`no_speech_thold`) is visible without recompiling.
- `meeting pump: inference tick` now includes `elapsed_ms` for the feed+drain round-trip, making slow-inference diagnosis straightforward.
- Added `whisper: inference complete` log at the whisper layer showing raw segment count before text-emptiness filtering.

### Changed

#### Architecture cleanup: pre-Linux/Windows structural readiness + post-#588 doc drift (#597)

- **Documentation refresh.** ARCHITECTURE.md updated to reflect the CoreAudio tap migration (system-audio backend section + module-map row + module shutdown discussion). Stale `sck_probe_lock` field and `SCK via sck_session` comments removed from the Rust source.
- **`macos_perms/` renamed to `permissions/macos.rs`** under a new `permissions/mod.rs` so future Linux (#106) and Windows (#107) permission impls have a home. Same parallel rename for `ipc/commands/macos.rs` → `permissions.rs`. IPC command names are unchanged (wire protocol stable).
- **`audio/mod.rs` (1900 lines) split** into `audio/{mod.rs, cpal.rs, tests.rs}` peers next to the existing `core_audio_tap.rs`. mod.rs is now ~530 lines containing only the trait + shared types; cpal.rs holds the cpal worker + cpal-specific tests. When Linux PulseAudio (#106) or Windows WASAPI loopback (#107) lands, those impls become peers under `audio/` rather than bloating one file.
- **`ipc/mod.rs` (2247 lines) split** into `ipc/{state.rs, builder.rs, pipeline.rs, tests.rs}` peers. mod.rs is now a 67-line front door (module declarations + re-exports). State, builder, and pipeline orchestration each have their own dedicated file.
- **`commands/dictation.rs` extracted** a `dictation/pipeline.rs` peer for the orchestration helpers (`start_dictation_inner`, `strip_whisper_brackets`, `load_vocabulary_prompt`, etc.). mod.rs keeps the Tauri command shells.

#### Lightweight Windows compile-only CI job (#597)

- New `rust-windows` workflow job runs `cargo check --no-default-features --all-targets` on `windows-latest`. Catches `cfg`-gating regressions (e.g. macOS-specific dep added without `#[cfg(target_os = "macos")]`) without paying the cmake + msvc + whisper.cpp build cost the original Windows-in-matrix decision rejected. Promotes back into the main matrix when Windows distribution lands.

#### First-run wizard simplified: permissions-first ordering, removed redundant dialog (#609)

- **Wizard now opens on the Permissions step** (was Welcome). New users hit the mandatory grants first and the explainer second — the previous order asked them to sit through copy before they could even use the app, then dropped them into a separate permissions surface.
- **Welcome step's primary button became "Start using Hush"** (was "Continue") — it's the dismiss action, not a forward step. Clarifies that the welcome screen is a parking spot, not another gate.
- **Removed the redundant third permissions dialog** that previously auto-opened after the wizard dismissed. The wizard's permission step is now the single new-user permission surface; the in-app `PermissionsDialog` only appears when a permission-denied error fires (the live trigger). Dismissing via Escape still persists the first-run flag.

#### Pre-push hook coverage extended (#592)

- The pre-push hook (`.githooks/pre-push`) now prints a friendly remediation hint when a step fails (e.g. "Fix: cd src-tauri && cargo fmt --all") instead of dumping raw tool output. EXIT trap names the in-flight step so contributors don't have to scroll back to find which gate broke.
- Opt-in slow-step gate via `HUSH_SLOW_HOOKS=1` runs `cargo test --lib --no-default-features` after the existing checks. Off by default to keep the hook interactive-friendly during rapid iteration; turn on for the final push to a PR.
- Hook header explains the cross-platform shape (every step uses `--no-default-features` so the hook works identically on macOS / Linux / Windows) and points at `learnings.md` for the rustfmt + clippy version-gap workarounds.

### Performance

#### Parallel Whisper model loads at startup (#561)

- The two `WhisperTranscription` contexts (dictation slot and meeting slot) are now loaded in parallel via `tokio::join!` with each model load on a `spawn_blocking` thread. On a warm filesystem this halves the sequential whisper load cost.
- Added `tracing::info!` timestamps at each phase of `build_default` (`database ready`, `whisper contexts loaded`, `diarizer ready`, `build_default complete`) so startup time can be profiled with `RUST_LOG=info npm run tauri dev`.

## [0.4.0] - 2026-05-05

### Added

#### Stale permission banner + guided recovery after reset (#547)

- Hush now shows an amber stale-permission banner when macOS reports any permission row as `"stale"`, with a one-click jump to Settings → Permissions.
- After a successful permission reset, the Permissions tab now walks the user through removing stale rows in System Settings, reopening Hush, and re-granting the required permissions.
- The reset flow intentionally avoids re-priming Screen Recording immediately after `tccutil reset`, preventing an unwanted prompt during cleanup.

#### In-app debug logging + issue-report plumbing (#537)

- Backend logs are now captured in an in-app ring buffer and streamed to the frontend, so the debug UI can replay startup events and stay live as new entries arrive.
- Settings → General → Advanced gained the Developer console toggle that exposes the log viewer without requiring a terminal.
- New `get_log_entries` / `get_app_version` IPCs provide the data that the debug console and issue-report generator use.

#### Microphone Boost slider in Settings (#535)

- Settings → General → Advanced now includes a Microphone Boost slider from 0 dB to +20 dB for quiet input devices.
- The gain is applied once per microphone path in both dictation and meeting transcription so boosted audio reaches Whisper and diarization consistently.
- System-audio capture is unchanged; the boost only affects microphone input.

#### Whisper Turbo in the model catalog (#519)

- The model picker now offers Whisper Turbo, a distilled Large-v3 option that trades the ~1.5 GB download for near-large accuracy at much higher speed.
- Existing download, SHA-256 verification, and auto-download plumbing work unchanged with the new catalog entry.
- The default model remains Whisper Base.

#### First-run permission wizard (#514)

- First launch now opens a two-step setup wizard instead of a prose-heavy modal, with a Welcome step followed by a focused Permissions step.
- Microphone and Input Monitoring can be requested inline from the wizard, while Screen Recording still deep-links to System Settings.
- Permission rows update live while the wizard is open, and finishing setup is a soft gate rather than a hard block.

#### Diarization on by default + first-run wespeaker download (#512)

- Speaker diarization now defaults to on for fresh installs, while existing users keep any explicit off setting they already saved.
- After a Whisper model download completes on first run, Hush now best-effort downloads the wespeaker diarizer model automatically.
- Speaker labels are shown only when at least two distinct speakers are detected, keeping single-speaker transcripts clean.

#### Audio-cue preview buttons + concurrent-cue debounce (#508)

- Settings → General → Audio cues now has preview buttons beside the start and done cue toggles, so each sound can be tested on demand.
- Preview playback bypasses the toggle gates because the user explicitly requested it, but still respects the one-cue-at-a-time debounce.
- Cue playback now silently drops overlapping requests instead of stacking many simultaneous audio streams.

#### Debug console floating window (#540)

- The debug console (Settings → General → Advanced → Developer console) now opens as a floating always-on-top palette window (`"debug"` Tauri window) instead of living inline in the Settings tab. The window stays visible while the user clicks around the app — making it practical for live debugging.
- New `open_debug_window` IPC command (`system.rs`) and matching Tauri window declaration in `tauri.conf.json` (760×520, `alwaysOnTop: true`, `visible: false`).
- New `src/routes/debug/+page.svelte` route — fully self-contained dark-terminal surface with the live log, a collapsible Issue Report generator, and "Copy Report" / "Open GitHub Issue" actions.
- A "Copy All" button in `DebugConsole.svelte` copies every visible log entry as plain text (in addition to the existing Clear button).
- Debug console toolbar colours changed from `var(--text-primary)` / `var(--bg-code)` to hardcoded terminal tokens (`#141414` / `#e6edf3`) — in light mode `--text-primary` is dark, making log text invisible on the dark console background.

#### About as a top-level sidebar section (#540)

- About is now a fourth sidebar navigation item (alongside Dictation, History, Settings) rather than a tab inside Settings. This makes it a ~one-click destination from anywhere in the app.
- An info-circle icon in `SidebarNav.svelte` identifies the About section; the sidebar `SidebarSection` type now includes `"about"`.
- The command palette "Show About" entry and the macOS native-menu "Check for Updates" event both route to the About section correctly.

### Changed

#### Dictation / Transcript / Transcription terminology (#518)

- User-facing copy now consistently uses "Dictation" for the act, "Transcript" for the output, and "Transcription" for the engine underneath.
- The History search placeholder, transcript headings, and replacements copy were updated to match the new terminology.

#### Toggle-able sidebar with persisted state (#517)

- The left sidebar now defaults to open on fresh installs, can be collapsed back to the icon rail, and remembers the user's choice in local storage.
- The collapse/expand toggle includes the matching accessibility state, and reduced-motion users skip the width transition.

#### Main page state split into focused modules (#546)

- `src/routes/+page.svelte` was decomposed into five focused state modules for audio, history, navigation, meeting sessions, and dictation.
- The main page dropped from 1,793 lines to 672 with no intended behavior change, making follow-up UI work safer.

#### Architecture maintainability sweep (#544)

- Dictation IPC commands were extracted into their own Rust module and continue to register via full module paths, reducing pressure on `ipc/commands/mod.rs`.
- Frontend IPC error/update contracts were centralized, and Playwright mocks were tightened to reduce Rust↔TS drift.

#### Developer workflow scripts + reference docs (#523, #507, #510)

- `npm run dev-reset` now kills running Hush processes and wipes local app state for clean first-run and onboarding testing.
- `npm run tauri:dmg` now ejects stale mounted Hush DMGs before building a new release artifact.
- `docs/developing.md` is now the canonical human contributor guide for local setup, command selection, and test workflows.

### Fixed

#### Debug window close, light-mode colours, and Cmd+` cycling (#543)

- Closing the debug palette (red-✕) no longer hides the main window. The debug window now uses the same `CloseRequested → hide()` pattern as the main window so focus returns correctly to the app on macOS.
- Debug console timestamps, log targets, and entry count are now readable in light mode. All colours inside `DebugConsole.svelte` now use a dedicated `--debug-*` token set defined on a `display: contents` wrapper — no `var(--text-*)` or `var(--border)` tokens that flip between themes.
- ⌘\` (Cycle Through Windows) now works. The Window submenu is explicitly registered as NSApp's `windowsMenu` via `set_as_windows_menu_for_nsapp()`.

#### Waveform sensitivity — log scale + adaptive gain (#539)

- `AudioWaveform` now maps raw RMS amplitude through a dBFS logarithmic scale rather than a linear multiplier. Conversational speech at −38 dBFS renders at ~38 % bar height instead of the near-invisible ~5 % it produced with `level × 400`.
- An adaptive ceiling tracker follows the loudest recent signal with a fast attack (~60 ms) and a very slow release (~11 s). The displayed range is always centred on what the mic or system audio is actually delivering, so quiet USB mics and boosted system capture both look equally alive without any manual gain knob.
- Both the main-window waveform and the HUD waveform benefit automatically; the HUD's `levelScale={480}` prop is now a no-op in log mode but kept for `logScale={false}` callers.
- `logScale` prop defaults to `true`; passing `logScale={false}` restores the original linear behaviour for any caller that needs it.

#### Model-state mismatch after rebuild / DB reset (#538)

- `build_transcriber` now branches strictly on whether `SELECTED_MODEL_ID` is stored in settings. When the key is absent the catalog default (Whisper Base) is tried if its file exists; when the key is present only that model is attempted — no silent fallback to the catalog default for a broken explicit selection.
- The stale "No transcription model loaded" error banner is now cleared immediately when the user successfully loads a model, even if the error was shown before the selection was made.

#### Dark mode audit — replace hardcoded colours with design tokens (#534)

- 21 Svelte surfaces now use the shared design tokens instead of hardcoded light-theme colours, fixing unreadable states across dark mode.
- Component-specific dark overrides now exist in both the OS-driven `@media` path and the manual `data-theme="dark"` path, so the in-app theme toggle matches system dark mode.
- Success, warning, and danger surfaces got missing dark overrides in About, History, model loading, and related panels.

#### HUD/waveform polish, dB meter, dark mode fixes, and title-bar cleanup (#529)

- The HUD waveform is taller, less cramped, and now shows a gentle flat line at silence instead of disappearing completely.
- `AudioWaveform` gained an optional live dBFS meter, while the main window lost the redundant custom title bar and the disabled macOS zoom button.
- Meeting-tab status cards, disclosures, and dark-mode tokens were cleaned up to match the rest of the app.

#### TCC permission reliability for bundled dev builds (#528)

- `npm run tauri:bundle` now re-signs Hush, installs it to `~/Applications/Hush.app`, and launches that copy so macOS TCC keys grants to the real bundle ID instead of a hash-based debug signature.
- `npm run tauri:dmg` applies the same re-sign step for release artifacts, and `dev-reset` / `dev-cleanup` now clean up the installed app plus legacy bundle-id leftovers.
- The macOS permissions docs were updated around the new one-command workflow for reliable permission testing.

#### Live model-download state + no-model escape hatch (#527)

- Cancelling or finishing a model download now updates the Settings UI immediately instead of waiting for an unrelated re-render.
- The "No transcription model loaded" error now includes an action button that jumps straight to Settings → Model, matching the existing first-run recovery path.

#### Bundle ID rename to io.github.khawkins98.hush (#526)

- Hush now uses `io.github.khawkins98.hush` as its bundle identifier instead of `com.khawkins.hush`, matching the GitHub namespace.
- Existing app data is migrated forward on first launch when possible, but old TCC grants do not carry across the rename and must be re-granted.

#### Theme-token hygiene — manual dark sync + `var(--danger)` sweep (#522)

- Manual Dark mode now uses the same calibrated surface tokens as OS-driven dark mode instead of an older drifting palette.
- 31 hardcoded danger colours were replaced with `var(--danger)`, making delete/error controls consistent across themes.

#### Updater polish — ErrorDisplay routing, version pinning, success-path tests (#506)

- Update-install failures now render through `ErrorDisplay`, so users get the same headline / hint / detail treatment as the rest of the app.
- The install flow now pins the expected version between Check and Install, preventing a silent version swap if the latest release changes underneath the user.
- Playwright now covers the successful install-progress path as well as the unavailable gate.

#### Meeting stop_manual close-failure race (#505)

- If `stop_manual` hits a database close failure while a new meeting is starting, the recovery path no longer clobbers the newer session.
- The old failed-close row is left for orphan-session reconciliation on next launch, preserving the retry behavior when no concurrent start happened.

## [0.3.0] - 2026-05-04

This release covers the post-v0.2.0 stretch: the menu-bar quick popover scaffolding, an audio-pipeline diagram in the welcome + About surfaces, progressive disclosure of advanced settings, four-format dictation export (text / Markdown / SRT / WebVTT), the live-waveform extraction into a reusable component, the Light / Dark / System theme override, the traffic-light permission-health model, the sidebar shell that subsumed the standalone Settings window, the Phase F vibe pass (indigo-violet accent, two-column dictation layout, spring hover, recording-pulse animation, sidebar dim-and-lock), HUD bug fixes (main-thread show/hide, pill widening for H:MM:SS, timer reset across sessions), per-event sound-cue split + cross-platform synthesis, the IPC + meeting refactor sweep, and the auto-update install flow (inert until maintainer Steps 1–4 land).

### Added

#### Sidebar shell + Settings inline (#479, #480)

- Three-window topology drops to two: standalone Settings window deleted, a 56 px icon column drives section switching inside the main window (Dictation / History / Settings).
- Native menu's "Settings…" item and tray's "Open Settings…" emit `settings:goto-tab` instead of opening a window.

#### Phase F vibe pass (#470, #471, #472, #473, #475)

- Indigo-violet accent (`#7c6ff7`) + 4-step dark-depth recalibration.
- Two-column dictation layout: source picker on the left, model chip on the right, flanking the centerpiece Record button.
- Panic-style spring hover (`scale(1.02)` + overshoot easing), Rogue Amoeba recording-pulse animation on the Stop button, ALL-CAPS Nova-style field labels, sidebar dim-and-lock during recording, reduced-motion honoured throughout.

#### Per-event sound-cue split + cross-platform synthesis (#446, #463)

- The "Audio cues" master toggle in Settings → General fans out to per-event sub-toggles ("Recording-start cue" / "Transcription-complete cue") so the user can silence one without losing the other.
- The cues themselves became cross-platform: synthesised WAVs at compile time (`build.rs::synth_cue_files`), played via `rodio` through CPAL on every platform. Replaces the macOS-only `NSSound soundNamed:"Tink"/"Glass"` path. Linux + Windows users now get the same cues macOS users already had.

#### Auto-update install flow — Steps 5–6 (#10)

- New `install_pending_update` IPC + AboutTab Install button + download progress + `updater:install-pending` handoff. Inert until maintainer-only Steps 1–4 land (signing keypair, `tauri.conf.json` `plugins.updater` block, CI signing secret, plugin registration in `lib.rs`).
- Typed `IpcError::UpdaterUnavailable` so the frontend `kind`-matches on the gate-error rather than substring-matching free-form copy.

#### IPC + meeting refactor sweep (#431 / #486, #487, #488 / #489)

- `src-tauri/src/ipc/commands/mod.rs` (3,329 LOC) split into 5 peer modules: `history.rs`, `settings.rs`, `system.rs`, `ptt.rs`, `diarizer.rs`. Drops `mod.rs` to 2,215 LOC, retaining only the dictation hot path + shared types.
- `src-tauri/src/meeting/manager.rs` (2,193 LOC) split into `manager.rs` (state shape + Drop impl) + `lifecycle.rs` (`start_manual` / `stop_manual` / `append_if_active`) + `classifier.rs` (`AppClassifier` table). Drops `manager.rs` to ~1,600 LOC.
- `RuntimeFlags` substruct + `learnings.md` 2026-05-02 sync-primitive convention closed out the audit-driven cleanup.

### Fixed

#### HUD pill width clipping at H:MM:SS + timer reset between sessions (#481, #483)

- HUD window width 250 → 290 px so "Recording 9:59:59" fits without clipping the grip / dismiss icons.
- `hud:state` payload changed from a bare string to `{ state, startedAtMs? }` so the persistent HUD page anchors `recordingStartedAt` to the backend clock — back-to-back sessions reset cleanly to 0:00.

#### Recording crash on macOS 26: HUD AppKit calls from a tokio worker (#476, #477)

- `hud::show_async` / `hide_async` wrappers dispatch onto `app.run_on_main_thread` so the AppKit `orderFront:` / `orderOut:` calls stay on the main thread. macOS 26 enforces the affinity strictly; the previous shape would `_os_crash` on `meeting_start_manual`'s tokio worker.
- Click-record unified through `meeting_start_manual` now that the HUD path is main-thread-safe — cues, partials, and the meeting pump all reach the dictation UI without a separate code path.

#### Menu-bar audit follow-ups + small UI nits

- HUD timer regression spec (`tests/e2e/hud-timer.spec.ts`) pins the back-to-back reset behaviour.
- Record button gains aria-label + status copy when no audio input devices are available.
- Settings tabs no longer overflow the toolbar at the main window's minimum width (bumped 560 → 720 px).
- `bundle_path` (history-export) replaces `debug_assert!` traversal-check with a release-build return-Err check.
- Recording-pulse animation honours `prefers-reduced-motion`.

#### Menu-bar quick-access popover (#427 Item 1, scaffolding)

- New `menu-bar` Tauri window in `tauri.conf.json` (320×220, transparent, always-on-top, decorations off, hidden by default). Capability file `src-tauri/capabilities/menu-bar.json` grants the minimum permission set: `core:event:default` for the `ui:recording-state` listener, `core:window:allow-show`/`-hide` for the "Open Hush" + Escape-dismiss flows.
- New `src/routes/menu-bar/+page.svelte` — compact popover with a state indicator dot (idle / recording), a start/stop button that drives the existing `start_dictation` / `stop_dictation` IPCs (no source-arg → falls through to `AudioSource::default_microphone()`), an "Open Hush" link, and Escape to dismiss.
- New tray menu item **"Quick popover"** (in `src-tauri/src/tray/mod.rs`) shows + focuses the window. Existing left-click-opens-menu and double-click-shows-main behaviour is preserved — switching the click idiom to popover-as-primary is a follow-up that needs hands-on macOS validation first.

Deliberately deferred to follow-ups: tray-anchored positioning (currently shows at the OS-picked default location), recent-transcript snippet, source picker.

#### Audio pipeline diagram in welcome + About surfaces (#427 Item 3)

- New `src/lib/AudioPipelineDiagram.svelte` — inline-SVG left-to-right diagram of the active capture path: Microphone (and System audio when a meeting is active) → Whisper engine → Transcript. Source / engine / output nodes pick up CSS tokens (`--accent`, `--bg-surface`, `--text-on-accent`) so dark-mode and the manual `data-theme` override flow through without per-theme variants.
- **Embedded in `FirstRunModal.svelte`** as a visual lead-in above the permissions sections — a first-time user sees the chain ending in their clipboard before reading any copy.
- **Embedded in `AboutTab.svelte`** as a later-encounter "how it works" explainer for users who skipped or forgot the first-run modal.
- Caption: *"Audio stays on your device end-to-end."* — restates the privacy posture in the same place a user is taking in the routing diagram.

The Phase C carry-over from #411 (a separate `WelcomePanel.svelte` surface) is intentionally not built — the existing `FirstRunModal.svelte` already covers the welcome experience with thoughtful a11y plumbing (focus trap, escape key, role=dialog), so the diagram embeds there directly rather than duplicating into a new component.

#### Progressive disclosure in Settings (#427 Item 2)

- New `src/lib/AdvancedSection.svelte` — a reusable `<details>`-style disclosure wrapper that hides power-user controls behind a labeled toggle. First-time visitors see only the essentials; power users click once and see everything.
- **Settings → General → Advanced** now wraps the *Performance* (transcription threads slider) and *First-run welcome* sections.
- **Settings → Meeting → Advanced — app overrides** now wraps the `MeetingAppOverridesPanel` so a fresh Meeting tab leads with the auto-start mode and Speakers toggle, not a row of empty form fields.
- Per-session expansion only — `open` is local component state. The audit's read was that settings are rarely deep-dived, so per-key persistence wasn't worth the complexity.

#### Rich single-dictation export formats (#427 Item 4)

- **Export-format picker** below the transcript in `ResultBlock.svelte`. The dictation transcript is still on the clipboard automatically as plain text; the picker re-writes the clipboard with the chosen format on click.
- Four formats: **Plain text**, **Markdown** (heading + body), **SRT** (single cue covering the captured duration), **WebVTT** (single cue, `.` separator + `WEBVTT` header).
- Last-used format is persisted in `localStorage["hush.export.format"]` and highlighted on next paint so a user who exports as the same shape repeatedly sees their preference surface.
- Pure conversion logic lives in `src/lib/export-formats.ts` so the helpers are reusable if a future surface (meeting-session export, quick action) wants the same formats.

#### Live waveform in main window's recording row (#411 phase B)

- Extracted the HUD's inline waveform animation (attack/release smoothing + 14-bar ring buffer + `audio:level` subscription) into a reusable `src/lib/AudioWaveform.svelte` leaf component. Default palette matches the HUD's red gradient; consumers can override via `--audio-waveform-bar-color`.
- Main window's `ControlsSection` now renders the waveform below the recording status row whenever `recording === true`. Dictation and meeting modes both feed the same `audio:level` pump, so the bars track mic activity in either path.
- HUD pill still renders the waveform; behaviour and visuals unchanged. `audio:level` listener moved from the HUD's `+page.svelte` into the component, so the HUD now only owns the elapsed-timer + lifecycle-state listeners.

#### Appearance / theme override (#411 phase A)

- **Settings → General → Appearance** picker (System / Light / Dark) lets users override the OS dark-mode preference. System (default) follows `prefers-color-scheme`; explicit values force the chosen theme regardless.
- **Persistence + cross-window sync** — preference stored in `localStorage["hush.theme"]`; root layout reads + applies synchronously at script-evaluation time so first paint already reflects the choice (no light-to-dark flash). A `hush:theme` Tauri event broadcasts changes so Settings → main → HUD all flip together.
- `app.css` gains explicit `:root[data-theme="light"]` and `:root[data-theme="dark"]` blocks alongside the existing `@media (prefers-color-scheme: dark)` block; `settings-tab.css` rules are gated with `:not([data-theme="light"])` and mirrored under `[data-theme="dark"]` so the manual override beats the OS preference.

#### Permission health: traffic-light staleness model (#378, #381, #382, #383, #384)

Three-state per-permission verdict (`confirmed` / `stale` / `not-granted`) layered on top of the live OS grant state, persisted via `permissions_*_last_confirmed` settings keys.

- **Backend** (#381) — `get_permission_health` IPC + `evaluate_permissions_health` classifier. `confirm_permission` writes a Unix-epoch-millis stamp from `start_dictation`'s success path so a future probe that flips to false against an existing stamp reads as Stale (was-granted-now-revoked) rather than NotGranted.
- **Wiring + SCK validation** (#382) — `confirm_permission` fires from `stop_dictation` (mic) and meeting-start (screen-recording) success paths; `validate_screen_recording_capability` runs a real `SCShareableContent::get()` call before stamping so a preflight=true / capability-actually-broken case (post cert/bundle-id rotation) is caught.
- **Reusable PermissionsDialog** (#383) — extracted `PermissionsRows` + `PermissionsDialog` from the Settings tab; chained from FirstRunModal's Got It dismiss for first-run actionability and popped from `meeting_start_manual` failures so the next click opens System Settings rather than getting buried in an error chip. Closes #232.
- **Unified Record flow** (#384) — single Record button auto-detects multi-speaker capability. SCK confirmed + mic source → meeting pump (mic + system audio); anything else → existing `start_dictation`. Mic-only badge with stale vs not-granted variants. Single-source diarizer guard skips the ONNX call when only one source bucket arrives. Closes #369 (UI slice).
- **Auto-copy parity** (#398) — click-driven Record in meeting mode now writes the joined transcript to the clipboard on Stop, matching the dictation path's instant-paste UX. Closes #385.
- **In-flight mode visibility** (#414) — recording status line shows "Recording · mic only / mic + system audio" so the mode change is visible mid-record, not just after Stop. Closes #409.
- **Auto-copy outcome notice** (#415) — success/failure toast above HistoryPanel; failure variant points at the History meeting row for manual recovery. Closes #408.
- **Typed PermissionDenied variant** (#416) — `IpcError::PermissionDenied(<permission>)` replaces substring scraping the chained message. Classifier runs at the IPC boundary (meeting + dictation start). Frontend `isPermissionShapedError` switches to discriminant check. Closes #386 partial.
- **Upfront mic AVAuthorizationStatus probe** (#424) — cpal's mic-denial chain doesn't include "microphone", so the post-call classifier rarely fired. Probing AVAuthorizationStatus before opening cpal surfaces the typed variant directly. Closes #417.
- **SCK probe race-protection** (#422) — `tokio::sync::Mutex` around the SCK auto-confirm so concurrent `get_permission_health` calls (window-focus + startup probe) don't each `spawn_blocking` the Cocoa round-trip. Closes #386.

#### Settings monolith decomposition (#332, #387, #389, #390, #391, #394, #396, #397, #392)

Six tab components extracted from the 2.4k-LOC `settings/+page.svelte`. Page becomes a thin tab-switcher (~500 LOC) wrapping lazy-mounted children that own their own state, IPC, and lifecycle. Cumulative shrink: **2428 → ~500 LOC (-79%)**.

- `PermissionsTab.svelte` (#387) — diagnostic + reset + permission-rows
- `VocabularyTab.svelte` (#389) — vocabulary CRUD + form
- `ReplacementsTab.svelte` (#390) — replacement CRUD + form
- `GeneralTab.svelte` (#391) — autostart + HUD + audio cues + transcription threads + PTT (largest slice, -590 LOC alone)
- `MeetingTab.svelte` (#394) — auto-start + diarization + diarizer-model installer + app-classifier overrides
- `AboutTab.svelte` (#396) — app metadata + manual update probe + license/source links. Closes #332 phase 1.
- `settings-tab.css` (#397) — shared module for the card primitives (`.settings-group`, `.toggle-row`, `button.ghost`, etc.) so palette tweaks land in one place. Closes #392.

#### Dictation usage stats (#293, #399, #407, #412)

Aggregate `get_dictation_stats` IPC + summary tile bar above History list: Sessions / Words / ~Saved (40 wpm baseline) / ~Keystrokes. Hidden when `sessionCount === 0`. Pluralization + estimate-honesty pass in #407. Multi-space word-count behaviour pinned in #412 (closes #406).

#### Test scaffold for IPC commands that emit Tauri events (#315, #400, #402, #420, #421)

- `ipc::events::EventEmitter` trait (#400) wraps `AppHandle::emit` so spawned-task paths can be driven from `#[tokio::test]` without a real Tauri runtime. `RecordingEventEmitter` (test-only) captures `(event, payload)` pairs into a `Mutex<Vec<…>>`.
- `download_diarizer_model_inner` extraction (#400) + `model_download_inner` extraction (#402). Both test the duplicate-rejection guard + cancel-handle cleanup-on-failure.
- `start_dictation_inner` updated to use the classifier (#420), and the Whisper download's exists-check moved inside the lock to close the audit-2 TOCTOU (#421). Closes #315.

#### Build + CI hardening

- `MACOSX_DEPLOYMENT_TARGET` + `CMAKE_OSX_DEPLOYMENT_TARGET` set via `.cargo/config.toml` `[env]` table (#404), so `npm run tauri dev` and bare `cargo build` both inherit the 14.0 baseline without shell-environment dependency. Closes #393 follow-up.
- New CI job `cargo-env-allowlist` lints the `[env]` keys against a hardcoded list (#413, hardened to `tomllib` parser in #423 to close edge-cases around quoted / indented / dotted-table keys). Closes #410, #418.

#### UI: brand icon refresh (#395, #401, #405)

- Transparent SVG over the white-bg PNG (#401) — no more white badge against the grey toolbar
- Custom microphone glyph for the 22 px optical size (#405) — replaces the original speech-bubbles-in-shield asset that read as "chat" rather than dictation. Closes #395.

#### Other UX + cleanup

- Permissions UX & copy polish (#388, #403): `confirm_permission` returns `IpcError::Internal` instead of `Settings` for unknown tokens; stale-hint copy softened to be observable rather than causal; PermissionsDialog default intro trimmed; focus-event debounce on `refreshPermissionHealth` (250 ms trailing edge).

### Fixed

- Settings → Meeting → Speakers errors are no longer hidden by
  the post-failure refresh (`loadDiarizationEnabled` was
  clobbering `diarizationError` on a successful read; the setter
  now owns that field exclusively). Caught by #302 e2e.

### Notes

- Diarization (D2) entry that previously lived here moves into the next tagged release alongside the headline items above. The work landed in PRs #295–#305 with the audit follow-up; behaviour is unchanged.

  Original entry: opt-in per-speaker labels in meeting transcripts ("Speaker 1", "Speaker 2", …) via the wespeaker ResNet34-LM ONNX model. Settings → Meeting → Speakers toggle persists; first-time enable auto-downloads the 26 MB model from Hugging Face (SHA-256 verified) and hot-swaps the diarizer without an app restart. Falls back to source-tagged You / Remote labels when off or unavailable. New `diarization-onnx` Cargo feature (default-on) gates the ort runtime + ndarray + realfft deps; contributors building `--no-default-features` skip the ~50 MB ORT vendored libs. Closes #111.

## [0.2.0] - 2026-04-30

This release covers the post-v0.1.0 meeting-mode pivot work plus the
post-#143 IA redesign and platform-polish stretch. Grouped by PR so
a reader can scan which features shipped in which change — the
unreleased queue grew long enough during the pivot scaffold that a
single flat list was hard to navigate.

Headline features beyond v0.1.0: full Meeting Mode (mic + macOS
system-audio capture with You/Remote-tagged transcripts, live
streaming partials, search + Copy transcript), three-window IA
(main + standalone Settings + transparent HUD), native macOS menu
bar + tray icon, manual update checker, autostart, first-run
welcome, programmatic TCC permission detection, and the cross-
platform release pipeline (macOS Apple-Silicon-only, Linux, Windows
artefacts attached to GitHub Releases on `v*` tags).

### Added

#### Permissions tab redesign + active-window title metadata (#247, #259)

- **Permission row redesign (#247)** — status moves from a floating uppercase label into a sentence-case coloured pill inline with the title (System Settings → Privacy & Security idiom). Right edge belongs solely to the action button. The dot is dropped; the pill's background carries the colour signal.
- **App-title subtitle on session rows (#259)** — captures the active window's title at session-open and renders it as an italic subtitle when distinct from `appName`. A YouTube-in-Vivaldi session reads as `Vivaldi — <video title>` rather than "Vivaldi" alone. Migration `0005_meeting_sessions_app_title.sql` adds nullable `app_title` column. Backend captures via `active-win-pos-rs` at the IPC entry / autostart-poller `Start` outcome — frontend doesn't need direct OS access.

#### Resilience: orphan-session reconciliation + double-close guard (#277)

`SessionManager::reconcile_orphan_sessions` runs at boot via `lib.rs::setup`. Sessions left open by a previous process that exited via kill / OS crash / panic now get `ended_at` stamped on next launch instead of staying as ghost "in-progress" rows. New `MeetingSessionRepository::list_open_sessions()` is the underlying primitive. Pairs with a `close_attempted: bool` flag on `ActiveSession` so a `stop_manual` retry after a transient SQLite failure goes straight to retrying the DB write instead of cycling pump-shutdown work that already completed. +2 lib tests.

#### Updater redirect policy: HF→signed-CDN chains (#278)

The model-download redirect closure used a per-hop `huggingface.co` / `hf.co` allowlist; HF's CDN now sometimes redirects to signed S3/R2 URLs whose hosts aren't on those zones. Browser-like trust model: a hop is allowed if the destination is on an HF host **or** the immediately-previous URL was. HTTPS-only enforced regardless. Hop-cap fires before host checks. Logic extracted to a pure `redirect_decision()` and pinned with seven cases.

#### Multi-agent review #2 follow-ups + small fixes (#260, #276, #279, #280, #281)

Outcomes from the second review cycle plus the colleague-issue triage:

- **Capability tightening (#260)** — main + Settings windows downsized from `core:default` (9 subsets) to the actual minimum (`core:event`, `core:app` for Settings). Closes #238.
- **Source-label drift fix (#276)** — `AudioSource::speaker_tag()` is now the single source of truth for the persistence-layer short form (`"mic"` / `"system"`). Pre-fix, #244's sources column used `kind_label()` (`"microphone"` / `"system-audio"`) while the dispatch sites used hand-rolled short forms; the frontend chip rendered the literal long forms through its default case. + zeroize on `SlidingWindowState::Drop` (#250), `get_by_id()` for O(1) session lookup + dead `session_started_at` removal (#253), `sha2 = "0.10"` pin (#256 part 3).
- **HUD UX trio (#279)** — `acceptFirstMouse: true` (one-click dismiss), `orderFront:` instead of `makeKeyAndOrderFront:` so the HUD doesn't steal keyboard focus, cursor-monitor lookup so the HUD lands on the active display.
- **Window lifecycle (#280)** — `CloseRequested` on main + Settings now hides instead of destroying. Autostart launches with `--background` and switches to Accessory activation policy so the LaunchAgent doesn't pop the main window at every login.
- **Tray + menu (#281)** — fix tray accelerator (was ⌘⌥H, the macOS "Hide All Other Apps" shortcut; now ⌃⌥H, matching the actually-registered hotkey). Menu "Check for Updates…" fires the probe directly and emits `Events.UpdaterResult` for the Settings About tab to render — one-click instead of two.

#### Onboarding: Screen Recording explainer in the first-run modal (#283)

Adds a third permissions section to the first-run modal between Input Monitoring and the footer. Walks the user through Meeting Mode's Screen Recording requirement (macOS bundles system-audio capture under that TCC category despite the name; Hush captures no pixels) and provides a deep-link to the Screen & System Audio Recording pane. Pre-fix users hit an unexpected TCC prompt the first time they tried Meeting Mode and reflexive dismissals silently broke the feature. Closes #269.

#### Multi-agent review #4 follow-ups + small fixes (#284, #285, #286)

- **Review #4 follow-ups (#284)** — `lib.rs::setup` hide-on-close had a redundant `prevent_close()` call in the failure branch (dropped); menu Check for Updates `PROBE_INFLIGHT` now uses an `InflightGuard` RAII struct so a panicking probe doesn't permanently disable the menu item; `is_background_launch` extracted to a testable helper with four unit tests; About-tab `updater:result` listener gated on `!updateChecking` to prevent double-announce; CLAUDE.md / learnings.md / CHANGELOG drift caught up.
- **Tray template icon (#285)** — pre-fix the tray builder fed `default_window_icon()` (full-colour RGBA) to `icon_as_template(true)`, producing a black blob on light menu bars. Generated `src-tauri/icons/tray-icon@2x.png` (32×32) by alpha-extracting the brand mark with `magick`; loaded via `Image::from_bytes` (gated behind a new `image-png` Tauri feature). Closes #275.
- **Plugin-os swap (#286)** — replaced the deprecated `navigator.platform` regex match in `+page.svelte` and `settings/+page.svelte` with `platform()` from `@tauri-apps/plugin-os`. New JS dep + Rust crate + plugin registration in `lib.rs`. Closes #272 (the part #282 had deferred).

#### Meeting Mode UX polish: listening pill, stopping banner, Copy transcript, source chips (#241, #243, #244)

Hands-on testing surfaced a cluster of visible-silence / missing-affordance gaps in the active-session UX:

- **Listening pill** (#241) — pulsing-shimmer indicator below the utterance counter that shows "Listening — last update *N*s ago" between utterances, or "first utterance can take ~10 s while the chunk window fills" before the first one. Whisper inference on a 10-s chunk takes several seconds on slower machines / larger models; the gap was reading as a hang.
- **Stopping banner** (#241) — replaces the Stop button between confirmation and the backend reporting `activeSessionId === null`. Same shimmer-bar visual idiom carries the "still working, just hold on" signal during the 10–15 s pump-drain window. 30-s watchdog clears the banner if the backend genuinely hangs.
- **You / Remote source labels** (#243) — replaces the broken D1 EnergyDiarizer wiring. The silence-gap heuristic collapsed cross-source utterances into "Speaker A" everywhere; reverted to source-only labels routed through `AudioSource::speaker_tag()` which the panel maps to "You" for mic and "Remote" for system audio.
- **Copy transcript** (#243) — button on the active session and on each historical session row (after expand). Plain-text format (one block per utterance, speaker + offset + text) lands in the clipboard via `navigator.clipboard.writeText`. Flashes "Copied!" for 2 s on success.
- **Source chips on session rows** (#244) — migration `0004_meeting_sessions_sources.sql` adds a CSV column persisting the captured sources at session-open. Renders "Mic + System audio" / "Mic" / "System audio" alongside the existing app-classification chip so a session whose foreground was a browser (classifier returns "Other") still shows what was actually recorded.

#### macOS permissions UX: SCK priming, auto-refresh, de-jargoned copy (#240, #245)

Two trip-hazards on the Permissions tab:

- **SCK priming on Grant click** (#240) — the per-row "Grant in Settings…" button on Screen Recording was deep-linking into a System Settings list that didn't yet contain Hush, because macOS only enrols an app once it actively requests the permission. New `prime_screen_recording_permission` IPC calls `SCShareableContent::get()` (lightweight enumeration that triggers the same TCC check as a full capture stream) before the deep-link, guaranteeing the row appears.
- **Auto-refresh + manual Refresh button** (#245) — the diagnostic was loaded once on mount and never re-checked, so the panel stayed frozen on "NOT YET GRANTED" after the user toggled a permission in System Settings. Now re-reads on Settings-window focus + a manual button covers side-by-side / keyboard-only cases.
- **De-jargoned recovery copy** (#245) — "reset all four TCC entries" replaced with "reset all four permission grants (Microphone, Screen Recording, Input Monitoring, Accessibility)".

#### Autostart poller: injectable probe trait + 9 wiring tests (#237)

The meeting auto-start poller (`run_meeting_autostart_poller`) had untested wiring around `active-win-pos-rs::get_active_window` → classifier → `AutostartDecision::decide`. Extracted the pure logic into `meeting::autostart_poller::evaluate_autostart_tick`, gated the OS call behind a `ForegroundAppProbe` trait, and added 9 tests covering off-mode reset, probe-failure no-change, transition-into-meeting Start, steady-state silence, session-active block, and classifier-override propagation.

#### Updater HTTP coverage: 9 wiremock tests (#236)

`updater::check_for_updates` had 5 unit tests (helpers + serialisation) but no HTTP-level coverage. Added `check_for_updates_at(client, url, current_version)` that tests can point at a local wiremock server, plus 9 tests covering up-to-date / update-available / 404 / 5xx / oversize body / malformed JSON / non-semver tag / unprefixed-tag normalisation / malformed `CARGO_PKG_VERSION`.

### Changed

#### Diarization wiring: `EnergyDiarizer` → `NoopDiarizer` (#243)

D1 silence-gap heuristic was reverted to `NoopDiarizer` after hands-on testing showed it collapsed cross-source utterances into a single "Speaker A". The heuristic operates on a chronologically-merged stream; with mic + system audio finals interleaving, there's no reliable inter-source gap for the heuristic to lock onto. `EnergyDiarizer` impl + tests stay on disk for the mic-only path; D2 (model-based ONNX, #111) is the upgrade.

#### HUD capability tightened: `core:default` → minimum needed (closes #238 partial, #239)

The HUD window's `capabilities/hud.json` granted `core:default` (a bundle of nine permission subsets). The HUD's actual API surface is two calls (`listen('audio:level', …)` + `getCurrentWebviewWindow().hide()`); replaced the umbrella with `core:event:default` + `core:window:allow-hide` so unused subsets are no longer dead surface area an attacker would inherit. Settings + main window tightening tracked separately in #238.

#### GitHub Actions SHA-pinned (#235)

Replaced floating-tag refs (`@v4`, `@v0`, `@stable`) across both workflow files with full commit SHAs + tag-name comments. Floating tags are mutable — a compromised action release pipeline can push a malicious commit under an existing tag. SHAs are immutable.

### Documentation

#### Periodic verification cadence in CONTRIBUTING.md (#234)

CONTRIBUTING gains a "Periodic verification cadence" section covering the multi-agent review pattern (writer / Rust / UX / security agents in parallel), UX walkthrough re-run, mechanical sweep, and the `learnings.md` round-record step. Cadence: every 2–3 substantial PRs while solo-maintained.

#### Multi-agent review follow-ups round 1 (#234)

First sweep using the cadence above. Synthesised findings shipped as a single PR: `IpcResult` promoted crate-public, updater 15 s timeout + 64 KiB body cap, action-led permissions banner, Settings header hierarchy fix, Model picker "Default" → "Selected" badge, HUD dismiss visibility bumped, Meeting panel "(coming soon)" → "(macOS only today, #106/#107)".

#### Meeting Mode auto-start lifecycle (#221, #219)

Auto-start a meeting session when a known meeting app comes to
focus — the open piece of #112's per-app classifier. Settings →
Meeting → Auto-start dropdown (Off / Always; "Ask" reserved for
the future prompt UI). A 3 s foreground poller spawned at boot
watches `active-win-pos-rs::get_active_window`, classifies via
`AppClassifier::default_table()`, and fires
`start_manual` on a transition into a Meeting verdict. The
classifier table itself expanded under #219 to cover macOS bundle
ids, Linux process names, and Windows .exe basenames for the
top meeting + media apps (Zoom, Teams, Discord, Slack, Webex,
Skype, GoToMeeting, BlueJeans, Loom, YouTube, Spotify, Apple
Music, iTunes, Apple TV, Podcasts, VLC, Plex / Plexamp).
Manual-start unchanged; auto-stop on app blur deferred. Off by
default — auto-recording the mic without an explicit opt-in is a
trust-loser.

#### Recording HUD on/off toggle (#218, #220)

Settings → General → Interface → "Show recording HUD" lets users
suppress the floating pill for both dictation and meeting mode.
Default on. Backend uses an `AtomicBool` mirror so the sync
`start_dictation` hot path reads the flag without locking. #220
added focused Rust unit tests for the IPC + the boot-time
persistence parse.

#### Meetings search filter (#216)

A `<input type="search">` in the Meetings panel header (visible
once at least one session exists) filters historical sessions
against `appName` and `notes` — frontend-only substring filter,
no FTS round-trip. Mirrors the History panel's affordance.

#### Manual "Check for updates" + release pipeline (#222, #223, #226, #227)

Hush now ships pre-built binaries via GitHub Releases. A new
`.github/workflows/release.yml` fires on `v*` tag pushes (or
manual `workflow_dispatch`), runs the build matrix on
macos-latest / ubuntu-latest / windows-latest via
[`tauri-action`](https://github.com/tauri-apps/tauri-action), and
attaches `.dmg` (Apple Silicon, macOS 26+), `.AppImage`, `.deb`,
`.msi`, and `.exe` artefacts to a draft GitHub Release.
Maintainer recipe in [`docs/releases.md`](./docs/releases.md).
Intel macOS not in the matrix — macOS 26 is Apple-Silicon-only
per project policy. macOS deployment target is 14.0 (the
Xcode 16.4 / macOS 15 SDK ceiling on `macos-latest`); design
target stays macOS 26.

Alongside the pipeline, a manual "Check for updates" probe
(#223 / #227) lets users find out when a new release is published —
Settings → About → "Check for updates" or
`Hush → Check for Updates…` from the macOS menu bar. Hush does
**not** auto-check on launch; every check is a click. The
backend's single read-only request to GitHub's
`/repos/khawkins98/Hush/releases/latest` returns one of three
results: up-to-date, an update is available (with a release-page
link), or the check failed (offline / rate-limited). The full
auto-update channel via `tauri-plugin-updater` (#10) is the
natural follow-up — gated on a signing-key decision.

First-wave releases ship unsigned: macOS users see a Gatekeeper
warning on first launch, Windows users see SmartScreen.
Code-signing (Developer ID + notarisation, EV cert) is on the
roadmap.

#### UX walkthrough polish round (#225) + macOS Permissions smoothing (#231)

A Playwright screenshot pass (`tests/e2e/zz-uxwalk.spec.ts`)
captured every screen / state and surfaced 9 visual / structural
fixes shipped under #225: active-model chip moved into the
section header (right-aligned status badge, no longer floating
mid-page), Settings → Meeting Auto-start group cardified to
match other settings rows, App-classification rows lose the
redundant kind-label `<span>`, Meeting-Mode panel guidance
de-duplicated (the "Click Start to begin recording…" sentence
folded into the "How it works" disclosure), session "MEETING"
chip softened from uppercase blue to sentence-case quiet variant,
History rows show model display names ("Whisper Base") instead
of raw filenames, Settings → Permissions restructured to be
action-led (3 buttons + "Why isn't Hush in the list?"
disclosure for the bundle-id forensics), `tccutil` mention
dropped from end-user copy, FirstRunModal section divider
strengthened.

The Permissions surface got a follow-up under #231:

- **Real bug fix:** the Reset button silently skipped Screen
  Recording (it ran `tccutil reset` for Microphone +
  ListenEvent + Accessibility but not ScreenCapture). Now resets
  all four.
- **Per-row "Grant in Settings…" buttons** on each permission
  card deep-link to the right System Settings pane.
- **Better post-reset copy** spells out the `−` button cleanup
  step for stale Hush.app rows that survive a `tccutil reset`
  (different signing identity from a previous build).
- **Dev-loop docs** in `docs/macos-permissions.md` explain the
  ad-hoc-signing identity churn that produces multiple
  `Hush.app` rows in System Settings, plus the canonical
  recovery procedure.

#### Refactor wave: state-layer extractions and event centralisation

The post-IA-redesign cleanup wave landed across several PRs:

- **FirstRunModal + MacosPermsPill extracted** from `+page.svelte`
  (#212), dropping the page from ~1.5k → ~1.15k LOC.
- **`lib/events.ts`** centralises Tauri event names (#214) so a
  typo on one side becomes a TypeScript error rather than a
  silent listener that never fires.
- **ModelFetch state bundled** in the Settings window (#215) —
  six `$state` declarations collapsed into one `modelFetch`
  struct, Map-mutation patterns simplified.
- **Active-model chip on Meetings + ModelPicker hint refresh**
  (#213) — Meetings get the same status chip Dictation has;
  picker hint copy aligned with the auto-download UX.
- **Click-to-confirm consistency** on ModelPicker Remove +
  AppOverrides Remove (#211) brings them under the same
  destructive-action pattern as History / Vocabulary /
  Replacements.
- **Drift round 2 docs** (#210) — sweep of stale comments and
  copy after a string of recent shipping (Phase C/D/E framing,
  `formatError` switch references, etc.).

#### IA redesign — sidebar + ⌘, Settings window + native macOS menu (#163, #164, #165, #167, #176)

The main window grew a left sidebar (Dictation / Meetings / History)
and a standalone Settings window (`⌘,` on macOS, "Settings" footer
button on the sidebar). The Settings window hosts the model picker,
vocabulary terms, replacement rules, macOS permissions diagnostic,
and a real General tab (autostart toggle via
`@tauri-apps/plugin-autostart`, hotkey display, "Show welcome on
next launch" first-run reset).

- **Phase 1 (#163):** sidebar nav inside the main window. New
  `AppSidebar.svelte` with brand mark, three section buttons +
  badges (history count, meetings count, animated red dot when a
  meeting is active), plus a temporary "Configuration" tab that
  Phase 3 emptied.
- **Phase 2 (#164):** Settings window scaffolding. New `settings`
  Tauri window, capability file, route, and `crate::settings_window`
  module (`show()`/`hide()` symmetric with the HUD module). Native
  macOS menu bar via new `app_menu` module: `Hush → Settings…`
  bound to `⌘,`, `View → Dictation/Meetings/History` bound to
  `⌘1/⌘2/⌘3`. Sidebar app-icon swapped from the "H" letter mark
  to the actual product icon.
- **Phase 3 (#165):** lifted the four config panels out of the
  main window into the Settings window. Cross-window invalidation
  is event-driven where it matters (`model:download-done` is
  broadcast; replacements/vocab changes pick up at the next
  dictation invocation). +page.svelte dropped ~158 LOC.
- **Phase 4-equivalent (#167):** real General tab — startup,
  hotkeys, first-run reset.
- **Deep-link from main window:** clicking "Open the Permissions
  diagnostic" on the Dictation tab fires a `settings:goto-tab`
  Tauri event so the Settings window opens directly to the
  Permissions tab.
- **E2E coverage (#176):** new `tests/e2e/settings-window.spec.ts`
  covering toolbar nav, the deep-link event, autostart toggle
  state, first-run reset confirmation, and PTT editor mount.
  9 new specs; full suite now 30 passing.

#### Live macOS TCC permission detection — green pill when granted (#166)

`diagnose_macos_permissions` now reads the actual grant state for
Microphone, Screen Recording, and Input Monitoring without
triggering OS prompts:

- New `crate::macos_perms` module uses
  `+[AVCaptureDevice authorizationStatusForMediaType:]` (mic),
  `CGPreflightScreenCaptureAccess()` (screen recording), and
  `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)` (input
  monitoring). All three are passive reads — calling them does NOT
  trigger the prompt.
- The earlier "TCC isn't programmatically readable" claim was
  overly broad; it's true for some buckets (Accessibility, Full
  Disk Access) but **false for the three Hush actually touches**.
- Frontend renders a compact green "macOS permissions OK" pill on
  the Dictation tab when mic + screen recording are granted (and
  input monitoring isn't denied — `not-determined` is acceptable
  since PTT is opt-in). Otherwise the existing yellow recovery
  hint stays.
- Settings → Permissions tab leads with three per-permission
  status rows (granted / denied / not-determined) + the existing
  collapsible diagnostic for recovery actions.
- Direct dependency on `objc2` (already transitive via
  `screencapturekit`) for the AV class-method call. CoreGraphics
  + IOKit reached via raw `extern "C"` since the function shapes
  are simple.

#### Configurable push-to-talk combo + in-app Enabled toggle (#170, #174)

PTT becomes a real settings UI rather than an env-var-only feature:

- New `PttCombo` type — sorted, deduplicated set of `PttKey`s.
  Single-key combos (the common case) work the same as the
  previous `PttKey`; multi-key combos like `Right ⌘ + Right Shift`
  fire when all keys are held simultaneously and release when any
  one releases.
- New `ComboMatcher` state machine — tracks held keys, emits
  `Pressed`/`Released` transitions on edges only (repeat
  `KeyPress` events from key-repeat don't double-fire). 8 new
  unit tests pin the matcher's edge semantics.
- `AppState` gains `ptt_combo: Arc<RwLock<PttCombo>>` and
  `ptt_active: Arc<AtomicBool>`. Listener thread reads both per
  event so a Settings UI edit takes effect on the next keystroke
  without restarting the rdev thread.
- Two new settings keys: `ptt_combo`, `ptt_enabled`. Env vars
  (`HUSH_PTT_ENABLE` / `HUSH_PTT_DISABLE` / `HUSH_PTT_HOTKEY`)
  still work as overrides.
- IPC: `ptt_get_config` / `ptt_set_config`. The latter
  hot-swaps the shared lock + atomic.
- New `PttHotkeyEditor.svelte` lives in Settings → General →
  Hotkeys. Three pieces: Enable checkbox, capture surface
  ("Record new combo…") that records held keys until release,
  and Reset-to-default. Letters / digits / arrows are silently
  ignored to prevent foot-gun bindings.
- Mac-friendly chip rendering (`⌘ ⌥ ⇧ ⌃`) on macOS, plain names
  elsewhere.
- **#174: listener-on-demand.** Toggling Enabled in Settings
  spawns the rdev thread immediately rather than requiring an app
  restart. The spawn is idempotent via a new
  `ptt_listener_spawned: Arc<AtomicBool>` latch. On macOS this is
  also when the Input Monitoring permission prompt fires — but
  after a deliberate user click, not at app boot.

#### HUD: drag handle + dismiss button (#162)

The recording overlay pill becomes movable + dismissible without
stopping the recording:

- Drag via Tauri 2's `data-tauri-drag-region` attribute on the
  pill root (replaces the older `-webkit-app-region: drag` CSS
  with known macOS quirks). Cursor switches to grab/grabbing.
- Dismiss via a small ghosted X button on the right edge. Click
  hides the HUD window without affecting the in-flight recording;
  the next dictation/meeting start re-shows it.
- Window width bumped 220 → 250 px to fit the X.

#### Layer 1 native UI — `color-scheme` + `accent-color` (#171, #172)

Two CSS primitives that cost nothing but pick up real native
behaviour from the user agent:

- **System font stack (#171)**: `-apple-system, BlinkMacSystemFont,
  "Segoe UI", Roboto, …` so each OS picks its native UI face
  (San Francisco on macOS, Segoe UI on Windows, distro default on
  Linux). Replaced the old `Inter, Avenir, Helvetica, Arial`
  cascade which fell through to Helvetica on macOS — noticeably
  off against the modern macOS UI.
- **`color-scheme: light dark`** + **`accent-color: auto`** on
  the main and settings windows (#172). User agent uses native
  dark scrollbars / form chrome instead of the light-mode default,
  and OS accent (Mac Highlight blue, Windows accent, GNOME
  accent) drives checkboxes / radios / range sliders. The HUD
  intentionally keeps its hand-rolled dark pill.
- Layer 2 (per-OS class on `<html>` + targeted overrides) tracked
  separately under #173 — deferred until macOS-only hands-on
  coverage stops being a liability for sight-unseen
  Windows/Linux work.

#### History entries: populate `duration_ms` + render in panel (#159)

The schema field had been `null` for every row since launch. Now
populated from the captured sample buffer's
`frames / (sample_rate × channels)` at `stop_dictation` time, with
saturating arithmetic against the impossible zero-format case.
HistoryPanel renders the duration after the timestamp using a
compact format: `0.4s` / `12s` / `m:ss`. Sub-second resolution
distinguishes a 0.4 s mis-press from a 4 s real clip.

#### Misc tooling and dev experience

- **`npm run tauri:bundle` (#152):** new macOS-only helper that
  builds a debug `.app` and opens it. Required for SCK / Screen
  Recording / system-audio TCC testing because the bare
  `cargo tauri dev` binary is attributed to the parent Terminal /
  iTerm, not Hush itself.
- **`Info.plist` TCC usage descriptions (#149)** + `CFBundleIdentifier`
  + `CFBundlePackageType` (#151). Without these, macOS 14+
  silently fails the gated API call instead of prompting, and the
  app never appears in System Settings → Privacy & Security under
  its own name. Embedded into the dev binary's `__info_plist`
  Mach-O section via `tauri::embed_plist::embed_info_plist!`.

### Changed

#### IPC commands.rs split into per-domain submodules (closes #82, via #168, #179, #180)

`src-tauri/src/ipc/commands.rs` had grown to ~1.9k LOC. Three
focused extractions reduced the parent file to 1341 LOC and
established the per-domain submodule pattern:

- **#168 — `commands/meeting.rs`** (~245 LOC): 7 commands, 3
  result types, the `MAX_MEETING_SOURCES` cap +
  `sanitise_meeting_sources` validator + its 6 unit tests.
- **#179 — `commands/models.rs`** (~330 LOC): 5 commands,
  `ModelCard` / `ModelSelectResult` / `DownloadProgress` /
  `DownloadStatus` types, the auto-download orchestration that
  wraps `transcription::download::download_with_progress`.
- **#180 — `commands/macos.rs`** (~245 LOC): 3 commands
  (`open_macos_privacy_pane`, `diagnose_macos_permissions`,
  `reset_macos_permissions`) + 2 result types + bundle-id const.
  Already cfg-gated by platform so the move was self-contained.

`lib.rs` now references commands by their full
`ipc::commands::<domain>::<cmd>` path because Tauri's
`generate_handler!` macro is path-sensitive — it generates a
hidden `__cmd__<name>` symbol as a sibling of each command, and
`pub use` re-exports do not carry that symbol with them. See
`learnings.md` 2026-04-25 for the original lesson; the new module
header in `commands/mod.rs` cites it so future contributors don't
re-discover the trap. The remaining domains (history, replacements,
vocabulary, first-run, ptt-config) stay inline — they're below
the standalone-file threshold the issue body itself flagged.

#### rdev pinned to fufesou's fork — PTT works on macOS 26+ (#169)

The macOS-26 hard-abort in PTT (rdev's CGEventTap callback calling
`TISGetInputSourceProperty` from a non-main thread, hitting
`dispatch_assert_queue_fail` on the first modifier press) is fixed
by pinning rdev to [fufesou's fork](https://github.com/fufesou/rdev)
— the one RustDesk ships in production. fufesou attaches the
CGEventTap to `CFRunLoopGetMain()` so the callback runs on the
main thread and TSM is happy.

- **First attempt that didn't work:** pinned to Narsil's upstream
  `main` past PR #147 (May 2025, "MacOS: set_is_main_thread").
  Hands-on test: instant crash on the first modifier press.
  Reading the patch: PR #147 only fixes the *send* path's TSM
  call site, not the *listen* path. `listen()` itself still
  attaches the tap to the calling thread's run loop.
- **Lesson** (in `learnings.md` 2026-04-27): "PR merged" ≠
  "your bug is fixed." Read the diff. Production users
  (RustDesk in this case) often patch around upstream's
  incompleteness for years before upstream catches up.
- Default PTT key on macOS becomes `RightMeta` (Right ⌘) —
  every Apple keyboard has it, but not every Apple keyboard has
  a Right Ctrl. Other platforms keep `RightControl`.
- The settings toolbar in the Settings window becomes
  `position: sticky` so it doesn't scroll out of view as the
  General tab grows past the window height.

#### Meeting-mode polish: SCK on by default, HUD on session, auto-expand on stop (#144, #146)

- ScreenCaptureKit is now an unconditional macOS dependency
  (no Cargo feature flag). Pre-2026-04-27 the dep was opt-in via
  a `screencapturekit` feature; the default `npm run tauri dev`
  shipped without SCK and the meeting panel's "Also record
  system audio" checkbox stayed disabled.
- Meeting session start shows the recording HUD; stop hides it.
  Same UX cue the dictation hot path provides.
- Just-stopped session row auto-expands its transcript so the
  user lands directly in the recording they just made.
- Stop-session button uses destructive red styling and requires
  an inline "End session? N utterances captured" confirmation
  before firing the IPC (closes #131). Two-step pattern with
  Cancel returning cleanly to unconfirmed Stop.

#### IPC error messages use full anyhow chain (#150)

`format!("...: {e}")` only renders the outermost anyhow context;
errors deep in the chain were truncated to "open audio session
for system-audio source" with no SCK detail. Switched to
`format!("...: {e:#}")` for the full chain at every IPC error
site. The audio-failure copy on the frontend's `formatError`
also became source-agnostic so SCK failures don't render
"Microphone error: try selecting a different input device" (#158).

### Fixed

#### macOS Info.plist regressions (#147, #148, #149, #151)

When ScreenCaptureKit dropped its feature flag (#144), the
crate's build-script-baked `cargo:rustc-link-arg` rpaths for
`libSwift_Concurrency.dylib` no longer propagated transitively
to the dev binary. `npm run tauri dev` SIGABRT'd with
`Library not loaded: @rpath/libswift_Concurrency.dylib`. Fixed
by baking the rpaths into the root crate via
`src-tauri/.cargo/config.toml` (#147) and capturing the hazard
in `learnings.md` (#148): "transitive deps don't propagate their
build-script rpaths through to the binary; the root crate has to
declare them itself."

CFBundleIdentifier was missing from the embedded `Info.plist`,
so macOS TCC had no per-app key to track the dev binary by — Hush
never appeared in System Settings → Privacy & Security under its
own name. Added in #151. Even with this, macOS attributes some
TCC requests from unsigned bare binaries to the parent process
(see "macOS TCC dev-binary quirk" in `CLAUDE.md`); for SCK and
Screen Recording specifically, prefer testing against
`npm run tauri:bundle`'s output `.app`.

#### PTT hint on macOS, live utterance counter, source-failure events, hotkey row layout

- **PTT shortcut hint hidden on macOS by default (#161):** the
  "hold Right Ctrl to push-to-talk" copy was misleading on macOS
  where PTT was disabled. Now omits the PTT clause on macOS and
  honestly explains the opt-in path in the welcome modal.
- **Live utterance counter (#157):** the meeting panel showed
  "0 utterances so far" while the transcript rendered finals.
  Counter was reading the stale `meetingSessions[].utteranceCount`
  (refreshed only at session start/stop) instead of the polled
  `activeDetail.utterances.length`.
- **Streaming PCM zeroed on drop + pump source-failure events
  (#145):** `SlidingWindowState`'s `Drop` impl overwrites the
  rolling f32 PCM Vec with zeros + clears `last_partial_text`
  before allocator return. The pump's `feed/drain` failure path
  emits a `meeting:source-failed` Tauri event with `{sessionId,
  sourceKind, reason}` and drops the source from the active
  session; the frontend renders dropped sources as struck-through
  chips with a "STOPPED CAPTURING" tag.
- **Settings → General Hotkeys row layout (#177):** the chord
  for "Toggle recording" was rendering each `<kbd>` chip on its
  own line because `.row-value` is a column-flex container and
  the bare `+` separators were treated as separate flex items.
  Wrap the chord in a `<span class="chord">` with its own
  `inline-flex` so the chord is one flex item and the
  `row-note` still flows below.

### Removed

#### Legacy `list_input_devices` IPC command (closes #155, via #160)

The picker migration to `audio_list_sources` had soaked across
multiple releases. Drops:

- The `#[tauri::command]` handler in `commands.rs`
- The registration in `lib.rs`
- The e2e mock entry
- Stale comments referencing the legacy command

The `AudioCapture::list_input_devices` trait method stays — it's
still the building block the default `list_audio_sources` impl
uses to enumerate mic devices.

#### "Configuration" tab on the main window (via #165)

The Phase 1 placeholder that held Model picker / Vocabulary /
Replacements / macOS Diagnostic until Phase 3 lifted them into
the standalone Settings window. `AppSection` union narrowed from
4 entries to 3 (Dictation / Meetings / History); sidebar and
View menu drop the entry; main page sheds the corresponding state
and CRUD handlers.

### Documentation

- **`CLAUDE.md` refresh (#175):** three Tauri windows documented
  (main + settings + hud), four-place IPC sync rule updated for
  the submodule paths, dev-launch smoke trigger list adds
  `app_menu/` and `settings_window/`, "Where things live"
  rewritten to cover the new modules.
- **`README.md` refresh (#178):** Meeting Mode + macOS
  system-audio + streaming Whisper move from Planned to Shipped;
  three-window IA + native macOS menu + configurable PTT + live
  permission detection + autostart toggle + HUD niceties added to
  Shipped; platform table updated to drop the "PTT disabled by
  default" caveat.
- **`learnings.md` 2026-04-27 entries:**
  - rpath / CI-green-dev-broken hazard (rpaths don't propagate
    from transitive deps)
  - macOS TCC dev-binary quirk (parent-process attribution for
    unsigned binaries)
  - macOS TCC status IS readable for the three categories Hush
    touches via AVFoundation / CoreGraphics / IOKit
  - rdev macOS-26 abort: Narsil's PR #147 was incomplete; fixed
    via fufesou's fork

### Fixed

#### Round-9 reviewer cycle — streaming polish (#108)

- **`↓ N new` pill now counts in-flight partials, not just settled
  finals.** A user who scrolled up during active speech was seeing
  "↓ 0 new" while whisper was actively revising the in-flight tail —
  the pill's "since I last looked" promise was finals-only. Both
  `liveTranscriptFrozenAt` (snapshot at scroll-up) and
  `liveTranscriptNewCount` (current-vs-snapshot) now include
  `currentPartials.length` in their counts.
- **Partial-row screen-reader handling.** Replaced the static
  `aria-label="In-flight partial transcript, still being refined"`
  on partial rows with an `aria-live="off"` directive plus an
  `sr-only` "(in progress)" suffix on the speaker badge. The
  previous shape had a static label on a row whose text content
  changed every poll — assistive tech could re-announce the entire
  text on each whisper revision. The new shape announces "(in
  progress)" once when the row mounts and lets the partial text
  revise silently.
- **Stale `current_partial: Option<...>` reference in `learnings.md`
  (2026-04-26 streaming entry) corrected to `current_partials:
  Vec<Utterance>`.** PR3's design landed plural-per-source, not
  singular-per-session. Future sessions reading the entry now see
  the actually-shipped shape.
- **"byte-identical" → "observably equivalent" in the Phase B
  CHANGELOG section.** The earlier entry literally contradicted the
  same-day `learnings.md` 2026-04-26 entry on the "byte-identical
  trap" — the term is precise CPU-cache-line vocabulary, not a
  description of transcription text equivalence.
- **New stable-cutoff boundary unit test.** Pins the `<=` semantics
  of the streaming policy: a segment ending exactly at the
  `commit_tail_ms`-derived cutoff commits as a final (rather than
  staying as a partial for one extra inference window). Prevents a
  silent regression from a future tightening to `<`.
- 205 lib tests + 10 e2e tests pass; clippy + fmt + svelte-check
  clean.

### Added

#### Live partial-utterance rendering in the meeting panel (#108 PR4)

- The meeting transcript renders `currentPartials` (PR3) below the
  settled finals with an italic + reduced-opacity treatment plus a
  dashed border-left and an animated "…" indicator next to the
  timestamp. Partials revise in place as whisper firms up the
  trailing tail; once aged past the commit threshold the pump emits
  them as finals and the styling solidifies. `prefers-reduced-motion`
  kills the indicator pulse.
- `MeetingSessionsPanel.svelte` keys partials by `speakerLabel` (one
  per source) so a revision swaps text in place rather than
  re-mounting the row. The autoscroll effect tracks both the finals
  count AND a partial-content fingerprint (`label:text` joined),
  so the live tail keeps following whether the user spoke a new
  word or whisper revised the in-flight tail.
- The empty-state copy ("every 10 seconds") and the active-session
  line ("about every 10 seconds") are updated to match the
  streaming cadence ("within a few seconds"; "italicised lines are
  still firming up").
- The `speakerLabel` helper is now structural-typed
  (`{ speakerLabel: string | null }`) so it accepts both
  `PersistedUtterance` (finals) and `StreamingUtterance` (partials)
  without duplication.
- 1 new e2e test pins the partial-rendering shape: 1 final + 2
  partials → 3 rows, 2 with `utterance-partial` class, italic
  computed-style, "…" indicator visible. 10/10 meeting-panel e2e
  tests pass.

What this completes for #108: the user now sees text appear within
~3 s of speech (vs ~10 s pre-#108), revising in place as whisper
firms up the segments. The four-PR sequence (PR1 streaming trait /
PR2 drain_into / PR3 pump rewrite / PR4 partial UI) closes the
streaming-meeting-mode UX promise. Real-meeting smoke validation
of CPU + revision behaviour is the remaining open item.

#### Streaming meeting pump — partials in IPC (#108 PR3)

- The meeting pump no longer chunks-and-restarts. It opens one
  `StreamingTranscribeSession` per audio source at session start
  (PR1), drains each `AudioSession` on a 500 ms tick (PR2), feeds
  the drained samples into the corresponding streaming session, and
  dispatches returned utterances: **finals** to the database (the
  existing `MeetingSessionRepository::append_utterance` path),
  **partials** to a new in-memory partials store keyed by
  `(session_id, speaker_label)`. The previous 10 s `CHUNK_DURATION`
  constant is gone; new `PUMP_TICK` of 500 ms is the only timing
  knob the pump owns. Whisper inference cadence (the ~3 s "when
  does a partial revise" interval) is internal to the streaming
  session.
- `meeting_session_get` IPC response gains `currentPartials:
  Vec<Utterance>` — the in-flight partials for the active session,
  sorted alphabetically by `speakerLabel` so render order is stable
  across polls. Closed sessions always return an empty
  `currentPartials` array. PR4 adds the visual treatment that
  distinguishes partials from finals.
- `SessionManager` gains a `partials: Arc<RwLock<HashMap<i64,
  HashMap<String, Utterance>>>>` field plus a `current_partials_for(
  session_id) -> Vec<Utterance>` reader. `RwLock` because the IPC
  poll path (~1/s) reads while the pump tick (~2/s) writes —
  readers shouldn't block each other. `stop_manual` clears the
  partials map for the closing session belt-and-braces; the pump's
  `finish()` path also clears entries as it commits final tail
  utterances.
- `dispatch_utterances` is the new per-tick routing helper: a final
  for source S clears the matching partial entry **before** the DB
  append (so a concurrent poll between clear-and-append sees neither
  rather than both, avoiding a brief duplicate render). Partials
  for source S **replace** the prior entry — at most one partial
  per source at any time. Cross-source isolation (mic final does
  not clear system partial) is pinned by tests.
- Streaming inference runs on `tokio::task::spawn_blocking` so
  whisper.cpp doesn't block the tokio worker thread. The streaming
  session round-trips through the spawn (taken out → moved in →
  returned with utterances) so the pump retains ownership across
  ticks. A panic in the spawned closure leaves the slot `None` for
  the rest of the session — that source goes dark until the next
  start, but the others continue.
- 8 new meeting-manager tests cover the partials store + dispatch
  contract: empty-on-new-session, partial replaces partial,
  per-source independence, final clears matching partial AND
  persists row, final does not clear other source's partial,
  empty-final filtering, stop_manual clears partials. Total: 27
  meeting-mode tests; 204 lib tests with `--features whisper`.
- 6 e2e mocks updated to include `currentPartials: []` on the
  `meeting_session_get` shape. Frontend type-check passes.
- What's deliberately not here yet (tracked in #108 PR4): visual
  rendering of partials in the panel — they arrive in the poll
  response but aren't yet rendered with italic / opacity. A
  consumer that ignored `currentPartials` would observably behave
  the same as today (modulo the lower latency on finals).

#### Audio drain-into for streaming-pump capture (#108 PR2)

- `AudioSession::drain_into(sink, ...) -> Result<CaptureFormat>` lets
  the meeting pump (#108 PR3) pull samples from a live capture
  handle on a tight tick (~500 ms) without stopping the session —
  the keystone shape change for streaming. Default impl errors so
  legacy mocks surface a clear diagnostic; the cpal mic backend and
  the ScreenCaptureKit system-audio backend both override.
- The cpal mic override routes a new `Cmd::DrainBuffer` to the audio
  worker thread (where the buffer Arc lives), `mem::take`'s the
  accumulated samples, and replies with `(samples, format)`. The
  worker round-trip is microsecond-scale — the alternative
  (leaking the buffer Arc into the handle at start time) would have
  required restructuring `Cmd::Start`'s reply shape, an invasive
  change for a one-call-per-tick path. The cpal stream keeps writing
  into the now-empty buffer between drains.
- The SCK override calls a new public `ScreenCaptureKitSession::drain_buffer()`
  helper — same `mem::take` discipline, no stream stop. The
  callback's Arc clone of the buffer remains valid across the drain.
- `stop()` continues to consume the handle as before; `drain_into`
  takes `&self` so it composes with the pump's `Vec<Box<dyn AudioSession>>`
  without lifetime gymnastics.
- 12 new audio-tests pin the contract: default-impl errors with an
  actionable message, override appends to the caller's sink (does
  not replace), repeated calls only return new samples since the
  previous drain. Total: 193 unit tests with `--features whisper`.

#### Phase A2: macOS system-audio capture via ScreenCaptureKit (#105)

- The "System audio" entry in the source picker is no longer the
  "coming soon" placeholder on macOS — it now drives a real
  ScreenCaptureKit capture session. Selecting it before pressing
  the dictation hotkey routes `start_dictation` through the new
  `audio::screencapturekit::ScreenCaptureKitSession` instead of
  the cpal mic path; samples land in the same `Vec<f32>` shape the
  rest of the transcription pipeline already consumes, so whisper
  / model-swap / replacements / history all work unchanged.
- Compiled behind a `screencapturekit` feature flag and a
  `cfg(target_os = "macos")` gate. Default builds remain cpal-only
  to keep CI and Linux/Windows tests deterministic; release macOS
  builds opt in via `cargo build --features screencapturekit`.
- Capture format is 48 kHz stereo f32 PCM, matching what the OS
  mixer already runs internally — avoids a forced resample at
  capture time. Existing `downmix_to_mono` and the whisper-side
  resampler reduce to 16 kHz mono ahead of transcription, same
  path as cpal mic input.
- TCC bucket is **Screen Recording** (Apple bundles audio-from-
  display under that prompt even when you capture zero pixels).
  First call triggers the prompt automatically; the existing
  `MacosDiagnosticPanel` already covers Screen Recording in its
  reset sweep.
- `AudioCapture::supports_source(SystemAudio)` returns `true` on
  macOS-with-feature, `false` everywhere else, so the source
  picker continues to render the option as disabled with a
  "coming soon" affordance on Linux / Windows / feature-off
  builds. Linux PulseAudio monitor (#106) and Windows WASAPI
  loopback (#107) are tracked as separate PRs.
- Test compilation needs `DYLD_FALLBACK_LIBRARY_PATH` pointed at
  the Xcode Swift toolchain (`/Applications/Xcode.app/Contents/
  Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/
  macosx`) when run with `--features screencapturekit`. Production
  app bundles inherit the Swift runtime from the macOS dyld
  shared cache and need no special handling.

#### Phase C runtime: manual-start meeting sessions (#110)

- Meeting Mode goes live in manual-start mode. The user clicks
  "Start a session" in the panel; the backend opens a session row
  via the new `SessionManager`. They dictate with the existing
  hotkey / button flow; each `stop_dictation` transcript lands as
  an utterance under the active session in addition to the existing
  history insert. They click "Stop session"; the manager writes
  `ended_at` and clears the active-session pointer. The panel
  renders a live-status indicator with a pulsing dot while a
  session is in progress.
- New `crate::meeting::SessionManager` owns the in-memory
  `Mutex<Option<i64>>` for the active-session id. Manual-start
  only — auto-detect from foreground app is a follow-up. The
  manager's `append_if_active` returns `Ok(false)` when no session
  is active, so the dictation hot path's behaviour is observably
  unchanged when meeting mode isn't being used.
- New `crate::meeting::AppClassifier` with hardcoded defaults
  (Zoom, Teams, Meet, Discord, Slack-call → Meeting; YouTube,
  Spotify, Apple Music → Media; everything else → Other). Used to
  stamp `app_kind` on new sessions for the panel's coloured tag.
  Per-user overrides are deferred to #112.
- Three new IPC commands: `meeting_active_session` (read),
  `meeting_start_manual` (write), `meeting_stop_manual` (write).
- `MeetingSessionsPanel.svelte` grows Start / Stop buttons + an
  active-session indicator. The page refreshes the panel after
  each successful `stop_dictation` so newly-appended utterances
  appear in the timeline.
- 9 new Rust unit tests cover the manager's lifecycle (start
  rejects concurrent starts, stop errors when no session, append
  computes cumulative timestamps correctly) plus the classifier's
  default-table behaviour. Total: 169 unit tests.

What's deliberately **not** here yet (tracked in #110):

- Auto-detect from foreground app — manual-start is the safer
  first step because it never records a meeting the user didn't
  intend to record.
- Streaming partial utterances — each session captures one final
  utterance per `stop_dictation` call. Streaming partials wait on
  #108.
- System-audio capture per platform — without #105 / #106 / #107
  shipped, meeting mode captures via mic only (a single-speaker
  "personal meeting transcript" experience).

#### Phase C scaffold: meeting sessions data layer + UI panel (#113)

- Meeting Mode scaffold (Phase C foundation; refs #33 / #109).
  Lands the data layer + UI shell for the meeting-transcript
  surface that the design memo (`docs/system-audio-meeting-mode-proposal.md`)
  describes. **What's wired today:**
  - Migration 0002: `meeting_sessions` and `utterances` tables,
    plus FTS5 index over utterance text. Additive only —
    existing v0.1.0 databases migrate cleanly.
  - `crate::meeting::{MeetingSession, NewMeetingSession,
    PersistedUtterance, NewPersistedUtterance, MeetingAppKind}`
    types + `MeetingSessionRepository` trait (sibling to the
    other Repository-pattern repos post-#88) + SQLite impl.
  - Four new IPC commands: `meeting_sessions_list`,
    `meeting_session_get`, `meeting_session_delete`,
    `meeting_session_set_notes`.
  - `MeetingSessionsPanel.svelte` rendered at the bottom of the
    main page. Today shows a scaffolded "no sessions yet"
    placeholder that explicitly enumerates what's pending and
    links to the relevant tracking issues (#105 macOS, #106
    Linux, #107 Windows, #108 streaming, #110 session manager,
    #111 diarization). Permanent privacy line surfaced at the
    top of the panel.
  - 9 new Rust tests pin the SQLite impl's behaviour: create,
    list, idempotent close_session, atomic append_utterance with
    count bump, ordered list_utterances, set_notes round-trip,
    and FK cascade on delete.
#### Streaming transcription — sliding-window foundation (#108 PR1)

- `Transcribe` trait gains `start_stream(format, prompt)` returning a
  `Box<dyn StreamingTranscribeSession>` handle the meeting pump (PR3)
  feeds samples into on the audio drain cadence. The handle exposes
  `feed`, `drain`, and `finish` so the pump can pull partials + finals
  on a tight tick without stopping capture. Default impl errors —
  backends that opt in (the whisper-rs path, plus a future Parakeet
  ONNX backend) override `start_stream` AND override
  `supports_streaming` to return `true`. Non-streaming backends and
  test mocks keep their existing behaviour unchanged; the dictation
  hot path stays on `transcribe_with_prompt`.
- `WhisperTranscription` overrides `start_stream` to construct a
  `WhisperStreamingSession` that runs whisper.cpp on a rolling 30 s
  window every ~3 s of new audio, emitting partials for the trailing
  tail and finals for segments aged past an 8 s commit threshold. The
  policy state machine (`SlidingWindowState`) is whisper-agnostic and
  unit-tested with a scripted `WhisperLikeInferer` mock — 15 unit
  tests pin the diff/commit logic against synthetic segment streams
  (window growth, partial revision, commit-and-slide, long-silence
  failsafe, empty-segment filtering, dedup high-water mark). The
  whisper bridge is tested end-to-end via the existing fixture WAV
  (`tests/fixtures/jfk.wav`) under a new `streaming_fixture`
  integration test, gated by `HUSH_TEST_MODEL` like the existing
  `audio_fixture` smoke. Smoke run against the bundled JFK clip with
  `ggml-base.bin` produced 3 mid-stream partials and a final
  matching the canonical "ask not what your country can do for you"
  transcript.
- The whisper context is now held behind an `Arc<Mutex<...>>` instead
  of a bare `Mutex` so streaming sessions can hold their own clones
  and run inferences from the meeting pump's blocking pool without
  coupling to the original `&self` lifetime. The dictation hot path's
  `transcribe`/`transcribe_with_prompt` call sites continue to work
  unchanged — `lock()` is the same shape regardless of `Arc` wrapping.
- See `learnings.md` (2026-04-26 entry) for the design discussion of
  time-based commit vs stability-based commit, the in-memory partial
  vs DB-write trade-off (PR3 will surface partials via the
  `meeting_session_get` IPC, not a separate Tauri event), and a few
  whisper-rs API specifics learned in passing (segment timestamps in
  10ms units; `set_no_context(true)` for sliding-window; `Send` but
  `!Sync` `WhisperContext`).

#### Phase B foundation: streaming-transcription scaffold

- `stop_dictation` now invokes inference through the streaming
  entry point (`Transcribe::transcribe_chunks`) rather than the
  one-shot `transcribe_with_prompt`. Default-impl behaviour is
  observably equivalent to before — the captured buffer is passed
  as a single chunk, the default impl produces one final utterance,
  the text reaches the clipboard exactly as it did pre-refactor — but
  the call site is now ready for a future Whisper-sliding-window or
  Parakeet backend that emits multiple partial utterances mid-
  recording. Non-final utterances are filtered out at this layer
  (they're for live UI updates in Phase C, not the dictation hot
  path's single clipboard write); a future PR forwards them via
  Tauri events when `supports_streaming()` is true. All 149 unit
  tests pass unchanged, confirming the refactor is observably
  identical.
- Streaming-transcription foundation (Phase B of the meeting-mode
  pivot, refs #33; design memo at
  `docs/system-audio-meeting-mode-proposal.md`). Adds the
  `Utterance` struct (`text`, `startedAtMs`, `endedAtMs`, `isFinal`,
  optional `speakerLabel`) — the unit a streaming backend emits and
  the row shape Phase C will persist into the meeting-sessions
  table. Extends the `Transcribe` trait with two new methods:
  `transcribe_chunks(chunks, format, prompt)` returning
  `Vec<Utterance>` (default impl is the one-shot fallback —
  concatenates chunks, calls `transcribe_with_prompt`, returns one
  `is_final = true` utterance spanning the recording), and
  `supports_streaming() -> bool` (default false). No behaviour
  change today — the dictation hot path still calls the one-shot
  `transcribe`. Future PRs landing Whisper sliding-window or
  Parakeet streaming override `transcribe_chunks` and flip the
  capability flag; the IPC layer will forward partial utterances
  to the meeting-mode UI when `supports_streaming` returns true.
  Six new unit tests pin the default behaviour (single final
  utterance, prompt forwarding, stereo duration arithmetic, empty-
  chunks safety, capability default, serde wire shape).
#### Phase A1: audio source picker (#98, #101)

- Audio source picker — first user-visible step of the system-audio +
  meeting-mode pivot (Phase A1, refs #33; design memo at
  `docs/system-audio-meeting-mode-proposal.md`). The mic dropdown is
  now a grouped `<select>` with two `<optgroup>`s: every input device
  under "Microphone", and a single "System audio" entry under "System
  audio". The system-audio option is rendered disabled with a
  "(coming soon — #33)" suffix until the per-platform backend ships.
  New IPC command `audio_list_sources` returns enriched listings
  including capability flags. `start_dictation` now takes a
  discriminated `AudioSource` argument (`{ kind: "microphone",
  deviceId }` or `{ kind: "system-audio" }`) instead of the bare
  device id; the IPC accepts `null` for the default-mic case so
  hotkey-triggered dictation stays one-click. `list_input_devices`
  is kept as a transitional alias for one release. Three new Rust
  tests pin the listing default impl, the override path, and the
  camelCase wire shape.
#### Phase A foundation (#96)

- `AudioSource` enum (`Microphone(Option<String>)` / `SystemAudio`)
  and `AudioCapture::start_with_source` trait method on the audio
  backend boundary (#96, foundation for #33). No behaviour change
  yet — the dictation hot path still calls `start(device_id)` —
  but downstream PRs that wire ScreenCaptureKit (macOS), WASAPI
  loopback (Windows), and PulseAudio monitor sources (Linux) now
  have a clean trait shape to slot into. Five new unit tests pin
  the default behaviour (Microphone forwards correctly, SystemAudio
  errors usefully, capability check defaults correct, serde wire
  shape round-trips). Refs the meeting-mode design memo at
  `docs/system-audio-meeting-mode-proposal.md`.

### Fixed

#### Round-7 review consolidation

- **Defensive guard against silent empty-clipboard.** `stop_dictation`
  filters utterances on `is_final`, then writes the concatenated text
  to the clipboard. Round-7 technical-quality reviewer caught a real
  failure mode: a future streaming backend that emits only partial
  utterances (and never a final) would slip through the filter as an
  empty string, and the user would get a clipboard with nothing in it
  with no error surfaced. Now we explicitly check for "utterances
  returned but none final" and surface it as
  `IpcError::Transcription` with a clear message. The default impl
  one-shot path always emits exactly one final, so this branch is
  only reachable for misbehaving overrides.
- **`app_kind_from_str` fails loud on unknown values** instead of
  silently defaulting to `Other`. Round-7 reviewer flagged the
  silent-default as data-corruption-masking — a rogue write of
  `"video-call"` would render as a generic "Other" session with no
  signal that anything was wrong. Now `FromRow` returns
  `sqlx::Error::Decode` with a descriptive message. A future variant
  added to `MeetingAppKind` is a deliberate code change that updates
  the match arm; the database is never expected to hold values the
  match doesn't cover.
- **`IpcError::MeetingSessions` variant added.** Meeting commands
  previously mapped errors to `IpcError::Settings` with a string
  prefix, drifting from the per-domain pattern (`History`,
  `Replacements`). Now the four meeting-session commands return their
  own kind (`meeting-sessions`) so the frontend can switch on the
  variant for tailored recovery copy when the streaming pump (#110)
  starts driving real writes.
- **First-run welcome modal pulled ahead of `Promise.all`.**
  Round-7 UX reviewer noted a real timing bug: when the first-run
  flag fetch raced against the parallel data fetches, a fresh-install
  user could see the no-model setup banner before the welcome modal
  landed — the modal explaining permissions appeared after the user
  had already started clicking around looking for the record button.
  Awaiting the flag synchronously makes the modal beat the rest of
  the UI to first paint. Cost: one extra IPC round-trip (cheap, a
  single SQLite read of a boolean).
- **Meeting panel placeholder reframed as product copy.** The earlier
  placeholder read like a GitHub-ticket summary ("Session manager —
  tracked in #110"). Round-7 UX reviewer caught the developer-y
  framing. Now the headline reads "Live meeting transcripts are
  coming soon" with a one-paragraph user-facing summary; the
  developer-facing tracking-issue list is preserved verbatim under a
  "Developer notes" `<details>` disclosure for readers who want to
  follow along.
- **Privacy line tightened.** The earlier framing leaked
  implementation trivia (the "30s ring buffer" detail) into a
  user-facing line. Now it leads with the user benefit ("Hush
  transcribes meeting audio live and never saves the audio itself")
  and moves the buffer detail into a "How it works" `<details>`
  disclosure for users who want the full mechanism.
- **PRD §5b "Meeting Mode (v1.x)" added.** The design memo from
  #93 had proposed adding this section; the pivot was actively
  shipping but the policy doc still described Hush as
  dictation-only. Documentation reviewer flagged. The PRD now
  carries the canonical "Meeting Mode v1.x" text, with §3 and §10
  tightened in lockstep.
- **Design memo status line updated.** The memo at
  `docs/system-audio-meeting-mode-proposal.md` still said "Draft for
  discussion. Not approved; not in the PRD yet." even after Phases
  A1, B foundation, and C scaffold had landed. Now reads "Approved
  direction; actively shipping" with the concrete phase status.

### Changed

- Removed unused `zip` dependency from Cargo.toml (#91). It was
  declared with an "Archive/export support" comment but no source
  file imported it; the only `zip` references in the codebase are
  `iter().zip()` calls from std. Removing it cuts 188 lines from
  Cargo.lock (zip pulled in a substantial transitive subtree: aes,
  bzip2-rs, deflate64, flate2, indexmap, lzma-rs, pbkdf2, zopfli,
  etc.) — meaningful build-time and binary-size savings, plus a
  smaller supply-chain surface to audit.
- `sha2` dependency upgraded 0.10 → 0.11 (#94). The 0.11 release
  dropped its `LowerHex` impl on the digest array returned by
  `finalize()` (the underlying type changed from `GenericArray` to
  `hybrid_array::Array`); replaced both `format!("{:x}", ...)`
  call sites in `transcription/download.rs` with a small inline
  `hex_encode` helper. No behaviour change for the user — the
  on-disk hex format is byte-identical to the prior `LowerHex`
  output.
- `active-win-pos-rs` dependency upgraded 0.8 → 0.10 (#95).
  Transparent — `get_active_window()`'s return type and the
  `ActiveWindow.app_name` / `.title` fields used in
  `capture_foreground` are unchanged. 0.10 is the line that has
  macOS 26 / Sequoia compatibility tweaks; staying on 0.8 risked
  foreground-detection drift on the project's primary target
  platform.

## [0.1.0] - 2026-04-26

First tagged release. Captures the M3-complete state of Hush —
end-to-end functional dictation on macOS 26 with history,
replacements, vocabulary, model picker, auto-download, first-run
welcome, recording HUD, and an in-app permission diagnostic.

### Added

- Bundled audio test fixture (#34, follow-up to part-a). The
  ~344 KB public-domain JFK "ask not what your country can do for
  you" clip (16 kHz mono PCM, lifted from whisper.cpp's
  `samples/jfk.wav`) now ships in `src-tauri/tests/fixtures/jfk.wav`
  and backs the default audio path of the integration test. A
  contributor with a model on disk can now run
  `HUSH_TEST_MODEL=/path/to/ggml-base.bin cargo test --features
  whisper --test audio_fixture -- --ignored` without staging an
  audio file separately. `HUSH_TEST_AUDIO` still overrides for
  contributors who want to point at a different clip. Whisper
  models stay out-of-repo (75 MB+ each); the model env var remains
  required.
- In-app macOS permission diagnostic and reset (#67). A collapsible
  section on the main page shows the bundle id, hint copy for
  Microphone and Input Monitoring, direct links to the relevant
  Privacy panes in System Settings, and a "Reset permissions" button
  that runs `tccutil reset` for the Microphone, ListenEvent (Input
  Monitoring), and Accessibility categories scoped to the Hush
  bundle id. Recovery path for the stuck-permission state previously
  documented only in `docs/macos-permissions.md`. The section is
  hidden entirely on non-macOS builds.
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
- Whisper model auto-download. Pure-logic streaming downloader
  (`transcription::download`) with SHA-256 verification: bytes
  stream into a `.part` sibling file, hash computed on the fly,
  atomic rename on success, `.part` deleted on failure or cancel.
  Frontend picker grows per-card actions — Download, Cancel,
  Try-again-on-failure, Remove — with a CSS progress bar driven by
  Tauri events (`model:download-progress`, `model:download-done`,
  `model:download-failed`). Catalog gains `download_url` (Hugging
  Face mirror) and `sha256` (per-model, empty until a contributor
  verifies each hash — auto-download refuses to start with an empty
  hash and surfaces a friendly "configure manually for now" hint).
  Backend tests run against a local `wiremock` server; no real
  Hugging Face round-trips in CI. Closes #30.

- Audio test fixture (#34 part-a): an `#[ignore]`d integration test
  in `src-tauri/tests/audio_fixture.rs` that loads a WAV via
  `HUSH_TEST_AUDIO` (defaults to the bundled `tests/fixtures/jfk.wav`
  once #89 landed), runs it through the full transcription stack,
  and asserts the output contains configurable expected words. WAV
  parsing via `hound` (dev-dep only). Validates the auto-download +
  transcription path end-to-end once a contributor places a model.
  System-audio loopback variant stays open behind #33.

- First-run welcome modal (closes #22). Explains the permissions
  Hush needs — Microphone for cpal, Input Monitoring for the rdev
  PTT listener — and links to System Settings → Privacy & Security
  via a new `open_macos_privacy_pane` command on macOS. Persists
  dismissal in the settings table so the modal only shows on a
  fresh install. The OS prompts themselves still fire at app
  startup; the welcome's job is to explain what just happened,
  not to trigger anything new.
- **Bug fix surfaced during #22:** PR #42 added the
  `model_download` / `model_cancel_download` / `model_remove`
  Tauri commands but never registered them in `lib.rs`'s
  `generate_handler!` list. Frontend invokes would have failed at
  runtime. All three are now wired up.

- Recording HUD overlay scaffold (scaffold half of #21). A second
  Tauri window (label `hud`) shown while dictation is active:
  borderless, transparent, always-on-top, no taskbar entry.
  Renders a pulsing red dot + "Recording" label. Show/hide hooks
  into `start_dictation` / `stop_dictation` so the HUD tracks the
  audio stream's lifecycle, not the slower transcription that
  follows. The level-meter half (cpal callbacks compute RMS, audio
  thread → Tauri event → meter animation) lands as a follow-up.
- Recording HUD level meter (closes the level-meter half of #21).
  Per-callback RMS is computed in the cpal sample-conversion
  loop and published into a lock-free `Arc<AtomicU32>` (encoded
  as `f32::to_bits()`); a 30 Hz tokio task reads the latest value
  and emits an `audio:level` Tauri event. The HUD page
  (`src/routes/hud/+page.svelte`) listens, smooths the value with
  a fast-attack / slow-release envelope on `requestAnimationFrame`,
  and renders a soft red bar to the right of the "Recording" label.
  The `AudioCapture` trait gained a default-impl `current_level()`
  so non-cpal backends and test mocks inherit a no-op zero — the
  HUD bar simply idles for them.

### Changed

- **Refactor: extract generic `Repository<T, NewT, Id>` trait (#36).**
  Replaces the four near-identical CRUD declarations on
  `ReplacementRepository` and `VocabularyRepository` with one generic
  trait in `src-tauri/src/repository.rs`. Each domain trait is now a
  marker that aliases the generic under a domain-meaningful name plus
  a blanket impl, so concrete types implement the four CRUD methods
  exactly once. `HistoryRepository` deliberately stays standalone (its
  paginated `list`, plus `search` / `count` / no-`update` semantics
  don't fit a uniform shape), but its `insert` method was renamed to
  `create` for naming consistency with the rest of the repos. The
  `spawn_history_insert` helper became `spawn_history_create` to
  match. `SettingsRepository` stays its own trait — K/V semantics are
  genuinely different. Pure refactor; tests unchanged.
- **Refactor: `AppStateBuilder` replaces 7-arg constructor (#37).**
  `AppState::new(audio, transcribe, history, replacements,
  vocabulary, settings, models_dir)` was at the readable threshold
  and the next features (auto-download state, system-audio source,
  HUD service) would each add another positional parameter.
  Replaced with a builder: `AppStateBuilder::new().audio(…).…build()?`.
  `build()` returns a descriptive error naming the first missing
  required field, so a future test that forgets one fails loudly
  instead of by silent panic. `transcribe` stays optional. Pure
  refactor — same `AppState`, same fields, same call paths.
- **Refactor: split monolithic `+page.svelte` into per-section
  components (#40).** No behavior change; e2e suite stayed green
  through the move. The 2351-line page is now a 1080-line layout
  that imports seven focused components from `src/lib/`:
  `ControlsSection`, `ResultBlock`,
  `HistoryPanel`, `ReplacementsPanel`, `VocabularyPanel`,
  `ModelPickerPanel`, `MacosDiagnosticPanel`. Cross-cutting state
  (`recording`, `busy`, `Promise.all` mount, download-progress
  listeners) stays in the parent; each child takes data and callback
  props. Shared TypeScript types live in `src/lib/types.ts`. Per-panel
  styles moved into each component's own `<style>` block (Svelte
  scopes by default).
- **Hot-load on model select + honest "needs-download" notice.** The
  picker used to show "Saved. Restart Hush to use the new model"
  after every selection — including selections of undownloaded
  models, where restart wouldn't help (the file isn't there). New
  flow: `model_select` returns `{ loaded: bool }`. If the file is on
  disk, the backend hot-swaps the loaded transcriber via
  `AppState::swap_transcriber` (no restart) and the notice reads
  "✓ Loaded. Ready to record." If not, the notice reads "Saved as
  default — but this model isn't downloaded yet. Click Download on
  the card below to fetch it." Selection persists either way, so a
  user can pre-select Whisper Large v3, click Download, restart,
  and have it picked up. The `transcribe` field on `AppState` moved
  from `Option<Arc<dyn Transcribe>>` to `Mutex<Option<...>>` to
  support the swap; the dictation hot path acquires the lock briefly
  only to clone the inner Arc. Whisper GGUF parsing happens on a
  `spawn_blocking` task so the IPC handler doesn't hold the tokio
  runtime for the 50–500 ms load. Model cards in the picker are now
  uniformly clickable (previously only downloaded cards were); the
  markup branches were unified into a single `<button>` element.
- **README + PRD honesty pass on PTT and platform support.** README's
  Shipped list now separates toggle-record (works everywhere) from
  push-to-talk (Linux + Windows only by default; macOS opt-in, with
  the rdev/macOS-26 caveat called out and linked to issues #69 + #70).
  A new "Platform support — honest version" table notes that
  Linux + Windows are theoretically supported and CI-validated but
  not hands-on tested by the maintainer, and invites contributions
  and bug reports for those platforms. PRD §3 (Goals) and §9 (v1
  feature list) both updated with reality checks dated 2026-04-26 so
  the policy doc stops promising what the code can't currently
  deliver on macOS 26.
- **Default toggle hotkey changed from `⌘/Ctrl+Shift+Space` to
  `Ctrl+⌥/Alt+H`** (literal Control + Option/Alt + H — `⌃⌥H` on
  macOS). The previous default conflicted with macOS's character-
  picker chord on some configurations. `Ctrl+Shift+H` was
  considered but collides with Finder's "Go to Home folder"; the
  Ctrl+Alt family doesn't have any system bindings on macOS,
  Linux, or Windows for the `H` key, and sits in the same modifier-
  family VoiceInk uses (`⌃⌥V`) so users coming from a similar
  tool find it immediately reachable. Frontend hint card, README,
  STATUS, and the hotkey doc comment all updated in lockstep.
  Override via `HUSH_TOGGLE_HOTKEY` env var.
- **macOS permission troubleshooting docs.** New
  `docs/macos-permissions.md` covers the dev-build permission
  flakiness — why `cargo tauri dev` permissions aren't as sticky as
  signed-bundle permissions, the symptoms ("PTT silently does
  nothing", "transcript is empty / silence", "prompt attributes to
  Terminal"), and the `tccutil reset Microphone com.khawkins.hush` /
  `tccutil reset ListenEvent com.khawkins.hush` recipe to unstick
  them. Linked from `CONTRIBUTING.md` and the README docs table.
- **`npm run dev-cleanup` convenience script.** Kills stale
  processes left over from a hung `cargo tauri dev` run — the dev
  binary itself, Tauri's runner, Vite's dev server (port 1420 freed
  via `lsof -ti :1420`). Pass `--reset` to also `tccutil reset` the
  three macOS TCC entries (`Microphone`, `ListenEvent`,
  `Accessibility`) so the next launch re-prompts cleanly. Lives in
  `scripts/dev-cleanup.sh`; the `--reset` flag is macOS-only and
  no-ops elsewhere.
- **HUD polish — top-right placement, light-desktop contrast,
  screen-reader title.** Three round-4 reviewer items the a11y batch
  in #48 deferred:
  - HUD now positions itself top-right of the primary monitor on
    every show (40 logical-px margin, multi-monitor aware via
    `Window::primary_monitor`). Previously the OS picked the spot,
    which often centered the HUD over whatever the user was
    dictating into. Computing on every show — not once at startup —
    handles laptops moved between displays mid-session.
  - Light-desktop contrast: a `prefers-color-scheme: light` block
    bumps the dot's red glow from `0.55` to `0.9` opacity and flips
    the pill border to `rgba(0, 0, 0, 0.2)` so the indicator stays
    visible against a bright wallpaper. Pill background stays dark
    — it's the contrast carrier for the white text.
  - HUD window title changed from `"Hush HUD"` to `"Hush —
    Recording"` so screen readers announce something meaningful
    when the window is enumerated. Visible in some platform
    accessibility trees even though `skipTaskbar: true` is set.
- **`stop_dictation` decomposed (closes #38).** The Tauri command's body
  shrank from ~95 lines across 8 inline steps to a flat sequence of
  named helpers: `stop_audio_capture`, `load_vocabulary_prompt`,
  `load_replacement_rules`, `take_foreground_snapshot`,
  `write_to_clipboard`, `fire_ready_notification`,
  `spawn_history_insert`. Behaviour-preserving: every helper keeps the
  best-effort-vs-fatal distinction the inline code had (vocabulary,
  replacements, notification, history are best-effort with `tracing`
  logging; audio.stop, transcription, clipboard remain fatal). New
  helpers are independently unit-tested, including the structural
  audio-error → `IpcError::Audio` mapping that previously relied on
  `stop_dictation`'s shape.
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

- **Audio buffer take is timing-tolerant on stream cleanup.** Earlier
  versions failed with "audio buffer still shared after
  stream drop".** `stop_session` previously used `Arc::try_unwrap` to
  pull the captured samples out of `Arc<Mutex<Vec<f32>>>`, requiring
  *sole* Arc ownership. On some platforms cpal's stream cleanup is
  asynchronous — the callback's Arc clone can outlive the
  `drop(session.stream)` call by a beat — so `try_unwrap` would error
  on a successful recording and the user got "Microphone error: audio
  buffer still shared after stream drop. Try selecting a different
  input device." Replaced with a `lock()` + `mem::take`. Locking is
  correct regardless of how many Arc clones are alive: if a final
  callback is mid-write we wait the milliseconds it takes to finish;
  otherwise the lock is uncontended. The leftover Arc clones drop on
  their own as cpal finishes cleanup. Surfaced during hands-on
  testing on macOS 26 — the issue was likely always intermittent on
  some configurations but the user kept hitting it.
- **Model download wasn't actually reaching the file** (regression
  surfaced by user during hands-on testing of #41/#72). Hugging
  Face migrated large-file serving to their Xet content-addressed
  storage CDN, hosted on `cas-bridge.xethub.hf.co` — a subdomain of
  `hf.co`, not `huggingface.co`. The redirect-allowlist predicate
  added in #53 only allowed `huggingface.co` and its subdomains, so
  every model download died at the very first redirect with
  "redirect to host outside huggingface.co". Predicate now allows
  both HF-owned zones (`huggingface.co` and `hf.co`). Suffix-match
  trap is still defended (typo-squats like `myhf.co` and
  `hf.co.attacker.com` are unit-tested as rejected). Hop cap of 4
  unchanged.
- **Whisper transcription compiled in by default** (closes
  the silent-no-model bug surfaced in hands-on testing). Pre-fix:
  `npm run tauri dev` built without `--features whisper`, so the
  binary contained no Whisper loader code. Users could download a
  model successfully — the file landed on disk at the right path
  with the right SHA — but on the next launch the app reported
  "no transcription model is loaded" because `build_transcriber`
  was a `cfg`-gated stub returning `None`. The diagnostic looked
  identical to "user forgot to download" but had nothing to do with
  the user's actions. `whisper` is now a `default` Cargo feature.
  cmake is therefore mandatory at build time; the README's
  Prerequisites block is updated to call this out in bold. UI-only
  contributors who don't want cmake can opt out via the new
  `npm run tauri:ui-only` script (`--no-default-features`).
- **First-time-user flow: "Set up your first model" banner.** Two
  problems hit the user on the first launch with no model: (a) the
  prominent action surface was Start recording, which on click
  surfaced a stale error pointing at `HUSH_MODEL_PATH` and rebuilding
  with `--features whisper` — instructions for the M1 dev workflow,
  not the M3 user workflow; (b) the actual setup path (the model
  picker) was below the fold with no signpost. Replaced with: a
  prominent "Set up your first model" banner above the recording
  controls, shown only when `models.some(isDownloaded) === false`,
  with a "Choose a model" button that scrolls to the picker. Start
  recording is also disabled in that state with a clear hover/aria
  hint ("Choose a model first") rather than a click-then-error
  flow. The `transcription-unavailable` error copy is rewritten to
  point at the in-app picker instead of env vars, and the click-
  through still scrolls to the picker. Two new Playwright specs pin
  the banner-shown and banner-hidden cases; the existing
  `transcription-unavailable` spec now asserts the new copy and
  asserts the old `HUSH_MODEL_PATH` reference does *not* appear.
- **Model auto-download is functional end-to-end** (closes #41). The
  five Whisper variants in `transcription::catalog` shipped with
  empty `sha256` strings — the auto-download orchestrator's
  defence-in-depth gate refused to start a download without a
  verified hash, so every "Download" click returned the friendly
  "configure manually for now" message and required the user to
  curl the model themselves and place it in the app-data models
  directory. Hashes are now sourced from Hugging Face's git-LFS
  `oid` field (content-addressed, can't drift independently of the
  file content) for `ggml-tiny.bin`, `ggml-base.bin`,
  `ggml-small.bin`, `ggml-medium.bin`, `ggml-large-v3.bin`.
  `ggml-tiny` was independently verified by downloading and running
  `shasum -a 256` against the API value. The download orchestrator's
  empty-hash gate stays in place so a future catalog addition can't
  silently bypass SHA verification.
- **PTT crash on macOS 26+ contained** (closes the crash
  half of the rdev issue; native CGEventTap replacement tracked
  separately). rdev 0.5's CGEventTap callback unconditionally calls
  `TSMGetInputSourceProperty` from its listener thread to compute a
  Unicode key-name string we never read. macOS 26's TSM tightened
  its dispatch-queue assertions and now `dispatch_assert_queue_fail`s
  on the first modifier-key event — a hard `__builtin_trap` (SIGTRAP),
  not a Rust panic, so `catch_unwind` can't save it. Mitigation: PTT
  listener is now skipped on macOS by default, with `HUSH_PTT_ENABLE=1`
  to opt in for users on older macOS where rdev still works, and
  `HUSH_PTT_DISABLE=1` as the kill switch on every platform. Toggle
  hotkey (Tauri's plugin) and button-driven dictation are unaffected.
  Documented in `docs/macos-permissions.md`. The proper fix — a
  native CGEventTap that bypasses TSM — is a follow-up tracking
  issue.
- **HUD window transparency on macOS via `macos-private-api` (closes #62).** The
  HUD's `transparent: true` window flag was a no-op on macOS without
  Tauri's `macos-private-api` Cargo feature + the matching
  `macOSPrivateApi: true` app-config flag. Without those, the dark
  translucent pill the HUD CSS draws was sitting inside a solid
  default window — defeating the design. Both flags are now wired
  on; the dev startup warning ("The window is set to be transparent
  but the `macos-private-api` is not enabled") is gone. Tauri docs
  flag a possible App Store implication; not relevant to Hush's v1
  distribution plan, captured in `learnings.md` for future
  reference.
- **Welcome modal tagline copy.** Said "Two permissions worth knowing
  about before you start" but the modal renders three sections —
  Microphone, Input Monitoring, and a privacy footer that isn't a
  permission per se. Re-worded to "Here's what to know about
  permissions and privacy before you start." Polish-graded leftover
  from the round-4 reviewer pass on #48.
- **Updater plugin no longer panics on app launch.**
  `tauri-plugin-updater::Builder::new().build()` was registered in
  `lib.rs` without a corresponding `plugins.updater` block in
  `tauri.conf.json` (the plugin requires `pubkey` + `endpoints` to
  deserialise). On startup the plugin's deserialiser hit a `null`
  config and the whole app crashed before the main window appeared
  with `PluginInitialization("updater", "...invalid type: null,
  expected struct Config")`. The plugin registration is commented
  out until #10 wires the signing key and endpoints; the Cargo and
  npm deps stay in place so #10 lands as a single focused PR.
- **Welcome modal a11y batch (closes #48).** Round-4 reviewer
  flagged four issues on the recent welcome / model-picker work:
  - Modal had no Escape-key dismissal — keyboard-only users were
    locked into clicking "Got it". A window-level keydown listener,
    gated on `showFirstRun`, now handles Escape (and also persists
    dismissal via `mark_first_run_completed`, matching button
    behaviour).
  - No focus trap — Tab could escape behind the backdrop. The
    modal now traps Tab within its three buttons (cycle forward
    from "Got it" wraps to "Open Microphone settings"; Shift+Tab
    from the first wraps back). Auto-focus lands on the first
    action on open; focus restores to the previously-focused
    element on dismiss.
  - Download progress bar's `aria-valuemax` lied when the total
    size was unknown — fell back to `100` while `aria-valuenow`
    held the byte count, so a screen reader announced
    "3 percent" at 15 MB of an unknown-size file. Indeterminate
    state now omits `aria-valuenow` / `aria-valuemax` (per
    WAI-ARIA convention) and adds an `aria-valuetext` that
    matches what's drawn.
  - Retry-UX race — the optimistic "downloading" chip was set
    *before* the IPC call, so a synchronous failure (e.g.
    SHA-256 not configured) caused a brief flash of progress.
    The optimistic state now sets after the invoke resolves, so
    failure paths simply never show the chip.

  Two new Playwright specs pin the Escape and focus-trap
  behaviour; the previously `fixme`-marked Escape spec is now
  real and passing.

### Tests

- **`drain_buffer` regression tests** for the audio-stop fix in PR #77.
  The cpal stream itself can't be unit-tested (no audio device in CI),
  but the load-bearing race-prone bit — "take the captured samples
  out of the shared `Arc<Mutex<Vec<f32>>>` regardless of how many Arc
  clones cpal hasn't dropped yet" — is now extracted as a free
  function `drain_buffer` and unit-tested. Three cases pinned: take
  from a unique Arc, take while two extra Arc clones are alive
  (simulating the cpal-cleanup-still-in-flight case the user hit on
  macOS 26), and empty-buffer no-op. A future regression that
  reintroduces `Arc::try_unwrap` (or any strong-count-sensitive
  operation) on this path fails the second test.
- **Frontend e2e via Playwright + mocked Tauri IPC.** New
  `tests/e2e/` suite drives the SvelteKit dev server in
  `HUSH_E2E=1` mode — `vite.config.js` swaps
  `@tauri-apps/api/{core,event}` for in-tree stubs in
  `tests/e2e/setup/`, so the page renders in plain Chromium without
  Tauri's runtime. Tests configure per-spec `invoke` handlers and
  fire backend-emitted events via `installMocks(page, overrides)`
  and `fireEvent(page, name, payload)`. New CI job runs the suite on
  Linux. Three smoke tests cover: returning user does not see the
  welcome modal, fresh install does and dismisses it on "Got it",
  and `transcription-unavailable` errors surface the model-path
  recovery hint. A fourth test (`fixme`-marked) documents the
  welcome-modal-no-Escape regression flagged in #48 — it flips
  green automatically when that fix lands. Full-stack flows (HUD
  lifecycle, hotkey registration, real audio, real download) stay
  open behind #57 (tauri-driver path).

### Security

- **HUD window has its own scoped capability** (closes #50). The
  recording HUD's secondary Tauri window (label `hud`) was not in
  any capability file — Tauri 2 defaults unlisted windows to deny,
  meaning the HUD's `listen('audio:level')` call (and so the level
  meter that just landed) silently never fires. Added
  `src-tauri/capabilities/hud.json` granting `core:default` only —
  the HUD doesn't need clipboard, notification, or shortcut
  permissions, so leaving them off keeps the blast radius minimal
  if a future page somehow runs untrusted content.
- **Download client redirect policy is host-restricted** (closes the
  Critical half of #49). The shared reqwest client previously inherited
  reqwest's default `Policy::default()` (up to 10 redirects to *any*
  host); a BGP/DNS hijack of `huggingface.co` could redirect into an
  attacker-controlled origin. SHA-256 verification still catches a
  swapped file, but the bandwidth + latency leak to a non-HF host is
  avoidable. New policy: hop-cap 4, every hop must be `huggingface.co`
  or a subdomain. The host-allowlist predicate is unit-tested,
  including the `evilhuggingface.co` / suffix-match-trap case.
- **README + PRD privacy claims clarified** (Important half of #49).
  Previously the README said "no internet required" — true for
  transcription, false for the first-run model download. Both
  documents now disclose: transcription is fully on-device, no audio
  ever leaves the machine, and the only network traffic is the
  one-time model download from Hugging Face.
- **`tauri-plugin-shell` removed entirely.** Was registered in
  `lib.rs` and present as `@tauri-apps/plugin-shell` in `package.json`
  but never invoked — `open_macos_privacy_pane` uses
  `std::process::Command::new("open")` directly with hard-coded
  whitelisted URLs. Removing the unused plugin tightens the
  capabilities surface (no `shell:allow-execute` exposure), shrinks
  the dep tree, and removes a future-PR footgun (a contributor
  reaching for the plugin would now have to add it back deliberately).
  `@tauri-apps/plugin-opener` was already de-registered on the Rust
  side in PR #31; cleaned up the npm-side leftover at the same time.

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
