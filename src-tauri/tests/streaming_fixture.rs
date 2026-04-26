//! End-to-end streaming-transcription test against the bundled WAV.
//!
//! Counterpart to `tests/audio_fixture.rs` for the streaming path
//! introduced in #108. Loads the JFK clip, splits it into ~250 ms
//! chunks, and feeds those chunks into a
//! [`WhisperStreamingSession`] to assert that:
//!
//! 1. **Partials appear mid-stream**, not just at the end. The pump's
//!    UX promise — text within ~3 s of speech — depends on this.
//! 2. **Finals concatenate to the expected words.** The streaming
//!    path's transcript should be at least as good as the one-shot
//!    path's; if the sliding-window logic drops words the smoke
//!    test fails loud.
//! 3. **`finish` flushes the in-flight tail** so the last few words
//!    aren't lost on Stop.
//!
//! ## Why `#[ignore]`d by default
//!
//! Same reasoning as `audio_fixture.rs`: needs a model file +
//! `whisper` Cargo feature + `cmake`. CI doesn't have them by
//! default. Run locally with:
//!
//! ```text
//! HUSH_TEST_MODEL=path/to/ggml-base.bin \
//! cargo test --features whisper --test streaming_fixture -- --ignored --nocapture
//! ```
//!
//! `--nocapture` is recommended so the per-tick partial / final log
//! lines surface — they're how you smoke-test the revision behaviour
//! that #108's brief flagged as empirically unknown.

#![cfg(feature = "whisper")]

use std::path::PathBuf;
use std::time::Duration;

use hush_lib::audio::{CaptureFormat, CapturedAudio};
use hush_lib::transcription::{Transcribe, Utterance, WhisperTranscription};

fn read_path_env(var: &str, default: Option<PathBuf>) -> Option<PathBuf> {
    let candidate = match std::env::var(var) {
        Ok(value) => PathBuf::from(value),
        Err(_) => match default {
            Some(d) => d,
            None => {
                eprintln!("skip: {var} is not set; skipping streaming_fixture test");
                return None;
            }
        },
    };
    if candidate.exists() {
        Some(candidate)
    } else {
        eprintln!(
            "skip: {var} → {} does not exist; skipping streaming_fixture test",
            candidate.display()
        );
        None
    }
}

fn bundled_jfk_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("jfk.wav")
}

fn load_wav_as_captured_audio(path: &std::path::Path) -> CapturedAudio {
    let mut reader = hound::WavReader::open(path).expect("open WAV fixture");
    let spec = reader.spec();
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

#[test]
#[ignore] // Requires HUSH_TEST_MODEL; see module doc.
fn streaming_fixture_emits_partials_and_finals() {
    let Some(audio_path) = read_path_env("HUSH_TEST_AUDIO", Some(bundled_jfk_path())) else {
        return;
    };
    let Some(model_path) = read_path_env("HUSH_TEST_MODEL", None) else {
        return;
    };

    let captured = load_wav_as_captured_audio(&audio_path);
    let format = captured.format;
    let total_samples = captured.samples.len();
    let total_ms = ((total_samples as u64) * 1000) / (format.sample_rate as u64).max(1);
    eprintln!(
        "streaming_fixture loaded: {} samples, {} Hz, {} channels, ~{} ms",
        total_samples, format.sample_rate, format.channels, total_ms
    );

    let transcriber = WhisperTranscription::new(&model_path).expect("load whisper model");
    assert!(
        transcriber.supports_streaming(),
        "WhisperTranscription must opt into streaming"
    );

    let mut session = transcriber
        .start_stream(format, "")
        .expect("open streaming session");

    // Chunk the WAV into ~250 ms slices to mimic the meeting pump's
    // drain cadence. Real captures arrive at the underlying device's
    // callback rate (~10 ms typically); 250 ms is the upper-bound
    // pump tick we'll ship in PR3.
    let chunk_samples = (format.sample_rate as usize / 4) * format.channels as usize;
    let mut all_emitted: Vec<Utterance> = Vec::new();
    let mut tick_index = 0;
    for chunk in captured.samples.chunks(chunk_samples) {
        session.feed(chunk).expect("feed");
        let drained = session.drain().expect("drain");
        if !drained.is_empty() {
            eprintln!("tick {tick_index}: drained {} utterances:", drained.len());
            for u in &drained {
                eprintln!(
                    "  {}: [{}-{}ms] {:?}",
                    if u.is_final { "FINAL  " } else { "PARTIAL" },
                    u.started_at_ms,
                    u.ended_at_ms,
                    u.text
                );
            }
            all_emitted.extend(drained);
        }
        tick_index += 1;
        // No actual sleep — the test runs as fast as whisper does.
        // A real pump would await audio between feeds; we substitute
        // a tiny pause so the per-tick log lines are visually
        // separable in --nocapture output.
        std::thread::sleep(Duration::from_millis(1));
    }
    let tail = session.finish().expect("finish");
    if !tail.is_empty() {
        eprintln!("finish: drained {} tail utterances:", tail.len());
        for u in &tail {
            eprintln!(
                "  {}: [{}-{}ms] {:?}",
                if u.is_final { "FINAL  " } else { "PARTIAL" },
                u.started_at_ms,
                u.ended_at_ms,
                u.text
            );
        }
        all_emitted.extend(tail);
    }

    // Per-utterance assertions.
    let partials_seen = all_emitted.iter().filter(|u| !u.is_final).count();
    let finals: Vec<_> = all_emitted.iter().filter(|u| u.is_final).collect();
    eprintln!(
        "streaming_fixture summary: {} partials emitted, {} finals committed",
        partials_seen,
        finals.len()
    );

    // (1) Partials must appear mid-stream — the keystone UX promise.
    assert!(
        partials_seen > 0,
        "expected at least one partial mid-stream; got only finals"
    );

    // (2) Finals concatenate to the expected words.
    let mut joined = String::new();
    for f in &finals {
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(&f.text);
    }
    let lower = joined.to_lowercase();
    eprintln!("concatenated finals: {joined:?}");

    let expected_words = ["ask", "country"];
    for word in expected_words {
        assert!(
            lower.contains(word),
            "expected finals to contain {word:?}; got: {joined:?}"
        );
    }

    // (3) finish flushed the tail. The JFK clip is short (~11 s); with
    // commit_tail_ms = 8 s the very tail of the audio always lives in
    // the tail/partial zone until finish forces it to commit. So the
    // last final's ended_at_ms should be near total_ms — within one
    // commit-tail window of the audio's true end.
    if let Some(last) = finals.last() {
        let total_ms_i = total_ms as i64;
        let last_end_i = last.ended_at_ms as i64;
        let lag = (total_ms_i - last_end_i).abs();
        // Generous tolerance (4 s) — whisper's segment timestamps are
        // ±200 ms typical and the 250 ms feed cadence adds another
        // half-tick of jitter. We're testing that finish flushed at
        // all, not exact alignment.
        assert!(
            lag < 4_000,
            "finish should flush close to end of audio; total_ms={total_ms_i} last_end_ms={last_end_i} lag={lag}"
        );
    } else {
        panic!("expected at least one final utterance, got zero");
    }
}
