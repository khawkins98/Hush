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

## 2026-04-25 — Whisper transcription: linear resampler over `rubato`

Whisper.cpp expects 16 kHz mono f32 PCM but consumer microphones almost
universally capture at 44.1 or 48 kHz. The transcription pipeline must
resample. Two viable options for M1:

- `rubato`: production-quality crate offering windowed-sinc, FFT-based, and
  polyphase resamplers. Higher fidelity, but pulls in `realfft`/`rustfft`
  and adds a few hundred KB of compiled code.
- A handwritten linear-interpolation resampler in `transcription::resample`.

Picked the linear resampler. Reasons in priority order:
1. Whisper's first stage is a mel spectrogram with 25 ms windows and 10 ms
   hops; aliasing artifacts above ~4 kHz are smoothed away by the mel
   filterbank long before they reach the encoder. Linear-vs-sinc accuracy
   delta on dictation audio is within measurement noise.
2. Zero additional dependencies on the default-feature build. Contributors
   without cmake can still run the resampler tests; CI without the
   `whisper` feature stays cheap.
3. The public surface is `resample_to_mono(samples, in_rate, out_rate) ->
   Vec<f32>`. If a future quality regression test shows linear is the
   bottleneck, swap the body for `rubato::FftFixedIn` without touching any
   caller.

Not addressed: pre-filter for downsampling. With 48 → 16 kHz, energy in the
8–24 kHz band aliases. For human speech (essentially no useful information
above 8 kHz) this is benign. If we ever target non-speech audio, this
assumption breaks and it is reason enough to swap in `rubato` regardless.

## 2026-04-25 — Whisper model path: caller-provided in M1, auto-download in M3

`WhisperTranscription::new` takes a `PathBuf` rather than auto-downloading
a model. Two reasons:

1. M1 is a transcription spike — we want to confirm the Rust path works
   end-to-end before building model-management infrastructure. Mixing
   "does whisper-rs work?" and "does our download/SHA-verify/caching pipe
   work?" into one milestone hides which side fails when something breaks.
2. The auto-download flow needs UX decisions (default model? download
   progress? failure recovery? disk-quota messaging?) that belong with the
   model picker UI, which lands in M3.

`new` does pre-check `Path::exists()` so the user gets a clean error rather
than whatever whisper.cpp surfaces from its file open path.

## 2026-04-25 — Whisper inference: `Mutex<WhisperContext>`, fresh state per call

`whisper.cpp` is not thread-safe across `whisper_full` calls on the same
context, so we hold a single `Mutex<WhisperContext>`. Dictation is
fundamentally serial (one mic, one user) so the lock is never contended in
practice; the mutex exists to keep the type `Sync` for IPC use, not for
real concurrency.

`whisper-rs` 0.14 separates context from state: the context holds the
model weights, the state holds the decoder KV cache. We create a fresh
state per `transcribe()` call rather than reusing one — this both avoids
cross-utterance attention-state leakage and keeps the per-call code path
simple. Cost is small (state allocation is microseconds against a
multi-second inference).

Thread count is fixed at 4 rather than `num_cpus`-based. Whisper.cpp
scales sub-linearly past ~4 threads on Apple Silicon and modern x86, and
we'd rather not fight the UI thread on small machines. The model picker
(M3) will expose this as a setting.

## 2026-04-25 — `cpal::Stream` is `!Send`: dedicated audio worker thread

`cpal::Stream` is `!Send` on most backends — its backing audio thread keeps thread-locals pointing at the host that constructed it, and moving the stream across threads is undefined behaviour on at least the macOS and Windows backends. That rules out the obvious `Mutex<Option<Stream>>`-on-the-public-struct pattern, because the stream cannot be sent across an `&self` boundary that is itself `Send + Sync`.

Pattern adopted: `CpalAudioCapture` spawns a long-lived worker thread (named `hush-audio`) that owns the stream. Public methods send `Cmd::{Start, Stop, ListDevices, Shutdown}` over an `mpsc` channel and block on a one-shot reply channel. The host is also constructed on the worker thread for the same thread-local-state reason.

The `mpsc::Sender` is wrapped in a `Mutex` because it is `Send` but `!Sync`, and the trait API is `&self`. Lock contention is irrelevant on the control plane (start/stop is human-paced) and the audio callback never touches it. If the control plane ever becomes hot we can move to `crossbeam-channel` (Sync sender) without a public-API change.

A lock-free `is_recording: AtomicBool` lives outside the channel so callers can poll without a round-trip; `Acquire`/`Release` ordering pairs the flag with the worker's session state.
