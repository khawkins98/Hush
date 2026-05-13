# Manual Smoke Test Checklist

Run this checklist before every tagged release on each supported platform.

## Platforms
- [ ] macOS 26 (Apple Silicon — primary target; Intel and macOS ≤ 15 out of scope)
- [ ] Windows 11 x64 (best-effort — not hands-on tested by the maintainer)
- [ ] Ubuntu 24.04 (best-effort — not hands-on tested by the maintainer)

## Steps

### First run
- [ ] App launches without errors
- [ ] Microphone permission prompt appears (macOS) or is not needed (Windows/Linux)
- [ ] Default model (`small`) is selected
- [ ] Model download completes and SHA is verified

### Core dictation loop
- [ ] Toggle-record hotkey starts recording (HUD appears)
- [ ] Level meter responds to voice input
- [ ] Toggle-record hotkey stops recording
- [ ] Transcription appears on clipboard within 1.5 s for a ~5 s utterance
- [ ] "Ready to paste" notification appears
- [ ] Clipboard contents match spoken text

### History
- [ ] Transcription appears in history view
- [ ] Full-text search filters results correctly
- [ ] Copy-to-clipboard works from history entry
- [ ] Delete removes entry

### Settings
- [ ] Model picker changes model (requires re-download if not cached)
- [ ] Hotkey rebind persists across restart
- [ ] Launch at login toggle works

### Network
- [ ] No outbound network traffic during normal operation (verify with Little Snitch / Wireshark)

### Permissions (macOS)
- [ ] Fresh install from DMG: Microphone prompt appears on first recording attempt
- [ ] Fresh install from DMG: Input Monitoring prompt appears on first PTT attempt
- [ ] After granting both permissions, PTT activates in the same session (no restart required)
- [ ] Permissions panel shows both Microphone and Input Monitoring as granted (green)

### Meeting auto-detection (macOS)
- [ ] Settings → Meeting → Auto-start mode is "Always" on fresh install
- [ ] Open a supported app (Zoom, Teams, Meet, etc.) and join/start a call — Hush starts a meeting session automatically within ~1 s of mic activation
- [ ] Stopping the call / leaving the meeting ends the session
- [ ] Manual "Start meeting" button works regardless of auto-start mode
- [ ] Setting auto-start to "Off" disables detection (mic activating in meeting app does not start session)
