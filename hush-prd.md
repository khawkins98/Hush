# Hush — Product Requirements Document

**Status:** Historical design document (v0.2, 2026-04-25). See [`STATUS.md`](./STATUS.md) and [`CHANGELOG.md`](./CHANGELOG.md) for current state.
**Owner:** Ken Hawkins
**Project:** Hush, a cross-platform offline voice-to-text app inspired by VoiceInk

> **Note:** This document is the original product spec and reflects the design direction at v0.2 (April 2026). Most v1 goals have shipped in the v0.5–v0.6 series. For current feature status, see [`STATUS.md`](./STATUS.md) and [`CHANGELOG.md`](./CHANGELOG.md). Sections §3–§12 and §13.1–§13.7 that appeared in the original spec are now superseded by [`STATUS.md`](./STATUS.md), [`ARCHITECTURE.md`](./ARCHITECTURE.md), [`CONTRIBUTING.md`](./CONTRIBUTING.md), and [`CLAUDE.md`](./CLAUDE.md). The sections retained here are the product context (§1–§2), non-goals (§4), and the attribution/legal posture (§13.8) — the last of which is referenced from multiple places and must stay in this file.

## 1. Summary

**Hush** is a cross-platform offline voice-to-text application, built with Tauri (Rust backend, web frontend) and inspired by the macOS-native VoiceInk. The goal of v1 is to deliver the core dictation loop (global hotkey → record → local Whisper transcription → text on clipboard) on macOS, Windows, and Linux from a single codebase, with several macOS-deep features deliberately deferred or descoped.

The name reflects the design intent: dictation that stays local to the device, with no cloud round-trip and no telemetry by default.

## 2. Background

VoiceInk (github.com/Beingpax/VoiceInk) is a Swift/SwiftUI app, GPLv3, that runs whisper.cpp locally and inserts transcribed text directly into the focused application via macOS Accessibility APIs. It also exposes a Parakeet model path through FluidAudio (CoreML on the Apple Neural Engine), a context-aware "Power Mode" keyed off active app and URL, and a smart-paste flow that reads selected text.

Hush preserves the privacy-first, offline-first design (whisper.cpp has stable Rust bindings) and broadens distribution to Windows and Linux. The cost is that several macOS-specific features either need rewriting in platform-specific Rust or do not have a viable cross-platform path at all. This PRD scopes a pragmatic v1 that ships quickly without chasing day-one feature parity.

Upstream VoiceInk does not accept pull requests, so Hush is a parallel project, not a contribution. The reimplementation is black-box: no contributor reads VoiceInk's Swift source. Design is taken from the public README and observable runtime behaviour. See §13.8 for the full posture.

## 4. Non-goals (v1)

The following are recognised as valuable but explicitly out of scope for v1. Each is a potential future release.

- Direct text insertion into the focused application. v1 puts the result on the clipboard; the user pastes.
- Reading selected text from the focused application.
- Browser URL detection for app/URL-aware Power Mode.
- Pausing media playback during recording.
- The full Power Mode rule engine (per-app and per-URL profile switching).
- AI assistant mode (VoiceInk's conversational ChatGPT-style flow).
- LLM-driven post-processing modes (writing-style transforms).
- Real-time LLM summarization of meeting transcripts. (Meeting Mode ships in v1.x. LLM summarization layered on top is a v2 concern; v1.x produces the transcript, the user can pipe it through whatever they like.)

## 13.8 License posture and upstream attribution

**Decision (2026-04-25).** Hush is a black-box reimplementation. No contributor reads VoiceInk's Swift source code at any point during development. Design comes from the public README, the running app's observable behaviour, and our own knowledge of how dictation apps work. The specific licence is open (see end of section).

**Why this approach.** VoiceInk is GPLv3. Copyright covers expression, not ideas; reimplementing the same product idea, UI patterns, and architectural choices is legally fine provided no copyrightable expression is copied. A clean reimplementation produces a legally independent work, free to carry whatever licence we choose.

"Clean-room" in the strict sense means one person reads the upstream and produces a behavioural specification, and a second person who has never seen the upstream implements from that specification. For a small or solo team this is impractical. The realistic equivalent is the self-imposed discipline above. This is sometimes called black-box reimplementation and is the standard practice for compatible reimplementations of open-source projects.

**The discipline in practice.**

- No contributor reads VoiceInk's Swift source. Before, during, or after writing the equivalent in Hush, the answer is the same: no.
- Contributors who have previously seen VoiceInk's source in another context declare it on joining. Modules they author in areas they have seen upstream code for are flagged for review or re-implementation by someone who has not.
- The discipline is recorded in `learnings.md` on day one and restated in `CONTRIBUTING.md` so new contributors meet it before their first PR.
- If the discipline is broken accidentally, the contributor declares it, and the affected module is re-implemented by someone clean. Better an awkward redo than a tainted codebase.

**Specific licence: Apache-2.0.** Chosen for its patent grant and compatibility with commercial downstream use. Settled before first public release.

**Important caveat.** This section reflects engineering judgement, not legal advice. A formal review by a lawyer is sensible before any paid release, and is required if the project is ever commercialised.

**Attribution checklist (in addition to whatever the chosen licence requires):**

- README "Acknowledgements" section names VoiceInk, links to the upstream repo, and credits Pax as the originator of the product concept.
- CONTRIBUTING explains the black-box discipline so new contributors do not accidentally taint the project.
- Module-level code comments cite VoiceInk where the inspiration is direct and specific (see `CONTRIBUTING.md` §Code comments).
- The first CHANGELOG entry records the project's origin: "Initial release. Hush is a behavioural reimplementation of VoiceInk (github.com/Beingpax/VoiceInk). No source code copied or referenced."
