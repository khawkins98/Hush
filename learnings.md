# Learnings Log

Engineering decision log for Hush. Append-only, dated entries. Captures dependency choices, platform quirks, false starts, and anything future contributors would benefit from knowing.

---

## 2026-04-25 — Project scaffold and stack decisions

**Tauri 2 + Svelte + TypeScript** chosen as the app framework.
- Svelte was preferred over React for a smaller JS bundle and cleaner reactivity model for the HUD overlay.
- Tauri 2 provides good access to platform APIs (global shortcuts, clipboard, notifications, autostart, updater) as first-party plugins.
- TypeScript over plain JavaScript: catches type errors at compile time, better IDE support.

**whisper-rs** chosen as the transcription backend.
- Direct Rust bindings to whisper.cpp; no FFI shim needed.
- Parakeet/FluidAudio/CoreML explicitly out of scope — see §5 of the PRD.

**cpal** chosen for audio capture.
- Cross-platform (macOS CoreAudio, Windows WASAPI, Linux ALSA/PulseAudio/JACK).
- Alternative considered: `cubeb`. Decision deferred to implementation — see TODO(#1).

**sqlx** chosen for SQLite persistence.
- Compile-time query verification.
- Async-native (tokio runtime).
- Embedded migrations via `sqlx::migrate!()`.

**rdev** chosen for push-to-talk key-down/key-up events.
- `tauri-plugin-global-shortcut` registers shortcuts but does not cleanly expose key-down vs key-up. rdev fills this gap.
- Known limitation: rdev may require Input Monitoring permission on macOS and has reduced reliability under Wayland. Documented in §10 of the PRD.

**active-win-pos-rs** chosen for foreground app detection.
- Provides app name and window title on macOS, Windows, and Linux via a single API.
- URL detection is not available and is deferred to a future release.

---

## 2026-04-25 — Black-box reimplementation discipline recorded

No Hush contributor reads VoiceInk's Swift source. Design is taken from VoiceInk's public README and observable runtime behaviour only. See §13.8 of `hush-prd.md` and `CONTRIBUTING.md`.

---

## 2026-04-25 — Audio capture: capture at native format, defer downmix and resample

The original module sketch said "open the selected device at 16 kHz mono PCM (whisper.cpp's required format)." That is aspirational. Almost no consumer microphone exposes 16 kHz mono natively; CoreAudio, WASAPI, and ALSA all routinely refuse to honour an arbitrary sample-rate request and return `StreamConfigNotSupported`. Negotiating a format at capture time means we either fail to open the stream on common hardware, or we silently fall back to a different format the caller does not know about.

Decision: capture at the device's `default_input_config()` and surface both the f32 PCM samples and the `CaptureFormat` they were captured in. Channel downmix lives in `audio::format` (pure-logic, unit-tested). Sample-rate conversion to 16 kHz will land alongside the transcription work (TODO(#2)) — `whisper-rs` can be evaluated for whether it accepts a native-rate buffer or whether we need `rubato`/equivalent in front of it. Either way, the audio capture layer does not need to know.

This keeps the audio module's contract narrow ("hand back what the device gave us, in a uniform sample type") and pushes format negotiation to the layer that can recover from it without losing the buffer.

---

## 2026-04-25 — `cpal::Stream` is `!Send`: dedicated audio worker thread

`cpal::Stream` is `!Send` on most backends — its backing audio thread keeps thread-locals pointing at the host that constructed it, and moving the stream across threads is undefined behaviour on at least the macOS and Windows backends. That rules out the obvious `Mutex<Option<Stream>>`-on-the-public-struct pattern, because the stream cannot be sent across an `&self` boundary that is itself `Send + Sync`.

Pattern adopted: `CpalAudioCapture` spawns a long-lived worker thread (named `hush-audio`) that owns the stream. Public methods send `Cmd::{Start, Stop, ListDevices, Shutdown}` over an `mpsc` channel and block on a one-shot reply channel. The host is also constructed on the worker thread for the same thread-local-state reason.

The `mpsc::Sender` is wrapped in a `Mutex` because it is `Send` but `!Sync`, and the trait API is `&self`. Lock contention is irrelevant on the control plane (start/stop is human-paced) and the audio callback never touches it. If the control plane ever becomes hot we can move to `crossbeam-channel` (Sync sender) without a public-API change.

A lock-free `is_recording: AtomicBool` lives outside the channel so callers can poll without a round-trip; `Acquire`/`Release` ordering pairs the flag with the worker's session state.
