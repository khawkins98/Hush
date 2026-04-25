# Test fixtures

This directory is intentionally near-empty. The audio fixture for
`tests/audio_fixture.rs` is **not committed** — see the test's
module doc and the `learnings.md` entry on the audio test fixture
for why.

## How to run the audio fixture test

The integration test needs two paths from the contributor's machine:

| Env var | Points at | Notes |
|---|---|---|
| `HUSH_TEST_AUDIO` | a WAV file with known canonical text | any sample rate / channel count; PCM int or float; the test resamples / downmixes |
| `HUSH_TEST_MODEL` | a GGUF Whisper model | e.g. `ggml-base.bin` from Hugging Face |
| `HUSH_TEST_EXPECTED_WORDS` *(optional)* | comma-separated words the transcript must contain | lower-cased before comparison; defaults to `"ask, country"` (matches the JFK clip below) |

```bash
HUSH_TEST_AUDIO=/path/to/clip.wav \
HUSH_TEST_MODEL=/path/to/ggml-base.bin \
cargo test --features whisper --test audio_fixture -- --ignored
```

## Recommended fixtures

These have known transcripts and licences that allow redistribution.
Pick whichever is convenient; the test doesn't care which one you
use as long as the env vars line up.

- **JFK "ask not what your country can do for you"** — public
  domain speech. ~10 seconds. Default expected words (`ask`,
  `country`) match this clip.
  Source: archive.org's JFK inaugural recording. Slice with
  `ffmpeg -i source.mp3 -t 10 -ar 16000 -ac 1 jfk.wav`.

- **LibriVox snippets** — public-domain audiobook recordings. Pick
  any clip with a known canonical text. Set
  `HUSH_TEST_EXPECTED_WORDS` to a few words from the clip.

- **Mozilla Common Voice (CC-0)** — short clips with shipped
  transcripts. Convenient because the transcript travels with the
  audio.

## Why the fixture isn't committed

A WAV at the size needed for a recognisable transcript is a few
hundred KB to a few MB. Committing one bloats clone size for a
test gated behind `#[ignore]` that most contributors never run.
Bundling via Git LFS would add friction (LFS quota / setup steps)
for a single dev-only file. The env-var approach lets the test
scaffold ship in the repo while the actual bytes stay out of git.

## Who runs this and when

- **Locally** — when touching the audio capture format conversion,
  the resampler, the downmix utility, or the whisper-rs glue.
  Catches "I broke the pipeline somewhere along the way"
  regressions in one shot.
- **In CI** — never, today. The test is `#[ignore]`d and CI
  doesn't have a model. We could enable it on the macOS runner if
  we cached a small model artifact between runs; not yet worth
  the complexity.

When system-audio capture (#33) lands, the `(b)` half of #34 — the
loopback test — gets its own integration test that plays the same
fixture through the system speakers and captures it back,
exercising the audio capture path end-to-end. This file-based test
stays around as the focused "transcription only" subset.
