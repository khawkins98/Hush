# Hush — Status Report

**Snapshot:** 2026-04-25
**Author:** Claude (working async on Ken's behalf)

This is a working status doc for catching up across sessions. It is
not the canonical CHANGELOG or PRD — see those for release notes and
product policy. The goal here is "what's the project state right now,
what's blocking, how do I verify it works."

---

## Synopsis: what's shipped on `main`

The dictation loop is end-to-end functional. Press a hotkey →
record → release → text on the clipboard with a notification, plus
a searchable history view, post-transcription replacements, Whisper
prompt-biased vocabulary, and a card-based model picker.

### Modules

| Module | Status | Tested? |
|---|---|---|
| `audio` (cpal capture) | shipped | ✅ unit |
| `transcription::resample` (linear resampler) | shipped | ✅ unit |
| `transcription::whisper` (whisper-rs glue, gated) | shipped | ✅ stub-tested; manual smoke pending |
| `transcription::catalog` (Whisper variants) | shipped | ✅ unit |
| `db` (sqlx pool + migrations) | shipped | ✅ unit (in-memory) |
| `history` (CRUD + FTS5 search) | shipped | ✅ unit |
| `dictionary::replacements` (post-transcription find/replace) | shipped | ✅ unit |
| `dictionary::vocabulary` (prompt-biasing) | shipped | ✅ unit |
| `settings` (key-value SQLite) | shipped | ✅ unit |
| `ipc` (15 Tauri commands) | shipped | ✅ unit + mock |
| `hotkey` (toggle via global-shortcut, PTT via rdev) | shipped | ✅ unit on parser |
| `updater` | stub | ❌ |

**Test count:** 102 unit tests, all passing on default + whisper
features.

### Merged PRs (most recent first)

- **#31** *open* — M3 polish batch: critical UI (parallel fetches,
  loading states, error scoping, vocab/replacements visual
  distinction), misc polish (focus, sticky hint, search spinner,
  aria-live), cleanup (opener plugin removal + CSP doc), CHANGELOG
  voice consistency pass.
- **#27** — Whisper model picker. Card grid UI matching the
  reference screenshot, settings-backed selection, no auto-download
  yet.
- **#26** — Vocabulary prompt-biasing (closes #6).
- **#25** — Post-transcription find/replace pipeline.
- **#24** — History persistence + searchable view.
- **#20** — M2 polish (recording feedback, friendly errors, hotkey
  discovery).
- **#19** — Push-to-talk hotkey (rdev).
- **#18** — sqlx pool + embedded migrations.
- **#17** — Toggle-record hotkey (global-shortcut).
- **#16** — IPC layer wiring the dictation pipeline.
- **#13** — Whisper-rs inference.
- **#12** — Cross-platform cpal audio capture.
- **#1**  — Initial scaffold.

(There are also chore/CI fixups along the way; see `git log`.)

---

## Major decisions resolved 2026-04-25 (afternoon)

Locked in by Ken in the same session that produced this report:

- **Parakeet** is on the v1.x roadmap via ONNX (`ort` crate). PRD §5 rewritten. Tracked as #32. macOS-specific paths remain rejected — if a model can't run via ONNX, it doesn't ship.
- **Auto-download** (#30), **macOS first-run onboarding** (#22), and **recording HUD** (#21) are all approved — proceed.
- **System audio capture** (#33, new) is in scope — capture system output (podcasts, meetings, video) alongside the microphone. Per-OS surface confined to the audio module's backend.
- **Audio test fixture** (#34, new) — bundle a public-domain clip and an integration test verifying a known transcript. Useful as both a regression net and a reference target for the system-audio work.
- **Hot-swap on model selection**: still deferred; user accepts "restart Hush" as v1 behaviour. Revisit if the friction shows up in real use.

## Build prerequisites

```bash
# Once, on a fresh macOS machine:
brew install cmake          # whisper-rs needs it
nvm install 22 && nvm use 22

# Per-checkout:
cd /Users/khawkins/Documents/git/Hush
npm install
```

`cargo build --lib` and `npm run build` succeed locally without
cmake. The dictation pipeline needs `--features whisper` (and thus
cmake) to actually transcribe.

---

## Concise testing guide

### Local prerequisites

```bash
# Rust stable, Node ≥ 22, plus per-platform Tauri build deps
rustup update stable
nvm install 22

# macOS only:
brew install cmake          # for the whisper-rs build
```

### Without the whisper feature (no model)

The fast path for testing the UI shell, history, replacements,
vocabulary, and the picker without an actual model.

```bash
cd /Users/khawkins/Documents/git/Hush
npm install         # once
npm run tauri dev
```

Verify:
- App launches, no console errors.
- Device dropdown lists your microphones (or a friendly message
  if you've blocked Hush from microphone access).
- Pressing **Start recording** → **Stop and transcribe** shows a
  red error: *"Transcription isn't set up yet. The model picker
  is coming…"* (this is the recovery copy from #20).
- The **Model** section shows five Whisper cards. All are greyed
  out because no `.bin` files are in the models directory yet.
  Each card shows the expected filename in its hint.
- The **Replacements** and **Vocabulary** panels can add and
  delete rules. Errors stay scoped to their panel.
- Pressing the toggle hotkey (`⌘/Ctrl+Shift+Space`) on macOS
  prompts for Input Monitoring; on Linux + X11 it Just Works.

### With the whisper feature (real transcription)

```bash
# 1. Download a model. The "base" Q5_0 is the recommended default
#    (~142 MB, real-time on a 2020-era laptop).
mkdir -p "$HOME/Library/Application Support/com.khawkins.hush/models"
curl -L -o "$HOME/Library/Application Support/com.khawkins.hush/models/ggml-base.bin" \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin

# 2. Run with the whisper feature on.
cd src-tauri
cargo tauri dev --features whisper
```

Then in the UI:
1. The **Model** section now shows `Whisper Base` as
   *Downloaded* — click it to select. (Restart needed for the
   selection to take effect — UI says so.)
2. Restart the app: `cargo tauri dev --features whisper`.
3. Pick your real microphone in the device dropdown.
4. Press the hotkey or the **Start recording** button.
5. Speak: "the quick brown fox jumps over the lazy dog".
6. Press the hotkey again (or the **Stop** button).
7. After 1–3 seconds (depending on your CPU), the transcription
   appears in the result block, lands on the clipboard, fires a
   "Ready to paste" notification, and is added to the history.
8. Open another app and `⌘V` / `Ctrl+V` — the transcribed text
   should paste.

### Smoke checklist

After the basics work, exercise these:

- [ ] Add a vocabulary term (e.g. `Hush`); record yourself saying
      it; verify the spelling lands correctly in the transcript.
- [ ] Add a replacement rule (e.g. `um ` → blank); record
      yourself saying "um hello"; verify the transcript reads
      `hello`.
- [ ] Add a transcription you don't want; click **Delete** on its
      row; verify it disappears.
- [ ] Type into the history search box; verify the spinner
      appears and the list filters in ~200 ms.
- [ ] Hold the PTT key (`Right Ctrl`) to speak; release to stop.
- [ ] Quit Hush, relaunch — every panel rehydrates from SQLite.

If any of those are broken, the right place to report is whichever
section's `*Error` panel surfaces first. The error message will be
prefixed with the section name.

---

## What's open right now

Open issues, in suggested working order:

1. **#30** Whisper auto-download — green-lit, proceed.
2. **#22** macOS first-run onboarding — green-lit; lands after #30
   so the welcome flow can include the picker.
3. **#21** Recording HUD overlay — green-lit; polish on a working
   loop.
4. **#32** Parakeet via ONNX — second engine. Same `Transcribe`
   trait; cross-platform only.
5. **#33** System audio capture — adds a system-output source
   alongside the microphone.
6. **#34** Audio test fixture — file-based integration test (a)
   independent; loopback test (b) blocks on #33.
7. **#10** Updater — M6 work, no urgency.
8. **#29** Misc polish — running list, fold into PRs as
   convenient.

Open PRs:

- **#31** M3 polish batch — review-cycle critical + misc + cleanup.

Architecture refactors from round-3 review (foundation work,
interleave with feature PRs):

- Extract a generic `Repository<T, Id>` trait covering the four
  near-identical CRUD repos.
- Replace `AppState::new(...)` with a builder.
- Extract a `ForegroundAppCapture` service from the IPC commands.
- Decompose `stop_dictation` into a Tauri-free orchestrator.
- Split the `dictionary` module into `replacements/` and
  `vocabulary/` submodules.
- Split `+page.svelte` into per-section Svelte components when it
  passes ~2K lines (currently ~1.5K).

---

## How to read this file later

The CHANGELOG is the user-facing record. PRD is policy. `learnings.md`
is the engineering decision log. **This file rots faster than any of
those** — re-write it when you're paged in, don't try to keep it
incrementally up-to-date.
