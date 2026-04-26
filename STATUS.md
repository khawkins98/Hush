# Hush — Status Report

**Snapshot:** 2026-04-26, late (post-v0.1.0 tag, picker shipped)
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

**v0.1.0 is tagged** (`git tag v0.1.0`, also pushed to origin).
Captures the M3-complete state: dictation loop end-to-end functional
on macOS 26 — hotkey or button → record → transcribe → clipboard →
notification → searchable history. Older macOS versions, Linux, and
Windows are not hands-on tested.

After v0.1.0 the project pivoted toward a **system-audio + meeting-mode
extension** (design memo at `docs/system-audio-meeting-mode-proposal.md`).
The dictation flow stays — meeting-mode is a second product surface
inside the same app, not a replacement.

What's landed since v0.1.0:

- **#96** — `AudioSource` enum (`Microphone(Option<String>)` /
  `SystemAudio`) + `AudioCapture::start_with_source` trait method.
  Foundation for #33; no behaviour change yet.
- **#98** — Phase A1 of #33: source picker UI + `audio_list_sources`
  IPC command + `start_dictation` taking discriminated `AudioSource`.
  The mic dropdown is now grouped (`<optgroup>` "Microphone" + "System
  audio"); the system-audio entry renders disabled with a "(coming
  soon — #33)" affordance until per-platform backends ship.
- Dependency hygiene: `zip` (unused) dropped, `sha2` 0.10 → 0.11,
  `active-win-pos-rs` 0.8 → 0.10. Held: `reqwest`, `whisper-rs`,
  `cpal`, frontend (vite/typescript/svelte) — those need user
  sign-off because of risk surface.

What ships next (per the phased plan in the design memo):

- **Phase A2/A3/A4**: per-platform `SystemAudio` impls. Linux
  PulseAudio monitor source, Windows WASAPI loopback, macOS
  ScreenCaptureKit. Each its own PR — needs hands-on testing on
  the target platform; not safe to do autonomously.
- **Phase B**: streaming transcription. Whisper sliding-window
  for the interim path; Parakeet (#32) for the eventual default.
  Also benefits the existing dictation hot-path latency.
- **Phase C**: sessions + meeting-mode UI surface (new tables,
  per-utterance transcripts, per-app classifier).
- **Phase D**: speaker diarization.
- **Phase E**: app-aware policy refinement.

Test count: **143** Rust unit tests (was 135 pre-pivot — +8 for
the AudioSource trait surface and listing default impl); **7**
Playwright frontend smoke tests (mocked-Tauri); 1 ignored
audio-fixture integration test that defaults to the bundled JFK
clip when `HUSH_TEST_MODEL` is set. Frontend type-check 0 errors /
0 warnings. Clippy + rustfmt clean.

For the prose record of what shipped and why, see
[`CHANGELOG.md`](./CHANGELOG.md) (`[Unreleased]` section) and
[`learnings.md`](./learnings.md) (engineering-decision log).

---

## Modules at a glance

| Module | Status | Tests |
|---|---|---|
| `audio` (cpal + RMS level meter + AudioSource trait, post-#96/#98) | shipped (mic only; SystemAudio per-OS pending) | 24 |
| `transcription::resample` | shipped | 9 |
| `transcription::whisper` (default-feature) | shipped | stub-tested + manual smoke |
| `transcription::catalog` (SHAs filled in) | shipped | 6 |
| `transcription::download` | shipped | 7 |
| `db` (sqlx pool + migrations) | shipped | 4 |
| `history` (CRUD + FTS5; `insert`→`create` post-#88) | shipped | 11 |
| `dictionary::replacements` (own submodule post-#86) | shipped | 9 + 6 sqlite |
| `dictionary::vocabulary` (own submodule post-#86) | shipped | 9 + 8 sqlite |
| `repository` (generic CRUD trait, new in #88) | shipped | — |
| `settings` (K/V) | shipped | 6 |
| `ipc` (~25 Tauri commands; `audio_list_sources` added in #98) | shipped | 17+ |
| `hotkey` (toggle ⌃⌥H; PTT macOS-disabled) | shipped | 12 (incl. enablement matrix) |
| `hud` (transparent on macOS via macos-private-api) | shipped | — (manual smoke) |
| `updater` | stub (registration deferred to #10) | — |

Frontend: 7 components under `src/lib/` (post-#84) + a thin
`src/routes/+page.svelte` (1080 lines of layout + cross-cutting
state) + shared types in `src/lib/types.ts`.

Tauri events flowing backend → frontend:
`hotkey:toggle`, `hotkey:ptt-press`, `hotkey:ptt-release`,
`audio:level`, `model:download-progress`, `model:download-done`,
`model:download-failed`.

---

## Decisions still in force

- **Whisper.cpp via `whisper-rs` is the v1 engine.** Cmake-gated
  behind the `whisper` Cargo feature, which is a **default**
  feature.
- **Parakeet** approved via ONNX (#32, not yet started).
- **Hot-load on model selection** is shipped.
- **Auto-download** gated on per-model verified SHA-256.
- **Download client redirect policy** is host-restricted to both
  `huggingface.co` and `hf.co` zones. Hop cap 4. SHA verification
  still applies on top.
- **No outbound network traffic** except the explicit user-clicked
  model download. Updater plugin registration is deferred until #10
  (it panicked on null config; commented out in `lib.rs` until the
  signing key/endpoints are wired).
- **PTT disabled by default on macOS** (#69) due to the rdev/TSM
  crash on macOS 26+. Native CGEventTap replacement parked under
  #70 until production demand justifies it.
- **macOS 26 is the only hands-on-tested platform.** Older macOS
  versions and Linux/Windows compile + pass CI but the maintainer
  doesn't run them.
- **CSP is `null`** — pre-existing tradeoff documented in
  `learnings.md`. Revisit before non-technical-user shipping.
- **Per-domain repository traits aliased through a generic
  `Repository<T, NewT, Id>`** for replacements + vocabulary
  (post-#88). History stays standalone (paginated list + search +
  count + no-update don't fit). Settings stays its own trait
  (K/V semantics).

---

## Build prerequisites

```bash
# Once on a fresh macOS machine:
brew install cmake          # whisper-rs needs it; mandatory now
nvm install 22 && nvm use 22

# Per-checkout:
cd Hush
npm install
```

---

## Concise testing guide

### a) Full app, default flow

```bash
npm run tauri dev
```

If you have a model in `~/Library/Application Support/com.khawkins.hush/models/`,
the boot log shows `loaded selected whisper model …` and you can
record immediately. If not:

1. The "Set up your first model" banner appears above the controls.
2. Click **Choose a model** → smooth-scroll to the picker.
3. Click **Download** on Whisper Base → progress bar runs → SHA-256
   verifies → file lands.
4. Click the card again → **"✓ Loaded. Ready to record."** No
   restart.
5. Press the hotkey (`⌃⌥H`) or click **Start** → speak → press
   hotkey or **Stop**. After a few seconds the transcript is on
   the clipboard.

### b) Stuck on macOS permissions

In-app: open the **macOS permissions — diagnostic and reset**
disclosure at the bottom of the page (#83 / #67). Or from the
shell:

```bash
npm run dev-cleanup -- --reset
# Resets TCC entries for com.khawkins.hush. Next launch re-prompts.
```

See `docs/macos-permissions.md` for the full troubleshooting tree.

### c) UI-only contributor without cmake

```bash
npm run tauri:ui-only
# Builds without the whisper feature. App boots, picker renders,
# Start surfaces the "no model loaded" path. UI iteration without
# the heavy cmake/whisper.cpp toolchain.
```

### d) Automated suites

```bash
cd src-tauri && cargo test --lib            # 135 unit tests
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
  cargo test --features whisper --test audio_fixture -- --ignored
                                             # bundled JFK fixture
cd .. && npm run check                       # frontend type check
npm run test:e2e                             # 7 Playwright specs (mocked Tauri)
```

---

## Open issues, by priority

### Pivot work — Phase A2/3/4 (per-platform `SystemAudio`)

Each needs hands-on testing on the target platform; not safe to do
autonomously. Order is by user-value-on-macOS-26-first:

- **macOS** ScreenCaptureKit. Needs either a Swift shim or a Rust
  wrapper crate (`screencapturekit-rs` etc.). Largest of the three
  per-platform PRs and the highest user value.
- **Linux** PulseAudio / PipeWire monitor source. Discoverable via
  cpal's existing input-device list (monitor sources show up as
  `Monitor of <Sink>`); the picker filters by name pattern.
- **Windows** WASAPI loopback. cpal already exposes loopback via
  `Device::loopback()` on the default output; mostly free.

### Pivot work — Phases B/C/D/E

- **Phase B** — Streaming transcription (Whisper sliding-window
  interim, Parakeet eventual default). Refactor + new trait +
  per-utterance event wiring. Independent of system-audio; benefits
  the existing dictation hot-path latency.
- **Phase C** — Sessions + meeting-mode UI. New tables, IPC
  commands, panel.
- **Phase D** — Diarization (energy-based first, model-based later).
- **Phase E** — App-aware policy refinement.

### Substantial / needs decision

- **#32** Parakeet via ONNX — second engine. Approved for
  Phase B's streaming-friendly path.
- **#10** Updater (signed channel) — needs the signing key +
  endpoint setup.
- **#70** Native CGEventTap to bring back default-on PTT on macOS.

### Smaller / scoped

- **#55** `rtrb` SPSC ring for cpal callback (replaces
  `Mutex<Vec<f32>>`). Needs hands-on mic smoke; CI can't verify.
  More important now that the audio path will multiplex mic +
  system audio in Phase C.
- **#57** tauri-driver Path B (full-stack E2E). Infra, large lift.
- **#82** `ipc/commands.rs` sub-module structure (1.4k LOC).
  Polish-only; will get worse as meeting-mode adds another ~10
  commands — worth revisiting if/when that lands.

### Held for user decision (dep upgrades)

`reqwest` 0.12 → 0.13 (security-relevant: redirect policy),
`whisper-rs` 0.14 → 0.16 (transcription correctness risk),
`cpal` 0.15 → 0.17 (audio backend risk; Phase A2/3/4 may be a
better moment to land this since we'll be exercising the audio
path heavily anyway), `vite` / `typescript` / `svelte` (frontend
tooling majors).

---

## How to read this file later

The CHANGELOG records what shipped. The PRD is policy.
`learnings.md` is the engineering-decision log. **This file rots
faster than any of those** — re-write it when you're paged in,
don't try to keep it incrementally up-to-date.
