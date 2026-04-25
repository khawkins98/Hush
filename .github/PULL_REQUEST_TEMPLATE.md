## Summary

<!-- What does this PR do, and why? Link the relevant issue or milestone. -->

## Test plan

<!-- Bulleted list of how you verified the change. Mention manual smoke
     steps if you ran any (per STATUS.md §c), and which automated suites
     you ran locally. -->

- [ ] `cargo test --lib` (Rust unit tests)
- [ ] `cargo test --lib --features whisper` if this touches the transcription path
- [ ] `npm run check` (frontend type check)
- [ ] `npm run test:e2e` if this touches `src/routes/`
- [ ] Manual smoke per `STATUS.md` §c if this touches the dictation path

## Checklist

- [ ] CI is green (clippy, rustfmt, cargo test, frontend type check, e2e)
- [ ] Commit title follows Conventional Commits (`type(scope): subject`)
- [ ] `CHANGELOG.md` entry added under `## [Unreleased]` if user-facing
- [ ] `learnings.md` entry added if a non-obvious engineering decision was made
- [ ] All TODOs reference a GitHub issue number (`// TODO(#123): ...`)
- [ ] I confirm I have **not** read VoiceInk's Swift source code (see CONTRIBUTING.md)
