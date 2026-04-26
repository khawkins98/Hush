# Contributing to Hush

Thank you for your interest in contributing! Please read this document before opening a PR.

---

## ⚠️ Upstream attribution discipline (required reading)

**VoiceInk's source code must never be read by any Hush contributor — before, during, or after writing equivalent functionality.**

Hush is a black-box reimplementation. Design comes from VoiceInk's public README, the running app's observable behaviour, and general knowledge of how dictation apps work. No copyrightable expression from VoiceInk has been or will be copied. This self-imposed discipline protects the project legally and keeps the codebase clean.

- If you have previously read VoiceInk's Swift source in any context, declare it. Any module you author in an area where you have seen upstream code must be reviewed or re-implemented by a contributor who has not.
- If the discipline is broken accidentally, declare it immediately. The affected module will be re-implemented by a clean contributor.

See §13.8 of `hush-prd.md` for the full reasoning.

---

## Development environment

```bash
# Install Rust stable
rustup update stable

# Install Node.js >= 20 (nvm recommended)
nvm install 22

# macOS only — whisper-rs needs cmake to build the whisper.cpp bindings
brew install cmake

# Clone and install
git clone https://github.com/khawkins98/Hush.git
cd Hush
npm install

# Run in dev mode
npm run tauri dev

# Run with the whisper feature so transcription actually works
cd src-tauri && cargo tauri dev --features whisper
```

The `whisper` feature is opt-in so a fresh checkout still builds without cmake — useful for contributors working on the UI layer who don't need the model loaded.

---

## Branching

- `main` is the only long-lived branch. Direct pushes are blocked.
- Branch names: `<type>/<short-kebab-description>`
  - Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `perf`, `ci`, `security`
  - Examples: `feat/whisper-integration`, `fix/hotkey-release-edge-case`
- All changes land via PR. Squash-merge into `main`.

---

## Commit format

Conventional Commits 1.0.0: `<type>(<scope>): <subject>`

- **Types:** feat, fix, docs, chore, refactor, test, style, perf, build, ci, security
- **Scopes:** audio, transcription, hotkey, ui, ux, dictionary, history, db, ipc, tauri, updater, build, e2e
- Subject: imperative mood, no full stop, under 72 characters
- Breaking changes: append `!` and add a `BREAKING CHANGE:` footer

---

## Testing

Hush uses several layers of tests, each catching a different class of regression.

### Rust unit tests (`cargo test --lib`)

Pure-logic tests at the trait + module boundaries. Fast (~100 ms total), run on every PR via CI on Linux + macOS.

- **Default features.** Excludes the `whisper` feature so a contributor without cmake can still run them.
- **`whisper` feature.** Same suite plus the `whisper`-gated paths. Needs cmake locally.
- **`#[tokio::test]`** for async commands and SQLite-backed repository tests. Each test gets an in-memory SQLite via `SqliteDatabase::open_in_memory()` so they don't touch disk and don't share state.
- **Hand-rolled mocks** at trait seams (`AudioCapture`, `Transcribe`, `HistoryRepository`, etc.) — see `src-tauri/src/ipc/mod.rs` for the `Noop*` and `Mem*` impls. Hand-rolled is preferred over `mockall` here for clearer test failures.

### Integration tests (`src-tauri/tests/`)

Tests that exercise larger slices than a single module. Two patterns in use:

- **`wiremock`-driven HTTP tests** for the model-download path. The download orchestrator is pure logic; the wiremock server stands in for Hugging Face. See `src-tauri/src/transcription/download.rs`'s test module.
- **`#[ignore]`'d env-var fixtures** for things that need a binary the repo can't ship. The audio fixture (`src-tauri/tests/audio_fixture.rs`) reads `HUSH_TEST_AUDIO` and runs a known WAV through the full transcription stack. Run it with `cargo test --features whisper -- --ignored`. See `src-tauri/tests/fixtures/README.md` for setup.

When adding an integration test that needs an external resource (model file, audio clip, network endpoint), prefer this pattern over committing the resource — `#[ignore]` plus an env-var pointer keeps the repo small and lets contributors opt in.

### Frontend e2e (`npm run test:e2e`)

Playwright + Chromium drives the SvelteKit dev server in `HUSH_E2E=1` mode, which swaps `@tauri-apps/api/{core,event}` for in-tree stubs. Tests configure per-spec `invoke` handlers and fire backend-emitted events. See `tests/e2e/README.md` for the authoring pattern.

What the suite catches: UI regressions, modal a11y, error-copy drift, retry-race UX, aria-attribute bugs.

What it does **not** catch: real IPC, HUD lifecycle, hotkey registration, real audio, real model download. Those flows are tracked behind issue #57 (tauri-driver Path B).

### Manual smoke

Before merging anything that touches the dictation hot path, run through the manual checklist in [`STATUS.md`](./STATUS.md) §c. The path involves a real microphone and (optionally) a real Whisper model — neither of which CI has access to.

### Type check (`npm run check`)

Runs `svelte-check` against the entire frontend including `vite.config.js`. Required to be clean for every PR; the CI job runs the same command.

---

## Code comments

- Public Rust APIs carry `///` doc comments with a one-line summary.
- Comments explain *why*, not *what*.
- Where a module's design was directly inspired by VoiceInk, the module header says so: `// Concept inspired by VoiceInk's <feature>. Reimplemented from observed public behaviour, no source code referenced. See §13.8.`
- Untagged TODOs (`// TODO:` without an issue number) **fail CI lint**. Use `// TODO(#NNN):` or `// FIXME(#NNN):`.

---

## PR checklist

Each PR template renders the checklist below. The short version:

- [ ] CI is green (clippy, rustfmt, cargo test, frontend type check, e2e)
- [ ] Conventional commit title
- [ ] `CHANGELOG.md` entry under `## [Unreleased]` if user-facing
- [ ] `learnings.md` entry if a non-obvious decision was made
- [ ] TODOs reference a GitHub issue number
- [ ] If this touches the dictation path, manual smoke per `STATUS.md` §c was run
- [ ] You confirm you have **not** read VoiceInk's Swift source

---

## Project documents at a glance

- [`README.md`](./README.md) — what Hush is, install, current status.
- [`hush-prd.md`](./hush-prd.md) — product requirements doc; the policy document for v1 scope, non-goals, and milestone plan.
- [`CHANGELOG.md`](./CHANGELOG.md) — Keep-a-Changelog-formatted record of what's shipped.
- [`learnings.md`](./learnings.md) — append-only engineering decision log. Why we picked X over Y, what false starts cost us, what would surprise the next contributor.
- [`STATUS.md`](./STATUS.md) — point-in-time hand-off doc. **Rots fast on purpose** — re-write rather than incrementally update.
- [`SECURITY.md`](./SECURITY.md) — vulnerability reporting policy.
- [`tests/e2e/README.md`](./tests/e2e/README.md) — Playwright suite authoring guide.
- [`src-tauri/tests/fixtures/README.md`](./src-tauri/tests/fixtures/README.md) — audio-fixture setup.
