# Hush — Status Report

**Snapshot:** 2026-04-26, late
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## Where the project stands

The dictation loop is **end-to-end functional on macOS 26 for the
maintainer**: hotkey or button → record → transcribe → clipboard →
notification → searchable history. Older macOS versions, Linux, and
Windows are not hands-on tested.

This session (post-2026-04-25) landed eight PRs that close the
M3-era polish punchlist and tighten internal architecture:

- **#83** — In-app macOS permission diagnostic + `tccutil reset`
  recovery panel (closes #67).
- **#84** — Split monolithic `+page.svelte` (2351 → 1080 lines) into
  seven per-section components under `src/lib/` (closes #40).
- **#85** — Round-6 reviewer consolidation: scoped CSS for the new
  diagnostic panel, CTA wording, type docstrings, CHANGELOG and
  docs grammar.
- **#86** — Split `dictionary/` into `replacements/` and
  `vocabulary/` submodules (closes #39).
- **#87** — `AppStateBuilder` replaces the 7-arg `AppState::new`
  constructor (closes #37).
- **#88** — Generic `Repository<T, NewT, Id>` trait for
  replacements + vocabulary; history's `insert` renamed to
  `create` for naming consistency (closes #36).
- **#89** — Bundled JFK audio fixture (~344 KB) so the
  `audio_fixture.rs` integration test runs from a single env var
  (closes #34 part-a).
- **#29** — Closed by audit: round-2 polish was substantially
  complete across earlier rounds (UNIQUE-update test, `aria-live`,
  focus management, sticky hint, dark-mode contrast all already
  done).

Test count: **135** Rust unit tests; **7** Playwright frontend
smoke tests (mocked-Tauri); 1 ignored audio-fixture integration
test that now defaults to the bundled JFK clip when
`HUSH_TEST_MODEL` is set. Frontend type-check 0 errors / 0 warnings.
Clippy + rustfmt clean.

For the prose record of what shipped and why, see
[`CHANGELOG.md`](./CHANGELOG.md) (`[Unreleased]` section) and
[`learnings.md`](./learnings.md) (engineering-decision log).

---

## Modules at a glance

| Module | Status | Tests |
|---|---|---|
| `audio` (cpal + RMS level meter + drain_buffer) | shipped | 16 |
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
| `ipc` (~24 Tauri commands; `AppStateBuilder` post-#87) | shipped | 17+ |
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

### Substantial / needs design or decision

1. **#33** System-audio loopback. Substantial; would benefit from a
   design pass first.
2. **#32** Parakeet via ONNX — second engine.
3. **#10** Updater (signed channel) — needs the signing key + endpoint
   setup.
4. **#70** Native CGEventTap to bring back default-on PTT on macOS.

### Smaller / scoped

- **#55** `rtrb` SPSC ring for cpal callback (replaces
  `Mutex<Vec<f32>>`). Needs hands-on mic smoke; CI can't verify.
- **#57** tauri-driver Path B (full-stack E2E). Infra, large lift.
- **#82** `ipc/commands.rs` sub-module structure (1.4k LOC). Polish-
  only; explicitly tracked but not actioned.

---

## How to read this file later

The CHANGELOG records what shipped. The PRD is policy.
`learnings.md` is the engineering-decision log. **This file rots
faster than any of those** — re-write it when you're paged in,
don't try to keep it incrementally up-to-date.
