# Hush — Status Report

**Snapshot:** 2026-04-25, evening
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

The dictation loop is **end-to-end functional and feature-complete for
v1's core path**: hotkey or button → record → transcribe → clipboard
→ notification → searchable history. The recording HUD overlay is
shipped with a live RMS-driven level meter. Auto-download is wired
through the model picker; replacements + vocabulary are wired into
the transcription pipeline. macOS first-run welcome explains
permissions. Whisper-only for now per PRD §5 (revised) — Parakeet
via ONNX is on the v1.x roadmap (#32).

Test count: **121** Rust unit tests; **3** Playwright frontend
smoke tests (mocked-Tauri); 1 ignored audio-fixture integration
test that activates with a contributor-supplied WAV. Frontend
type-check clean, zero warnings. Clippy + rustfmt clean.

For the prose record of what shipped and why, see
[`CHANGELOG.md`](./CHANGELOG.md) (`[Unreleased]` section) and
[`learnings.md`](./learnings.md) (engineering-decision log).

---

## Modules at a glance

| Module | Status | Tests |
|---|---|---|
| `audio` (cpal + RMS level meter) | shipped | 13 |
| `transcription::resample` | shipped | 9 |
| `transcription::whisper` (gated) | shipped | stub-tested + manual smoke |
| `transcription::catalog` | shipped | 6 |
| `transcription::download` | shipped | 7 |
| `db` (sqlx pool + migrations) | shipped | 4 |
| `history` (CRUD + FTS5) | shipped | 11 |
| `dictionary::replacements` | shipped | 9 + 6 sqlite |
| `dictionary::vocabulary` | shipped | 9 + 7 sqlite |
| `settings` (K/V) | shipped | 6 |
| `ipc` (~21 Tauri commands) | shipped | 15 (incl. helper tests for `stop_dictation` decomposition) |
| `hotkey` (toggle + PTT) | shipped | 3 + manual |
| `hud` (secondary window + level meter) | shipped | — (manual smoke) |
| `updater` | stub | — |

Tauri events flowing backend → frontend:
`hotkey:toggle`, `hotkey:ptt-press`, `hotkey:ptt-release`,
`audio:level`, `model:download-progress`, `model:download-done`,
`model:download-failed`.

---

## Decisions still in force

Locked in 2026-04-25:

- **Whisper.cpp via `whisper-rs` is the v1 engine.** Cmake-gated
  behind the `whisper` Cargo feature.
- **Parakeet** is approved via the cross-platform ONNX path (#32 —
  not yet started). NO macOS-specific engines — if it can't run via
  ONNX, it doesn't ship.
- **Hot-swap on model selection** is deferred — selection writes
  the setting, the user restarts.
- **Auto-download** stays gated per-model on a verified SHA-256
  (#41 tracks the contributor task to fill them in). No
  trust-on-first-use.
- **Download client redirect policy is host-restricted** to
  `huggingface.co` (predicate unit-tested for typo-squat traps).
  Hop-cap 4. SHA-256 verification still applies on top.
- **No outbound network traffic** except the explicit user-clicked
  model download. Updater plugin is stubbed; no telemetry.
  README/PRD now disclose the model fetch as the only network
  surface.
- **CSP is currently `null`**, acceptable while the frontend stays
  small. Revisit before shipping to non-technical users.
- **`tauri-plugin-shell` was removed.** It was registered but never
  invoked; the privacy-pane command uses `std::process::Command`
  directly with hard-coded URLs.

---

## Build prerequisites

```bash
# Once on a fresh macOS machine:
brew install cmake          # whisper-rs needs it
nvm install 22 && nvm use 22

# Per-checkout:
cd /Users/khawkins/Documents/git/Hush
npm install
```

`cargo build --lib` and `npm run build` succeed without cmake (the
`whisper` feature is opt-in). The dictation pipeline needs the
`whisper` feature to actually transcribe.

---

## Concise testing guide

### a) Without a model — UI shell smoke test

```bash
npm run tauri dev
```

Verify in order:
- App launches, no console errors.
- Device dropdown lists your microphones.
- All five model cards render. Each currently shows a **Download**
  button — clicking it surfaces an error chip on the card (because
  the per-model SHA-256 hashes are still empty, gating
  auto-download until #41 lands).
- Replacements and Vocabulary panels both add/delete rules.
- The toggle hotkey (`Ctrl+⌥/Alt+H`) on macOS prompts for
  Input Monitoring; on Linux + X11 it works straight away.
- Pressing Start → Stop without a model shows the friendly
  "transcription isn't set up yet" error.
- The recording HUD window appears on Start, disappears on Stop,
  and the level bar moves with voice.

### b) With a model — full dictation loop

Until #41 fills in SHAs and auto-download works for every model,
the recommended path is the manual placement that auto-download
will replace:

```bash
mkdir -p "$HOME/Library/Application Support/com.khawkins.hush/models"
curl -L -o "$HOME/Library/Application Support/com.khawkins.hush/models/ggml-base.bin" \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin

cd src-tauri
cargo tauri dev --features whisper
```

Then in the UI:
1. The Whisper Base card shows as **Downloaded** with a Default
   badge — click it (or accept the default).
2. Pick your real microphone in the device dropdown.
3. Press the hotkey or click **Start recording**.
4. Speak: "the quick brown fox jumps over the lazy dog".
5. Press the hotkey again or click **Stop**.
6. After 1–3 seconds the transcript appears, lands on the
   clipboard, fires a "Ready to paste" notification, and shows up
   in History.

### c) Quick smoke after touching dictation code

Manual checks worth doing before merging anything that touches the
dictation path:

- [ ] Add a vocabulary term (e.g. `Hush`); record yourself saying
      it; verify the spelling lands correctly.
- [ ] Add a replacement rule (`um ` → blank); record "um hello";
      verify the transcript reads `hello`.
- [ ] Add a row, click **Delete**; row disappears.
- [ ] Type into the history search — spinner appears; list filters
      in ~200 ms.
- [ ] Hold the PTT key (`Right Ctrl` by default); release to stop.
- [ ] Quit Hush, relaunch — every panel rehydrates from SQLite.
- [ ] HUD overlay appears on Start, level bar tracks voice, hides
      on Stop.

### d) Automated suites

```bash
# Rust unit tests (default features)
cd src-tauri && cargo test --lib

# Rust unit tests including the whisper-gated path (needs cmake)
cargo test --lib --features whisper

# Frontend type check
npm run check

# Frontend e2e via Playwright (mocked Tauri IPC)
npm run test:e2e
```

The audio-fixture integration test in
`src-tauri/tests/audio_fixture.rs` is `#[ignore]`'d by default; set
`HUSH_TEST_AUDIO=/path/to/clip.wav` and run
`cargo test --features whisper -- --ignored audio_fixture` to
exercise the full transcription pipeline end-to-end against a known
clip.

---

## Open issues, by priority

### Next to land

1. **#48** Welcome modal a11y batch (Escape + focus trap +
   `aria-valuemax` for null-total downloads + retry-UX race).
   Smallest closer of the round-4 reviewer findings; auto-flips
   the `fixme`-marked Playwright spec to green.
2. **#41** Fill in per-model SHA-256 hashes — unblocks
   auto-download for each Whisper variant.
3. **#33** System-audio loopback (loopback half of the audio
   fixture; CoreAudio aggregate device on macOS, PulseAudio
   monitor on Linux).
4. **#32** Parakeet via ONNX — second engine.

### Tracking issues opened during round-4 review

- **#48** A11y polish on welcome modal + download progress.
- **#49** Security hardening (✅ closed by PR #53 — redirect
  policy, README disclosure, shell-plugin removal).
- **#50** HUD window scoped capability (✅ closed by PR #56).
- **#51** Docs drift — README status, CONTRIBUTING test
  patterns. (Closed in part by this docs refresh.)
- **#55** Replace `Mutex<Vec<f32>>` in cpal callback with `rtrb`
  SPSC ring (realtime safety). Not urgent — uncontended today.
- **#57** Tauri-driver E2E (full-stack, complement to the
  Playwright Path A suite).

### Backlog

- **#10** Updater (signed channel) — M6 release-engineering work.
- **#29** Polish punch list — running tracker; fold leftovers
  into PRs as convenient.

### Architecture refactors (interleave with feature work)

The round-3 architecture review opened five tracking issues:

- **#36** Extract `Repository<T, Id>` trait — before #32 Parakeet
  brings a fifth repo.
- **#37** `AppStateBuilder` — before the next feature adds a 9th
  field. **Architecture comment dropped** with two notes from
  the 2026-04-25 web-research pass: don't wrap the result in
  outer `Arc`, and re-evaluate cohesion boundary
  (`TranscriptionDeps` vs `EditingDeps` vs `DownloadDeps`)
  before locking the builder API.
- **#38** Decompose `stop_dictation` (✅ closed by PR #52).
- **#39** Split `dictionary` into `replacements/` + `vocabulary/`
  submodules — before #32 lands.
- **#40** Split `+page.svelte` into per-section components —
  before the next UI panel lands. **Architecture comment
  dropped** suggesting class-based reactive state in
  `.svelte.ts` over module-level `$state` exports, plus a
  `$effect` audit.

---

## How to read this file later

The CHANGELOG records what shipped. The PRD is policy. `learnings.md`
is the engineering-decision log. **This file rots faster than any of
those** — re-write it when you're paged in, don't try to keep it
incrementally up-to-date.
