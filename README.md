<div align="center">

<img src="static/app-icon@2x.png" alt="Hush" width="128" height="128">

# Hush

**Local voice-to-text and meeting transcription.**
macOS, Linux, and Windows. Free. Open source. No account. No cloud.

Dictate anywhere with a hotkey. Capture meetings with mic + system audio in parallel (macOS). Label You vs Remote.

[Download](https://github.com/khawkins98/Hush/releases) · [Privacy](#privacy-grep-the-source) · [Engineering](#engineering) · [Contribute](./CONTRIBUTING.md)

</div>

---

## What it is

- **A push-to-talk dictation tool.** Hold your hotkey (default `Right ⌘` on macOS, `Right Ctrl` on Linux + Windows), speak, release. The transcript is on your clipboard, ready to paste, before you've moved your hands. No browser tab, no web service, no upload.
- **A meeting transcription tool.** Click Record while you're in a call — or let Hush start automatically when a supported meeting app activates your mic (on by default; toggle in Settings → Meeting → Auto-start). Current macOS auto-detection covers Zoom, Teams, Google Meet, Discord, Slack, Webex, FaceTime, Skype, GoToMeeting, BlueJeans, Loom, Tuple, and Around. Auto-started sessions stop automatically when the meeting app releases the mic. Hush captures your mic and the call's system audio in parallel, runs both through whisper.cpp locally, and gives you a searchable transcript with **You / Remote** labels — or **Speaker 1, Speaker 2…** if you turn on the (optional, local) wespeaker diarisation model. *(Parallel system-audio capture currently ships on macOS via a CoreAudio process tap — no Screen Recording permission required. Auto-detection is macOS-only via the CoreAudio HAL. Linux + Windows system-audio support is tracked in [#106](https://github.com/khawkins98/Hush/issues/106) / [#107](https://github.com/khawkins98/Hush/issues/107) — meeting mode runs mic-only there for now.)*
- **Both, in one app, sharing one history.** Most tools pick one lane. Hush is dictation **and** meetings, with the same model loaded once and the same on-disk history.

The audio never leaves your machine. The audio never lands on disk either — it's processed in RAM (a 30-second rolling ring during meetings; for dictation, drained directly through the transcriber and dropped when the call returns) and is gone as soon as the transcript is on your clipboard.

---

## How it compares

The table splits "Free?" (no-cost tier exists) from "Open source?" (source published under an OSI license) — they're often conflated, and the user's exposure is different. A free tier on a closed-source SaaS is the vendor's choice to revisit; an OSS license is yours to fork.

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

The cross-platform OSS rows are the closest competitors. Within them the niches differ: **Whispering** is dictation-only with a similar Tauri+Svelte stack; **Buzz** does live-mic + post-hoc file imports without a global push-to-talk hotkey; **Meetily** is meeting-only without dictation. **OpenWhispr** is the most direct overlap — local dictation + meetings, same three OSes, MIT-licensed; if Hush isn't to your taste it's worth a look. Note that their distributed builds include a cloud service (fastest transcription, no model download) with a freemium cap — **local processing is always unlimited**, but cloud transcription tops out at 2,000 words/week on the free tier ($8/month for unlimited). All features work fully offline with a downloaded model, no account required.

Hush's wedge: dictation **and** parallel-source meeting capture in one app, sharing one whisper.cpp model load and one searchable history, with a verifiable privacy posture (see below). The closest macOS comparable, [VoiceInk](https://github.com/Beingpax/VoiceInk), is the project that inspired Hush — see [Acknowledgements](#acknowledgements) for the relationship.

---

## Privacy: grep the source

Hush's privacy posture is the differentiator, so it's verifiable rather than promised.

- **Two outbound `reqwest` callers exist in the entire codebase.** Both are user-initiated. Grep `src-tauri/src` for `reqwest::Client` if you don't believe it:
  - **Whisper / speaker model downloads** when you click Download in the model picker. HTTPS-only, host-pinned to `huggingface.co` / `*.hf.co` (one signed-CDN hop allowed for HF's storage backend), redirect-cap of 4, **SHA-256 verified on every download**. Once the model is cached, transcription is fully offline.
  - **Manual update check** when you click "Check for updates". Single read-only request to `api.github.com/repos/khawkins98/Hush/releases/latest`. No identifying headers beyond the default user agent.
- **No telemetry. No analytics. No crash reporter. No startup beacon.** Auto-update is not enabled by default.
- **Audio never lands on disk.** Mic + system-audio capture goes into [`audio_buffer::AudioRollingBuffer`](./src-tauri/src/meeting/audio_buffer.rs) — a 30-second RAM ring. Nothing is written to a temp file, no WAV is staged. The transcript is the only persisted artefact.

If telemetry or auto-update ever ships, it'll be opt-in with a separate privacy review.

---

## For people in regulated work

If you handle audio you can't upload — therapy sessions, legal calls, journalist interviews, qualitative research — Hush is the option that doesn't make you think about compliance:

- The audio never travels off your laptop.
- There's no account, so there's no vendor with your call history.
- Model files are SHA-256 verified before they run.
- Source is Apache 2.0; you can audit, fork, and run your own build.

A workflow page for knowledge workers in privacy-sensitive roles is on the roadmap. In the meantime: install, set your push-to-talk hotkey, and start a meeting from the panel — Hush stays out of the way.

---

## Install

Pre-built binaries: **[GitHub Releases](https://github.com/khawkins98/Hush/releases)** → pick the latest `v*` tag.

| Platform | File | Notes |
|---|---|---|
| **macOS** | `.dmg` | Apple Silicon only; macOS 26 is the supported target |
| **Linux** | `.AppImage` or `.deb` | Any distro / Debian + Ubuntu; CI-built, not hands-on tested |
| **Windows** | `.msi` (recommended) or `.exe` | CI-built, not hands-on tested |

Early releases are not signed with an Apple Developer ID (the certificate programme costs $99/year — this is a solo hobby project and the membership fee doesn't make sense yet). On macOS, right-click `Hush.app` → **Open** on first launch to bypass the Gatekeeper warning; subsequent launches are silent. After an update, one or more permissions (Microphone, Input Monitoring) may show "Was granted — now revoked" in Settings → Permissions — this is a macOS TCC side-effect of ad-hoc signing. Re-grant each one in System Settings or use the Reset button in Settings → Permissions. Full troubleshooting steps are in [`docs/macos-permissions.md`](./docs/macos-permissions.md). Code-signing would eliminate this entirely; if you'd like to sponsor it, GitHub Sponsors is at [github.com/sponsors/khawkins98](https://github.com/sponsors/khawkins98).

Windows shows a SmartScreen warning — click **More info** → **Run anyway**. A Windows EV cert is also on the roadmap.

Hush does **not** check for updates automatically. To check manually: **Settings → About → Check for updates**, or on macOS the **Hush** menu.

---

## Platform support

| Platform | Status | Hands-on tested |
|---|---|---|
| **macOS 26** | Primary target. Daily-driven by the maintainer. | ✅ Yes |
| **Linux (X11)** | Theoretically supported; CI builds + tests on `ubuntu-latest`. | ⚠️ Not hands-on tested |
| **Linux (Wayland)** | Toggle hotkey works through the desktop portal; PTT degrades gracefully (rdev requires X11). | ⚠️ Not hands-on tested |
| **Windows** | Built and published in the release pipeline. | ⚠️ Not hands-on tested |

**Linux and Windows hands-on contributions are very welcome.** If you run Hush on either and something is broken, [open an issue](https://github.com/khawkins98/Hush/issues/new) with steps to reproduce + your platform version. PRs that fix platform-specific gaps are exactly the right contribution shape.

---

## Engineering

For people who care about how it's built before they trust it:

- **Trait-seam pattern at every OS-touching boundary.** `AudioCapture`, `Transcribe`, `Diarize`, `HistoryRepository`, `EventEmitter`, etc. — every one is a trait with a hand-rolled mock, so the IPC layer's tests run without a real audio device, real SQLite, or a real Tauri runtime. See [`ARCHITECTURE.md`](./ARCHITECTURE.md).
- **Event-driven meeting auto-detection (macOS).** Rather than polling the foreground app every 3 s, Hush registers CoreAudio HAL listeners on `kAudioDevicePropertyDeviceIsRunningSomewhere` for all input devices. When any mic activates, it checks the frontmost app against a classifier and starts a session automatically if the user has opted in; when the meeting app releases the mic, the session stops automatically. The full decision matrix — all seven guarded conditions in the state machine — is in [`ARCHITECTURE.md → Detection logic matrix`](./ARCHITECTURE.md#detection-logic-matrix).
- **Four-place IPC sync rule.** A `#[tauri::command]` lives in four places (Rust handler, `generate_handler!`, TS type, Playwright mock). CI lints the mock-completeness mismatch as of #437.
- **Supply-chain pin policy.** `tract-onnx` uses a standard caret pin (`ort` was replaced in #641 to eliminate Metal-routed `IOAccelerator` growth), `rdev` is a git fork pin. Both decisions are documented in [`learnings.md`](./learnings.md). CI's `supply-chain-pins` job blocks new RC pins / git deps that aren't on the explicit allowlist.
- **Decision log.** [`learnings.md`](./learnings.md) is append-only, dated, and captures the *why* behind every non-obvious architectural call — including the parts that didn't work and got reverted.

This isn't Electron-with-a-mic-icon. Four native windows (main, HUD overlay, menu-bar popover, developer debug), each with its own capability file. Settings is inline in the main window — no separate window. Native menu bar with `⌘1/⌘2` section nav. Tray icon as template so dark/light menu bars adapt cleanly. Autostart with Accessory activation policy (no Dock icon for background launches). Traffic-light permission health that distinguishes "stale" from "revoked" for the macOS TCC store.

---

## Documentation

| For | Read |
|---|---|
| **Discovering Hush** — what it does | This README + the live app (install it, open Settings → menus describe what each thing does) |
| **What's shipped right now** | [STATUS.md](./STATUS.md) (rolling snapshot), [CHANGELOG.md](./CHANGELOG.md) (release-by-release record) |
| **Attribution + legal posture** | [hush-prd.md](./hush-prd.md) — black-box reimplementation discipline (§13.8), product non-goals, and VoiceInk attribution rationale. The original full product spec is historical; see STATUS.md and CHANGELOG.md for current state |
| **How it's built** | [ARCHITECTURE.md](./ARCHITECTURE.md) — stack, four-window topology, trait seams, meeting pump, module map |
| **Installing + using** | [Releases](https://github.com/khawkins98/Hush/releases), [`docs/macos-permissions.md`](./docs/macos-permissions.md) for macOS TCC troubleshooting |
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
