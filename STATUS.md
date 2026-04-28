# Hush — Status Report

**Snapshot:** 2026-04-28, post-IA-redesign + multi-agent review
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

**Daily-usable on macOS 26.** v0.1.0 was tagged with the dictation
hot-path; ~85 PRs since (the pivot to Meeting Mode and a full IA
redesign) have brought the app to the shape it ships in today.

What's in the build right now:

- **Three windows** — main app + standalone Settings (⌘,) + transparent
  recording HUD. Sidebar nav inside main: Dictation / Meetings /
  History.
- **Dictation** — toggle hotkey (⌃⌥H) + configurable push-to-talk
  combo. PTT is **on by default everywhere** as of #194 — fires the
  macOS Input Monitoring TCC prompt at boot, but the toggle hotkey
  and PTT both work out of the box. Disable in Settings → General →
  Hotkeys if not needed. Default combo: `Right ⌘` on macOS,
  `Right Ctrl` elsewhere.
- **Meeting Mode** — long-running multi-source capture (mic + macOS
  system-audio in parallel via ScreenCaptureKit), live partial-utterance
  rendering, You/Remote-tagged transcripts, searchable session history.
  Streaming Whisper sliding-window powers the live partials.
- **Settings window** — model picker (auto-download from Hugging Face,
  SHA-256 verified), vocabulary terms, find/replace rules, macOS
  permissions diagnostic, autostart toggle, hotkey display, first-run
  welcome reset.
- **macOS niceties** — native menu bar (⌘1/⌘2/⌘3 sidebar shortcuts,
  ⌘, opens Settings), status-bar icon (Show / Toggle Recording /
  Quit), live TCC permission detection (green "Permissions OK" pill on
  Dictation when mic + Screen Recording are granted), HUD drag +
  dismiss, system font stack + native form-control rendering.
- **Library** — SQLite history with FTS5 + recording duration,
  vocabulary + replacements CRUD.

What's deferred:

- **Auto-update** ([#10](https://github.com/khawkins98/Hush/issues/10))
  — needs a pubkey decision before the updater plugin can wire up.
- **Parakeet ONNX backend** (#32) — green-lit on 2026-04-25; multi-PR.
- **Per-platform system audio**: Linux ([#106](https://github.com/khawkins98/Hush/issues/106))
  and Windows ([#107](https://github.com/khawkins98/Hush/issues/107)).
  Need hands-on testing on those platforms; no maintainer machine for
  either.
- **D2 model-based speaker diarization** ([#111](https://github.com/khawkins98/Hush/issues/111))
  — D1 silence-gap heuristic shipped under #191/#201 (renders Speaker
  A/B in meeting transcripts); D2 (ONNX speaker-embedding) needs a
  model decision. Known D1 caveat: per-source pump runs the
  diarizer independently — see `learnings.md` 2026-04-28.
- **Auto-start Meeting Mode on foreground-app detection**
  ([#112](https://github.com/khawkins98/Hush/issues/112)) — per-app
  override storage + Settings UI shipped under #112/#192;
  auto-start lifecycle still pending (manual-start works today).
- **Mac App Store distribution** ([#114](https://github.com/khawkins98/Hush/issues/114))
  — needs Ken's call.

The last multi-agent review (writer / Rust / UX / security) ran on
2026-04-28 against #182. Every critical finding is fixed; the deferred
items are tracked in the relevant issues. Security found nothing
exploitable.

---

## Modules at a glance

Backend (`src-tauri/src/`):

- `audio/` — cpal mic + ScreenCaptureKit system-audio + the
  `AudioSession` handle trait used by the meeting pump.
- `transcription/` — `Transcribe` trait, whisper-rs backend, GGUF
  auto-download (SHA-256 verified, host-restricted to
  `huggingface.co` and `hf.co`), sliding-window streaming.
- `meeting/` — `SessionManager` + chunking pump + `AppClassifier`.
- `ipc/` — `AppState`, `AppStateBuilder`, `IpcError`. Commands split
  into `commands/{mod,meeting,models,macos}.rs` per domain.
- `hotkey/` — `tauri-plugin-global-shortcut` for the toggle hotkey;
  pinned `fufesou/rdev` for PTT (the Narsil upstream's macOS-26 fix
  is incomplete; fufesou attaches the tap to `CFRunLoopGetMain()`).
- `hud/` — borderless transparent always-on-top recording pill with
  drag + dismiss + level meter.
- `settings_window/` — `show()`/`hide()` for the Settings window.
- `app_menu/` — native macOS menu bar (no-op elsewhere).
- `tray/` — status-bar / system-tray icon (cross-platform).
- `macos_perms/` — programmatic TCC reads via AVFoundation /
  CoreGraphics / IOKit (no OS prompts triggered).

Frontend (`src/`):

- `routes/+page.svelte` — main window; orchestrates Dictation /
  Meetings / History sections.
- `routes/settings/+page.svelte` — standalone Settings window.
- `routes/hud/+page.svelte` — recording HUD pill.
- `lib/*.svelte` — Svelte 5 (runes-based) component library:
  `AppSidebar`, `PttHotkeyEditor`, plus the existing panels.
- `lib/format.ts`, `lib/types.ts` — shared TS types mirroring backend
  serde shapes (camelCase).

---

## Decisions still in force

These are calls already made; future contributors should treat them as
load-bearing unless explicitly revisiting.

- **macOS 26+ only.** Older macOS isn't a target; no `@available`
  guards, no compat shims. Linux/Windows compile cleanly via CI
  (ubuntu-latest, no Windows runner today) but are not hands-on tested.
- **Black-box reimplementation discipline.** No reading VoiceInk's
  source code, ever. See `hush-prd.md` §13.8 + the
  `learnings.md` discipline note.
- **`whisper` is a default Cargo feature.** UI-only contributors opt
  out with `--no-default-features`. ScreenCaptureKit is unconditional
  on macOS (no feature flag).
- **PTT stays opt-in via the Settings UI.** The macOS-26 abort that
  forced default-off is fixed (fufesou rdev fork), but enabling the
  listener fires the Input Monitoring prompt — a privacy surprise
  worth a deliberate user click.
- **No telemetry.** The updater plugin is currently stubbed; if
  telemetry ever lands it will be opt-in with a separate privacy review.

---

## Build prerequisites

- Rust stable
- Node.js ≥ 20
- `cmake` (for whisper.cpp's bindings — the default build needs it)
- Platform build deps from
  [Tauri prerequisites](https://tauri.app/start/prerequisites/)

```bash
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install
npm run tauri dev          # full app
# or:
cd src-tauri && cargo tauri dev --no-default-features   # UI-only path (no cmake required)
```

For the macOS `.app` bundle (required for SCK / Screen Recording
testing because the bare dev binary doesn't register cleanly with
TCC):

```bash
npm run tauri:bundle
```

---

## Concise testing guide

### a) Full app, default flow

`npm run tauri dev`, give the app a few seconds to compile + boot.
On a fresh install you should see:

1. The first-run welcome modal (macOS-only — covers Microphone +
   Input Monitoring).
2. After dismissing, the main window renders the Dictation tab. The
   "Set up your first model" banner shows because no GGUF is on disk
   yet; click into it to reach the Settings → Model picker.

### b) Stuck on macOS permissions

If the recording dot pulses but the level meter stays flat, mic
access is the most likely cause. Check:

- The Dictation tab's permission hint card (only renders when
  something is *actually denied*, not for `not-determined`)
- Settings → Permissions tab for the per-permission status pills
  (Granted / Denied / Not yet granted)
- For the dev binary specifically, macOS attributes the request to
  the parent process (iTerm / Terminal that ran `npm run tauri dev`)
  rather than Hush itself — see CLAUDE.md's "macOS TCC dev-binary
  quirk" section. Use `npm run tauri:bundle` for the proper-app
  smoke path.

### c) Manual smoke before merging dictation-touching changes

These can't be exercised by CI:

- [ ] `npm run tauri dev` boots without panicking (covers
      `setup` / plugin / capability / rpath regressions)
- [ ] Toggle hotkey (⌃⌥H) starts + stops recording
- [ ] Recording HUD appears + drags + dismiss button works
- [ ] On stop, transcript lands in the clipboard + a "Ready to
      paste" notification fires
- [ ] History panel populates the new entry with the right
      timestamp + duration
- [ ] If touching meeting flows: start a meeting from the Meetings
      tab, talk, watch live partials firm up to finals, stop the
      session (with the inline confirmation prompt), confirm the
      session row auto-expands its transcript
- [ ] If touching PTT: open Settings → General → Hotkeys, toggle
      Enable on (macOS prompt fires), record a new combo, hold
      the combo, confirm dictation starts/stops

### d) Automated suites

- `cd src-tauri && cargo test --lib` — 214 unit tests, fast
- `cd src-tauri && cargo clippy --all-targets -- -D warnings` —
  must be clean
- `cd src-tauri && cargo fmt --all -- --check` — must be clean
- `npm run check` — svelte-check, must be clean
- `npm run test:e2e` — 41 Path A specs (Playwright + Chromium,
  mocked IPC)
- `npm run test:e2e:tauri` — Path B (tauri-driver, real binary).
  Scaffold + smoke spec landed under #202 (refs #57); CI is
  deferred until tauri-driver's macOS path stabilises. Run
  locally per `tests/e2e-tauri/README.md`.
- `cd src-tauri && HUSH_TEST_AUDIO=/path/to/sample.wav cargo test -- --ignored`
  for the audio fixture (needs a real WAV)

---

## Open work, by priority

### Awaiting user decision

- [#10](https://github.com/khawkins98/Hush/issues/10) — Auto-update
  signed channel. Needs pubkey + endpoint decision before the
  `tauri-plugin-updater` can be uncommented in `lib.rs`.
- [#114](https://github.com/khawkins98/Hush/issues/114) — Mac App
  Store distribution. Decision call.
- [#32](https://github.com/khawkins98/Hush/issues/32) — Parakeet
  ONNX as a second engine. Greenlit but multi-PR; product input on
  scope sequencing welcome.

### Hardware-blocked

- [#106](https://github.com/khawkins98/Hush/issues/106) — Linux
  system-audio (PulseAudio / PipeWire monitor source).
- [#107](https://github.com/khawkins98/Hush/issues/107) — Windows
  system-audio (WASAPI loopback).

### Multi-PR roadmap

- [#111](https://github.com/khawkins98/Hush/issues/111) — Speaker
  diarization. **D1 shipped** (silence-gap `EnergyDiarizer`,
  #191/#201, in production); **D2 deferred** (model-based ONNX
  speaker-embedding, needs a model decision).
- [#112](https://github.com/khawkins98/Hush/issues/112) — Per-app
  classifier policy. **Override storage + Settings UI shipped**
  under #112/#192; auto-start-on-classify lifecycle still pending
  (manual-start works today).
- [#173](https://github.com/khawkins98/Hush/issues/173) — Layer 2
  native UI (per-OS class + targeted CSS overrides).
  Deferred until macOS-only hands-on coverage stops being a liability
  for sight-unseen Windows/Linux work.

### Polish, deferred-on-purpose

- [#57](https://github.com/khawkins98/Hush/issues/57) — tauri-driver
  E2E for full-stack flows (HUD lifecycle, real audio, real model
  download). **Scaffold landed (#202)**: directory structure,
  `wdio.conf.ts`, smoke spec, README. CI integration deferred
  until tauri-driver's macOS path stabilises; spec coverage grows
  as Path A's mock-shaped gaps surface.
- [#156](https://github.com/khawkins98/Hush/issues/156) —
  `+page.svelte` state-layer refactor. Phase 3 lifted ~158 LOC; the
  file is still ~1.5k. Worth more extraction (meeting state into a
  composable) when next a contributor reports navigation friction.

### Recently shipped, removed from this list

- #55 (rtrb SPSC ring buffer) — landed #193.
- #116 (AppState DataServices grouping) — landed #190.

---

## Recently shipped (for inbound contributors)

If you pulled `main` and want to know what changed: `CHANGELOG.md`'s
`[Unreleased]` block lists ~80 PRs since v0.1.0, grouped by theme
under standard Keep-a-Changelog headings. The most recent stretch
(post-#143) covers the IA redesign, Settings window, configurable
PTT, macOS TCC live detection, tray icon, and the multi-agent
review-driven fixes (#183 / #184 / #185).
