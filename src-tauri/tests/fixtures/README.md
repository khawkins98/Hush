# Test fixtures

`jfk.wav` is committed: a ~344 KB public-domain JFK "ask not what your
country can do for you" clip (16 kHz mono PCM, lifted from
whisper.cpp's `samples/jfk.wav`). It backs the default audio path
for `tests/audio_fixture.rs`, `tests/streaming_fixture.rs`, and
`tests/meeting_fixture.rs`. Whisper-model files are still
out-of-repo (75 MB+ per model is too large to commit comfortably);
contributors point `HUSH_TEST_MODEL` at one they've downloaded
locally.

## Integration tests

There are three `#[ignore]`d integration tests that use this fixture:

| Test file | What it exercises | Feature flags |
|---|---|---|
| `audio_fixture.rs` | One-shot `WhisperTranscription::transcribe` | `whisper` |
| `streaming_fixture.rs` | `WhisperStreamingSession` feed + drain loop | `whisper` |
| `meeting_fixture.rs` | Full `SessionManager` → pump → DB path via `WavFileAudioCapture` seam | `whisper,test-utils` |

### How to run

| Env var | Points at | Notes |
|---|---|---|
| `HUSH_TEST_AUDIO` *(optional)* | a WAV file with known canonical text | defaults to the bundled `jfk.wav` |
| `HUSH_TEST_MODEL` | a GGUF Whisper model | e.g. `ggml-base.bin` from Hugging Face |
| `HUSH_TEST_EXPECTED_WORDS` *(optional)* | comma-separated words the transcript must contain | lower-cased before comparison; defaults to `"ask, country"` for audio/streaming and `"country"` for meeting fixture |

```bash
# audio_fixture — one-shot transcription path
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
cargo test --features whisper --test audio_fixture -- --ignored

# streaming_fixture — streaming transcription path
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
cargo test --features whisper --test streaming_fixture -- --ignored --nocapture

# meeting_fixture — full SessionManager + pump + AudioCapture seam
# Requires the `test-utils` feature for WavFileAudioCapture.
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
cargo test --features whisper,test-utils --test meeting_fixture -- --ignored --nocapture
```

## `test-utils` feature and `WavFileAudioCapture`

`tests/meeting_fixture.rs` is the first test to use the
`AudioCapture` seam boundary rather than calling transcription
functions directly. It does so via
[`WavFileAudioCapture`](../src/audio/file_source.rs) — a
file-backed `AudioCapture` / `AudioSession` impl that serves
pre-loaded WAV samples to the meeting pump in 500 ms chunks, matching
the `PUMP_TICK` cadence the production path uses.

`WavFileAudioCapture` lives in `src/audio/file_source.rs` and is
compiled only when the `test-utils` Cargo feature is enabled. This
keeps it out of production binaries while making it accessible from
integration tests in `tests/`.

## Why the model is still not committed

GGUF Whisper models are 75 MB (tiny) to 3 GB (large-v3). Even with
LFS, the smallest model is too big to ship in the repo as a test
asset. The env-var approach for `HUSH_TEST_MODEL` keeps that out of
git while letting the test scaffold ship.

## Who runs this and when

- **Locally** — when touching the audio capture format conversion,
  the resampler, the meeting pump drain path, or the whisper-rs glue.
  Catches "I broke the pipeline somewhere along the way"
  regressions in one shot.
- **In CI** — not yet. The tests are `#[ignore]`d and CI doesn't
  have a model. We could enable them on the macOS runner by caching a
  small model artifact between runs; deferred until the value is clearer.

## Other fixtures (BYO)

If you want to point `HUSH_TEST_AUDIO` at something other than the bundled clip:

- **LibriVox snippets** — public-domain audiobook recordings. Pick any clip with a known
  canonical text. Set `HUSH_TEST_EXPECTED_WORDS` to a few words from the clip.
- **Mozilla Common Voice (CC-0)** — short clips with shipped transcripts. Convenient because
  the transcript travels with the audio.

The tests resample and downmix whatever they get, so any sample
rate / channel count / PCM-int-or-float WAV works.

