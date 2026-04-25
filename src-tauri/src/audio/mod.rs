// Audio capture module — cross-platform microphone input via `cpal`.
//
// Concept inspired by VoiceInk's AVFoundation-based audio capture.
// Reimplemented from observed public behaviour; no source code referenced. See §13.8 of the PRD.
//
// Responsibilities:
//   - Enumerate available input devices.
//   - Open the selected device at 16 kHz mono PCM (whisper.cpp's required format).
//   - Fill a ring buffer while the hotkey is held / toggle is active.
//   - Flush the buffer on stop and hand off samples to the transcription layer.

// TODO(#1): implement device enumeration and capture stream using `cpal`
