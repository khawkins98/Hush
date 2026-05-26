# Architecture

How Hush is built. For *what it is*, see [`README.md`](./README.md). For *what's shipped right now*, see [`STATUS.md`](./STATUS.md). For the *contributor workflow*, see [`CLAUDE.md`](./CLAUDE.md) and [`CONTRIBUTING.md`](./CONTRIBUTING.md). For attribution and legal posture (black-box reimplementation discipline), see [`hush-prd.md` §13.8](./hush-prd.md).

---

## Stack

Hush is a [Tauri 2](https://tauri.app/) desktop app:

- **Backend:** Rust (`src-tauri/`). Audio capture, transcription, persistence, OS integration.
- **Frontend:** SvelteKit + Svelte 5 (`src/`), runes-based (`$state`, `$derived`, `$effect`, `$props()`).
- **IPC:** Tauri commands (Rust ↔ TS), serde-encoded with `camelCase` rename.
- **Persistence:** SQLite via `sqlx`, with FTS5 for history search.
- **Inference:** [whisper.cpp](https://github.com/ggerganov/whisper.cpp) via `whisper-rs`. Optional ONNX speaker-embedding (wespeaker ResNet34-LM) via `tract-onnx` (pure Rust; replaces ORT as of #641).

Primary target: **macOS 26+ on Apple Silicon.** Linux and Windows compile cleanly in CI but are not hands-on tested.

---

## Four windows (three production + one developer-only)

```
┌─────────────────────┐    ┌──────────────┐    ┌───────────────┐    ┌───────────────┐
│  main               │    │  hud         │    │  menu-bar     │    │  debug        │
│  ─────              │    │  ───         │    │  ─────────    │    │  ─────        │
│  Sidebar nav:       │    │  Borderless  │    │  Compact      │    │  Always-on-   │
│   • Dictation       │    │  transparent │    │  popover for  │    │  top palette  │
│   • History         │    │  always-on-  │    │  start/stop + │    │  showing live │
│   • Settings        │    │  top pill    │    │  "Open Hush"  │    │  tracing log  │
│   • About           │    │  Loads /hud  │    │  Loads        │    │  Loads /debug │
│  Loads /            │    │              │    │  /menu-bar    │    │  (dev only)   │
└─────────────────────┘    └──────────────┘    └───────────────┘    └───────────────┘
```

Each window has its own capability file in `src-tauri/capabilities/` (`default.json`, `hud.json`, `menu-bar.json`, `debug.json`). Adding a permission to a window is deliberate — every grant widens that window's blast radius.

**Settings is inline** (since #479). The standalone settings window was merged into the main window as a sidebar panel. Settings is no longer a separate `tauri::WebviewWindow` — it's a Svelte component that renders inside `routes/+page.svelte`. The native menu's "Settings…" and tray's "Open Settings…" emit `settings:goto-tab` which the main page handles.

**Lifecycle.** `main` intercepts `WindowEvent::CloseRequested` and calls `window.hide()` instead of letting Tauri destroy. The tray icon stays alive; ⌘Q (or tray Quit) actually exits. The `hud`, `menu-bar`, and `debug` windows use the standard show/hide pair.

**Background launch.** The autostart plugin registers Hush with `--background`; on login the setup hook hides the main window and switches activation policy to `Accessory` (no Dock icon). User-initiated launches don't pass the flag and show the main window normally.

**Native menu bar (macOS).** `src-tauri/src/app_menu/` — `Hush → Settings…` (⌘,), `View → Dictation/History` (⌘1/⌘2). Menu events emit `settings:goto-tab` or `menu:goto-section` to the main window.

---

## Trait-seam pattern

Every OS-touching layer is a trait, with a concrete impl + hand-rolled mocks at the boundary. The IPC layer holds `Arc<dyn Trait>` so tests can substitute deterministic stubs without spinning up real audio / SQLite / network.

The load-bearing seams:

| Trait | File | Prod impl | Test impl |
|---|---|---|---|
| `audio::AudioCapture` | `audio/mod.rs` | `CpalAudioCapture` | `MockAudio` / domain-specific stubs in `ipc/tests.rs`, `meeting/test_support.rs`, and `audio/tests.rs`; `WavFileAudioCapture` under `--features test-utils` |
| `audio::AudioSession` | `audio/mod.rs` | `CpalMicSessionHandle`; `CoreAudioTapSession` for macOS system audio | `NoOpSession` / `StubSession` in `meeting/test_support.rs`; `WavFileAudioSession` under `--features test-utils` |
| `transcription::Transcribe` | `transcription/mod.rs` | `WhisperTranscription` (gated on `whisper`) | `EchoTranscribe` / `FailingTranscribe` in `ipc/tests.rs`; `NoopStreamTranscribe` in `meeting/test_support.rs`; domain-specific stubs in `commands/**/tests.rs` |
| `diarization::Diarize` | `diarization/mod.rs` | `FlagGatedDiarizer` → `OnnxDiarizer` / `NoopDiarizer` | `NoopDiarizer`; targeted test doubles such as `RecordingDiarizer` in `diarization/mod.rs` tests |
| `events::EventEmitter` | `events.rs` | `ipc::events::TauriEventEmitter` | `events::NoopEventEmitter`; `ipc::events::RecordingEventEmitter` |
| `history::HistoryRepository` | `history/mod.rs` | `SqliteHistoryRepository` | `NoopHistory` / `MemHistory` in `ipc/tests.rs` |
| `meeting::MeetingSessionRepository` | `meeting/mod.rs` | `SqliteMeetingSessionRepository` | `NoopMeetings` in `ipc/tests.rs`; `FailingCloseRepo` in `meeting/test_support.rs`; in-memory SQLite in `SessionManager::new_for_test` |
| `meeting::MeetingAppOverrideRepository` | `meeting/app_overrides.rs` | `SqliteMeetingAppOverrideRepository` | `NoOpAppOverrides` in `meeting/test_support.rs`; `NoopMeetingAppOverrides` in `ipc/tests.rs` |
| `dictionary::VocabularyRepository` | `dictionary/vocabulary/mod.rs` | `SqliteVocabularyRepository` | `NoopVocabulary` in `ipc/tests.rs` |
| `dictionary::ReplacementRepository` | `dictionary/replacements/mod.rs` | `SqliteReplacementRepository` | `NoopReplacements` in `ipc/tests.rs` |
| `settings::SettingsRepository` | `settings/mod.rs` | `SqliteSettingsRepository` | `MemSettings` in `ipc/tests.rs` |

`AppState` (in `ipc/`) is the composition root. `AppStateBuilder` wires the prod impls; tests compose mocks. Tauri's `manage` makes `AppState` available to every command handler.

`AppState` is organised into six domain sub-structs (completed in #737) plus two flat fields:

| Sub-struct / field | Fields | Purpose |
|---|---|---|
| `data: DataServices` | `history`, `replacements`, `vocabulary`, `meetings`, `overrides` | Repository seams for all persisted domain data |
| `flags: RuntimeFlags` | `hud_visible`, `sound_cues`, `diarization_enabled`, `inference_in_progress`, `hotkey_toggle_error`, `recorder_active_sessions` | `AtomicBool`/`Mutex` runtime state shared between the IPC layer and background tasks |
| `ptt: PttState` | `combo`, `active`, `spawned` | Push-to-talk hotkey combo, active flag, and listener task handle |
| `update_check: UpdateCheckCache` | `last`, `inflight` | Manual update-check result cache + in-flight de-dup lock |
| `inference: InferenceState` | `transcribe`, `transcribe_meeting`, `diarize`, `diarize_slot`, `transcriber_generation` | Hot-swap slots for both transcription paths and the diarizer; generation counter guards stale rebuild races |
| `models: ModelStore` | `models_dir`, `downloads` | GGUF/ONNX directory path + in-flight download cancel-handle registry |
| `http: reqwest::Client` | — | Single shared HTTP client (used by both downloads and update-check) |
| `settings: Arc<dyn SettingsRepository>` | — | Settings seam (flat because it's used across sub-struct domains) |

Two `InferenceState` fields carry cross-layer invariants worth flagging when adjacent code changes:
- `transcriber_generation: Arc<AtomicU64>` — race guard for background transcriber rebuilds. Any async task that rebuilds a transcriber must snapshot/compare the generation before installing its result ([#801](https://github.com/khawkins98/Hush/issues/801)).
- `hotkey_toggle_error` lives in `RuntimeFlags` and records the one-time startup result of `register_hotkeys`; the UI surfaces it via IPC. Don't re-diagnose by retrying from the frontend.

**Hot-swappable slots** (all in `InferenceState`):

- `TranscribeSlot = Arc<Mutex<Option<Arc<dyn Transcribe>>>>` — model hot-swap propagates without restart. `AppState` holds **two** independent slots ([#248](https://github.com/khawkins98/Hush/issues/248)): `inference.transcribe` (dictation hot path, read by `stop_dictation`) and `inference.transcribe_meeting` (cloned into `SessionManager`). `model_select` loads two `WhisperTranscription` instances from the same GGUF and writes both via `swap_transcriber(new_dictation, new_meeting)` — the underlying model weights are mmap'd, so the marginal RAM cost is small. The split removes mutex contention between a dictation-hotkey press and an in-flight meeting pump tick.
- `DiarizeSlot = Arc<RwLock<Arc<dyn Diarize>>>` (`inference.diarize_slot`) — wespeaker model download takes effect on the next pump tick.
- `inference_threads: Arc<AtomicI32>` ([#255](https://github.com/khawkins98/Hush/issues/255)) — Settings → General slider value, shared between AppState and every loaded `WhisperTranscription` (both slots above) so a slider change takes effect on the next inference call without a model reload.

On macOS, the `screencapturekit` crate is still linked **unconditionally** (no feature flag) for the permission-diagnostic path and its objc2 bindings, even though runtime system-audio capture moved to the CoreAudio process-tap backend in #588.

---

## Audio capture

`AudioCapture` exposes two APIs:

- **Singleton** — `start_with_source(source) -> ()` + `stop() -> CapturedAudio`. The dictation hot path; one capture at a time.
- **Handle-based** — `start_session(source) -> Box<dyn AudioSession>`. The meeting pump opens one handle per source (mic + macOS system-audio in parallel). Each handle's `stop()` consumes `Box<Self>` so a double-stop is a compile error.

`active_sessions: AtomicU32` refcounts in-flight captures so `is_recording()` returns `count > 0` whether the caller went through the singleton or handle path. `MAX_BUFFER_FRAMES` defends against runaway buffer growth in cpal callbacks.

The cpal mic path hands audio to the consumer via an **`rtrb` SPSC ring** ([#251](https://github.com/khawkins98/Hush/issues/251)) — wait-free producer push from the realtime callback thread, wait-free consumer drain. If the user-selected mic is missing at session start, `start_cpal_session` logs a warning and **falls back to the system default input device** ([#705](https://github.com/khawkins98/Hush/issues/705)) rather than hard-failing; mid-session disconnects still surface as typed `DeviceLost` errors so the meeting pump can show recovery UI.

System-audio capture on macOS uses **`AudioHardwareCreateProcessTap`** (the CoreAudio process tap API, macOS 14.2+) via a small Swift helper binary at `resources/macos-audio-tap.swift`, compiled by `build.rs` to `src-tauri/resources/hush-audio-tap-capture` and bundled as a Tauri resource ([#588](https://github.com/khawkins98/Hush/issues/588), [#594](https://github.com/khawkins98/Hush/pull/594)). The Swift binary writes a 12-byte `HUSH` magic + sample-rate + channel-count header to stdout, then streams interleaved f32 LE PCM continuously. The Rust side (`audio/core_audio_tap.rs`) spawns that binary, reads the header, and pumps samples from the child's stdout into an `rtrb` ring that the meeting-pump drains per tick.

This replaces an earlier ScreenCaptureKit path (removed in #588). The codec processing SCK applied internally was producing PCM that triggered Whisper's `no_speech_thold` gate to drop every segment as silence — a class of bug well-suited to direct PCM capture. The CoreAudio tap delivers raw, uncompressed audio with zero codec round-trip, and uses the `NSAudioCaptureUsageDescription` permission rather than the alarming `NSScreenCaptureUsageDescription` lock-icon dialog.

Linux ([#106](https://github.com/khawkins98/Hush/issues/106)) and Windows ([#107](https://github.com/khawkins98/Hush/issues/107)) impls are open issues; on those platforms `AudioSource::SystemAudio` returns an explicit "not yet implemented" error from the trait. The trait seam is in place — the second implementations are not.

---

## Meeting pump

`meeting::SessionManager::start_manual(sources, app_name)` runs continuously:

```
open handles + per-source StreamingTranscribeSession(s)
                    │
                    ▼
          spawn tokio task: run_pump()
                    │ every PUMP_TICK (500 ms)
                    ▼
       drain each AudioSession into per-source scratch buffers
                    │
                    ├─ mirror canonical 16 kHz mono audio into AudioRollingBuffer
                    │
                    └─ feed each source's StreamingTranscribeSession in spawn_blocking
                                   │
                                   ▼
                      collect TickBucket { source, utterances, audio }
                                   │
                                   ▼
                    diarize_and_dispatch_merged(...) across all sources
                                   │
                    ┌──────────────┴──────────────┐
                    ▼                             ▼
          update in-memory partials      persist final utterances
          + emit live events             + emit session events / notices
```

**State machine.** `Mutex<SessionState>` where `SessionState` is `Idle | Opening | Active(...) | Releasing`. The `Opening` sentinel is held across the async DB / handle-open work so concurrent `meeting_start_manual` IPC calls can't race past the precondition. `Releasing` covers the brief foreground window between `stop_manual` signalling cancel and the pump confirming audio is released; it blocks a concurrent meeting start (the capture singleton isn't free yet) but not a dictation start.

**Shutdown / release-then-finalize.** `stop_manual` signals cancel and awaits only an audio-released checkpoint — not the full tail flush. The pump's cancel path is:

1. Final drain of each source's ring buffer.
2. **Ack-waited stop** of every `AudioSession` (`handle.stop()` round-trips `Cmd::Stop` and returns the captured tail, rather than relying on `Drop` which discards the tail and doesn't guarantee the device is free before the reply). The tail samples are fed into the streaming session.
3. Emit `meeting:finalizing { sessionId }` — frontend clears `activeId` (unblocking PTT/dictation) and shows "Finishing transcription…".
4. Fire the `audio_released` oneshot, allowing `stop_manual` to flip `Releasing → Idle`, park the pump's continuation in the single `finalizing` lane, and return. **Sub-second.**
5. **Background phase** (no audio held): `flush_sessions` (`finish()` per source, up to 60 s each) → tail diarization → persist tail finals → speaker-identity resolution (`session_centroids()`, #667) → `repo.close_session(id)` → emit `meeting:session-ended`.

`SessionManager` holds a single `finalizing: Mutex<Option<JoinHandle<()>>>`. A new meeting `start_manual` awaits this handle before claiming the slot (the new session would otherwise share the diarizer cluster state and the meeting `WhisperContext` with the finalizing one — see the "Deferred: concurrent meetings" note below). Dictation `start` does **not** wait: it uses a separate transcriber slot (#248) and no diarizer, so it proceeds as soon as `live = Idle`. `SessionManager::Drop` aborts the handle (abort-and-reconcile: the `finish()` runs in `spawn_blocking` and cannot be cancelled, so joining would hang shutdown; any unclosed session row is cleaned up by `reconcile_orphan_sessions` on next launch).

**Meeting events emitted:**

| Event | Payload | When |
|---|---|---|
| `meeting:session-started` | `{ sessionId }` | Pump open, before first tick |
| `meeting:source-failed` | `{ sessionId, source, error }` | A capture handle fails mid-session |
| `meeting:finalizing` | `{ sessionId }` | Audio released; background tail flush starting |
| `meeting:session-ended` | `{ sessionId }` | Background finalization complete; DB row closed |
| `meeting:tail-dropped` | `{ sessionId }` | A `finish()` timed out or errored (backstop, fires in background) |

**Deferred: concurrent meetings.** A new meeting cannot start while a previous one finalizes — it awaits the `finalizing` handle. This is deliberate: all meeting streaming sessions clone one `Arc<Mutex<WhisperContext>>` (`transcription/whisper.rs:460`); `infer`/`finish` hold that lock across the entire inference, so a live meeting and a finalizing meeting sharing it would freeze the live transcript. The shared diarizer has the same problem. Enabling concurrency requires per-session whisper contexts + diarizer isolation. See `docs/meeting-background-finalization-proposal.md` "Deferred" for the step-by-step.

`SessionManager::Drop` aborts the pump's `JoinHandle` on app shutdown; `CpalMicSessionHandle` and `CoreAudioTapSession` both have `Drop` impls that release their OS resources (the tap session sends `SIGTERM` to the Swift helper, with a `SIGKILL` fallback after 1 s).

**Privacy.** Audio is buffered in RAM (`AudioRollingBuffer`, ~30 s window) and never written to disk. Only the resulting transcript text is persisted.

---

## Meeting auto-detection (macOS, #665)

Hush can auto-start a meeting session when a Meeting-classified app is frontmost and the microphone activates. Two independent background tasks handle this:

**`run_profile_autoactivate_poller`** — a 3-second ticker that detects which app is in focus and, when it has a per-app profile (preferred audio source / model), emits `app:profile-activated` so the frontend updates its dropdowns. Independent of meeting auto-start mode.

**`run_meeting_detection_task`** (macOS only) — event-driven via the CoreAudio HAL. Registers `AudioObjectAddPropertyListener` callbacks on `kAudioDevicePropertyDeviceIsRunningSomewhere` for all enumerated input devices, plus a hot-plug listener on `kAudioObjectSystemObject` for `kAudioHardwarePropertyDevices`. The callback posts `tokio::sync::Notify::notify_one()`; the task loop awaits each notification, then evaluates the pure state machine `evaluate_mic_state(inputs) -> MicStateOutcome`.

```
CoreAudio HAL thread  ──notify_one()──▶  Notify  ──notified().await──▶  task loop
                                                                             │
                                                             is_any_device_active()
                                                                  + get_active_window()
                                                                  + classifier.classify()
                                                                             │
                                                             evaluate_mic_state(inputs)
                                                                             │
                                                   ┌─────────────────────────────────────┐
                                    MicStateOutcome::Start { app_name }   AutoStop
                                                   │                         │
                                             start_manual(...)         stop_and_rebuild()
```

### Detection logic matrix

`evaluate_mic_state` is a pure function; every auto-start decision passes through it. The table below is exhaustive — rows are evaluated **top-to-bottom**; the first matching row wins.

| # | `mic_is_active` | `mode` | `session_active` | `session_emitted` | `frontmost_app_kind` | Outcome | Side-effect on caller |
|---|---|---|---|---|---|---|---|
| 1a | ❌ false | any | ✅ true | ✅ true | any | `AutoStop` | Caller stops session, resets `session_emitted = false` |
| 1b | ❌ false | any | any | any | any | `ResetSessionEmitted` | Caller resets `session_emitted = false` |
| 2 | ✅ true | `Off` | any | any | any | `Idle` | — |
| 3 | ✅ true | `Always` | ✅ true | any | any | `Idle` | — (session already running) |
| 4 | ✅ true | `Always` | ❌ false | ✅ true | any | `Idle` | — (already started this cycle) |
| 5 | ✅ true | `Always` | ❌ false | ❌ false | `Other`/`Media` | `Idle` | — (app not a meeting app) |
| 6 | ✅ true | `Always` | ❌ false | ❌ false | `Meeting` | `Start { app_name }` | Caller sets `session_emitted = true`, calls `start_manual` |

**Row 1a — auto-stop:** Mic went quiet and we hold an auto-started session (`session_emitted = true`). Stop the session so users don't end up with a ghost recording after their call ends. Uses the same `stop_meeting_and_rebuild_transcriber` helper as the manual Stop button, so transcribers and diarizer are rebuilt in the background.

**Row 1b — mic quiet:** Mic went quiet and no auto-started session is running. Reset `session_emitted` so the next mic activation can start a fresh session.

**Row 3 — session already running:** Prevents auto-start from racing with a manual start or from firing again if the user manually stopped and immediately re-triggered the mic.

**Row 4 — `session_emitted` guard:** The HAL may fire multiple property-change events during a single activation cycle (e.g. device re-checks, hot-plug refresh). Without this guard, each event while the mic is active would re-call `start_manual`.

**Row 5 — app classification:** The frontmost app at evaluation time must be classified as `Meeting` by `AppClassifier`. Apps classified as `Media` (music players, video editors) or `Other` do not trigger auto-start. See [App classification](#app-classification) below.

### App classification

`AppClassifier::default_table` classifies apps by executable name (Linux/Windows) or macOS bundle ID. The `Meeting` set covers:

| App | macOS bundle ID | Other identifiers |
|---|---|---|
| Zoom | `us.zoom.xos` | `zoom.us`, `Zoom.exe` |
| Microsoft Teams (new) | `com.microsoft.teams2` | `msteams`, `Teams.exe` |
| Microsoft Teams (classic) | `com.microsoft.teams` | — |
| Google Meet (Chrome) | `com.google.Chrome` | `google-chrome`, `chrome.exe` |
| Slack (calls) | `com.tinyspeck.slackmacgap` | `slack`, `Slack.exe` |
| Webex | `com.cisco.webexmeetingsapp` | `Webex`, `webex`, `CiscoCollabHost.exe` |
| Discord | — | `Discord`, `discord`, `Discord.exe` |
| Skype | — | `Skype`, `skype`, `Skype.exe` |
| GoToMeeting | — | `GoToMeeting`, `Citrix GoToMeeting.exe` |
| BlueJeans | — | `BlueJeans`, `BlueJeans.exe` |
| FaceTime | `com.apple.FaceTime` | — |
| Tuple | `app.tuple.app` | — |
| Around | `co.around.Around` | — |
| Loom | `com.loom.desktop` | `Loom.exe` |

Apps classified as `Media` (music players, video editors, etc.) never trigger auto-start even if the mic activates. Everything else is `Other` and also blocked.

### Input-only device filter

`kAudioDevicePropertyDeviceIsRunningSomewhere` fires for **both** input and output devices. The monitor registers listeners only for input devices (those with at least one input stream buffer), checked via `kAudioDevicePropertyStreamConfiguration` + `kAudioObjectPropertyScopeInput`. Output-only devices (speakers, display audio) are skipped — without this filter, playing music would trigger auto-start.

### Hot-plug handling

A `SystemListenerHandle` listens for `kAudioHardwarePropertyDevices` changes on `kAudioObjectSystemObject`. When a device is added or removed, `MicCameraMonitor::refresh_devices()` re-enumerates input devices and re-installs listeners. USB headsets and Bluetooth audio devices plugged in after launch are automatically covered.

### Memory safety

`DeviceListenerHandle` stores an `Arc<Notify>` clone. `Drop` calls `AudioObjectRemovePropertyListener` **first** (synchronous — the HAL waits for all in-flight callbacks to drain before returning), then drops the `Arc`. The HAL callback reconstructs the `Arc` from the raw pointer using `Arc::from_raw` + `Arc::into_raw` (no net reference-count change). No dangling-pointer risk regardless of callback scheduling.

---

## Diarization

`FlagGatedDiarizer` reads the `diarization_enabled` `AtomicBool` from `AppState` and routes to:

- **`OnnxDiarizer`** — wespeaker ResNet34-LM ONNX speaker-embedding (~26 MB) + online 1-NN-with-threshold clustering for session-stable IDs. Model auto-downloads from Hugging Face on first enable, SHA-256 verified.
- **`NoopDiarizer`** — fallback. Source-derived `"mic"` / `"system"` tags pass through and the panel maps them to "You" / "Remote".

The `OnnxDiarizer` is gated behind the `diarization-onnx` Cargo feature (default-on). The earlier D1 silence-gap heuristic (`EnergyDiarizer`) and the offline agglomerative `cluster_with_threshold` were both removed in #310 once the streaming D2 matcher proved stable; `cluster.rs` retains only `cosine_distance` + `DEFAULT_DISTANCE_THRESHOLD`.

### ORT → tract-onnx migration (#641)

The diarizer originally used ONNX Runtime (ORT), which, despite an explicit CPU execution provider, routed operations through Metal Performance Shaders on Apple Silicon. This caused `IOAccelerator` allocations (~1.25 GB/hr during long meetings) that were not reclaimed after session end, eventually exhausting virtual memory. Switched to `tract-onnx` (pure Rust, zero Metal dispatch) in v0.5.0:

- **Binary size:** −45 MB (no vendored ORT runtime)
- **Latency:** unchanged (~50–100 ms/utterance on a CPU-bound path)
- **Memory:** zero `IOAccelerator` growth; all allocations return to the standard Rust heap
- **Compatibility:** wespeaker ResNet34-LM uses only standard ONNX ops (Conv, Gemm, Add, Relu, etc.) supported by tract

`TypedRunnableModel<TypedModel>` is `Send + Sync`; no `Mutex` needed around the loaded model. The `MelExtractor` is owned by `OnnxDiarizer` to avoid per-call re-init cost. CoreML/Neural Engine acceleration is not pursued — the CPU-only path is sufficient for the diarization workload.

---

## IPC

Tauri commands (`#[tauri::command]`) live in `src-tauri/src/ipc/commands/`. The four-place sync rule (Rust handler → `tauri::generate_handler![]` registration → TS type → Playwright mock) is documented in [CLAUDE.md → "The four-place IPC sync rule"](./CLAUDE.md#the-four-place-ipc-sync-rule). CI catches Rust-only and TS-only breaks but cannot catch shape mismatches between them — that's a hands-on responsibility.

`IpcError` is a tagged enum; new variants need a corresponding case in `src/lib/errors.ts::formatErrorDisplay` so the structured `{ headline, hint?, details? }` shape renders correctly in `ErrorDisplay.svelte`.

---

## Logging / observability

Tracing events flow into three independent sinks, composed in `lib.rs::init_tracing`:

1. **stderr fmt layer** — `tracing_subscriber::fmt`, filtered by `RUST_LOG` (defaults to `info`). The dev-loop default; visible when running `cargo tauri dev` or via `Console.app` for bundled builds.
2. **Daily-rolling file appender** — writes plain-text logs to `~/Library/Logs/io.github.khawkins98.hush/hush.log.<YYYY-MM-DD>` on macOS, via `tracing-appender` (#624). Same `RUST_LOG` filter, ANSI stripped, non-blocking writer with a guard returned from `init_tracing`. macOS-only by `cfg`; gracefully no-ops on other platforms. Disable with `HUSH_LOG_FILE=off`. The Settings → Debug tab surfaces the path with "Reveal in Finder" + "Copy grep command" buttons (#627).
3. **`DebugLogLayer` in-memory ring** — `src-tauri/src/debug_log/`, captures the last N events into a `Mutex<VecDeque>` and emits each event over the `log:event` Tauri event so the floating debug-console window can render the live stream. Bounded ring; drops the oldest event when full. Survives the lifetime of the process only; for post-hoc grepping use sink 2.

---

## Runtime tunables (environment variables)

Read once at process / session construction. Mid-session changes do not take effect.

| Var | Default | Effect |
| --- | --- | --- |
| `RUST_LOG` | `info` | Filter applied to both stderr and file sinks. |
| `HUSH_LOG_FILE` | (on) | Set to `off` (or `0`) to disable the daily-rolling file appender. |
| `HUSH_WHISPER_STATE_RECREATE_INTERVAL` | `30` | Number of streaming inferences before `WhisperState` is dropped + lazy-recreated to bound whisper.cpp's per-call C-heap accumulation (#623). Set to `0` for "never recreate" (legacy / A-B test). |
| `HUSH_DIARIZER_THRESHOLD` | `0.4` | Cosine-distance threshold for declaring two utterance embeddings distinct speakers in `OnnxDiarizer::SessionClusterState::assign` (#316 / #633). Lower → more clusters; higher → speakers merge. Range `[0.0, 2.0]`. Out-of-range values warn and fall back to default. |

For the in-process tunables that aren't env-driven (inference thread count, model selection, diarization toggle), see the hot-swap slots in the trait-seam pattern section above — those flow through Settings.

---

## Persistence

SQLite via `sqlx`. Migrations in `src-tauri/migrations/` (sqlx-managed, applied at startup). Schemas:

- **History** — dictation transcripts, with FTS5 over the text + foreground app metadata. Recordings shorter than 1 second are stored as *ignored* entries (`ignored = 1`): no transcript is attempted, stats queries filter them out (`WHERE ignored = 0`), and bulk export skips them. They remain visible in the UI as a dimmed "Recording too short — not transcribed" row so users can see accidental hotkey presses were registered (#682).
- **Meeting sessions** — session rows + utterance rows; `ended_at` set on stop.
- **Vocabulary / replacements** — Personal Dictionary CRUD.
- **Settings** — key/value, including PTT combo, autostart, diarization toggle, app overrides.

The `models/` directory under `<app-data>/` holds the GGUF whisper checkpoints + the wespeaker ONNX file. SHA-256 verified on download; host-restricted to `huggingface.co` / `*.hf.co` (one signed-CDN hop allowed for HF's storage backend), hop-cap 4.

---

## Module map

**Backend** (`src-tauri/src/`):

| Module | Responsibility |
|---|---|
| `app_menu/` | Native macOS menu bar wiring (`Hush`, `View`, update check, section navigation) |
| `audio/` | cpal mic capture + macOS CoreAudio process tap (via Swift helper at `resources/macos-audio-tap.swift`) + `AudioSession` handle trait; `file_source.rs` adds the WAV-backed test seam under `--features test-utils` |
| `audio_cues.rs` | Start / done cue synthesis and playback |
| `db/` | SQLite database bootstrap / migrations wiring |
| `debug_log/` | In-memory tracing ring buffer for the debug console |
| `diarization/` | `Diarize` trait, tract-onnx wespeaker impl, online clustering, mel-FB features |
| `dictionary/` | Vocabulary + replacement repositories; `packs.rs` defines compile-time preset packs whose enabled slugs live in settings |
| `events.rs` | Crate-root `EventEmitter` trait seam shared by IPC and meeting code |
| `history/` | Dictation history repository + FTS-backed search/export |
| `hotkey/` | `tauri-plugin-global-shortcut` for toggle; pinned `fufesou/rdev` for PTT |
| `hud/` | Recording HUD pill (drag, dismiss, level meter) |
| `ipc/` | `AppState`, `AppStateBuilder`, `IpcError`, production startup helpers, Tauri event emitters, and command handlers split by domain (`dictation/`, `meeting`, `models`, `permissions`, `ptt`, `system`, etc.) |
| `meeting/` | `SessionManager`, streaming pump, lifecycle, recovery, event emission, app classifier, per-app overrides, repository, and macOS CoreAudio event-driven auto-start (`autostart.rs`, `mic_camera_monitor.rs`) |
| `permissions/` | Cross-platform permission state. `permissions/macos.rs` does programmatic TCC reads via AVFoundation / CoreGraphics / IOKit; `permissions/mod.rs` is the home for future Linux / Windows impls. Renamed from `macos_perms/` in #597. |
| `repository.rs` | Generic CRUD supertrait used by vocabulary / replacements / meetings |
| `settings/` | Settings repository plus string/JSON codec helpers |
| `transcription/` | `Transcribe` trait, whisper-rs backend, streaming session machinery, GGUF download/catalog, resample |
| `tray/` | Status-bar / system-tray icon and menu-bar popover launcher |
| `updater/` | Manual `check_for_updates` probe against GitHub releases |
| `lib.rs` / `main.rs` | Tauri builder, plugin registration, tracing init, and binary entrypoint |

**Frontend** (`src/`):

| Path | Responsibility |
|---|---|
| `routes/+page.svelte` | Main window — Dictation / History / Settings / About sections |
| `routes/hud/+page.svelte` | HUD pill |
| `routes/menu-bar/+page.svelte` | Menu-bar quick-access popover |
| `routes/debug/+page.svelte` | Floating debug console palette (developer only) |
| `lib/state/dictation.svelte.ts` | Recording lifecycle state machine (see below) |
| `lib/state/audio.svelte.ts` | Audio source selection + session state |
| `lib/state/history.svelte.ts` | Dictation history list + refresh |
| `lib/state/meeting-sessions.svelte.ts` | Meeting session list, active session, notices |
| `lib/state/meeting-settings.svelte.ts` | Meeting tab: auto-start config, per-app overrides load/save |
| `lib/state/diarizer.svelte.ts` | Diarizer model status, download, removal, enable toggle |
| `lib/state/models.svelte.ts` | Whisper model list, download progress, selection |
| `lib/state/vocabulary.svelte.ts` | Vocabulary terms CRUD + language-style picker |
| `lib/state/permissions.svelte.ts` | macOS TCC permission state: `diagnostic`, `permStatuses`, health, dialog; `diagnose()` / `refreshHealth()` / `openDialog()` (#722) |
| `lib/state/onboarding.svelte.ts` | First-run wizard flag: `showFirstRun`, `check()`, `completeFirstRun()` (#722) |
| `lib/state/replacements.svelte.ts` | Replacements CRUD (find/replace rules, load/add/remove) |
| `lib/state/ptt.svelte.ts` | PTT persisted config: combo, enabled, listenerRunning; load/persist IPC |
| `lib/state/general-settings.svelte.ts` | General settings (autostart, clipboard, sound cues, theme) |
| `lib/state/general-runtime.svelte.ts` | Runtime/performance settings (inference threads, mic gain, first-run reset) |
| `lib/state/palette.svelte.ts` | Command palette (Cmd+K) state: open/close, query, filtered results |
| `lib/state/nav.svelte.ts` | Sidebar + settings tab navigation state |
| `lib/AppLifecycle.svelte` | App-level lifecycle container: Tauri event listeners (hotkey, PTT, menu, model-download, meetings), permission side-effects, first-run checks; no markup |
| `lib/HistoryActionRow.svelte` | Shared action row (copy / export-format menu / delete) reused by `HistoryDictationRow` and `HistoryMeetingRow` |
| `lib/*.svelte` | Svelte 5 component library (panels, modals, sidebar, error display, form editors) |
| `lib/types.ts` | TS shapes mirroring backend serde structs (camelCase) |
| `lib/errors.ts` | `IpcError` → `ErrorDisplay` mapping |

---

## Frontend recording lifecycle

`lib/state/dictation.svelte.ts` owns the dictation recording lifecycle as a discriminated-union state machine:

```
type RecordingPhase =
  | { tag: 'idle' }
  | { tag: 'starting' }
  | { tag: 'recording'; mode: RecordMode; meetingId: number | null; startedAtMs: number }
  | { tag: 'stopping'; mode: RecordMode; meetingId: number | null; startedAtMs: number }
  | { tag: 'transcribing' }
```

`recording`, `busy`, `transcribing`, and `recordMode` are `$derived` from `phase`; illegal combinations (e.g. `recording && transcribing`) are structurally impossible.

**Two start paths, one stop path:**

- `start()` — uses `start_dictation` / `stop_dictation`. Applies vocabulary biasing, text replacements, and backend clipboard write. Used by toggle hotkey and PTT.
- `startRecord()` — uses `meeting_start_manual` / `meeting_stop_manual`. Adds system-audio when the platform reports `is_supported = true` for the `system-audio` source listing (today: macOS only). The derived flag is still named `screenRecordingLive` in source for historical reasons; system audio no longer requires Screen Recording permission post-#588. Used by the UI record button.
- `stop(trailingMs?)` — shared stop path. If `meetingOnlyActive` is true (auto-detected meeting session running outside the dictation state machine), it delegates straight to `meeting.stopSession()` ([#959](https://github.com/khawkins98/Hush/issues/959)). Otherwise it guards on `phase.tag === 'recording'`, applies the trailing-silence buffer (500 ms by default), then delegates to `_stopDictation()` or `_stopMeeting()`.

**Stop helpers:**

- `_stopDictation()` — calls `stop_dictation`, receives the result directly, transitions to `idle`.
- `_stopMeeting()` — calls `meeting_stop_manual` (which awaits pump drain before returning), then transitions through `transcribing` while fetching `meeting_session_get` once — the result is shared for both clipboard copy and the result block.

**Stop-failure recovery:** if `_stopMeeting` throws, the catch block calls `meeting_active_session` directly. If the session is gone on the backend → `idle`. If still live → restore to `recording` so the user can retry.

**Meeting-only active state.** Auto-detected meeting sessions run through `meeting-sessions.svelte.ts` (`meeting.activeId`, `meeting.busy`, `meeting.stopSession()`), not through the `dictation` state machine. `DictationSection.svelte` derives:

```
meetingOnlyActive = meeting.activeId !== null && !dictation.recording && !dictation.busy
anyRecordingActive = dictation.recording || meetingOnlyActive
```

`RecordPanel` receives `meetingOnlyActive` and enters a red-waveform meeting mode when true. `+page.svelte` derives the same pair and wires all global signals — document title, tray `UiRecordingState`, sidebar recording dot, toggle hotkey, command palette — to `anyRecordingActive` so they respond to both dictation and meeting sessions.

**Import path:** `.svelte.ts` modules must be imported via the `.svelte` path (e.g. `import { dictation } from '$lib/state/dictation.svelte'`), not `.ts`.

---

## Testing infrastructure

### Rust unit tests

Standard `cargo test --lib`. No audio device needed. The trait-seam pattern means every IPC command can be tested with in-memory stubs for `AudioCapture`, `Transcribe`, `Diarize`, `HistoryRepository`, and `MeetingSessionRepository`.

### Rust integration tests (`src-tauri/tests/`)

Two categories of `#[ignore]`'d integration tests that require real external resources:

| Test file | Feature flag | Env vars required | What it tests |
|---|---|---|---|
| `tests/meeting_fixture.rs` | `test-utils,whisper` | `HUSH_TEST_MODEL`, `HUSH_TEST_AUDIO` | Full `SessionManager → pump → WhisperTranscription → SQLite` path via `WavFileAudioCapture` |
| `tests/diarization_fixture.rs` | `diarization-onnx` | `HUSH_DIARIZER_MODEL`, `HUSH_TEST_SPEAKER1_AUDIO`, `HUSH_TEST_SPEAKER2_AUDIO` | `AudioRollingBuffer → OnnxDiarizer → speaker_label` pipeline: two-speaker distinctness + sub-threshold passthrough |

Run commands and download instructions are in [`src-tauri/tests/fixtures/README.md`](./src-tauri/tests/fixtures/README.md).

**`WavFileAudioCapture`** (in `audio/file_source.rs`, compiled under `--features test-utils`) is a deterministic file-backed `AudioCapture` / `AudioSession` implementation that replays pre-loaded WAV samples to the meeting pump in configurable-size chunks. It serves as the seam between the live hardware path and the test path.

### Frontend e2e tests

Two paths, both in `tests/`:

- **Path A** (`tests/e2e/`, `npm run test:e2e`) — Playwright with mocked Tauri IPC (`tests/e2e/_mock.ts`). Fast, no real binary needed. Mocks are serialised via `toString()` and rebuilt in the page context; they cannot capture closure variables — per-test counters must go through `page.exposeFunction`.
- **Path B** (`tests/e2e-tauri/`, `npm run test:e2e:tauri`) — tauri-driver + WebdriverIO against a real debug binary. CI integration deferred until tauri-driver's macOS path stabilises.

---

## Cross-cutting

- **Conventions** (commit format, branch naming, comment style, untagged-TODO lint) — see [CLAUDE.md → Conventions](./CLAUDE.md#conventions).
- **macOS TCC dev-binary quirk** — see [`docs/macos-permissions.md`](./docs/macos-permissions.md).
- **Release pipeline** — see [`docs/releases.md`](./docs/releases.md).
- **Why a particular call was made** — see [`learnings.md`](./learnings.md), the append-only decision log.
- **Black-box reimplementation discipline** — VoiceInk's source is never read; see [`hush-prd.md` §13.8](./hush-prd.md).
