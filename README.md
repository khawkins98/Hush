<div align="center">

<img src="static/app-icon@2x.png" alt="Hush" width="128" height="128">

# Hush

**Voice-to-text that stays on your machine.**

Dictate anywhere. Transcribe meetings with per-speaker labels. No cloud, no telemetry, no audio leaves your device.

[Download](https://github.com/khawkins98/Hush/releases) · [What's shipped](./STATUS.md) · [Privacy](#privacy) · [Contribute](./CONTRIBUTING.md)

</div>

---

## Why Hush

- **🤫 100% local.** Transcription runs on your machine via [whisper.cpp](https://github.com/ggerganov/whisper.cpp). Audio is never uploaded — not for transcription, not for analytics, not for anything. The only network traffic is the one-time Whisper model download.
- **📞 Meeting Mode that respects the room.** Capture mic + macOS system audio in parallel; transcripts label You vs Remote, or "Speaker 1, 2, …" when you turn diarization on. Sessions are searchable, copyable, and stay on disk where you can delete them.
- **🎙️ Dictation as a real OS citizen.** Global push-to-talk you configure (default `Right ⌘`), a recording HUD that doesn't get in the way, transcripts on the clipboard ready to paste. No browser tab. No web service.
- **🔓 Open source and honest about scope.** macOS is the daily-driven primary target. Linux and Windows compile and ship in CI but the maintainer doesn't run them; bug reports there are best-effort. See the [platform table](#platform-support) below.

> **Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk).** No source code was copied or referenced. See [Acknowledgements](#acknowledgements).

---

## Install

Pre-built binaries: **[GitHub Releases](https://github.com/khawkins98/Hush/releases)** → pick the latest `v*` tag.

| Platform | File | Notes |
|---|---|---|
| **macOS** | `.dmg` | Apple Silicon only; macOS 26 is the supported target |
| **Linux** | `.AppImage` or `.deb` | Any distro / Debian + Ubuntu |
| **Windows** | `.msi` (recommended) or `.exe` | |

The early releases are unsigned. macOS shows a Gatekeeper warning on first launch — right-click `Hush.app` → **Open** the first time, subsequent launches are silent. Windows shows a SmartScreen warning — click **More info** → **Run anyway**. Code-signing (Apple Developer ID + EV cert on Windows) is on the roadmap.

Hush does **not** check for updates automatically. To check manually: **Settings → About → Check for updates**, or on macOS the **Hush** menu. The check makes one read-only request to the GitHub releases API; you download and install the same way you did the first time.

---

## Privacy

Hush's privacy posture is the differentiator, so it's spelled out:

- **No audio leaves the device.** whisper.cpp runs locally; there is no cloud round-trip.
- **No telemetry, no analytics, no startup beacon.** Auto-update is not enabled by default. Hush does not phone home unprompted.
- **Two outbound network surfaces, both user-initiated:**
  - **Whisper / speaker model downloads** from Hugging Face when you click Download in the model picker (or enable Speakers in Settings → Meeting). HTTPS-only, host-restricted to `huggingface.co` / `*.hf.co` (one signed-CDN hop allowed for HF's storage backend), hop-cap 4, SHA-256 verified on every download. Once cached, transcription is fully offline.
  - **Manual update check** when you click "Check for updates". Single read-only request to `api.github.com`; no identifying headers beyond the default user agent.

If telemetry or auto-update ever ships, it will be opt-in with a separate privacy review.

---

## Platform support

| Platform | Status | Hands-on tested |
|---|---|---|
| **macOS 26** | Primary target. Daily-driven by the maintainer (macOS ≤ 15 may work, but I've not tested). | ✅ Yes |
| **Linux (X11)** | Theoretically supported; CI builds + tests on `ubuntu-latest`. | ⚠️ Not hands-on tested |
| **Linux (Wayland)** | Toggle hotkey works through the desktop portal; PTT degrades gracefully (rdev requires X11). | ⚠️ Not hands-on tested |
| **Windows** | Built and published in the release pipeline. | ⚠️ Not hands-on tested |

**Linux and Windows hands-on contributions are very welcome.** If you run Hush on either and something is broken, [open an issue](https://github.com/khawkins98/Hush/issues/new) with steps to reproduce + your platform version. PRs that fix platform-specific gaps are exactly the right contribution shape.

---

## Documentation

| For | Read |
|---|---|
| **Discovering Hush** — what it does | This README + the live app (install it, open Settings → menus describe what each thing does) |
| **What's shipped right now** | [STATUS.md](./STATUS.md) (rolling snapshot), [CHANGELOG.md](./CHANGELOG.md) (release-by-release record) |
| **What it's meant to be** | [hush-prd.md](./hush-prd.md) — product spec, scope, non-goals, milestone plan |
| **How it's built** | [ARCHITECTURE.md](./ARCHITECTURE.md) — stack, three-window topology, trait seams, meeting pump, module map |
| **Installing + using** | [Releases](https://github.com/khawkins98/Hush/releases), [`docs/macos-permissions.md`](./docs/macos-permissions.md) for macOS TCC troubleshooting |
| **Building + contributing** | [CONTRIBUTING.md](./CONTRIBUTING.md), [CLAUDE.md](./CLAUDE.md) for the Claude-assisted contributor workflow |
| **Why decisions were made** | [learnings.md](./learnings.md) — append-only engineering decision log |
| **Reporting a vulnerability** | [SECURITY.md](./SECURITY.md) |

---

## Acknowledgements

Hush is inspired by [VoiceInk](https://github.com/Beingpax/VoiceInk) by [Pax](https://github.com/Beingpax), a fantastic macOS-native dictation app. Hush reimplements the same product concept for cross-platform use. **No VoiceInk source code was copied or referenced** at any point during development. Design was derived from VoiceInk's public README and observable runtime behaviour. See [`hush-prd.md`](./hush-prd.md) §13.8 for the full reasoning.

---

## License

Apache 2.0 — see [`LICENSE`](./LICENSE).
