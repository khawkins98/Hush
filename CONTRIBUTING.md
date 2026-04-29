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

# UI-only path: app launches without the Whisper transcription
# backend so cmake isn't required on the machine. Useful for
# frontend-only work; transcription returns
# `IpcError::TranscriptionUnavailable` if you click Start.
cd src-tauri && cargo tauri dev --no-default-features
```

The `whisper` feature is **default-on** as of 2026-04-26 so a vanilla `npm run tauri dev` ships transcription. Contributors who don't have cmake (or who only need the UI layer) opt out with `--no-default-features`.

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

What it does **not** catch: real IPC, HUD lifecycle, hotkey registration, real audio, real model download. Those are Path B.

### Frontend e2e — Path B (`npm run test:e2e:tauri`)

`tauri-driver` + WebdriverIO drives a real built Hush binary. Catches the flows Path A's mocks can't: real `invoke` round-trips, real `listen` events, HUD secondary-window lifecycle, real model download against `wiremock`. Scaffold landed under #202 (refs #57); CI is deferred until tauri-driver's macOS path stabilises. Run locally per `tests/e2e-tauri/README.md` — `cargo install tauri-driver --locked`, `npm run tauri build -- --debug`, then the test command above.

### Manual smoke

Before merging anything that touches the dictation hot path, run through the manual checklist in [`STATUS.md`](./STATUS.md) §c. The path involves a real microphone and (optionally) a real Whisper model — neither of which CI has access to.

### Dev-launch smoke (`npm run tauri dev`)

A separate, much lighter check: **run `npm run tauri dev` once before opening a PR that touches startup**. CI does not run a real Tauri runtime — every test target is `cargo test --lib`, `cargo clippy`, or Playwright in plain Chromium with mocked IPC. That means a panic at app boot (plugin initialization, capability misconfig, `AppState::build_default` failure, missing `tauri.conf.json` block) is **invisible to CI** and only surfaces when a contributor pulls the branch. The fix is cheap: launch the dev app, wait for the "starting Hush" tracing log, confirm no panic, kill it. ~30 seconds.

Required when your PR touches:

- `src-tauri/src/lib.rs` (especially the `tauri::Builder` chain or the `setup` hook)
- `src-tauri/tauri.conf.json` (window config, plugin config blocks)
- `src-tauri/Cargo.toml` (adding/removing a Tauri plugin dep)
- `src-tauri/capabilities/*.json`
- Anything that adds or removes a `.plugin(...)` call

### Type check (`npm run check`)

Runs `svelte-check` against the entire frontend including `vite.config.js`. Required to be clean for every PR; the CI job runs the same command.

---

## Adding a new IPC command

A `#[tauri::command]` is touched in **four** places that all have to stay in sync. Skipping a step doesn't always fail CI — sometimes the symptom is a runtime `undefined` field in the frontend or a missing-handler runtime error — so this list is the canonical recipe.

### 1. Define the Rust types

In `src-tauri/src/ipc/commands/mod.rs` (or one of the existing domain submodules — `commands/meeting.rs`, `commands/models.rs`, `commands/macos.rs` — extracted under #82), add the request/response struct. New domain-cohesive command groups should follow the same submodule pattern. Apply `#[serde(rename_all = "camelCase")]` so the wire shape matches JS conventions:

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyCommandResult {
    pub example_field: String,
    pub another_field: u32,  // serialises as `anotherField`
}
```

Define the command itself with `#[tauri::command]`:

```rust
#[tauri::command]
pub async fn my_command(state: State<'_, AppState>, arg: String) -> IpcResult<MyCommandResult> {
    // ...
}
```

Errors should map to a variant of `IpcError` so the frontend's `formatErrorDisplay` (in `src/lib/errors.ts`) can produce a tailored `{ headline, hint?, details? }` for the shared `ErrorDisplay` component. Adding a new error variant means updating that mapper — see step 4 below.

### 2. Register the command in the Tauri builder

In `src-tauri/src/lib.rs`, add the command to the `tauri::generate_handler![...]` macro:

```rust
.invoke_handler(tauri::generate_handler![
    // ... existing commands ...
    ipc::commands::my_command,                       // for top-level commands
    ipc::commands::meeting::meeting_my_command,      // for submodule commands
])
```

The macro looks for `__cmd__<name>` siblings in the same module as the function. Use the **full module path** (`ipc::commands::my_command`, or `ipc::commands::meeting::…` for submodule commands), not a re-export — `pub use` does not carry the macro's hidden symbol. (See `learnings.md` 2026-04-25 for the trap that cost us once.)

### 3. Add the TypeScript type and call site

Declare a TypeScript type that matches the Rust struct's serialised shape. Two conventional locations:

- **Shared across the page and panel components.** Add it to `src/lib/types.ts` (the post-#84 split moved cross-cutting types here) and `import type { MyCommandResult } from "$lib/types";` from both the parent page and any child panel that needs it.
- **Used only by `+page.svelte`.** Declare it inline in the `<script>` block at the top of the page.

```typescript
export type MyCommandResult = {
    exampleField: string;
    anotherField: number;
};
```

Then `invoke<MyCommandResult>("my_command", { arg: "..." })`.

**The shape must match exactly.** A typo here (`example_field` instead of `exampleField`, `string` instead of `number`) compiles fine and produces silent `undefined`s at runtime. The Playwright e2e suite catches this *only* if a spec asserts on the field — it's not automatic.

### 4. Update the Playwright mock

In `tests/e2e/_mock.ts`, add a default handler so e2e tests have a stub for the new command:

```typescript
my_command: (args: unknown) => {
    const a = args as { arg: string };
    return { exampleField: a.arg, anotherField: 42 };
},
```

The mock's field shape must mirror the Rust struct — same camelCase names, same types. A round-5 review caught a regression where the model-card mock had `sizeBytes` / `speed` fields while the Rust side serialised `sizeMb` / `speedRating`; tests passed by luck.

If the new command needs error simulation in a test, override at the spec level (`installMocks(page, { my_command: () => { throw { kind: "settings", message: "..." } } })`).

If the command introduces a new `IpcError` variant, also update `formatErrorDisplay` in `src/lib/errors.ts` so the user gets tailored copy in the shared `ErrorDisplay` instead of the generic default.

### Verifying

After all four steps:

```bash
cd src-tauri && cargo build --lib   # Rust struct + command compile
cargo test --lib                    # IPC tests still pass
cd ..
npm run check                       # TypeScript types compile
npm run test:e2e                    # Mocks work end-to-end
npm run tauri dev                   # Real backend roundtrip (if it touches startup)
```

If any of these surface a mismatch, fix at the appropriate layer above. The four places are coupled; CI catches Rust-only and TS-only breaks but cannot catch type-shape mismatches between them — that's a hands-on smoke responsibility.

---

## Destructive UI actions: click-to-confirm

Buttons that delete user data (clear history, delete a session, remove a vocabulary term or replacement, remove an installed model) use a click-to-confirm pattern instead of a `window.confirm` dialog: the first click swaps the button into an "Are you sure?" state that auto-resets after ~5 s, the second click commits. This keeps the dialog-free, keyboard-friendly feel of the rest of the app and avoids the OS-modal context-switch.

When adding a new destructive surface, mirror the pattern: a `pendingConfirmId` (or `pendingConfirm` boolean) `$state`, a `setTimeout` to clear it, and matching aria copy ("Confirm deletion" vs "Delete"). Voice should be consistent — short, plain ("Delete it?", not "Are you absolutely sure you wish to proceed?").

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
- [ ] If this touches `lib.rs`, `tauri.conf.json`, plugin registrations, `Cargo.toml` deps, or `capabilities/`, dev-launch smoke run (`npm run tauri dev` boots without panic)
- [ ] You confirm you have **not** read VoiceInk's Swift source

---

## Cutting a release

Release engineering lives in [`docs/releases.md`](./docs/releases.md). Short version: bump the three version files (`Cargo.toml`, `tauri.conf.json`, `Cargo.lock`), move `[Unreleased]` content to a dated section in `CHANGELOG.md`, push a `v*` tag, review the draft GitHub Release, click Publish. The actual cross-platform build runs in `.github/workflows/release.yml` via `tauri-action`.

To smoke the release pipeline without cutting a real tag: `gh workflow run release.yml` (or "Run workflow" in the Actions UI). It publishes to a `dispatch-<run-id>` draft you can delete after inspection.

---

## Project documents at a glance

- [`README.md`](./README.md) — what Hush is, install, current status.
- [`hush-prd.md`](./hush-prd.md) — product requirements doc; the policy document for v1 scope, non-goals, and milestone plan.
- [`CHANGELOG.md`](./CHANGELOG.md) — Keep-a-Changelog-formatted record of what's shipped.
- [`docs/releases.md`](./docs/releases.md) — maintainer's release-cutting recipe.
- [`learnings.md`](./learnings.md) — append-only engineering decision log. Why we picked X over Y, what false starts cost us, what would surprise the next contributor.
- [`STATUS.md`](./STATUS.md) — point-in-time hand-off doc. **Rots fast on purpose** — re-write rather than incrementally update.
- [`SECURITY.md`](./SECURITY.md) — vulnerability reporting policy.
- [`tests/e2e/README.md`](./tests/e2e/README.md) — Playwright suite authoring guide.
- [`src-tauri/tests/fixtures/README.md`](./src-tauri/tests/fixtures/README.md) — audio-fixture setup.
- [`docs/macos-permissions.md`](./docs/macos-permissions.md) — troubleshooting Microphone + Input Monitoring on dev builds, plus the `tccutil reset` recipe for stuck states.
