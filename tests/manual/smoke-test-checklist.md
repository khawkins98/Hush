# Manual Smoke Test Checklist

Run this checklist before every tagged release on each supported platform.

## Platforms
- [ ] macOS 14+ (Apple Silicon)
- [ ] macOS 14+ (Intel)
- [ ] Windows 11 x64
- [ ] Ubuntu 24.04

## Steps

### First run
- [ ] App launches without errors
- [ ] Microphone permission prompt appears (macOS) or is not needed (Windows/Linux)
- [ ] Default model (`base`) is selected
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
