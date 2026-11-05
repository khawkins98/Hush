# Privacy and network behavior

[Back to Hush](../README.md). This page records the privacy behavior and source locations behind the README summary. For vulnerability reports, use [SECURITY.md](../SECURITY.md).


Privacy is the point of Hush, so it's built to be verified rather than promised.

- **Model downloads and manual update checks make outbound requests.** Both are user-initiated. From the repository root, inspect `src-tauri/src/transcription/download.rs` and `src-tauri/src/updater/mod.rs`:
  - **Whisper / speaker model downloads** when you click Download in the model picker. HTTPS-only, host-pinned to `huggingface.co` / `*.hf.co` (one signed-CDN hop allowed for HF's storage backend), redirect-cap of 4, **SHA-256 verified on every download**. Once the model is cached, transcription is fully offline.
  - **Manual update check** when you click "Check for updates". Single read-only request to `api.github.com/repos/khawkins98/Hush/releases/latest`. The request identifies Hush and its version in the User-Agent and includes GitHub API Accept/version headers; it does not send a transcript.
- **No telemetry. No analytics. No crash reporter. No startup beacon.** Auto-update is not enabled by default.
- **Audio never lands on disk.** Mic + system-audio capture goes into [`audio_buffer::AudioRollingBuffer`](../src-tauri/src/meeting/audio_buffer.rs) — a 30-second RAM ring. Nothing is written to a temp file, no WAV is staged. The transcript is the only persisted artefact.

If telemetry or auto-update ever ships, it'll be opt-in with a separate privacy review.
