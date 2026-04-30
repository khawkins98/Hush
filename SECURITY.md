# Security Policy

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Email the maintainer at the address listed on the GitHub profile with the subject line `[Hush] Security Vulnerability`. Include:

- A description of the issue.
- Steps to reproduce, including OS and Hush version.
- Any proof-of-concept code or output, if available.
- Whether you would like to be credited in the eventual fix announcement.

We will acknowledge receipt within **72 hours** and aim to provide a fix or mitigation within **14 days** for issues we judge as Critical or High severity. Lower-severity issues may roll into a regular release cycle.

---

## Supported versions

Hush is pre-v1 and ships from `main`. Until the first tagged release, **only `main` is supported** — please reproduce against the latest commit before reporting. Once v1 ships, this policy will list which release lines receive security fixes.

---

## Security posture

The following are deliberate design choices, documented here so a reviewer can verify intent matches behaviour:

### Transcription is local

No audio ever leaves the device. Transcription runs entirely in-process via `whisper-rs` (Rust bindings to `whisper.cpp`). There is no cloud transcription path, opt-in or otherwise.

### Network surface is one user-initiated download

The only outbound network traffic is the Whisper model download triggered explicitly when the user clicks **Download** in the model picker. The download client:

- **Host-restricts redirects** to `huggingface.co` and its subdomains. Cross-origin redirects fail closed before any bytes transfer to a foreign host.
- **Caps redirect depth** at 4 hops.
- **Verifies SHA-256** of the downloaded bytes against a value embedded in the static catalog before the file is moved into the models directory. A failed hash deletes the partial file and surfaces an error to the user.
- **Refuses to download** any model whose catalog entry has an empty SHA-256 string. (All shipped catalog entries carry a verified hash; #41 closed 2026-04-26.)

### No telemetry

The Tauri updater plugin is registered but not wired to a release channel. No first-party telemetry, error reporting, or analytics is shipped. If telemetry is ever added it will be opt-in and will undergo a separate privacy review.

### Permissions follow the principle of least privilege

- The recording HUD's window has its own scoped capability (`core:default` only) — it does not have clipboard, notification, or shortcut grants because it does not need them.
- The main window's capability lists exactly the plugins it uses (clipboard, notification, global shortcut).
- `tauri-plugin-shell` is intentionally not registered. The macOS privacy-pane command uses `std::process::Command` directly with hard-coded URLs.
- Push-to-talk uses `rdev`, which on macOS triggers an Input Monitoring prompt. Hush does not poll keys or store key events; the listener emits press/release events to the frontend and exits when the app does.

### Trust boundaries that are documented but not yet hardened

- **Content Security Policy** is currently `null`. Acceptable while the frontend renders only first-party content from local IPC; will be tightened before shipping to non-technical users.
- **Update-channel signing** is not yet wired (#10).

---

## Coordinated disclosure

If you find a vulnerability we acknowledge, we'll work with you on a disclosure timeline. Default is 90 days from acknowledgement to public disclosure, shorter if the issue is actively exploited. Credit is offered unless you prefer to remain anonymous.
