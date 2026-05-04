---
name: Bug report
about: Something isn't working
labels: bug
---

<!-- Please read CONTRIBUTING.md before filing — especially the "Issues and labels" section.
     Area / status / priority labels are applied by the maintainer during triage.
     Mention the affected area in your description if it's obvious (audio, UI, release, testing). -->

**Describe the bug**
A clear and concise description of the problem.

**Steps to reproduce**
1. ...

**Expected behaviour**
What you expected to happen.

**Actual behaviour**
What actually happened.

**Environment**
- OS + version (e.g. macOS 14.5, Ubuntu 24.04 + GNOME on X11):
- Hush version or commit SHA:
- Whisper model in use (e.g. ggml-base.bin):
- Audio device:

**macOS only — permission state**
<!-- For PTT issues: System Settings → Privacy & Security → Input Monitoring should list Hush. -->
<!-- For mic issues: same path → Microphone. -->
- Microphone permission granted: yes / no / not sure
- Input Monitoring permission granted (PTT users): yes / no / not sure

**Logs**
<!-- Paste relevant output from the Hush log or the system console.
     For tracing output run with `RUST_LOG=info npm run tauri dev`. -->
