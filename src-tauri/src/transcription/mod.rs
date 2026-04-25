// Transcription module — local Whisper inference via `whisper-rs`.
//
// Concept inspired by VoiceInk's whisper.cpp Swift bridge.
// Reimplemented from observed public behaviour; no source code referenced. See §13.8 of the PRD.
//
// Responsibilities:
//   - Load a quantised GGUF model from the app-data directory.
//   - Accept 16 kHz mono f32 PCM samples and return a transcribed String.
//   - Optionally inject a Personal Dictionary vocabulary hint into the Whisper prompt.
//   - Report progress/state changes via a Tauri event so the HUD can update.
//
// Supported models: tiny, base (default Q5_0), small, medium, large-v3.
// Parakeet / FluidAudio / CoreML is explicitly out of scope. See §5 of the PRD.
//
// Build note: this module is gated behind the `whisper` Cargo feature because whisper-rs
// requires cmake and a C++ toolchain. Enable with `cargo build --features whisper`.

// TODO(#2): implement model loading, inference, and progress reporting
