# Hush — Status Report

**Snapshot:** 2026-05-05, post-v0.4.0 — state-machine frontend, parallel startup, fixture tests
**Author:** Claude (working async on Ken's behalf)

A working hand-off doc; not the canonical CHANGELOG or PRD. The goal:
"what's the project state right now, what's blocking, how do I verify
it works." This file is meant to **rot fast** — re-write on next
pickup, don't try to keep it incrementally up-to-date.

---

## What's shipped since v0.4.0 (2026-05-05)

- **Parallel whisper startup** (#561 / #571) — the two `WhisperTranscription` contexts (dictation + meeting slots) now load in parallel via `tokio::join!`. Startup time on warm FS drops by ~half the sequential whisper load cost. Timestamped `tracing::info!` markers in `build_default` let you profile startup with `RUST_LOG=info npm run tauri dev`.
- **WAV fixture seam** (#559 / #572) — `WavFileAudioCapture` / `WavFileAudioSession` under `--features test-utils` lets integration tests feed real WAV samples through the meeting pump pipeline without live hardware.
- **Diarization fixture** (#314 / #573) — two `#[ignore]`'d integration tests for the full `AudioRollingBuffer → OnnxDiarizer → speaker_label` pipeline: two-speaker distinctness and sub-threshold silence passthrough.
- **CI rustfmt version mismatch** — documented in `learnings.md`; workaround is to read the exact diff from the CI job log and apply manually when `dtolnay/rust-toolchain` (January 2026 stable, rustfmt 1.7.x) disagrees with local 1.8.0.

## What's in v0.4.0 (2026-05-05)

The bigger threads since the previous snapshot:

- **PTT trailing silence + hold guard** (#548, #550) — 500 ms silence buffer on key-up, 100 ms hold guard against accidental taps, stuck-recording race fixed.
- **Frontend state machine** (#558) — 7 flat `$state` vars replaced with a single `RecordingPhase` discriminated union (`idle | starting | recording | stopping | transcribing`). Illegal state combinations are structurally impossible. `setTimeout` delays in the stop path removed.
- **Toggle hotkey + command palette stop apply buffer** (#560 / #563) — all four stop paths now apply the 500 ms trailing silence consistently.
- **Transcription progress indicator** (#566 / #569) — `N%` progress shown in HUD pill and RecordPanel while Whisper processes.
- **SCK drain fix** (#555 / #568) — `active_sessions` decremented unconditionally on ScreenCaptureKit stop failure (was leaking the refcount).
- **Diarizer buffer drain fix** (#553 / #570) — zero-fill on failed tick keeps diarizer timeline aligned with transcription session clock.
- **Meeting pump debug logging** (#533 / #551, #564) — structured logs distinguish model-not-loaded / no-audio / Whisper no-speech suppression for the 0-utterance bug.
- **RecordingPhase e2e tests** (#562 / #567) — Playwright specs for the state machine transitions (idle→starting→recording→stopping→idle, stop guard, retry after network error).
- **Diagnostic logging conventions** (#565) — `docs/contributing-audio-diagnostics.md` + CONTRIBUTING.md guidance on structured backend log patterns.
- **Stale permission banner + guided recovery** (#547) — amber banner when macOS reports a stale TCC row, one-click jump to Settings → Permissions.
- **In-app debug logging** (#537) — ring-buffer backend logs streamed to frontend, Settings → General → Advanced debug console toggle.
- **Microphone Boost slider** (#535) — 0 dB to +20 dB gain applied on mic path (dictation + meeting), system audio unchanged.
- **Whisper Turbo in model catalog** (#519) — distilled Large-v3 option; existing plumbing works unchanged.
- **First-run permission wizard** (#514) — two-step setup (Welcome + Permissions) with inline Mic/InputMonitoring requests.
- **Diarization on by default + auto-download** (#512) — fresh installs enable diarization and auto-download wespeaker after Whisper model download.
- **SCK system-audio unconditional link** (no feature flag) — screen-capture kit linked on macOS always; the `screencapturekit` feature flag was removed.

---

## Where the project stands

**Daily-usable on macOS 26 on Apple Silicon.** v0.4.0 is the latest release tag. The app has a working:

- **Dictation** — PTT (default: Right ⌘, configurable), toggle hotkey (⌃⌥H), command palette stop. Vocabulary prompt biasing, custom replacements, backend clipboard write.
- **Meeting mode** — long-running multi-source capture (mic + macOS system-audio via ScreenCaptureKit), 10 s chunked Whisper inference, live partial-utterance rendering, model-based speaker diarization (wespeaker ResNet34-LM ONNX + online 1-NN clustering), fallback source-tag labels.
- **History** — FTS5 full-text search over dictation transcripts and meeting utterances.
- **App profiles** — per-app preferred mic source and model, with auto-switch on focus.
- **Settings inline** — sidebar panel inside the main window (Settings is no longer a standalone Tauri WebviewWindow; #479 merged it in).
- **Four windows**: main (Dictation / History / Settings / About) + HUD pill (transparent, always-on-top) + menu-bar popover + debug console (developer only).

---

## How to verify it works

```bash
# Full app (requires cmake, network for first ONNX fetch)
npm run tauri dev

# UI-only (no Whisper, no ONNX — fast iteration path)
cd src-tauri && cargo tauri dev --no-default-features

# Rust unit tests (no audio device needed)
cd src-tauri && cargo test --lib

# Rust unit tests + whisper-gated paths
cd src-tauri && cargo test --lib --features whisper

# Frontend type check (required clean for every PR)
npm run check

# Playwright e2e (mocked IPC)
npm run test:e2e

# WAV fixture integration test (needs a model + sample WAV)
cd src-tauri && HUSH_TEST_MODEL=/path/to/ggml-base.en.bin \
  HUSH_TEST_AUDIO=/path/to/sample.wav \
  cargo test --features whisper,test-utils --test meeting_fixture -- --ignored --nocapture

# Diarization fixture integration test (needs wespeaker ONNX + two WAV files)
cd src-tauri && HUSH_DIARIZER_MODEL=/path/to/wespeaker.onnx \
  HUSH_TEST_SPEAKER1_AUDIO=/path/to/speaker1.wav \
  HUSH_TEST_SPEAKER2_AUDIO=/path/to/speaker2.wav \
  cargo test --features diarization-onnx --test diarization_fixture -- --ignored --nocapture
```

**TCC / permission testing** (Screen Recording, Microphone, Input Monitoring) requires a proper `.app` bundle:

```bash
npm run tauri:bundle   # build + re-sign + install to ~/Applications/Hush.app + launch
```

See `docs/macos-permissions.md` for the full TCC troubleshooting guide.

---

## Open work

### Hardware-blocked

- [#533](https://github.com/khawkins98/Hush/issues/533) — meeting mode 0 utterances with mic + system audio. Debug logging (#533 / #564) is in place. Root cause requires hands-on hardware testing to isolate — model not loaded, SCK not flowing, or Whisper no-speech suppression. Check `RUST_LOG=debug npm run tauri dev` and look for `meeting pump` / `whisper: inference complete` lines.

### Awaiting maintainer action

- [#10](https://github.com/khawkins98/Hush/issues/10) — Auto-update signed channel. Needs pubkey + `tauri-plugin-updater` endpoint. Steps 5–6 (IPC + About UI) shipped in #491; Steps 1–4 (keypair, `tauri.conf.json`, CI secrets, plugin registration) are maintainer-only.

### Multi-PR / large features

- [#224](https://github.com/khawkins98/Hush/issues/224) — Hush as MCP server (transcripts as resources, opt-in tools for start/stop). Off by default, localhost-only, per-install token. Large feature; no code started.

### Research

- [#316](https://github.com/khawkins98/Hush/issues/316) — Observe SessionClusterState 1-NN chaining drift on real meetings. The diarization fixture scaffold (#314 / #573) is the first step; actual drift measurement requires recording real multi-speaker sessions over ≥30 min.

### Design

- [#524](https://github.com/khawkins98/Hush/issues/524) — App icon, tray icon, palette seed, UI component direction. Non-code; needs design input.

### Deferred tracker

- [#545](https://github.com/khawkins98/Hush/issues/545) — Catch-all for out-of-scope / not-yet-feasible items (Linux system-audio #106, Windows system-audio #107, Mac App Store #114, Parakeet ONNX #32, tauri-driver full-stack E2E #57).

---

## For inbound contributors

`CHANGELOG.md`'s `[Unreleased]` block is the canonical record of everything since v0.4.0. `ARCHITECTURE.md` describes the full stack, trait-seam pattern, meeting pump dataflow, and module map. `CLAUDE.md` covers the four-place IPC sync rule, dev commands, and commit conventions. `learnings.md` is the append-only engineering decision log.
