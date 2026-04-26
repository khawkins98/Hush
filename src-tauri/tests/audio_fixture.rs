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
//! ## Bundled fixture
//!
//! `tests/fixtures/jfk.wav` is committed (~344 KB, public-domain JFK
//! "ask not what your country can do for you" clip — 16 kHz mono PCM
//! lifted from whisper.cpp's `samples/jfk.wav`). Used as the default
//! when `HUSH_TEST_AUDIO` is unset, so contributors with a model on
//! disk can just run `cargo test --features whisper -- --ignored`
//! without staging an audio file. Override with `HUSH_TEST_AUDIO` if
//! you want to point at a different clip.
//!
//! When system-audio capture (#33) lands, the loopback half of #34
//! gets its own integration test that plays the same fixture
//! through the system speakers and captures it via the loopback
//! source — a strict superset of this test.

#![cfg(feature = "whisper")]

use std::path::PathBuf;

use hush_lib::audio::{CaptureFormat, CapturedAudio};
use hush_lib::transcription::{Transcribe, WhisperTranscription};

/// Resolve the path env var, returning `None` with a skip message
/// printed to stderr if the env-var-pointed file does not exist.
/// Tests `unwrap()` the result and skip via early-return at the call
/// site — Rust's test harness has no native skip mechanism, so a soft
/// pass is the only option.
///
/// `default` is the fall-back used when the env var is unset; pass
/// `None` for "no default; unset env var means skip" (the model path,
/// which we deliberately don't commit), or `Some(path)` for "fall
/// back to this bundled fixture" (the audio path).
fn read_path_env(var: &str, default: Option<PathBuf>) -> Option<PathBuf> {
    let candidate = match std::env::var(var) {
        Ok(value) => PathBuf::from(value),
        Err(_) => match default {
            Some(d) => d,
            None => {
                eprintln!("skip: {var} is not set; skipping audio_fixture test");
                return None;
            }
        },
    };

    if candidate.exists() {
        Some(candidate)
    } else {
        eprintln!(
            "skip: {var} → {} does not exist; skipping audio_fixture test",
            candidate.display()
        );
        None
    }
}

/// Path to the bundled JFK clip relative to this test file.
///
/// Resolved at runtime via `CARGO_MANIFEST_DIR` so the test works
/// regardless of where `cargo` is invoked from.
fn bundled_jfk_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("jfk.wav")
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
    let Some(audio_path) = read_path_env("HUSH_TEST_AUDIO", Some(bundled_jfk_path())) else {
        return;
    };
    let Some(model_path) = read_path_env("HUSH_TEST_MODEL", None) else {
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
