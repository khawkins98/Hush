// Personal Dictionary module — vocabulary hints and find/replace pipeline.
//
// Concept inspired by VoiceInk's Personal Dictionary feature.
// Reimplemented from observed public behaviour; no source code referenced. See §13.8 of the PRD.
//
// Responsibilities:
//   - Maintain `dictionary_terms` (words biased into the Whisper initial_prompt).
//   - Maintain `replacements` (literal find/replace pairs applied post-transcription).
//   - Expose CRUD commands to the frontend settings panel.
//   - Apply replacements in order after transcription completes.

// TODO(#6): implement dictionary CRUD and the post-transcription replacement pipeline
