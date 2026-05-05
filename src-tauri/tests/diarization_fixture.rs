//! End-to-end diarization integration test: pump-style pipeline against real
//! voice recordings.
//!
//! This test exercises the full path used in production:
//!
//! ```text
//! WAV samples → AudioRollingBuffer::append
//!             → slice_ms (16 kHz mono)
//!             → OnnxDiarizer::label_utterances (per-session cluster state)
//!             → speaker_label assigned to Utterance
//! ```
//!
//! # Running
//!
//! ```bash
//! HUSH_DIARIZATION_MODEL_PATH=/path/to/voxceleb_resnet34_LM.onnx \
//! HUSH_TEST_SPEAKER1_WAV=/path/to/speaker1.wav \
//! HUSH_TEST_SPEAKER2_WAV=/path/to/speaker2.wav \
//!   cargo test --features diarization-onnx --test diarization_fixture -- --ignored --nocapture
//! ```
//!
//! The wespeaker model is available from:
//! `huggingface-cli download Wespeaker/wespeaker-voxceleb-resnet34-LM voxceleb_resnet34_LM.onnx`
//!
//! Each speaker WAV should be ~5 s of clear speech at any sample rate (WAV
//! PCM, mono or stereo). `AudioRollingBuffer` handles resampling to 16 kHz mono.
//!
//! # Test inventory
//!
//! - [`two_speakers_get_distinct_labels`] — full end-to-end: two WAVs with distinct
//!   voices land in different clusters; a third utterance from speaker 1 stays in
//!   the original cluster (stability test for the 1-NN chaining concern in #316).
//! - [`short_audio_leaves_speaker_label_unchanged`] — utterance whose audio is below
//!   the `MIN_FRAMES_FOR_EMBEDDING` floor is left with `speaker_label = None`.
//!
//! Both tests are `#[ignore]`'d; CI skips them automatically.

#![cfg(feature = "diarization-onnx")]

use hush_lib::audio::CaptureFormat;
use hush_lib::diarization::onnx::OnnxDiarizer;
use hush_lib::diarization::Diarize;
use hush_lib::meeting::audio_buffer::AudioRollingBuffer;
use hush_lib::transcription::Utterance;

/// 16 kHz mono — matches `CANONICAL_FORMAT` in `meeting/pump.rs`. The pump
/// slices audio from `AudioRollingBuffer` (which stores 16 kHz mono) and
/// passes this format when calling `label_utterances`.
const CANONICAL_FORMAT: CaptureFormat = CaptureFormat {
    sample_rate: 16_000,
    channels: 1,
};

/// Load a WAV file as 16 kHz mono `f32` samples via the rolling buffer, then
/// return the full-duration slice. The caller gets canonical-format audio
/// ready for `label_utterances`.
fn load_wav_via_buffer(path: &str) -> (Vec<f32>, u64) {
    let reader = hound::WavReader::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let spec = reader.spec();
    let fmt = CaptureFormat {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    };

    let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
    let samples: Vec<f32> = reader
        .into_samples::<i32>()
        .map(|s| s.expect("decode sample") as f32 / max)
        .collect();

    let mut buf = AudioRollingBuffer::new();
    buf.append(&samples, fmt);

    // Duration in ms based on original sample count and sample rate
    let duration_ms =
        (samples.len() as u64 * 1000) / (spec.sample_rate as u64 * spec.channels as u64);

    let audio = buf.slice_ms(0, duration_ms);
    (audio, duration_ms)
}

fn make_utterance(end_ms: u64) -> Utterance {
    Utterance {
        text: String::new(),
        started_at_ms: 0,
        ended_at_ms: end_ms,
        is_final: true,
        speaker_label: None,
    }
}

/// Two distinct speakers produce distinct `speaker_label` values, and a
/// repeat utterance from speaker 1 maps back to the same cluster (cluster
/// stability — exercises the 1-NN chaining path flagged in #316).
///
/// Requires:
/// - `HUSH_DIARIZATION_MODEL_PATH` — path to the wespeaker ONNX model
/// - `HUSH_TEST_SPEAKER1_WAV` — WAV with speaker 1's voice (~5 s)
/// - `HUSH_TEST_SPEAKER2_WAV` — WAV with speaker 2's voice (~5 s)
#[test]
#[ignore]
fn two_speakers_get_distinct_labels() {
    let model_path = match std::env::var("HUSH_DIARIZATION_MODEL_PATH") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping: set HUSH_DIARIZATION_MODEL_PATH");
            return;
        }
    };
    let sp1_path = match std::env::var("HUSH_TEST_SPEAKER1_WAV") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping: set HUSH_TEST_SPEAKER1_WAV");
            return;
        }
    };
    let sp2_path = match std::env::var("HUSH_TEST_SPEAKER2_WAV") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping: set HUSH_TEST_SPEAKER2_WAV");
            return;
        }
    };

    let diarizer = OnnxDiarizer::new(&model_path).expect("load wespeaker model");

    let (sp1_audio, sp1_ms) = load_wav_via_buffer(&sp1_path);
    let (sp2_audio, sp2_ms) = load_wav_via_buffer(&sp2_path);

    eprintln!(
        "speaker1: {} samples ({sp1_ms} ms), speaker2: {} samples ({sp2_ms} ms)",
        sp1_audio.len(),
        sp2_audio.len()
    );

    // Utterance 1: speaker 1
    let mut u1 = make_utterance(sp1_ms);
    diarizer.label_utterances(
        std::slice::from_mut(&mut u1),
        std::slice::from_ref(&sp1_audio),
        CANONICAL_FORMAT,
    );

    // Utterance 2: speaker 2 — should land in a different cluster
    let mut u2 = make_utterance(sp2_ms);
    diarizer.label_utterances(
        std::slice::from_mut(&mut u2),
        &[sp2_audio],
        CANONICAL_FORMAT,
    );

    eprintln!(
        "u1 label={:?}, u2 label={:?}",
        u1.speaker_label, u2.speaker_label
    );

    assert!(
        u1.speaker_label.is_some(),
        "speaker 1 utterance should have been labelled"
    );
    assert!(
        u2.speaker_label.is_some(),
        "speaker 2 utterance should have been labelled"
    );
    assert_ne!(
        u1.speaker_label, u2.speaker_label,
        "distinct speakers must land in different clusters"
    );

    // Utterance 3: speaker 1 again — cluster stability check (#316)
    let mut u3 = make_utterance(sp1_ms);
    diarizer.label_utterances(
        std::slice::from_mut(&mut u3),
        &[sp1_audio],
        CANONICAL_FORMAT,
    );

    eprintln!("u3 (speaker1 repeat) label={:?}", u3.speaker_label);

    assert_eq!(
        u1.speaker_label, u3.speaker_label,
        "speaker 1 repeat should map back to the same cluster (1-NN stability)"
    );
}

/// Audio shorter than the `MIN_FRAMES_FOR_EMBEDDING` floor (25 mel frames ≈
/// 250 ms) causes `embed` to fail; `label_utterances` must leave
/// `speaker_label` unchanged rather than panicking or assigning a spurious ID.
///
/// Requires `HUSH_DIARIZATION_MODEL_PATH`.
#[test]
#[ignore]
fn short_audio_leaves_speaker_label_unchanged() {
    let model_path = match std::env::var("HUSH_DIARIZATION_MODEL_PATH") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping: set HUSH_DIARIZATION_MODEL_PATH");
            return;
        }
    };

    let diarizer = OnnxDiarizer::new(&model_path).expect("load wespeaker model");

    // 100 ms of silence at 16 kHz — well below the 250 ms floor
    let short_silence = vec![0.0_f32; 1_600];

    let mut u = Utterance {
        text: String::new(),
        started_at_ms: 0,
        ended_at_ms: 100,
        is_final: true,
        speaker_label: None,
    };
    diarizer.label_utterances(
        std::slice::from_mut(&mut u),
        &[short_silence],
        CANONICAL_FORMAT,
    );

    assert_eq!(
        u.speaker_label, None,
        "short audio below MIN_FRAMES floor must leave speaker_label unchanged"
    );
}
