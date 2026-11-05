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

Start with [Developing Hush](./docs/developing.md#first-time-setup) for the toolchain
and checkout setup, then choose a command from its
[task table](./docs/developing.md#which-command-to-run). That guide owns the
commands, test capabilities, native permission path and troubleshooting.

## Dev workflow: which command to run

- Browser/UI iteration uses a mocked backend; real audio needs the native app.
- Permission and installer claims need the installed-DMG path. An unsigned dev
  binary exercises a different macOS identity.
- Use [the validation sections](./docs/developing.md#testing-layers) appropriate to
  the changed behavior, including the required frontend check and native startup
  smoke when applicable. Reset utilities have side effects; inspect their scope
  before using them.

---

## Issues and labels

Labels are applied by the maintainer during triage. When opening an issue you don't need to apply them yourself — but mentioning the affected area in the body helps.

### Area labels

| Label | Covers |
|---|---|
| `area: audio` | Audio capture, transcription, diarization, meeting pipeline |
| `area: ui/ux` | UI components, visual design, layout, discoverability |
| `area: release` | Updater, signing, distribution, CI/CD |
| `area: testing` | Test infrastructure, E2E, integration tests |

Apply one per issue; apply two for issues that genuinely span areas (e.g. a diarization integration test is both `area: audio` and `area: testing`).

### Status labels

- `status: blocked` — cannot move forward without an external prerequisite (upstream library support, a maintainer keypair step, a platform SDK gap, etc.). Always add a comment explaining what's blocking and what would unblock it.
- `status: deferred` — out of scope or not currently feasible (see [Deferred issues](#deferred-issues) below). Applied when closing an issue that is real but won't be picked up in the near term.

### Priority labels

- `priority: quick-win` — completable in under an hour. Add only when the scope is genuinely small — it's most useful for grabbing something when time is short.

### Type labels

- `bug` — something isn't working as documented or expected
- `enhancement` — new feature or improvement to existing behaviour
- `documentation` — docs-only change
- `question` — needs more information before it can be triaged

---

## Deferred issues

Some issues are closed with `status: deferred` rather than simply deleted. This means the work is real, but it is **not feasible or in scope right now** — typically because:

- it requires a platform (Linux, Windows) that no current maintainer regularly tests, or
- it depends on an upstream library that isn't stable enough to build on, or
- it is a deliberate scope decision to keep the product focused while the core matures.

Deferred issues are **closed** so they don't clutter the active backlog, but they are **not abandoned**. All deferred issues are grouped in the [deferred work tracker (#545)](https://github.com/khawkins98/Hush/issues/545), which records each item, its reason, and the condition that would reopen it.

### Deferring an issue

1. Label it `status: deferred`.
2. Close it with a comment that explains *why* it's deferred and *what would unblock it*.
3. Add a row to the relevant table in [issue #545](https://github.com/khawkins98/Hush/issues/545).

### Reopening a deferred issue

When the reopen condition is met:

1. Reopen the original issue and remove the `status: deferred` label.
2. Leave a comment on [issue #545](https://github.com/khawkins98/Hush/issues/545) noting the date and what changed.
3. If the scope has shifted materially since the issue was filed, close the original and open a fresh issue instead — then update #545 accordingly.

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

Hush has four test layers: Rust unit tests (`cargo test --lib`), integration tests (`src-tauri/tests/`), frontend e2e with mocked IPC (`npm run test:e2e`), and frontend e2e with a real binary (`npm run test:e2e:tauri`). Before merging anything that touches the dictation hot path, also run the manual smoke checklist in [`STATUS.md`](./STATUS.md) §c.

For commands, what each layer catches, and when to run each one, see **[docs/developing.md — Testing layers](./docs/developing.md#testing-layers)**.

---

## Adding a new IPC command

Use [the complete IPC recipe](./docs/ipc-commands.md). It owns the four-place
sync rule, full-path registration requirement, mock serialization constraints,
error mapping, per-window plugin permissions and verification commands. Keep
all four representations aligned in the same change.

---

## Destructive UI actions: click-to-confirm

Buttons that delete user data (clear history, delete a session, remove a vocabulary term or replacement, remove an installed model) use a click-to-confirm pattern instead of a `window.confirm` dialog: the first click swaps the button into an "Are you sure?" state that auto-resets after ~5 s, the second click commits. This keeps the dialog-free, keyboard-friendly feel of the rest of the app and avoids the OS-modal context-switch.

When adding a new destructive surface, mirror the pattern: a `pendingConfirmId` (or `pendingConfirm` boolean) `$state`, a `setTimeout` to clear it, and matching aria copy ("Confirm deletion" vs "Delete"). Voice should be consistent — short, plain ("Delete it?", not "Are you absolutely sure you wish to proceed?").

## Code comments and documentation

- Public Rust APIs carry `///` doc comments with a one-line summary.
- Comments explain *why*, not *what*. Non-obvious constraints, design tradeoffs, and intent deserve a comment; mechanical steps don't.
- Where a module's design was directly inspired by VoiceInk, the module header says so: `// Concept inspired by VoiceInk's <feature>. Reimplemented from observed public behaviour, no source code referenced. See §13.8.`
- Untagged TODOs (`// TODO:` without an issue number) **fail CI lint**. Use `// TODO(#NNN):` or `// FIXME(#NNN):`.

### Keep the docs current

Your PR touches code — keep the docs honest too:

| Changed | Update |
|---------|--------|
| User-visible behaviour | `CHANGELOG.md` under `## [Unreleased]` |
| Non-obvious architectural decision | `learnings.md` entry |
| Developer workflow, commands, or new gotcha | `docs/developing.md` |
| New module or seam added/removed | `ARCHITECTURE.md` module map |

Keep one maintained home for each task: `docs/developing.md` owns commands and
validation, `docs/ipc-commands.md` owns the IPC contribution recipe, and
`ARCHITECTURE.md` owns the module/seam map. Update the relevant home and link from
entry points instead of repeating command catalogs in README or agent guidance.
Keep historical proposals, dated measurements, `CHANGELOG.md` and `learnings.md`
in their recorded scope. Documentation changes must preserve source-attribution,
permission-reset and validation requirements.

### Diagnostic logging (Rust audio/transcription code)

New Rust code in the audio capture → transcription pipeline should be observable via `RUST_LOG=hush=debug` without ad-hoc print statements. Use `tracing::debug!` for one-per-event signals (inference ran, tick result), `tracing::trace!` for per-poll noise. Prefer named structured fields over string interpolation so log lines are grep-friendly:

```rust
// ✓
tracing::debug!(raw_segments, non_empty_segments, window_ms, "inference ran");
// ✗
tracing::debug!("inference ran: {raw} raw, {ne} non-empty");
```

See `docs/developing.md` → "Diagnosing meeting mode" and `learnings.md` "2026-05-06" for the full conventions.

---

## PR checklist

- [ ] CI is green (clippy, rustfmt, cargo test, frontend type check, e2e)
- [ ] Conventional commit title
- [ ] `CHANGELOG.md` entry under `## [Unreleased]` if user-facing
- [ ] `learnings.md` entry if a non-obvious decision was made
- [ ] TODOs reference a GitHub issue number
- [ ] If this touches the dictation path, manual smoke per `STATUS.md` §c was run
- [ ] If this touches `lib.rs`, `tauri.conf.json`, plugin registrations, `Cargo.toml` deps, or `capabilities/`, dev-launch smoke run (`npm run tauri dev` boots without panic)
- [ ] You confirm you have **not** read VoiceInk's Swift source

---

## Periodic verification cadence

CI catches regressions in the obvious dimensions (Rust compilation, clippy, rustfmt, type check, Path A e2e). It does not catch product-quality drift: copy that contradicts itself across recent PRs, accessibility regressions in newly extracted components, security or design assumptions that decayed since the last review, UX inconsistencies introduced by independent fixes. Run the cadence below every **2–3 substantial PRs** while the project is solo-maintained, or any time several non-trivial UI/IPC PRs have landed without a sweep.

### 1. Multi-agent review

Run four review agents in parallel, each with the same diff scope (e.g. "everything since the last review tag" or "PRs N..M"):

- **writer** — prose / docs / changelog / README / panel copy / error messages. Checks for contradictions, stale references, tone drift, broken doc links.
- **rust** — backend code health: trait seams, error variants, deferred TODO debt, unsoundness, panicking paths, async cancellation correctness.
- **ux** — frontend walkthrough across the main window and its surfaces (inline Settings, Meeting, History, About), the HUD overlay, and the menu-bar popover. Affordance consistency, heading hierarchy, destructive-action confirmation, dark-mode parity, empty/error states.
- **security** — TCC permission surface, IPC capability allowlists, command injection vectors, dependency posture, network endpoints.

Spawn them in **one message** with multiple `Agent` tool calls so they run concurrently. Ask each for a short, prioritised report: top 3–5 findings with severity + concrete file:line pointers. Synthesize into one fix list — bundle into a single `chore/multi-agent-review-followup` PR rather than splitting per agent (the changes are usually small and orthogonal). File deferred / larger items as GitHub issues.

### 2. UX walkthrough spec

Re-run the Playwright walkthrough that exercises the post-IA-redesign app flow:

```bash
npm run test:e2e -- tests/e2e/walkthrough.spec.ts
```

This catches the "changed copy in component A, broke an assertion in spec B" class of regression that pure type-check + clippy can't see. If the spec needs updating because copy *intentionally* changed, update it in the same PR as the copy change — never split.

### 3. Mechanical sweep

Before merging the follow-up PR, run the full mechanical pass locally:

```bash
(cd src-tauri && cargo test --lib --features whisper)
(cd src-tauri && cargo clippy --all-targets -- -D warnings)
(cd src-tauri && cargo fmt --all -- --check)
npm run check
npm run test:e2e
npm run tauri dev   # dev-launch smoke if anything in the dev-launch list above changed
```

### 4. Record the round

Add a one-line entry to `learnings.md` with the date, the PRs in scope, and the headline finding (or "no exploitable findings — code health steady"). The log is the durable record that the cadence ran; future contributors can scan it to know when the last sweep was.

---

## Cutting a release

Release engineering lives in [`docs/releases.md`](./docs/releases.md). Short version: bump the three version files (`Cargo.toml`, `tauri.conf.json`, `package.json`), move `[Unreleased]` content to a dated section in `CHANGELOG.md`, push a `v*` tag, review the draft GitHub Release, click Publish. The actual cross-platform build runs in `.github/workflows/release.yml` via `tauri-action`.

To smoke the release pipeline without cutting a real tag: `gh workflow run release.yml` (or "Run workflow" in the Actions UI). It publishes to a `dispatch-<run-id>` draft you can delete after inspection.

---

## Project documents at a glance

- [`README.md`](./README.md) — what Hush is, install, current status.
- [`hush-prd.md`](./hush-prd.md) — historical product spec. Most of v1 has shipped; what's worth reading is §13.8 (attribution and black-box reimplementation posture) and §4 (non-goals). For current feature state see STATUS.md and CHANGELOG.md.
- [`CHANGELOG.md`](./CHANGELOG.md) — Keep-a-Changelog-formatted record of what's shipped.
- [`docs/releases.md`](./docs/releases.md) — maintainer's release-cutting recipe.
- [`learnings.md`](./learnings.md) — append-only engineering decision log. Why we picked X over Y, what false starts cost us, what would surprise the next contributor.
- [`STATUS.md`](./STATUS.md) — point-in-time hand-off doc. **Rots fast on purpose** — re-write rather than incrementally update.
- [`SECURITY.md`](./SECURITY.md) — vulnerability reporting policy.
- [`tests/e2e/README.md`](./tests/e2e/README.md) — Playwright suite authoring guide.
- [`src-tauri/tests/fixtures/README.md`](./src-tauri/tests/fixtures/README.md) — audio-fixture setup.
- [`docs/macos-permissions.md`](./docs/macos-permissions.md) — troubleshooting Microphone + Input Monitoring on dev builds, plus the `tccutil reset` recipe for stuck states.
