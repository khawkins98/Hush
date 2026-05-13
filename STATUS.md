# Hush — Status Report

**Snapshot:** 2026-05-13, current v0.5.x handoff
**Latest release:** v0.5.3

A working handoff doc, not the canonical changelog or PRD. The goal is still: *what's the project state right now, what changed recently, and what should the next contributor verify or pick up?* This file is meant to **rot fast** — rewrite it on the next pickup instead of trying to preserve every old bullet forever.

---

## Executive summary

Hush is now in a solid **v0.5.x** shape on macOS 26+:

- **Transcribe (formerly Dictation)** works end-to-end with PTT, toggle hotkey, vocabulary biasing, replacements, clipboard write, and a usable HUD overlay.
- **Meeting capture** works with microphone + system audio, live chunked transcription, speaker labeling, app classification, and per-app override plumbing.
- **History** is a merged dictation + meeting feed with search, hover-revealed actions, and cleaner row formatting.
- **Settings** are inline in the main window; the old standalone settings window is gone.
- **About / debug / menu-bar surfaces** are all live and useful rather than placeholder chrome.

The biggest product-level change since the old v0.4.0 snapshot: **Meeting Mode no longer needs Screen Recording.** System audio now uses the CoreAudio process-tap backend, so the macOS permission story is down to **Microphone + Input Monitoring**.

---

## Current app shape

Per `ARCHITECTURE.md`, the app now has **four windows**:

1. **main** — Transcribe, History, inline Settings, About
2. **hud** — always-on-top recording pill
3. **menu-bar** — compact popover for quick controls
4. **debug** — developer-only log / diagnostics surface

Settings is inline inside the main window now; it is **not** a separate Tauri window anymore.

Backend shape is still the same high-level trait-seam pattern:

- Rust backend in `src-tauri/`
- SvelteKit / Svelte 5 frontend in `src/`
- Tauri IPC as the contract between them
- SQLite + FTS5 for persistence / search
- whisper.cpp for transcription
- tract-onnx wespeaker path for diarization

---

## What shipped after the old v0.4.0 snapshot

### v0.5.0 highlights

- **System audio backend changed:** CoreAudio process tap replaced the old ScreenCaptureKit path. This removed the alarming Screen Recording prompt and fixed the PCM path that had been tripping Whisper's no-speech gate.
- **Meeting audio reliability improved:** device-lost handling, mic auto-fallback, reconnect-on-replug, and clearer failure banners all landed.
- **Diarization stack stabilised:** the ORT-based path was replaced with tract-onnx to avoid the long-meeting memory leak / arena-retention behaviour.
- **Startup diagnostics improved:** build timestamp and startup phase timings surfaced in the UI/debug tooling.

### v0.5.1 highlights

- **About-tab external links no longer crash the app.**
- **Build timestamp now renders correctly in About.**
- **Input Monitoring prompt no longer fires at startup.** Hush now waits until the user actually engages the PTT path.
- **Changelog link added in About.**

### Current mainline / unreleased polish on top of v0.5.1

- **HUD pill double-click raises the main window.**
- **Settings sidebar double-scrollbar is fixed.**
- **Meeting tab narrow-width overflow is fixed.**
- **Permissions copy has been updated for the two-permission model.**
- **About tab version + Tauri runtime now display correctly.**
- **Autostart error banner is suppressed in dev builds where it is structurally unactionable.**
- **History row actions are hidden until hover/focus, reducing row clutter.**
- **History feed merge is now O(N) instead of sorting every combined refresh.**
- **Default model for new installs is now Whisper Small.**
- **Multiple copy / UI cleanups landed:** sentence-case section headers, sidebar/nav cleanup, redundant visual badges removed.

### Very recent code-health work on `main`

These are worth knowing because they change where to look when debugging:

- **`meeting::run_pump` was decomposed into smaller focused helpers** (`b58024f`).
- **`GeneralTab` was decomposed and prop-drilling from `+page.svelte` was reduced** (`30473ff`).
- **User-visible "Dictation" labels were renamed to "Transcribe"** (`8553acd`).
- **The microphone / Input Monitoring prompt flow was tightened again** (`221d1c5`).
- **TCC identity fixed for DMG installs on macOS 26** (PR #672, commit `b818384`). Quarantine-xattr strip + `exec()` on first launch ensures the stable bundle ID `io.github.khawkins98.hush` is seen by TCC; CGEventTap blocked until Input Monitoring is `Granted` to prevent a silent TCC Deny.

### In-draft work (PR #673, branch `feat/event-driven-meeting-detection-v2`)

These are not on `main` yet — real-device testing needed before merge:

- **Meeting auto-detection enabled by default.** `from_setting(None)` now returns `Always` instead of `Off`. New installs start detecting meetings immediately without any settings change.
- **Event-driven detection via CoreAudio HAL.** The `MicCameraMonitor` registers a `kAudioDevicePropertyDeviceIsRunningSomewhere` listener on every input device. The `evaluate_mic_state` pure function (6 guards, top-to-bottom) decides what action to take on each event: auto-start a session, do nothing, or reset the `session_emitted` guard.
- **Diagnostic debug tracing.** Every HAL event logs mic state, mode, session state, frontmost app, and classifier outcome at `RUST_LOG=hush=debug`. Makes it straightforward to confirm whether detection fired and why.

---

## What works today

### Transcribe

- Start/stop recording from the main UI
- Push-to-talk and toggle-hotkey flows
- Vocabulary and replacements
- Clipboard write / basic result handling
- HUD feedback during recording / transcription

### Meeting

- Manual meeting start/stop
- **Auto-detection via CoreAudio HAL** — starts when mic activates while Zoom, Teams, Meet, Discord, Slack, Webex, FaceTime, or Skype is frontmost (macOS, enabled by default; Settings → Meeting → Auto-start)
- Microphone + system-audio capture on macOS
- Live chunked transcript updates
- Speaker labeling via diarization
- App classification defaults + per-app override support
- Meeting history persistence and retrieval

### History / management

- Combined history feed for dictations and meetings
- Full-text search
- Copy / export / delete actions
- Cleaner row metadata and timestamps

### Settings / diagnostics

- Inline settings shell is the canonical settings surface now
- About panel exposes version/build info and changelog link
- Debug window surfaces runtime logs / timing info
- Permissions panel reflects the current two-permission model

---

## Fast verification commands

```bash
# Main frontend/type safety gate
npm run check

# Mocked-IPC Playwright suite
npm run test:e2e

# Real app runtime (fast iteration)
npm run tauri dev

# macOS permission / TCC smoke path
npm run tauri:bundle
```

Use `tauri:bundle` — not the raw dev binary — for any claim about macOS permission prompts or bundle-identity behaviour.

---

## Next pickup / open work worth flagging

- **PR #673 — event-driven meeting auto-detection** is in draft. Needs real-device validation: open Zoom/Teams and join a call with `Always` mode active; check that a session starts within ~1 s of mic activation. Enable debug logging with `RUST_LOG=hush=debug` to trace the detection path if it doesn't fire.
- **#10 — signed updater channel** is still maintainer-blocked. UI-side update plumbing exists; signing/notarised release-channel work does not.
- **#224 — MCP server mode** is still just a proposal. No implementation has started.
- **#316 — diarization drift research** still needs real-meeting observation; the current session-state matcher is good enough for v1, but not deeply measured.
- **#545 — deferred platform / harness work** remains the place to look for Linux/Windows system-audio work and the real-binary tauri-driver E2E track.
- **#524 — design direction** (icon / tray icon / palette / component direction) still needs a human aesthetic call, not more engineering speculation.

If you are picking up UX work, start from `CHANGELOG.md`'s **[Unreleased]** block plus the last handful of commits on `main`; that's where the freshest polish/refactor context lives.

---

## Orientation for new contributors

- `CHANGELOG.md` = canonical shipped / unreleased change log
- `ARCHITECTURE.md` = current topology, seams, and module map
- `CLAUDE.md` / `docs/developing.md` = command reference + workflow gotchas
- `learnings.md` = durable engineering decision log
