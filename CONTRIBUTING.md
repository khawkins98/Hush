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

# Clone and install
git clone https://github.com/khawkins/Hush.git
cd Hush
npm install

# Run in dev mode
npm run tauri dev

# Run Rust tests
cd src-tauri && cargo test

# Lint
cargo clippy -- -D warnings
cargo fmt --check
```

---

## Branching

- `main` is the only long-lived branch. Direct pushes are blocked.
- Branch names: `<type>/<short-kebab-description>`
  - Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `perf`, `ci`
  - Examples: `feat/whisper-integration`, `fix/hotkey-release-edge-case`
- All changes land via PR. Squash-merge into `main`.

---

## Commit format

Conventional Commits 1.0.0: `<type>(<scope>): <subject>`

- **Types:** feat, fix, docs, chore, refactor, test, style, perf, build, ci
- **Scopes:** audio, transcription, hotkey, ui, dictionary, history, db, ipc, updater, build
- Subject: imperative mood, no full stop, under 72 characters
- Breaking changes: append `!` and add a `BREAKING CHANGE:` footer

---

## PR checklist

- [ ] CI is green (clippy, rustfmt, tests)
- [ ] Conventional commit title
- [ ] `CHANGELOG.md` entry under `## [Unreleased]` if user-facing
- [ ] `learnings.md` entry if a non-obvious decision was made
- [ ] TODOs reference a GitHub issue number (`// TODO(#123): ...`)

---

## Code comments

- Public Rust APIs carry `///` doc comments with a one-line summary.
- Comments explain *why*, not *what*.
- Where a module's design was directly inspired by VoiceInk, the module header says so: `// Concept inspired by VoiceInk's <feature>. Reimplemented from observed public behaviour, no source code referenced. See §13.8.`
- Untagged TODOs (`// TODO:` without an issue number) **fail CI lint**.
