# Hush — Status Report

**Snapshot:** 2026-05-15, current v0.6.x state
**Latest release:** v0.6.3

A working handoff doc, not the canonical changelog or PRD. The goal is still: *what's the project state right now, what changed recently, and what should the next contributor verify or pick up?* This file is meant to **rot fast** — rewrite it on the next pickup instead of trying to preserve every old bullet forever.

---

## Executive summary

Hush is now in a solid **v0.6.x** shape on macOS 26+:

- **Transcribe (formerly Dictation)** works end-to-end with PTT, toggle hotkey, vocabulary biasing, replacements, clipboard write, and a usable HUD overlay.
- **Meeting capture** works with microphone + system audio, live chunked transcription, speaker labeling, app classification, and per-app override plumbing.
- **History** is a merged dictation + meeting feed with search, always-visible icon actions, multi-format export (plain text, Markdown, CSV, SRT, VTT, JSON), and expand-in-place meeting transcripts.
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

## What shipped

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

### v0.5.3 highlights

- TCC identity fixed for DMG installs on macOS 26 (PR #672). Quarantine-xattr strip + `exec()` on first launch ensures the stable bundle ID `io.github.khawkins98.hush` is seen by TCC; CGEventTap blocked until Input Monitoring is `Granted` to prevent a silent TCC Deny.

### v0.6.0 highlights

- **Event-driven meeting auto-detection enabled by default.** Hush registers CoreAudio HAL listeners (`kAudioDevicePropertyDeviceIsRunningSomewhere`) — when any mic activates while a supported meeting app is frontmost, a session starts automatically. When the mic goes quiet, the session stops.
- **Unified recording UI.** Dictation and meeting recording share the same `RecordPanel`, removing the old divergent code paths.
- **HUD "done" state.** Brief "Copied!" confirmation after a dictation result is written to clipboard (#669).
- **Live transcript typing indicator** while Whisper chunks arrive during a meeting (#670).

### v0.6.1 highlights

- History multi-format export: plain text, Markdown, CSV, SRT, VTT, JSON (#679).
- History action icons always visible (no hover-to-reveal); meeting rows expand/collapse via chevron toggle.
- Clicking anywhere on a history row expands/collapses it.

### v0.6.3 highlights

- Recordings under 1 second are not transcribed; a dimmed "Recording too short — not transcribed" entry is stored in History so users can see the hotkey tap was detected. Stats and CSV export exclude these ignored entries (#682).

### v0.6.x-dev refactors (unreleased, landed on main post-v0.6.3)

Five PRs reorganised internals without user-visible changes:

- **#688** — Split 1155-line dictation handler into `mod.rs` (handlers) + `pipeline.rs` (helpers) + `tests.rs` (integration tests).
- **#689** — Extracted `AppLifecycle.svelte` (Tauri event listeners) and `lib/state/palette.svelte.ts` (command palette state) from `+page.svelte`.
- **#690** — Extracted shared `HistoryActionRow.svelte` from the two history row types; eliminates duplicate action-row markup.
- **#691** — Added `MemHistory` in-memory mock + `AppStateBuilder` composition pattern; 18 new IPC integration tests for history/settings/diarizer commands (run via `cargo test --lib`).
- **#692** — Added `dictionary/packs.rs` — static preset vocabulary packs (compile-time, never DB-materialised). New settings keys `enabled_packs` (JSON array of active pack slugs) and `language_style` (Whisper prompt tone prefix). UI for pack selection is live in Settings → Vocabulary.

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
- **Auto-detection via CoreAudio HAL** — starts when mic activates while Zoom, Teams, Meet, Discord, Slack, Webex, FaceTime, Skype, GoToMeeting, BlueJeans, Loom, Tuple, or Around is frontmost (macOS, enabled by default; Settings → Meeting → Auto-start)
- Microphone + system-audio capture on macOS
- Live chunked transcript updates
- Speaker labeling via diarization
- App classification defaults + per-app override support
- Meeting history persistence and retrieval

### History / management

- Combined history feed for dictations and meetings
- Full-text search
- Always-visible copy / export (multi-format: text, Markdown, CSV, SRT, VTT, JSON) / delete actions
- Expand-in-place meeting transcript; click anywhere on a row to expand/collapse
- Ignored rows (recordings < 1s) shown dimmed with "Recording too short" label; excluded from stats and export

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
