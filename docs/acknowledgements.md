# Acknowledgements and source discipline

[Back to Hush](../README.md). These credits and source-use boundaries remain part of the project's attribution record.


Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk) by [Pax](https://github.com/Beingpax) — a fantastic macOS-native dictation app that solved the local-whisper-with-good-UX problem first. Hush takes the same product concept, adds meeting capture as a peer feature, and ships cross-platform.

**No VoiceInk source code was copied or referenced** at any point during development. Design was derived from VoiceInk's public README and observable runtime behaviour — see [`hush-prd.md`](../hush-prd.md) §13.8 for the full reasoning. If you like Hush, [VoiceInk](https://github.com/Beingpax/VoiceInk) deserves a look too.

### Detto and GrembleVoice

[Detto](https://github.com/Gremble-io/Detto) by [Jason Craik](https://github.com/Gremble-io) prompted a design review of Hush's transcription pipeline in July 2026. Two ideas came out of it:

- **Channel-based speaker separation.** Detto labels mic audio "You" and system audio "Them" — separating speakers by capture channel rather than by voice embedding. Hush adopted the underlying insight: the capture device is ground truth for the local talker, so feeding it to an embedding matcher can only disagree with what you already know ([#1003](https://github.com/khawkins98/Hush/issues/1003)).
- **On-device LLM refinement**, and Detto's two-phase design for it (emit immediately, refine in the background). Not yet built in Hush; the design is written up in [#1004](https://github.com/khawkins98/Hush/issues/1004).

**No Detto source code was read or used.** Both ideas came from Detto's public README and `ARCHITECTURE.md`, which are unusually generous documents. Detto is [BSL 1.1](https://github.com/Gremble-io/Detto/blob/main/LICENSE) — source-available, not open source, and incompatible with Hush's Apache 2.0 until it converts to MIT on 2030-05-12 — so its code could not have been reused even had we wanted to. The same don't-read-the-source discipline Hush applies to VoiceInk applies here, for the same reason: Detto is a near neighbour, and the distance is worth keeping visible.

[GrembleVoice](https://github.com/Gremble-io/gremble-voice), Detto's speech engine, is a separate package under **Apache 2.0** — the same licence as Hush, and genuinely reusable. It isn't in the comparison table above because it's a Swift library rather than an app. Nothing from it has been ported yet (Hush's backend is Rust), but it's a legitimate reference for anyone working on the ASR or diarization layers.
