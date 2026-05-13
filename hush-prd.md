# Hush — Product Requirements Document

**Status:** Draft v0.2
**Date:** 2026-04-25
**Owner:** Ken Hawkins
**Project:** Hush, a cross-platform offline voice-to-text app inspired by VoiceInk

## 1. Summary

**Hush** is a cross-platform offline voice-to-text application, built with Tauri (Rust backend, web frontend) and inspired by the macOS-native VoiceInk. The goal of v1 is to deliver the core dictation loop (global hotkey → record → local Whisper transcription → text on clipboard) on macOS, Windows, and Linux from a single codebase, with several macOS-deep features deliberately deferred or descoped.

The name reflects the design intent: dictation that stays local to the device, with no cloud round-trip and no telemetry by default.

## 2. Background

VoiceInk (github.com/Beingpax/VoiceInk) is a Swift/SwiftUI app, GPLv3, that runs whisper.cpp locally and inserts transcribed text directly into the focused application via macOS Accessibility APIs. It also exposes a Parakeet model path through FluidAudio (CoreML on the Apple Neural Engine), a context-aware "Power Mode" keyed off active app and URL, and a smart-paste flow that reads selected text.

Hush preserves the privacy-first, offline-first design (whisper.cpp has stable Rust bindings) and broadens distribution to Windows and Linux. The cost is that several macOS-specific features either need rewriting in platform-specific Rust or do not have a viable cross-platform path at all. This PRD scopes a pragmatic v1 that ships quickly without chasing day-one feature parity.

Upstream VoiceInk does not accept pull requests, so Hush is a parallel project, not a contribution. The reimplementation is black-box: no contributor reads VoiceInk's Swift source. Design is taken from the public README and observable runtime behaviour. See §13.8 for the full posture.

## 3. Goals (v1)

- Cross-platform offline dictation on macOS, Windows, and Linux from one codebase. **Reality check (2026-04-26):** macOS is the maintainer's daily-driver and primary target; Linux and Windows are validated only at the CI level (`cargo test`, `clippy`, `npm run check`) and not hands-on tested. Hush should run on Linux/X11 and Windows as far as the code goes — contributions and bug reports for those platforms are welcome and where they hit issues, those issues become the prioritisation signal.
- Local-only transcription using whisper.cpp via Rust bindings. No audio ever leaves the device, no cloud round-trip during transcription. (The Whisper model itself is fetched once from Hugging Face when the user picks one in the picker, then cached in `<app-data>/models/`. After download, transcription is fully offline.)
- Global toggle-record hotkey + push-to-talk on every platform (default-on as of #194; macOS unblocked via the fufesou rdev fork).
- Transcribed output written to the system clipboard, with a confirmation notification.
- Foreground application name captured on each transcription, stored as metadata.
- Local SQLite history with search, copy, and delete.
- Personal Dictionary: custom vocabulary and find/replace pairs applied post-transcription.
- Auto-update channel via the Tauri updater plugin.

## 4. Non-goals (v1)

The following are recognised as valuable but explicitly out of scope for v1. Each lands in §8 with a future design pass attached.

- Direct text insertion into the focused application. v1 puts the result on the clipboard; the user pastes.
- Reading selected text from the focused application.
- Browser URL detection for app/URL-aware Power Mode.
- Pausing media playback during recording.
- The full Power Mode rule engine (per-app and per-URL profile switching).
- AI assistant mode (VoiceInk's conversational ChatGPT-style flow).
- LLM-driven post-processing modes (writing-style transforms).
- Real-time LLM summarization of meeting transcripts. (Meeting Mode itself ships in v1.x — see §5b. LLM summarization layered on top is a v2 concern; v1.x produces the transcript, the user can pipe it through whatever they like.)

## 5. Engine roadmap

**Whisper.cpp via `whisper-rs` is the v1 engine.** Five sizes (tiny / base / small / medium / large-v3) ship behind the `Transcribe` trait; whisper.cpp's CPU baseline must work everywhere before we add accelerated paths.

**Parakeet via ONNX is on the v1.x roadmap.** Decision revised on 2026-04-25: the original §5 stance ruled Parakeet out entirely because the VoiceInk path is CoreML / Apple-Neural-Engine and would break the cross-platform promise. The ONNX export of Parakeet (NVIDIA publishes it for Triton / ONNX-Runtime use) is a viable cross-platform path — one inference runtime, one binary, all three OSes. We will add Parakeet as a second engine behind the existing `Transcribe` trait once the Whisper stack is shipping cleanly.

What is **explicitly out**: any macOS-specific engine path (FluidAudio, native CoreML bindings) that would require substantial macOS-only Rust to maintain. The cross-platform promise is the constraint we will not relax — if a model can't run via ONNX (or another truly cross-platform runtime), it does not ship.

When Parakeet lands, the model picker grows a second engine card alongside the Whisper variants, and the `Transcribe` trait grows a `transcribe_with_options()` method that engines can use for engine-specific tuning. The settings schema already accommodates this (the `selected_model_id` is engine-prefixed as `whisper-base` / `parakeet-v2-en`).

## 5b. Meeting Mode (v1.x)

Hush v1's core flow is one-shot dictation: the user holds a hotkey, talks, gets a transcript on the clipboard. v1.x adds a passive-transcription surface ("Meeting Mode") that captures system audio + microphone during meetings, transcribes streaming, and persists per-utterance transcripts grouped into sessions. Audio never lands on disk — only transcripts and timestamps.

Meeting auto-start is controlled by a **global Always / Off toggle** in Settings → Meeting → Auto-start mode. The default for new installs is **Always** (enabled out of the box). When `Always` is active, Hush monitors CoreAudio device state via the HAL property listener; when the microphone activates and the frontmost app is a recognised meeting client (Zoom, Teams, Meet, Discord, Slack-call, Webex, FaceTime, Skype), Hush automatically starts a meeting session and auto-stops it when the meeting app releases the mic. The user can turn this off globally, or manually start/stop sessions regardless of the mode.

Media apps (YouTube, Spotify, Apple Music) are intentionally absent from the default classifier table, so playback does not trigger auto-start.

**Phase E (#112)** — per-app policy overrides ("always for Zoom, never for Discord") — is the next layer on top and is not yet shipped. The global toggle is the shipped v1.x design.

Streaming transcription depends on either the Whisper.cpp sliding-window pattern or Parakeet (the streaming-friendly second engine — see §5). Whichever ships first is the v1.x default; the other becomes a settable preference.

Diarization (per-speaker labels) ships in two layers. **Default off:** the source-tagged "You" / "Remote" view (Granola-style; mic = You, system audio = Remote). **Opt-in (Settings → Meeting → Speakers):** model-based per-speaker labels — utterances run through an ONNX speaker-embedding model (wespeaker ResNet34-LM, 26 MB, auto-downloaded on first enable, SHA-256 verified). Cluster IDs are stable across pump ticks via online 1-NN-with-threshold matching, so the speaker who's "Speaker 1" early in a meeting stays "Speaker 1" throughout. Closed in #111 (PR chain #295–#300 + audit follow-ups #303–#305). The trait-seam (`Diarize`) and `FlagGatedDiarizer` wrapper survive for any future swap-ins (e.g. a smaller / larger speaker model).

**Privacy guarantee:** audio is buffered in RAM for ~30 s during inference and discarded. No WAV files, no SQLite blobs, no temp files. The Sessions panel surfaces this guarantee permanently, not as a one-time banner.

Out of scope for v1.x:

- Cloud transcription (project identity stays local-only).
- Direct meeting-platform APIs (Zoom SDK, Teams Graph).
- Calendar metadata.
- Real-time LLM summarization (the user can pipe transcripts elsewhere).
- Cross-session voice fingerprinting / "always recognise Ken's voice."

Phased delivery (each phase independently shippable; tracked under #33):

- **Phase A** — System audio capture per platform. macOS via CoreAudio process tap (#105 — ScreenCaptureKit replaced in v0.5.0), Linux via PulseAudio monitor (#106), Windows via WASAPI loopback (#107).
- **Phase B** — Streaming transcription (#108). Whisper sliding-window first; Parakeet later.
- **Phase C** — Sessions + meeting-mode UI (#110). Foundation (data layer + IPC + scaffolded panel) shipped post-v0.1.0.
- **Phase D** — Diarization (#111). Mic-vs-system bubble UI ships in Phase C; real per-speaker labels via wespeaker ONNX shipped 2026-04-30 across #295–#300 + audit follow-ups #303–#305.
- **Phase E** — Per-app classifier policy refinement (#112).

Design memo at `docs/system-audio-meeting-mode-proposal.md`. The privacy framing draws from Granola's "transcribes, doesn't record" stance; the consent-on-new-voice framing from Limitless's pendant.

## 6. Architecture overview

**Frontend.** Tauri webview rendering an SPA. Proposed stack: Svelte + Tailwind (smaller bundle than React, better for the HUD overlay). Handles settings, history view, dictionary management, and the floating recording HUD.

**Backend (Rust).** Tauri command handlers exposing audio capture, transcription, history queries, hotkey registration, meeting auto-detection (CoreAudio HAL listener on macOS), dictionary application, and clipboard writes.

**Transcription.** whisper.cpp via the `whisper-rs` crate. Quantised GGUF models stored in the platform's app-data directory, downloaded on first use. Default to `small` Q5_0; let the user pick `tiny`, `base`, `small`, `medium`, `large-v3`.

**Audio.** `cpal` for cross-platform input enumeration and capture. 16 kHz mono PCM into a ring buffer, flushed to whisper-rs at recording stop.

**Storage.** SQLite via `sqlx`, with tables for `history`, `dictionary_terms`, `replacements`, and `settings`.

**Hotkeys.** `tauri-plugin-global-shortcut` for toggle-record. For push-to-talk we likely need `rdev` directly, since toggle-style global shortcut plugins do not always expose key-down vs key-up cleanly.

**Foreground app.** `active-win-pos-rs`, polled at recording-start, captures the app name and window title. No URL detection in v1.

**Output.** Tauri's clipboard plugin writes the final string to the system clipboard. A native notification confirms "Ready to paste". Auto-paste (synthetic Cmd/Ctrl+V) is out of scope for v1; it would pull in the same permission-prompt complexity we are trying to avoid.

## 7. Component mapping

| Capability | VoiceInk (Swift) | Hush (Tauri) |
|---|---|---|
| Transcription engine | whisper.cpp via Swift bridge | `whisper-rs` |
| Alt engine | Parakeet via FluidAudio (CoreML) | Not implemented (see §5) |
| Audio capture | AVFoundation | `cpal` |
| Global hotkeys | KeyboardShortcuts | `tauri-plugin-global-shortcut`, with `rdev` for PTT |
| Launch at login | LaunchAtLogin | `tauri-plugin-autostart` |
| Auto-update | Sparkle | `tauri-plugin-updater` |
| Text output | Accessibility API direct insertion | Clipboard write + notification |
| Selected text read | SelectedTextKit | Deferred (§8) |
| Foreground app | NSWorkspace | `active-win-pos-rs` |
| Browser URL detection | AppleScript per browser | Deferred (§8) |
| Media pause during recording | MediaRemoteAdapter | Deferred (§8) |
| History / settings store | Core Data | SQLite via `sqlx` |
| HUD overlay | SwiftUI floating panel | Tauri transparent borderless window |
| File compression | Zip | `zip` crate |
| Atomic primitives | swift-atomics | Rust standard `std::sync::atomic` |

## 8. Deferred to future releases

These are tagged as future work, with a brief note on what each will need when picked up.

**Direct text insertion into the focused app.** Will require synthetic paste keystroke via `enigo` plus per-platform permission flows: Accessibility on macOS, no special permission on Windows, Wayland/X11 split on Linux.

**Reading selected text from the focused app.** No clean cross-platform crate exists. Per-OS implementation: AXUIElement on macOS, UI Automation on Windows, AT-SPI on Linux.

**Browser URL detection for Power Mode.** macOS likely uses AppleScript per browser. Windows and Linux probably need a small WebExtension shipped alongside the app. Out of scope until §8.1 and §8.2 land, since URL-awareness without text-insertion or selected-text gives little user value.

**Media playback pause during recording.** macOS has MediaRemote, Windows has the Global System Media Transport Controls API, Linux uses MPRIS over D-Bus. Each path is small individually but the abstraction layer is non-trivial.

**Full Power Mode rule engine.** Depends on the items above. Once foreground app + URL + selected text are all available, the rule engine becomes a straightforward config layer over them.

**AI assistant mode and LLM post-processing.** Pure HTTP, no platform integration. Cheap to add once the core loop is stable; deferred to keep v1 focused.

## 9. In-scope feature list (v1)

- Multi-platform builds: macOS Apple Silicon (.dmg; macOS 26 is the supported target), Windows x64 + arm64, Linux x64 (.deb and .AppImage). macOS Intel is out of scope per the macOS 26-only design target — see `learnings.md` 2026-04-26 for the rationale.
- Whisper model picker with download progress, SHA verification, and disk-usage display.
- Push-to-talk and toggle-record hotkeys, user-configurable. Default-on across all platforms as of #194; on macOS the Input Monitoring TCC prompt fires at first listener spawn. The macOS 26 rdev abort (#69) is resolved by pinning the [fufesou rdev fork](https://github.com/fufesou/rdev) (CGEventTap attached to `CFRunLoopGetMain()`).
- Recording HUD overlay (transparent window) shown while listening, with a level meter.
- Transcribed text written to system clipboard with a "Ready to paste" notification.
- History view: paginated list, full-text search, copy-to-clipboard, delete, export to CSV.
- Personal Dictionary: custom terms biased into the Whisper prompt, plus literal find/replace pairs applied post-transcription.
- Foreground app name and window title captured on each history entry.
- Settings: model, hotkeys, audio input device, dictionary, launch-at-login, update channel, telemetry opt-in (off by default).
- Auto-update via signed updater feed.

## 10. Risks and open questions

**Performance on lower-end Windows hardware.** Whisper.cpp without ANE acceleration is materially slower than VoiceInk on Apple Silicon. Mitigation: default to `base` Q5_0, expose a clear quality/speed selector, document expected latency per tier.

**First-run permissions UX on macOS.** Microphone access is required immediately; Input Monitoring is required for global hotkeys to fire reliably when the app is unfocused. We will need a guided first-run that surfaces these prompts one at a time with plain-language copy.

**Global hotkeys under Wayland.** Inconsistent across compositors. Likely we document GNOME as the supported target initially and fall back gracefully elsewhere.

**Code signing and notarisation.** Each platform has its own pipeline: Apple notarisation, Windows code signing (EV cert or SmartScreen reputation build-up), Linux largely unsigned. Budget and timeline must allow for this.

**Distribution channels.** GitHub Releases as the source of truth. Secondary channels: Homebrew cask (macOS), winget and Chocolatey (Windows), Flathub (Linux). We will need a small landing page for download links.

**Mac App Store: off the table (resolved 2026-05-03).** `macOSPrivateApi: true` in `tauri.conf.json` is required for HUD transparency and permanently disqualifies Hush from MAS distribution (#114). This is an accepted trade-off: Hush is a side project where direct image-download distribution is sufficient, and the transparent HUD is more valuable than the MAS channel. Future contributors: do not attempt MAS distribution without first redesigning the HUD to avoid Apple private APIs.

**Project name. Resolved (2026-04-25).** Project name is **Hush**. Reflects the privacy-first, quiet-by-default design intent and avoids confusion with VoiceInk. Domain availability (.app, .dev, .com) and trademark search to be completed before public release.

**Licensing posture and upstream attribution. Path resolved (2026-04-25).** Black-box reimplementation under our own licence (Path A in §13.8). Self-imposed discipline: no contributor reads VoiceInk's Swift source, ever. The specific licence (Apache-2.0, MIT, GPLv3, or other) remains open and will be settled before first public release. Default fallback: Apache-2.0.

## 11. Proposed milestones

**M1 — Transcription spike (week 1–2):** Tauri shell, `whisper-rs` integration, `cpal` capture. Transcribe a hardcoded WAV file end-to-end from a button click on all three platforms. Confirms the Rust path is viable.

**M2 — Hotkey + clipboard loop (week 3–4):** Global hotkey registers, audio captures while held, transcription lands on the clipboard, notification confirms. Minimal HUD. The smallest useful version of the product.

**M3 — Persistence (week 5–6):** SQLite schema, history view, settings panel, model picker with download UI.

**M4 — Personal Dictionary (week 7):** Whisper prompt biasing, find/replace pipeline, dictionary CRUD.

**M5 — Foreground app capture (week 8):** Wire `active-win-pos-rs`, store app name and window title on each history row, surface in the history view as a filter.

**M6 — Polish and updates (week 9–10):** First-run permission flow, error handling, Tauri updater plugin, signing pipelines for all three platforms.

**M7 — Beta release:** Tagged build, signed artefacts on GitHub Releases, basic landing page, opt-in beta channel.

## 12. Success criteria for v1

- Cold-start to first transcription under 3 seconds on a 2020-era laptop using the `base` model.
- Transcription accuracy matching the whisper.cpp baseline. The wrapper introduces no measurable error.
- Hotkey-to-clipboard round-trip under 1.5 seconds for a five-second utterance, `base` model, Apple Silicon.
- Zero outbound network traffic during normal operation, verified by an offline smoke test.
- Successful install and full round-trip on macOS 26+ (older macOS explicitly out of scope), Windows 11, and Ubuntu 24.04.

## 13. Engineering conventions

This section establishes repository hygiene and development practices the project will follow from day one. Treat as project policy, not aspiration. Conventions are cheap to set up before there is any code; expensive once there is.

### 13.1 Repository files

The repository carries the following at the root, alongside the source tree:

- `README.md`: what it is, install, quick-start, screenshots.
- `CONTRIBUTING.md`: dev environment setup, branch and PR conventions, commit format, test expectations, the upstream-attribution discipline (§13.8).
- `CODE_OF_CONDUCT.md`: Contributor Covenant v2.1.
- `LICENSE`: see §13.8.
- `CHANGELOG.md`: Keep a Changelog format, see §13.4.
- `learnings.md`: engineering decision log, see §13.6.
- `SECURITY.md`: responsible disclosure contact and process. Stub initially, fleshed out before first public release.
- `.editorconfig`, `rustfmt.toml`, `clippy.toml`: formatting and lint config.
- `.github/PULL_REQUEST_TEMPLATE.md` and `.github/ISSUE_TEMPLATE/`: PR and issue templates.

### 13.2 Branching and pull requests

Trunk-based with short-lived feature branches.

- `main` is the only long-lived branch. Direct pushes are blocked by branch protection.
- Feature branches follow `<type>/<short-kebab-description>`, where `<type>` is one of `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `perf`, `ci`. Examples: `feat/whisper-integration`, `fix/hotkey-release-edge-case`.
- All changes land via pull request, including solo work. The PR is the audit trail.
- PRs require: green CI, conventional commit title, a `CHANGELOG.md` entry under `## [Unreleased]` if the change is user-facing, and a `learnings.md` entry where a non-obvious decision was made.
- Squash-merge into `main`. The PR title becomes the commit message, which keeps `main` legible and aligns one commit to one feature.

### 13.3 Conventional commits

Format: `<type>(<scope>): <subject>` per Conventional Commits 1.0.0.

- Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `style`, `perf`, `build`, `ci`.
- Scopes draw from the architecture: `audio`, `transcription`, `hotkey`, `ui`, `dictionary`, `history`, `db`, `ipc`, `updater`, `build`.
- Breaking changes append `!` to the type (e.g. `feat(ipc)!:`) and include a `BREAKING CHANGE:` footer with the migration note.
- Subject in imperative mood, no full stop, under 72 characters.

This pays for itself when we wire up automated changelog generation (§13.4).

### 13.4 Changelog

Format: Keep a Changelog v1.1.0. Versioning: SemVer.

Sections per release: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. An `## [Unreleased]` block sits at the top during development.

The CHANGELOG is curated, human-readable, and end-user facing. We can use `git-cliff` or `release-please` to draft entries from conventional commits, but the released text gets a manual editing pass before tagging. The CHANGELOG is for users; the git log is for developers.

Pre-1.0 we accept breaking changes between minor versions, with a clear `BREAKING` callout. After 1.0 we follow SemVer strictly.

### 13.5 Test-driven development

New functionality starts with a failing test. Bug fixes start with a failing test that reproduces the bug. Red, green, refactor.

**Pragmatic boundary.** Real-time audio capture, OS-level hotkey behaviour, and GUI plumbing are not unit-test friendly. We accept this and aim for high coverage on the pure-logic layers (dictionary application, replacement pipeline, history query and filter, settings serialisation, model picker logic) and integration tests on the IPC boundary. Audio device behaviour and end-to-end transcription get manual smoke tests with documented checklists in `tests/manual/`.

**Tooling.**

- Rust: `cargo test` with `#[cfg(test)] mod tests` for unit, `tests/` for integration. Coverage tracked via `cargo-llvm-cov` in CI.
- Frontend: Vitest for unit. Playwright for end-to-end if the project gets there.
- IPC commands written against trait objects so OS-touching code can be mocked at the seam.

CI runs the full test suite plus `cargo clippy -- -D warnings` and `rustfmt --check` on every PR. A red CI blocks merge.

### 13.6 The learnings log

`learnings.md` is a running engineering decision log. Append-only, dated entries, written in the same PR as the decision. Captures the things too small for a formal ADR but too useful to lose: dependency choices, platform quirks, false starts ("we tried X, it didn't work because Y, here's what we did instead"), Whisper model performance observations, hotkey behaviour by OS, anything that a future contributor (or a future-Ken six months from now) would benefit from.

The format is loose. The discipline is consistency of capture. A reasonable entry:

```
## 2026-05-14 — chose `cpal` over `cubeb` for audio capture

Tested both. Cubeb gave lower latency on macOS but its Windows
backend was less stable in our soak test (3 crashes in 2 hours
of continuous capture). Cpal's Wasapi backend was rock solid.
Trading 15ms of latency for stability.
```

### 13.7 Code comments and documentation

- Public Rust APIs carry `///` doc comments with at minimum a one-line summary, and an example block where non-trivial.
- Inline comments explain *why*, not *what*. The code already says what.
- Where a design was clearly informed by VoiceInk (the HUD overlay pattern, the Personal Dictionary as Whisper-prompt-bias plus post-replace pipeline, the global push-to-talk model), the relevant module header acknowledges this in plain English. Suggested form: `// Concept inspired by VoiceInk's <feature>. Reimplemented from observed public behaviour, no source code referenced. See §13.8.`
- TODO and FIXME tags must reference a GitHub issue number. Untagged TODOs fail CI lint.

### 13.8 License posture and upstream attribution

**Decision (2026-04-25).** Hush is a black-box reimplementation. No contributor reads VoiceInk's Swift source code at any point during development. Design comes from the public README, the running app's observable behaviour, and our own knowledge of how dictation apps work. The specific licence is open (see end of section).

**Why this approach.** VoiceInk is GPLv3. Copyright covers expression, not ideas; reimplementing the same product idea, UI patterns, and architectural choices is legally fine provided no copyrightable expression is copied. A clean reimplementation produces a legally independent work, free to carry whatever licence we choose.

"Clean-room" in the strict sense means one person reads the upstream and produces a behavioural specification, and a second person who has never seen the upstream implements from that specification. For a small or solo team this is impractical. The realistic equivalent is the self-imposed discipline above. This is sometimes called black-box reimplementation and is the standard practice for compatible reimplementations of open-source projects.

**The discipline in practice.**

- No contributor reads VoiceInk's Swift source. Before, during, or after writing the equivalent in Hush, the answer is the same: no.
- Contributors who have previously seen VoiceInk's source in another context declare it on joining. Modules they author in areas they have seen upstream code for are flagged for review or re-implementation by someone who has not.
- The discipline is recorded in `learnings.md` on day one and restated in `CONTRIBUTING.md` so new contributors meet it before their first PR.
- If the discipline is broken accidentally, the contributor declares it, and the affected module is re-implemented by someone clean. Better an awkward redo than a tainted codebase.

**Specific licence: open question.** Path A leaves us free to pick. Candidates:

- **Apache-2.0:** permissive, includes patent grant, friendly to commercial wrapping or downstream forks. Most common choice for new Rust desktop apps.
- **MIT:** simpler, no explicit patent grant. Slightly less defensive than Apache-2.0.
- **GPLv3:** stays in the spirit of upstream, ensures downstream forks stay open, costs commercial flexibility.

Default if nothing else is settled before first public release: Apache-2.0. Decision deadline: end of M3 (week 6).

**Important caveat.** This section reflects engineering judgement, not legal advice. A formal review by a lawyer is sensible before any paid release, and is required if the project is ever commercialised.

**Attribution checklist (in addition to whatever the chosen licence requires):**

- README "Acknowledgements" section names VoiceInk, links to the upstream repo, and credits Pax as the originator of the product concept.
- CONTRIBUTING explains the black-box discipline so new contributors do not accidentally taint the project.
- Module-level code comments cite VoiceInk where the inspiration is direct and specific (see §13.7).
- The first CHANGELOG entry records the project's origin: "Initial release. Hush is a behavioural reimplementation of VoiceInk (github.com/Beingpax/VoiceInk). No source code copied or referenced."

## 14. Out of scope for this document

- Pricing, licensing tiers, paid feature gating.
- Detailed UI mockups (covered in a separate design doc).
- Marketing site copy.
- Telemetry schema (separate privacy review needed before any telemetry is added, even opt-in).
