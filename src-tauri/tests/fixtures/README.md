# Test fixtures

`jfk.wav` is committed: a ~344 KB public-domain JFK "ask not what your
country can do for you" clip (16 kHz mono PCM, lifted from
whisper.cpp's `samples/jfk.wav`). It backs the default audio path
for `tests/audio_fixture.rs`. Whisper-model files are still
out-of-repo (75 MB+ per model is too large to commit comfortably);
contributors point `HUSH_TEST_MODEL` at one they've downloaded
locally.

## How to run the audio fixture test

The integration test needs:

| Env var | Points at | Notes |
|---|---|---|
| `HUSH_TEST_AUDIO` *(optional)* | a WAV file with known canonical text | defaults to the bundled `jfk.wav`; override to point at a different clip |
| `HUSH_TEST_MODEL` | a GGUF Whisper model | e.g. `ggml-base.bin` from Hugging Face |
| `HUSH_TEST_EXPECTED_WORDS` *(optional)* | comma-separated words the transcript must contain | lower-cased before comparison; defaults to `"ask, country"` (matches the bundled JFK clip) |

```bash
# Minimal (uses bundled jfk.wav + default expected words):
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
cargo test --features whisper --test audio_fixture -- --ignored

# Override the audio fixture:
HUSH_TEST_AUDIO=/path/to/clip.wav \
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
HUSH_TEST_EXPECTED_WORDS="hello,world" \
cargo test --features whisper --test audio_fixture -- --ignored
```

## Why a bundled fixture and not just env-var pointers

The earlier shape of this directory required both env vars to be set
before the test would run. That meant a contributor with a model on
disk still had to find and stage an audio clip (and remember its
path) before the test was useful. Bundling the JFK clip removes that
friction — once a model is on disk, the test runs with a single env
var. The clip is small enough to fit comfortably in the repo (~344
KB) and its license (public domain) carries no redistribution
constraints.

## Other fixtures (BYO)

If you want to point `HUSH_TEST_AUDIO` at something other than the
bundled clip:

- **LibriVox snippets** — public-domain audiobook recordings. Pick
  any clip with a known canonical text. Set
  `HUSH_TEST_EXPECTED_WORDS` to a few words from the clip.
- **Mozilla Common Voice (CC-0)** — short clips with shipped
  transcripts. Convenient because the transcript travels with the
  audio.

The test resamples and downmixes whatever it gets, so any sample
rate / channel count / PCM-int-or-float WAV works.

## Why the model is still not committed

GGUF Whisper models are 75 MB (tiny) to 3 GB (large-v3). Even with
LFS, the smallest model is too big to ship in the repo as a test
asset. The env-var approach for `HUSH_TEST_MODEL` keeps that out of
git while letting the test scaffold ship.

## Who runs this and when

- **Locally** — when touching the audio capture format conversion,
  the resampler, the downmix utility, or the whisper-rs glue.
  Catches "I broke the pipeline somewhere along the way"
  regressions in one shot.
- **In CI** — not yet. The test is `#[ignore]`d and CI doesn't
  have a model. We could enable it on the macOS runner by caching a
  small model artifact between runs (now that the audio side is
  available); deferred until the value is clearer.

When system-audio capture (#33) lands, the `(b)` half of #34 — the
loopback test — gets its own integration test that plays the same
fixture through the system speakers and captures it back,
exercising the audio capture path end-to-end. This file-based test
stays around as the focused "transcription only" subset.
