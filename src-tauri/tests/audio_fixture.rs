//! End-to-end transcription test against a known-text WAV fixture.
//!
//! Closes the file-based half of #34 (audio test fixture). The
//! system-audio loopback half stays behind #33.
//!
//! ## What this test exercises
//!
//! Loads a WAV file from disk, hands the samples to
//! [`hush_lib::transcription::WhisperTranscription`], and asserts that
//! the resulting transcript contains a few specific words from the
//! file's known canonical text. End-to-end: cpal's
//! [`CapturedAudio`](hush_lib::audio::CapturedAudio) shape, the
//! resampler in `transcription::resample`, the downmix utility in
//! `audio::format`, and whisper-rs inference all participate.
//!
//! ## Why `#[ignore]`d by default
//!
//! Whisper inference needs a model file (75 MB minimum), the
//! `whisper` Cargo feature, and `cmake` on the host. CI doesn't have
//! any of those by default, so the test is gated. Contributors run
//! it locally with:
//!
//! ```text
//! HUSH_TEST_AUDIO=path/to/clip.wav \
//! HUSH_TEST_MODEL=path/to/ggml-base.bin \
//! cargo test --features whisper --test audio_fixture -- --ignored
//! ```
//!
//! ## Why the fixture is not committed
//!
//! No public-domain audio file with a verifiable transcript is small
//! enough to commit comfortably to a git repo (LibriVox excerpts are
//! a few MB minimum; LFS adds friction). The contributor running
//! the test downloads the fixture out-of-band and points
//! `HUSH_TEST_AUDIO` at it. See `tests/fixtures/README.md` for
//! recommended sources.
//!
//! When system-audio capture (#33) lands, the loopback half of #34
//! gets its own integration test that plays the same fixture
//! through the system speakers and captures it via the loopback
//! source — a strict superset of this test.

#![cfg(feature = "whisper")]

use std::path::PathBuf;

use hush_lib::audio::{CaptureFormat, CapturedAudio};
use hush_lib::transcription::{Transcribe, WhisperTranscription};

/// Read the path env var, returning `None` with a helpful skip
/// message printed to stderr if it is unset or points at a missing
/// file. Tests `unwrap()` the result and skip via early-return at
/// the call site — Rust's test harness has no native skip mechanism,
/// so a soft pass is the only option.
fn read_path_env(var: &str) -> Option<PathBuf> {
    match std::env::var(var) {
        Ok(value) => {
            let path = PathBuf::from(&value);
            if path.exists() {
                Some(path)
            } else {
                eprintln!(
                    "skip: {var}={value} but the file does not exist; skipping audio_fixture test"
                );
                None
            }
        }
        Err(_) => {
            eprintln!("skip: {var} is not set; skipping audio_fixture test");
            None
        }
    }
}

/// Load a WAV file into the [`CapturedAudio`] shape the transcription
/// pipeline expects.
///
/// `hound` handles the parsing for every PCM variant whisper.cpp
/// users typically have on disk (16-bit / 24-bit / 32-bit int,
/// 32-bit float). The transcription module's resampler converts
/// from the file's native rate to 16 kHz; downmix from stereo to
/// mono lives in `audio::format`. The test doesn't mind the format
/// of its input — that's the whole point.
fn load_wav_as_captured_audio(path: &std::path::Path) -> CapturedAudio {
    let mut reader = hound::WavReader::open(path).expect("open WAV fixture");
    let spec = reader.spec();

    // Convert to f32 in [-1.0, 1.0]. Hound returns either ints or
    // floats depending on the WAV's sample format; we normalise to
    // the f32 the rest of the pipeline expects.
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.expect("WAV int sample") as f32 / max)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("WAV float sample"))
            .collect(),
    };

    CapturedAudio {
        samples,
        format: CaptureFormat {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        },
    }
}

/// Words the transcript must contain (lower-cased) for the test to
/// pass. Configurable via `HUSH_TEST_EXPECTED_WORDS` (comma-separated)
/// so the contributor can swap fixtures without recompiling. Defaults
/// to a small set that matches the recommended JFK clip in the
/// fixtures README.
fn expected_words() -> Vec<String> {
    std::env::var("HUSH_TEST_EXPECTED_WORDS")
        .ok()
        .map(|csv| {
            csv.split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_else(|| {
            // Default: words from the JFK "ask not what your country" clip.
            // Robust under `base` and larger; conservatively chosen.
            vec!["ask".into(), "country".into()]
        })
}

#[test]
#[ignore] // Requires HUSH_TEST_AUDIO + HUSH_TEST_MODEL; see module doc.
fn fixture_audio_transcribes_to_expected_words() {
    let Some(audio_path) = read_path_env("HUSH_TEST_AUDIO") else {
        return;
    };
    let Some(model_path) = read_path_env("HUSH_TEST_MODEL") else {
        return;
    };

    let captured = load_wav_as_captured_audio(&audio_path);
    let transcriber = WhisperTranscription::new(&model_path).expect("load whisper model");

    let transcript = transcriber
        .transcribe(&captured)
        .expect("transcribe fixture");

    let expected = expected_words();
    let lower = transcript.to_lowercase();

    eprintln!("audio_fixture transcript: {transcript:?}");

    for word in &expected {
        assert!(
            lower.contains(word),
            "expected transcript to contain {word:?}; got: {transcript:?}",
        );
    }
}
