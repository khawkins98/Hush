# Getting started with Hush

End-to-end walkthrough: install → first recording → meeting capture.

---

## 1. Install

### Option A — Homebrew (macOS, no security warning)

```bash
brew install --cask --no-quarantine khawkins98/tap/hush
```

The `--no-quarantine` flag means Hush opens straight away — no Gatekeeper dialog. Skip to [step 2](#2-first-launch-and-permissions).

### Option B — Download the DMG (macOS)

1. Go to [github.com/khawkins98/Hush/releases](https://github.com/khawkins98/Hush/releases) and download the latest `.dmg`.
2. Open the DMG and drag **Hush.app** into your Applications folder.
3. macOS will block the first launch with a security warning because Hush is not signed with an Apple Developer ID. To get past it:
   - **Right-click** Hush.app in Applications → **Open** → click **Open** again in the dialog.
   - If that's greyed out (macOS 26 with Gatekeeper fully locked down), open Terminal and run:
     ```bash
     xattr -rd com.apple.quarantine /Applications/Hush.app
     ```
     Then open Hush normally.
4. After the first successful open, all future launches are silent.

The DMG also includes a **Read Me First.txt** with the same instructions if you need them offline.

### Option C — Linux / Windows

Download the `.AppImage` / `.deb` (Linux) or `.msi` (Windows) from [Releases](https://github.com/khawkins98/Hush/releases).

- **Linux:** run `chmod +x Hush_*.AppImage && ./Hush_*.AppImage`, or `sudo dpkg -i hush_*.deb`.
- **Windows:** run the `.msi`; click **More info** → **Run anyway** past the SmartScreen warning.

---

## 2. First launch and permissions

On first launch Hush shows a welcome screen that walks you through the two permissions it needs:

| Permission | What it's for | When it prompts |
|---|---|---|
| **Microphone** | Recording your voice (dictation + meeting audio) | First time you record |
| **Input Monitoring** | Push-to-talk hotkey while another app is in focus | First time PTT is armed |

Grant both when prompted. If you miss a prompt or accidentally deny one, you can re-grant from **Settings → Permissions** inside Hush.

> **No Screen Recording required.** System audio (the remote side of calls) uses a CoreAudio process tap that macOS does not classify as screen recording.

---

## 3. Dictation (push-to-talk)

1. Make sure your cursor is in the text field where you want to paste.
2. Hold **Right ⌘** (macOS default) — Hush's HUD appears while you're holding.
3. Speak. Release the key.
4. The transcript is on your clipboard and pasted at the cursor automatically.

The hotkey is configurable in **Settings → General → Hotkeys**. A toggle hotkey (`⌃⌥H` by default) lets you flip the microphone on/off hands-free instead of holding.

---

## 4. Meeting transcription

Hush can transcribe meetings two ways:

**Auto-start (on by default):** Hush watches for supported apps (Zoom, Teams, Google Meet, Slack, Discord, and others) activating your mic. When one does, recording starts automatically; when the meeting app releases the mic, recording stops. Enable/disable in **Settings → Meeting → Auto-start**.

**Manual start:** Click **Record** in the Hush main window at any time.

Both capture your mic and the call's system audio in parallel and produce a searchable transcript with **You / Remote** labels. If you enable the optional diarisation model (**Settings → Meeting → Speaker identification**), multiple remote participants are labelled separately as **Speaker 1, Speaker 2…**

---

## 5. Browsing history

All transcripts — dictation snippets and full meeting sessions — are saved to **History**. Click the clock icon in the sidebar. Meetings are searchable; dictation entries show the text and timestamp.

---

## 6. Check for updates

Hush does not auto-update. To check: **Settings → About → Check for updates**, or on macOS: **Hush** menu → **Check for Updates…**

---

## Troubleshooting

| Symptom | First thing to try |
|---|---|
| Push-to-talk does nothing | Settings → Permissions — check Input Monitoring is green |
| Microphone prompt never appeared | Settings → Permissions → Reset, then re-enable PTT |
| Meeting auto-start doesn't fire | Settings → Meeting → Auto-start is on; supported app list is in [ARCHITECTURE.md](../ARCHITECTURE.md#app-classification) |
| "Was granted — now revoked" after an update | Settings → Permissions → Reset, or re-grant in System Settings |
| System audio silent in meetings | Settings → Meeting → Audio source — select the correct output device |

Full TCC / permissions troubleshooting: [`docs/macos-permissions.md`](./macos-permissions.md).
