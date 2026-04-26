# Hush — Status Report

**Snapshot:** 2026-04-26, evening
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

The dictation loop is **end-to-end functional on macOS for the
maintainer**: hotkey or button → record → transcribe → clipboard →
notification → searchable history. Hands-on testing this round
(2026-04-26) closed the model integration story:

- SHA-256 hashes filled in for all five Whisper variants (#72), so
  the picker's Download button actually works.
- Hugging Face's CDN moved to `*.hf.co`; the redirect predicate now
  allows both HF zones (#74).
- `whisper` is a default Cargo feature; `npm run tauri dev` boots
  with the loader present (#75). cmake is a hard prereq.
- Hot-load on model select — pick a downloaded model, the
  transcriber swaps without restart. Pick an undownloaded one, the
  picker tells you to Download first (#76).
- Audio buffer take is now timing-tolerant on stream cleanup (#77),
  with regression tests pinning the invariant (#78).

Test count: **133** Rust unit tests; **7** Playwright frontend
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
| `audio` (cpal + RMS level meter + drain_buffer) | shipped | 16 |
| `transcription::resample` | shipped | 9 |
| `transcription::whisper` (default-feature now) | shipped | stub-tested + manual smoke |
| `transcription::catalog` (SHAs filled in #72) | shipped | 6 |
| `transcription::download` | shipped | 7 |
| `db` (sqlx pool + migrations) | shipped | 4 |
| `history` (CRUD + FTS5) | shipped | 11 |
| `dictionary::replacements` | shipped | 9 + 6 sqlite |
| `dictionary::vocabulary` | shipped | 9 + 7 sqlite |
| `settings` (K/V) | shipped | 6 |
| `ipc` (~22 Tauri commands; transcribe Mutex hot-swap) | shipped | 17+ |
| `hotkey` (toggle ⌃⌥H; PTT macOS-disabled) | shipped | 12 (incl. enablement matrix) |
| `hud` (transparent on macOS via macos-private-api; top-right placement) | shipped | — (manual smoke) |
| `updater` | stub (registration deferred to #10) | — |

Tauri events flowing backend → frontend:
`hotkey:toggle`, `hotkey:ptt-press`, `hotkey:ptt-release`,
`audio:level`, `model:download-progress`, `model:download-done`,
`model:download-failed`.

---

## Decisions still in force

Locked in over rounds 1–5 (latest revisions noted):

- **Whisper.cpp via `whisper-rs` is the v1 engine.** Cmake-gated
  behind the `whisper` Cargo feature, which is now a **default**
  feature (revised 2026-04-26 in PR #75 because the user hit a
  silent-no-model bug from forgetting `--features whisper`).
- **Parakeet** approved via ONNX (#32, not yet started).
- **Hot-load on model selection** is **shipped** as of #76. (Earlier
  rounds noted it as deferred; that's no longer accurate.)
- **Auto-download** gated on per-model verified SHA-256 (filled in
  #72). Empty-hash gate stays in place for future catalog adds.
- **Download client redirect policy** is host-restricted to both
  `huggingface.co` and `hf.co` zones (revised 2026-04-26 in PR #74
  after HF migrated their CDN to `*.xethub.hf.co`). Hop cap 4. SHA
  verification still applies on top.
- **No outbound network traffic** except the explicit user-clicked
  model download. Updater plugin registration is deferred until #10
  (it panicked on null config; commented out in `lib.rs` until the
  signing key/endpoints are wired).
- **PTT disabled by default on macOS** (#69) due to the rdev/TSM
  crash on macOS 26+. Native CGEventTap replacement parked under
  #70 until production demand justifies it.
- **CSP is `null`** — pre-existing tradeoff documented in
  `learnings.md`. Revisit before non-technical-user shipping.
- **`tauri-plugin-shell` removed.** Was registered but never
  invoked.

---

## Build prerequisites

```bash
# Once on a fresh macOS machine:
brew install cmake          # whisper-rs needs it; mandatory now
nvm install 22 && nvm use 22

# Per-checkout:
cd /Users/khawkins/Documents/git/Hush
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
cd src-tauri && cargo test --lib            # 133 unit tests
cargo test --lib -- --ignored audio_fixture # whisper-fixture (needs HUSH_TEST_AUDIO)
cd .. && npm run check                      # frontend type check
npm run test:e2e                            # 7 Playwright specs (mocked Tauri)
```

---

## Open issues, by priority

### Next to land

1. **#48-class polish from round 5** — see CHANGELOG `[Unreleased]`
   and the round-5 review consolidation. Tracking PRs are open as
   of this snapshot.
2. **#33** System-audio loopback (loopback half of audio fixture).
3. **#32** Parakeet via ONNX — second engine.

### Tracking from earlier rounds

- **#10** Updater (signed channel) — release-engineering work.
- **#29** Polish punch list.
- **#36** `Repository<T,Id>` trait extraction.
- **#37** `AppStateBuilder`.
- **#39** `dictionary` split into `replacements/` + `vocabulary/`.
- **#40** `+page.svelte` split into per-section components.
- **#55** `rtrb` SPSC ring for cpal callback (replaces `Mutex<Vec<f32>>`).
- **#57** tauri-driver Path B (full-stack E2E).
- **#67** In-app macOS permission diagnostic.
- **#70** Native CGEventTap on macOS (replaces rdev for PTT).

---

## How to read this file later

The CHANGELOG records what shipped. The PRD is policy.
`learnings.md` is the engineering-decision log. **This file rots
faster than any of those** — re-write it when you're paged in,
don't try to keep it incrementally up-to-date.
