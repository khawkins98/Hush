# Architecture

How Hush is built. For *what* it is, see [`hush-prd.md`](./hush-prd.md). For *what's shipped right now*, see [`STATUS.md`](./STATUS.md). For the *contributor workflow*, see [`CLAUDE.md`](./CLAUDE.md) and [`CONTRIBUTING.md`](./CONTRIBUTING.md).

---

## Stack

Hush is a [Tauri 2](https://tauri.app/) desktop app:

- **Backend:** Rust (`src-tauri/`). Audio capture, transcription, persistence, OS integration.
- **Frontend:** SvelteKit + Svelte 5 (`src/`), runes-based (`$state`, `$derived`, `$effect`, `$props()`).
- **IPC:** Tauri commands (Rust ↔ TS), serde-encoded with `camelCase` rename.
- **Persistence:** SQLite via `sqlx`, with FTS5 for history search.
- **Inference:** [whisper.cpp](https://github.com/ggerganov/whisper.cpp) via `whisper-rs`. Optional ONNX speaker-embedding (wespeaker ResNet34-LM) via `ort`.

Primary target: **macOS 26+ on Apple Silicon.** Linux and Windows compile cleanly in CI but are not hands-on tested.

---

## Three windows

```
┌─────────────────────┐    ┌─────────────────────┐    ┌──────────────┐
│  main               │    │  settings           │    │  hud         │
│  ─────              │    │  ─────              │    │  ───         │
│  Sidebar nav:       │    │  Model picker       │    │  Borderless  │
│   • Dictation       │    │  Vocabulary         │    │  transparent │
│   • Meetings        │    │  Replacements       │    │  always-on-  │
│   • History         │    │  TCC diagnostic     │    │  top pill    │
│  Loads /            │    │  PTT editor         │    │  Loads /hud  │
│                     │    │  Loads /settings    │    │              │
└─────────────────────┘    └─────────────────────┘    └──────────────┘
```

Each window has its own capability file in `src-tauri/capabilities/` (`default.json`, `settings.json`, `hud.json`). Adding a permission to a window is deliberate — every grant widens that window's blast radius.

**Lifecycle.** `main` and `settings` intercept `WindowEvent::CloseRequested` and call `window.hide()` instead of letting Tauri destroy. The tray icon stays alive; ⌘Q (or tray Quit) actually exits. The `hud` uses the standard show/hide pair.

**Background launch.** The autostart plugin registers Hush with `--background`; on login the setup hook hides the main window and switches activation policy to `Accessory` (no Dock icon). User-initiated launches don't pass the flag and show the main window normally.

**Native menu bar (macOS).** `src-tauri/src/app_menu/` — `Hush → Settings…` (⌘,), `View → Dictation/Meetings/History` (⌘1/⌘2/⌘3). Menu events emit `menu:goto-section` to `main` or call `settings_window::show` directly.

---

## Trait-seam pattern

Every OS-touching layer is a trait, with a concrete impl + hand-rolled mocks at the boundary. The IPC layer holds `Arc<dyn Trait>` so tests can substitute deterministic stubs without spinning up real audio / SQLite / network.

The load-bearing seams:

| Trait | File | Prod impl | Test impl |
|---|---|---|---|
| `audio::AudioCapture` | `audio/mod.rs` | `CpalAudioCapture` | inline mocks in `ipc/mod.rs` tests |
| `transcription::Transcribe` | `transcription/mod.rs` | `WhisperTranscribe` (gated on `whisper`) | trait default + `Noop*` |
| `diarization::Diarize` | `diarization/mod.rs` | `FlagGatedDiarizer` → `OnnxDiarizer` / `NoopDiarizer` | `NoopDiarizer` |
| `history::HistoryRepository` | `history/` | `SqliteHistoryRepository` | `Mem*` |
| `meeting::MeetingSessionRepository` | `meeting/` | `SqliteMeetingSessionRepository` | `Mem*` |
| `dictionary::*Repository` | `dictionary/` | SQLite-backed | `Mem*` |
| `settings::SettingsRepository` | `settings/` | SQLite-backed | `Mem*` |

`AppState` (in `ipc/`) is the composition root. `AppStateBuilder` wires the prod impls; tests compose mocks. Tauri's `manage` makes `AppState` available to every command handler.

**Hot-swappable slots.**

- `TranscribeSlot = Arc<Mutex<Option<Arc<dyn Transcribe>>>>` — model hot-swap propagates without restart. `AppState` holds **two** independent slots ([#248](https://github.com/khawkins98/Hush/issues/248)): `transcribe` (dictation hot path, read by `stop_dictation`) and `transcribe_meeting` (cloned into `SessionManager`). `model_select` loads two `WhisperTranscription` instances from the same GGUF and writes both via `swap_transcriber(new_dictation, new_meeting)` — the underlying model weights are mmap'd, so the marginal RAM cost is small. The split removes mutex contention between a dictation-hotkey press and an in-flight meeting pump tick.
- `DiarizeSlot = Arc<RwLock<Arc<dyn Diarize>>>` — wespeaker model download takes effect on the next pump tick.
- `inference_threads: Arc<AtomicI32>` ([#255](https://github.com/khawkins98/Hush/issues/255)) — Settings → General slider value, shared between AppState and every loaded `WhisperTranscription` (both slots above) so a slider change takes effect on the next inference call without a model reload.

---

## Audio capture

`AudioCapture` exposes two APIs:

- **Singleton** — `start_with_source(source) -> ()` + `stop() -> CapturedAudio`. The dictation hot path; one capture at a time.
- **Handle-based** — `start_session(source) -> Box<dyn AudioSession>`. The meeting pump opens one handle per source (mic + macOS system-audio in parallel). Each handle's `stop()` consumes `Box<Self>` so a double-stop is a compile error.

`active_sessions: AtomicU32` refcounts in-flight captures so `is_recording()` returns `count > 0` whether the caller went through the singleton or handle path. `MAX_BUFFER_FRAMES` defends against runaway buffer growth in cpal callbacks.

Both the cpal mic path and the ScreenCaptureKit system-audio path now hand audio to the consumer via an **`rtrb` SPSC ring** ([#251](https://github.com/khawkins98/Hush/issues/251)) — wait-free producer push from the realtime callback thread, wait-free consumer drain. SCK's callback signature takes `&self`, so the producer is wrapped in an `UnsafeCell<Producer<f32>>` + `unsafe impl Sync` whose SAFETY argument grounds in ScreenCaptureKit's serial-per-handler dispatch contract. See `learnings.md` 2026-04-30 entry.

System-audio capture uses **ScreenCaptureKit** on macOS (linked unconditionally — no feature flag). Linux ([#106](https://github.com/khawkins98/Hush/issues/106)) and Windows ([#107](https://github.com/khawkins98/Hush/issues/107)) impls are open issues.

---

## Meeting pump

`meeting::SessionManager::start_manual(sources, app_name)` runs continuously:

```
                    ┌───────────────────────────────────┐
                    │   spawn tokio task: run_pump()    │
                    └─────────────────┬─────────────────┘
                                      │ every CHUNK_DURATION (10s)
                                      ▼
   ┌─────────────────┐    drain    ┌──────────────────┐
   │  mic handle     │ ──────────▶ │                  │
   ├─────────────────┤             │  Whisper         │
   │  system handle  │ ──────────▶ │  spawn_blocking  │
   └─────────────────┘             └────────┬─────────┘
                                            │ utterances
                                            ▼
                                  ┌──────────────────┐
                                  │  Diarize         │
                                  │  label_utts()    │ ◀── audio slice
                                  └────────┬─────────┘     from rolling buffer
                                           │
                                           ▼
                                  ┌──────────────────┐
                                  │  emit IPC event  │
                                  │  + persist row   │
                                  └──────────────────┘
```

**State machine.** `Mutex<SessionState>` where `SessionState` is `Idle | Opening | Active(...)`. The `Opening` sentinel is held across the async DB / handle-open work so concurrent `meeting_start_manual` IPC calls can't race past the precondition.

**Shutdown.** `stop_manual` sets the cancel flag, awaits the pump's final-chunk drain, writes `ended_at` on the session row. `SessionManager::Drop` aborts the pump's `JoinHandle` on app shutdown; `CpalMicSessionHandle` and `SckSessionHandle` both have `Drop` impls that release their OS resources.

**Privacy.** Audio is buffered in RAM (`AudioRollingBuffer`, ~30 s window) and never written to disk. Only the resulting transcript text is persisted.

---

## Diarization

`FlagGatedDiarizer` reads the `diarization_enabled` `AtomicBool` from `AppState` and routes to:

- **`OnnxDiarizer`** — wespeaker ResNet34-LM ONNX speaker-embedding (~26 MB) + online 1-NN-with-threshold clustering for session-stable IDs. Model auto-downloads from Hugging Face on first enable, SHA-256 verified.
- **`NoopDiarizer`** — fallback. Source-derived `"mic"` / `"system"` tags pass through and the panel maps them to "You" / "Remote".

The `OnnxDiarizer` is gated behind the `diarization-onnx` Cargo feature (default-on). `EnergyDiarizer` (D1, silence-gap heuristic) sits on disk for reference but isn't wired — D2 superseded it.

---

## IPC

Tauri commands (`#[tauri::command]`) live in `src-tauri/src/ipc/commands/`. The four-place sync rule (Rust handler → `tauri::generate_handler![]` registration → TS type → Playwright mock) is documented in [CLAUDE.md → "The four-place IPC sync rule"](./CLAUDE.md#the-four-place-ipc-sync-rule). CI catches Rust-only and TS-only breaks but cannot catch shape mismatches between them — that's a hands-on responsibility.

`IpcError` is a tagged enum; new variants need a corresponding case in `src/lib/errors.ts::formatErrorDisplay` so the structured `{ headline, hint?, details? }` shape renders correctly in `ErrorDisplay.svelte`.

---

## Persistence

SQLite via `sqlx`. Migrations in `src-tauri/migrations/` (sqlx-managed, applied at startup). Schemas:

- **History** — dictation transcripts, with FTS5 over the text + foreground app metadata.
- **Meeting sessions** — session rows + utterance rows; `ended_at` set on stop.
- **Vocabulary / replacements** — Personal Dictionary CRUD.
- **Settings** — key/value, including PTT combo, autostart, diarization toggle, app overrides.

The `models/` directory under `<app-data>/` holds the GGUF whisper checkpoints + the wespeaker ONNX file. SHA-256 verified on download; host-restricted to `huggingface.co` / `*.hf.co` (one signed-CDN hop allowed for HF's storage backend), hop-cap 4.

---

## Module map

**Backend** (`src-tauri/src/`):

| Module | Responsibility |
|---|---|
| `audio/` | cpal mic + SCK system-audio + the `AudioSession` handle trait |
| `transcription/` | `Transcribe` trait, whisper-rs backend, GGUF download + resample |
| `diarization/` | `Diarize` trait, ONNX wespeaker impl, online clustering, mel-FB features |
| `meeting/` | `SessionManager` + chunking pump + `AppClassifier` + per-app overrides |
| `ipc/` | `AppState`, `AppStateBuilder`, `IpcError`, command handlers (split by domain) |
| `hotkey/` | `tauri-plugin-global-shortcut` for toggle; pinned `fufesou/rdev` for PTT |
| `hud/` | Recording HUD pill (drag, dismiss, level meter) |
| `settings_window/` | `show()` / `hide()` for the standalone Settings window |
| `app_menu/` | Native macOS menu bar (no-op elsewhere) |
| `tray/` | Status-bar / system-tray icon (cross-platform) |
| `macos_perms/` | Programmatic TCC reads via AVFoundation / CoreGraphics / IOKit |
| `updater/` | Manual "Check for updates" probe against GitHub releases |

**Frontend** (`src/`):

| Path | Responsibility |
|---|---|
| `routes/+page.svelte` | Main window — Dictation / Meetings / History sections |
| `routes/settings/+page.svelte` | Settings window — model picker, vocab, replacements, diagnostics |
| `routes/hud/+page.svelte` | HUD pill |
| `lib/*.svelte` | Svelte 5 component library (panels, sidebar, error display, PTT editor) |
| `lib/types.ts` | TS shapes mirroring backend serde structs (camelCase) |
| `lib/errors.ts` | `IpcError` → `ErrorDisplay` mapping |

---

## Cross-cutting

- **Conventions** (commit format, branch naming, comment style, untagged-TODO lint) — see [CLAUDE.md → Conventions](./CLAUDE.md#conventions).
- **macOS TCC dev-binary quirk** — see [`docs/macos-permissions.md`](./docs/macos-permissions.md).
- **Release pipeline** — see [`docs/releases.md`](./docs/releases.md).
- **Why a particular call was made** — see [`learnings.md`](./learnings.md), the append-only decision log.
- **Black-box reimplementation discipline** — VoiceInk's source is never read; see [`hush-prd.md` §13.8](./hush-prd.md).
