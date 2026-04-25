# Hush — Status Report

**Snapshot:** 2026-04-25, late afternoon
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

The dictation loop is **end-to-end functional and feature-complete for
v1's core path**: hotkey or button → record → transcribe → clipboard
→ notification → searchable history. The model picker now does
auto-download; replacements and vocabulary biasing are wired into the
transcription pipeline. Whisper-only for now per PRD §5 (revised) —
Parakeet via ONNX is on the v1.x roadmap (#32).

Test count: **109** Rust unit tests, all passing on default features.
Frontend type-check clean, zero warnings.

For the prose record of what shipped and why, see
[`CHANGELOG.md`](./CHANGELOG.md) (`[Unreleased]` section) and
[`learnings.md`](./learnings.md) (engineering-decision log).

---

## Modules at a glance

| Module | Status | Tests |
|---|---|---|
| `audio` | shipped | 9 |
| `transcription::resample` | shipped | 9 |
| `transcription::whisper` (gated) | shipped | stub-tested + manual smoke |
| `transcription::catalog` | shipped | 6 |
| `transcription::download` | shipped (#42) | 7 |
| `db` (sqlx pool + migrations) | shipped | 4 |
| `history` (CRUD + FTS5) | shipped | 11 |
| `dictionary::replacements` | shipped | 9 + 6 sqlite |
| `dictionary::vocabulary` | shipped | 9 + 7 sqlite |
| `settings` (K/V) | shipped | 6 |
| `ipc` (~17 Tauri commands) | shipped | 8 + mocks |
| `hotkey` (toggle + PTT) | shipped | 3 + manual |
| `updater` | stub | — |

Five Tauri events flow backend → frontend:
`hotkey:toggle`, `hotkey:ptt-press`, `hotkey:ptt-release`,
`model:download-progress`, `model:download-done`,
`model:download-failed`.

---

## Decisions still in force

Locked in 2026-04-25 (afternoon and earlier):

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
- **No outbound network traffic** except the explicit user-clicked
  model download. Updater plugin is stubbed; no telemetry.
- **CSP is currently `null`**, acceptable while the frontend stays
  small. Revisit before shipping to non-technical users.

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
- The toggle hotkey (`⌘/Ctrl+Shift+Space`) on macOS prompts for
  Input Monitoring; on Linux + X11 it works straight away.
- Pressing Start → Stop without a model shows the friendly
  "transcription isn't set up yet" error.

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

When auto-download is unblocked (after #41), step (b) above
collapses to "click Download on the card". The end-to-end pipeline
test in #34(a) (next planned PR) automates the
`downloaded model + known-text WAV → expected transcript`
verification.

---

## Open issues, by priority

### Next to land

1. **#34** Audio test fixture (file-based integration test). Small,
   independent, validates the auto-download work end-to-end.
2. **#41** Fill in per-model SHA-256 hashes — unblocks
   auto-download for each Whisper variant.
3. **#22** macOS first-run permission onboarding.
4. **#21** Recording HUD with level meter.
5. **#32** Parakeet via ONNX (second engine).
6. **#33** System audio capture.

### Backlog

- **#10** Updater (signed channel) — M6 release-engineering work.
- **#29** Polish punch list — running tracker; fold leftovers into
  PRs as convenient.

### Architecture refactors (interleave with feature work)

The round-3 architecture review opened five tracking issues:

- **#36** Extract `Repository<T, Id>` trait — before #32 Parakeet
  brings a fifth repo.
- **#37** `AppStateBuilder` — before the next feature adds an 8th
  field.
- **#38** Decompose `stop_dictation` into a Tauri-free
  orchestrator — when #21 HUD or #33 system-audio touches the same
  surface.
- **#39** Split `dictionary` into `replacements/` + `vocabulary/`
  submodules — before #32 lands.
- **#40** Split `+page.svelte` into per-section components —
  before the next UI panel (#22 / #33) lands.

---

## How to read this file later

The CHANGELOG records what shipped. The PRD is policy. `learnings.md`
is the engineering-decision log. **This file rots faster than any of
those** — re-write it when you're paged in, don't try to keep it
incrementally up-to-date.
