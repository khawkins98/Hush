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
        .start_stream(
            format,
            "",
            // #974: Task 2 wired the VAD gate parameter through; the
            // fixture runs against the no-op session because this test
            // exercises the whisper streaming policy, not the gate.
            Box::new(hush_lib::vad::NoopVadSession),
        )
        .expect("open streaming session");

    // Chunk the WAV into ~250 ms slices to mimic the meeting pump's
    // drain cadence. Real captures arrive at the underlying device's
    // callback rate (~10 ms typically); 250 ms is the upper-bound
    // pump tick we'll ship in PR3.
    let chunk_samples = (format.sample_rate as usize / 4) * format.channels as usize;
    let mut all_emitted: Vec<Utterance> = Vec::new();
    for (tick_index, chunk) in captured.samples.chunks(chunk_samples).enumerate() {
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

/// Build a 16 kHz mono "meeting-like" signal from the JFK clip: leading
/// silence, speech, a stretch of low-level noise (the condition Whisper
/// hallucinates on), speech again, trailing silence. Deterministic
/// pseudo-random noise so runs are comparable.
fn gappy_meeting_signal(jfk: &CapturedAudio) -> Vec<f32> {
    assert_eq!(jfk.format.channels, 1, "jfk fixture is mono");
    let speech = hush_lib::transcription::resample::resample_to_mono(
        &jfk.samples,
        jfk.format.sample_rate,
        16_000,
    );
    let mut seed: u32 = 0x1234_5678;
    let mut noise = |n: usize, amp: f32| -> Vec<f32> {
        (0..n)
            .map(|_| {
                // xorshift32 → uniform [-1, 1).
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    };
    let mut out = Vec::new();
    out.extend(noise(16_000 * 2, 0.0005)); // 2 s near-silence
    out.extend_from_slice(&speech);
    out.extend(noise(16_000 * 5, 0.004)); // 5 s low hiss (~-48 dBFS)
    out.extend_from_slice(&speech);
    out.extend(noise(16_000 * 3, 0.0005)); // 3 s near-silence
    out
}

/// Real-audio A/B harness for the #1013 streaming options. Feeds the
/// gappy signal through a real Silero-gated streaming session in 500 ms
/// ticks (the pump cadence) and prints every final with its emit lag.
/// Run with and without `HUSH_VAD_BOUNDARY=1`,
/// `HUSH_STREAM_LOCAL_AGREEMENT=1`, `HUSH_FINAL_MIN_AVG_LOGPROB=…`,
/// `RUST_LOG=hush=debug` to compare:
///
/// ```text
/// HUSH_TEST_MODEL=… cargo test --features whisper --test streaming_fixture \
///   streaming_fixture_gappy_signal -- --ignored --nocapture
/// ```
#[test]
#[ignore] // Requires HUSH_TEST_MODEL; see module doc.
fn streaming_fixture_gappy_signal() {
    let Some(model_path) = read_path_env("HUSH_TEST_MODEL", None) else {
        return;
    };
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let jfk = load_wav_as_captured_audio(&bundled_jfk_path());
    let signal = gappy_meeting_signal(&jfk);
    let total_ms = signal.len() as u64 * 1000 / 16_000;

    let transcriber = WhisperTranscription::new(&model_path).expect("load whisper model");
    let vad = hush_lib::vad::onnx::SileroVad::load().expect("load bundled Silero VAD");
    use hush_lib::vad::VadModel as _;
    let mut session = transcriber
        .start_stream(
            CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
            "",
            vad.new_session(),
        )
        .expect("open streaming session");

    let started = std::time::Instant::now();
    let chunk = 16_000 / 2;
    let mut finals: Vec<(u64, Utterance)> = Vec::new();
    let mut partials = 0usize;
    for (i, c) in signal.chunks(chunk).enumerate() {
        session.feed(c).expect("feed");
        let fed_ms = ((i + 1) * chunk) as u64 * 1000 / 16_000;
        for u in session.drain().expect("drain") {
            if u.is_final {
                finals.push((fed_ms, u));
            } else {
                partials += 1;
            }
        }
    }
    for u in session.finish().expect("finish") {
        finals.push((total_ms, u));
    }
    let wall = started.elapsed();

    eprintln!(
        "gappy: {} ms of audio, {} finals, {} partials, wall {:?} (env: VAD_BOUNDARY={:?} LOCAL_AGREEMENT={:?} MIN_LOGPROB={:?})",
        total_ms,
        finals.len(),
        partials,
        wall,
        std::env::var("HUSH_VAD_BOUNDARY").ok(),
        std::env::var("HUSH_STREAM_LOCAL_AGREEMENT").ok(),
        std::env::var("HUSH_FINAL_MIN_AVG_LOGPROB").ok(),
    );
    let mut lags = Vec::new();
    for (fed_ms, u) in &finals {
        let lag = fed_ms.saturating_sub(u.ended_at_ms);
        lags.push(lag);
        eprintln!(
            "  FINAL [{:>6}-{:>6}ms] emitted@{:>6}ms lag {:>5}ms {:?}",
            u.started_at_ms, u.ended_at_ms, fed_ms, lag, u.text
        );
    }
    if !lags.is_empty() {
        let mean = lags.iter().sum::<u64>() / lags.len() as u64;
        eprintln!("gappy: mean final lag {mean} ms (finish-flushed finals included)");
    }
    let joined = finals
        .iter()
        .map(|(_, u)| u.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    // Both copies of the speech must survive whatever filters are on.
    assert!(
        joined.matches("country").count() >= 2,
        "expected both JFK passages; got {joined:?}"
    );
}
