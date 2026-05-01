# Hush — Status Report

**Snapshot:** 2026-04-29, post-release-pipeline + manual update probe + perms-smoothing
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

**Daily-usable on macOS 26.** v0.1.0 was tagged with the dictation
hot-path; ~100 PRs since have brought the app to the shape it ships
in today. The post-IA-redesign stretch (Settings window, native
menu, status-bar icon) landed first; more recently the focus has been
on shipping the install / update path (release workflow + manual
update probe), polishing a UX walkthrough into a coherent set of
fixes, and closing dev-iteration friction around macOS permissions.

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
  rendering, **model-based speaker diarization** (#111, #295–#308) via
  the wespeaker ResNet34-LM ONNX embedding model + online clustering,
  routed through `FlagGatedDiarizer`; opt-in via the Speakers tab with
  in-app model download (#301/#304). Falls back to source-tag labels
  (You / Remote) when the toggle is off or the model isn't loaded.
  Searchable session history (#216 search filter), per-app classifier
  with cross-platform defaults (#219: Zoom/Teams/Discord/Slack/Webex/
  Skype + media apps across macOS bundle ids, Linux process names,
  Windows .exe basenames). **Auto-start** when a classified meeting
  app focuses (#221) — opt-in via Settings → Meeting → Auto-start
  (Off / Always); manual-start unchanged.
- **Settings window** — model picker (auto-download from Hugging Face,
  SHA-256 verified, friendly display names in History rows #225),
  vocabulary terms, find/replace rules, **per-app meeting overrides
  with click-to-confirm Remove**, macOS permissions diagnostic with
  per-row "Grant in Settings…" deep-links and **all-four** TCC
  reset (#231 — fixed a bug where the previous reset skipped
  ScreenCapture), autostart toggle, **HUD-show toggle** (#218),
  hotkey display, first-run welcome reset, **manual "Check for
  updates"** probe (#227 — Settings → About + macOS menu bar).
- **macOS niceties** — native menu bar (⌘1/⌘2/⌘3 sidebar shortcuts,
  ⌘, opens Settings, **Hush → Check for Updates…**), status-bar icon
  (Show / Toggle Recording / Quit), live TCC permission detection
  (green "Permissions OK" pill on Dictation when granted), HUD drag +
  dismiss, system font stack + native form-control rendering.
- **Library** — SQLite history with FTS5 + recording duration,
  vocabulary + replacements CRUD.
- **Release pipeline** (#226) — `.github/workflows/release.yml`
  fires on `v*` tags or `workflow_dispatch`, builds via
  `tauri-action` on macos-latest / ubuntu-latest / windows-latest,
  attaches `.dmg` (Apple Silicon, macOS 26), `.AppImage`, `.deb`,
  `.msi`, `.exe` to a draft GitHub Release. Maintainer recipe in
  [`docs/releases.md`](./docs/releases.md). Smoke-tested via
  `gh workflow run release.yml` 2026-04-29; Linux + Windows legs
  produce clean artefacts, the macOS leg has an open issue with
  `cmake-rs`'s deployment-target propagation through whisper-rs-sys
  (`learnings.md` 2026-04-29).

What's deferred:

- **Auto-update** ([#10](https://github.com/khawkins98/Hush/issues/10))
  — needs a pubkey decision before the updater plugin can wire up.
  The release pipeline is now in place to feed it artefacts. Manual
  "Check for updates" (#223 / #227) ships in the meantime.
- **Parakeet ONNX backend** (#32) — green-lit on 2026-04-25; multi-PR.
- **Per-platform system audio**: Linux ([#106](https://github.com/khawkins98/Hush/issues/106))
  and Windows ([#107](https://github.com/khawkins98/Hush/issues/107)).
  Need hands-on testing on those platforms; no maintainer machine for
  either. Classifier defaults (#219) already cover the per-OS app
  names so the moment system-audio capture lands the meeting auto-
  start path will recognise Zoom/Teams/etc. there too.
- **Mac App Store distribution** ([#114](https://github.com/khawkins98/Hush/issues/114))
  — needs Ken's call.
- **Permissions-as-its-own-dialog** ([#232](https://github.com/khawkins98/Hush/issues/232))
  — extract the per-row UI into a reusable dialog so first-run +
  ad-hoc launches share the same surface as Settings.
- **MCP server** ([#224](https://github.com/khawkins98/Hush/issues/224))
  — expose transcripts/meetings/vocab/replacements as MCP resources
  + opt-in tools for start/stop. Off by default, localhost-only,
  per-install token.

The last multi-agent review (writer / Rust / UX / security) ran on
2026-04-28 against #182. Every critical finding is fixed; the
deferred items are tracked in the relevant issues. Security found
nothing exploitable. A subsequent UX walkthrough (Playwright
screenshot pass, spec at `tests/e2e/zz-uxwalk.spec.ts`) produced 9
visual / structural fixes (#225) and a Permissions surface refactor
(#231).

---

## Modules at a glance

The full module map (backend + frontend, with responsibilities) is in
[`ARCHITECTURE.md`](./ARCHITECTURE.md). The high-level shape:

- **Backend** (`src-tauri/src/`): `audio/`, `transcription/`,
  `diarization/`, `meeting/`, `ipc/`, `hotkey/`, `hud/`,
  `settings_window/`, `app_menu/`, `tray/`, `macos_perms/`, `updater/`.
- **Frontend** (`src/`): `routes/{+page,settings,hud}/+page.svelte`
  for the three windows; `lib/*.svelte` for the Svelte 5 component
  library; `lib/{types,errors,format}.ts` for shared TS shapes.

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

- `cd src-tauri && cargo test --lib` — 263+ unit tests, fast
- `cd src-tauri && cargo clippy --all-targets -- -D warnings` —
  must be clean
- `cd src-tauri && cargo fmt --all -- --check` — must be clean
- `npm run check` — svelte-check, must be clean
- `npm run test:e2e` — 70+ Path A specs (Playwright + Chromium,
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

- [#173](https://github.com/khawkins98/Hush/issues/173) — Layer 2
  native UI (per-OS class + targeted CSS overrides).
  Deferred until macOS-only hands-on coverage stops being a liability
  for sight-unseen Windows/Linux work.
- [#224](https://github.com/khawkins98/Hush/issues/224) — Hush as
  MCP server (transcripts as resources, opt-in tools for start/stop).
  Off by default, localhost-only, per-install token.

### Polish, deferred-on-purpose

- [#57](https://github.com/khawkins98/Hush/issues/57) — tauri-driver
  E2E for full-stack flows (HUD lifecycle, real audio, real model
  download). **Scaffold landed (#202)**: directory structure,
  `wdio.conf.ts`, smoke spec, README. CI integration deferred
  until tauri-driver's macOS path stabilises; spec coverage grows
  as Path A's mock-shaped gaps surface.
- `+page.svelte` state-layer refactor (#156, closed 2026-04-27).
  Multiple extractions landed (#212 FirstRunModal + MacosPermsPill);
  the file is now ~1.2k. Further extraction (meeting state into a
  composable) is the next natural step if a contributor reports
  navigation friction — open a fresh issue if so.
- [#232](https://github.com/khawkins98/Hush/issues/232) — extract
  Permissions UI into a reusable dialog so first-run + ad-hoc
  launches share the surface with Settings.

### Recently shipped, removed from this list

- #55 (rtrb SPSC ring buffer) — landed #193.
- #116 (AppState DataServices grouping) — landed #190.
- #112 (per-app classifier policy + auto-start lifecycle) — both
  halves shipped: overrides under #192, auto-start under #221.
- #217 (cross-platform classifier defaults) — landed #219.
- #218 (HUD-show toggle), #220 (HUD IPC unit tests).
- #222 (release pipeline), #223 (manual update probe).
- #225 (UX walkthrough polish — 9 visual/structural fixes).
- #231 (perms smoothing — fixed Reset bug, per-row Grant buttons,
  dev-loop docs).

---

## Recently shipped (for inbound contributors)

If you pulled `main` and want to know what changed: `CHANGELOG.md`'s
`[Unreleased]` block lists ~80 PRs since v0.1.0, grouped by theme
under standard Keep-a-Changelog headings. The most recent stretch
(post-#143) covers the IA redesign, Settings window, configurable
PTT, macOS TCC live detection, tray icon, and the multi-agent
review-driven fixes (#183 / #184 / #185).
