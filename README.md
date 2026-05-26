<div align="center">

<img src="static/app-icon@2x.png" alt="Hush" width="128" height="128">

# Hush

**Local voice-to-text and meeting transcription.**
macOS, Linux, and Windows. Free. Open source. No account. No cloud.

Dictate anywhere with a hotkey. Capture meetings with mic + system audio in parallel (macOS). Label You vs Remote.

[Download](https://github.com/khawkins98/Hush/releases) · [Privacy](#privacy-grep-the-source) · [Engineering](#engineering) · [Contribute](./CONTRIBUTING.md)

</div>

---

## Why I made it

This started as a hobby project. I wanted transcripts of my work calls, but I never liked recording my colleagues just to get them — I needed the words, not a sound file of everyone that outlives the meeting. I'd also gotten hooked on a push-to-talk dictation hotkey and didn't want to give it up. What nagged at me was keeping two separate tools for that: one for dictation, one for meetings. I wanted a single app that did both, kept everything on my machine, and left no recording behind.

So I wanted to see how far I could get building it myself. Hush is what I came up with.

---

## What it is

- **A push-to-talk dictation tool.** Hold your hotkey (default `Right ⌘` on macOS, `Right Ctrl` on Linux + Windows), speak, release. The transcript is on your clipboard, ready to paste, before you've moved your hands. No browser tab, no web service, no upload.
- **A meeting transcription tool.** Click Record in a call — or let Hush auto-start when a meeting app takes your mic (Zoom, Teams, Meet, Slack, and a dozen others; on by default, toggle in Settings). It captures your mic and the call's system audio in parallel, transcribes both locally with whisper.cpp, and labels them **You / Remote** — or **Speaker 1, 2…** with the optional local diarisation model. Parallel system audio and auto-detection are macOS-only for now ([#106](https://github.com/khawkins98/Hush/issues/106) / [#107](https://github.com/khawkins98/Hush/issues/107)); Linux and Windows run meeting mode mic-only.
- **Both, in one app, sharing one history.** Most tools pick one lane. Hush does dictation and meetings, with one model load and one searchable history.

The audio never leaves your machine, and never lands on disk — it's processed in RAM and gone the moment the transcript is on your clipboard.

---

## How it compares

"Free?" and "Open source?" are split on purpose — they're often conflated, but a free tier on closed-source SaaS is the vendor's to revoke, while an OSS license is yours to fork.

|  | Local? | Dictation? | Meetings? | Free? | Open source? | Platforms |
|---|---|---|---|---|---|---|
| **Hush** | ✅ | ✅ hotkey | ✅ mic + system audio in parallel (macOS) | ✅ | ✅ Apache 2.0 | macOS · Linux · Windows |
| [OpenWhispr](https://github.com/OpenWhispr/openwhispr) | ✅ | ✅ | ✅ auto-detect Zoom / Teams | ✅ local unlimited; cloud tier: 2k words/wk free, $8/mo Pro | ✅ MIT | macOS · Linux · Windows |
| [Whispering](https://github.com/EpicenterHQ/epicenter) | ✅ | ✅ | — | ✅ | ✅ AGPLv3 | macOS · Linux · Windows |
| [Buzz](https://github.com/chidiwilliams/buzz) | ✅ | partial (live mic, no PTT into other apps) | file import only | ✅ | ✅ MIT | macOS · Linux · Windows |
| [Meetily](https://meetily.ai) | ✅ | — | ✅ system audio, no bot | ✅ | ✅ MIT | macOS · Linux · Windows |
| [VoiceInk](https://github.com/Beingpax/VoiceInk) | ✅ | ✅ | — | ✅ | ✅ | macOS only |
| [MacWhisper](https://goodsnooze.gumroad.com/l/macwhisper) | ✅ | ✅ | partial (file import) | freemium | — | macOS only |
| [Superwhisper](https://superwhisper.com) | ✅ | ✅ | ✅ free tier ([Meeting Transcription](https://superwhisper.com/meeting-transcription)) | freemium | — | macOS · Windows · iOS |
| [Granola](https://www.granola.ai) | cloud LLM | — | ✅ | freemium | — | macOS · Windows |
| [Otter](https://otter.ai) / [Fireflies](https://fireflies.ai) / [Fathom](https://fathom.video) | — | — | ✅ (cloud bot or web) | freemium | — | web |

The cross-platform OSS rows are the real competition. [OpenWhispr](https://github.com/OpenWhispr/openwhispr) is the closest — local dictation + meetings on the same three OSes — though its builds bundle an optional cloud tier (local stays unlimited; cloud caps at 2k words/week free). What sets Hush apart: dictation and parallel-source meeting capture in one app, one whisper.cpp load, one searchable history, with a privacy posture you can verify (below). The closest macOS app, [VoiceInk](https://github.com/Beingpax/VoiceInk), is what inspired Hush — see [Acknowledgements](#acknowledgements).

---

## Privacy: grep the source

Privacy is the point of Hush, so it's built to be verified rather than promised.

- **Two outbound `reqwest` callers exist in the entire codebase.** Both are user-initiated. Grep `src-tauri/src` for `reqwest::Client` if you don't believe it:
  - **Whisper / speaker model downloads** when you click Download in the model picker. HTTPS-only, host-pinned to `huggingface.co` / `*.hf.co` (one signed-CDN hop allowed for HF's storage backend), redirect-cap of 4, **SHA-256 verified on every download**. Once the model is cached, transcription is fully offline.
  - **Manual update check** when you click "Check for updates". Single read-only request to `api.github.com/repos/khawkins98/Hush/releases/latest`. No identifying headers beyond the default user agent.
- **No telemetry. No analytics. No crash reporter. No startup beacon.** Auto-update is not enabled by default.
- **Audio never lands on disk.** Mic + system-audio capture goes into [`audio_buffer::AudioRollingBuffer`](./src-tauri/src/meeting/audio_buffer.rs) — a 30-second RAM ring. Nothing is written to a temp file, no WAV is staged. The transcript is the only persisted artefact.

If telemetry or auto-update ever ships, it'll be opt-in with a separate privacy review.

That's the posture that matters if you handle audio you can't upload — therapy, legal, journalism, research: nothing leaves your laptop, no account holds your call history, and you can audit or fork the source yourself.

---

## Install

Hush isn't signed with an Apple Developer ID — the $99/year certificate isn't worth it for a solo hobby project — so macOS (and Windows) show a security warning on first launch. Clearing it is a one-time, ~30-second step.

**macOS — Homebrew (recommended):**

```bash
brew install --cask khawkins98/tap/hush
```

Updates: `brew upgrade --cask hush`. Or download the `.dmg` from [Releases](https://github.com/khawkins98/Hush/releases) and drag Hush.app to Applications.

**Linux / Windows:** [Releases](https://github.com/khawkins98/Hush/releases) → latest `v*` tag (`.AppImage` / `.deb`, or `.msi` / `.exe`). CI-built, not hands-on tested.

**New to Hush?** [`docs/getting-started.md`](./docs/getting-started.md) is the full walkthrough — install, the one-time Gatekeeper / SmartScreen bypass, permissions, your first recording, and meeting capture. macOS permission troubleshooting (including the "was granted — now revoked" prompt after updates) lives in [`docs/macos-permissions.md`](./docs/macos-permissions.md).

Hush does not auto-update — check manually via **Settings → About → Check for updates** (or the **Hush** menu on macOS). Code-signing would remove the warnings; it's sponsorable at [github.com/sponsors/khawkins98](https://github.com/sponsors/khawkins98).

---

## Platform support

**macOS 26** is the primary target — I daily-drive it, and it's the only hands-on-tested platform. **Linux and Windows** build and test in CI but aren't hands-on tested (meeting mode is mic-only there; on Wayland, push-to-talk degrades to the toggle hotkey). Hands-on reports and platform fixes are very welcome — [open an issue](https://github.com/khawkins98/Hush/issues/new) with repro steps and your version, or send a PR.

---

## Engineering

For people who want to know how it's built before trusting it:

- **A trait seam at every OS boundary** — `AudioCapture`, `Transcribe`, `Diarize`, `HistoryRepository`, and friends are traits with hand-rolled mocks, so the IPC tests run with no real audio device, SQLite, or Tauri runtime.
- **Event-driven meeting auto-detection** — CoreAudio HAL listeners catch a mic going live, classify the frontmost app, and start or stop a session. No foreground polling.
- **An append-only decision log** — [`learnings.md`](./learnings.md) is dated and captures the *why* behind every non-obvious call, including the parts that got reverted.

This isn't Electron with a mic icon: four native windows, each with its own capability file; a native menu bar; tray-icon templating that adapts to light/dark menu bars; and traffic-light permission health that tells "stale" from "revoked" in the macOS TCC store. Full stack, window topology, trait seams, and module map are in [`ARCHITECTURE.md`](./ARCHITECTURE.md).

---

## Documentation

| For | Read |
|---|---|
| **Discovering Hush** — what it does | This README + the live app (install it, open Settings → menus describe what each thing does) |
| **Install + first recording** | [docs/getting-started.md](./docs/getting-started.md) — install, Gatekeeper / SmartScreen bypass, permissions, dictation, meetings. macOS TCC troubleshooting: [`docs/macos-permissions.md`](./docs/macos-permissions.md) |
| **What's shipped right now** | [STATUS.md](./STATUS.md) (rolling snapshot), [CHANGELOG.md](./CHANGELOG.md) (release-by-release record) |
| **Attribution + legal posture** | [hush-prd.md](./hush-prd.md) §13.8 — black-box reimplementation discipline and VoiceInk attribution rationale (the rest of that spec is historical) |
| **How it's built** | [ARCHITECTURE.md](./ARCHITECTURE.md) — stack, four-window topology, trait seams, meeting pump, module map |
| **Running it locally / dev commands** | [docs/developing.md](./docs/developing.md) — setup, command reference, macOS quirks, test layers |
| **Building + contributing** | [CONTRIBUTING.md](./CONTRIBUTING.md), [CLAUDE.md](./CLAUDE.md) for the Claude-assisted contributor workflow |
| **Why decisions were made** | [learnings.md](./learnings.md) — append-only engineering decision log |
| **Reporting a vulnerability** | [SECURITY.md](./SECURITY.md) |

---

## Acknowledgements

Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk) by [Pax](https://github.com/Beingpax) — a fantastic macOS-native dictation app that solved the local-whisper-with-good-UX problem first. Hush takes the same product concept, adds meeting capture as a peer feature, and ships cross-platform.

**No VoiceInk source code was copied or referenced** at any point during development. Design was derived from VoiceInk's public README and observable runtime behaviour — see [`hush-prd.md`](./hush-prd.md) §13.8 for the full reasoning. If you like Hush, [VoiceInk](https://github.com/Beingpax/VoiceInk) deserves a look too.

---

## License

Apache 2.0 — see [`LICENSE`](./LICENSE).
