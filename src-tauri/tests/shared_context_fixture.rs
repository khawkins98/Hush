//! Real-model checks for the shared `WhisperContext` (learnings.md
//! 2026-10-01): the dictation and meeting transcribers are built from ONE
//! model load via `WhisperTranscription::share_context`.
//!
//! - `dictation_and_streaming_concurrently_on_shared_context` — the
//!   concurrency claim: a one-shot dictation transcription and a streaming
//!   (meeting) session run at the same time on one context, each on its own
//!   `WhisperState`, and both produce the expected text.
//! - `memory_shared_context_vs_double_load` — the memory claim: reports
//!   physical footprint AND RSS (docs/memory-debugging.md "iron rule") for
//!   one shared context vs the pre-2026-10 double load, and whether a
//!   context that has served many inferences holds more than a fresh one
//!   (the question behind dropping the #636 meeting-stop rebuild).
//!
//! `#[ignore]`d like the other fixtures — needs a model + `whisper`:
//!
//! ```text
//! HUSH_TEST_MODEL=path/to/ggml-small-q8_0.bin \
//! cargo test --features whisper --test shared_context_fixture -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` matters for the memory test: it measures this
//! process, so a concurrently-running test would pollute the numbers.

#![cfg(feature = "whisper")]

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::time::Instant;

use hush_lib::audio::{CaptureFormat, CapturedAudio};
use hush_lib::transcription::{Transcribe, WhisperTranscription};

fn model_path() -> Option<PathBuf> {
    let Ok(value) = std::env::var("HUSH_TEST_MODEL") else {
        eprintln!("skip: HUSH_TEST_MODEL is not set");
        return None;
    };
    let path = PathBuf::from(value);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skip: HUSH_TEST_MODEL → {} does not exist", path.display());
        None
    }
}

fn jfk() -> CapturedAudio {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("jfk.wav");
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

/// Run one streaming session over the clip the way the meeting pump does
/// (~250 ms feed + drain ticks, then `finish`) and return the joined finals.
fn stream_clip(transcriber: &WhisperTranscription, audio: &CapturedAudio) -> String {
    let mut session = transcriber
        .start_stream(audio.format, "", Box::new(hush_lib::vad::NoopVadSession))
        .expect("open streaming session");
    let chunk = (audio.format.sample_rate as usize / 4) * audio.format.channels as usize;
    let mut finals = Vec::new();
    for c in audio.samples.chunks(chunk) {
        session.feed(c).expect("feed");
        finals.extend(
            session
                .drain()
                .expect("drain")
                .into_iter()
                .filter(|u| u.is_final),
        );
    }
    finals.extend(
        session
            .finish()
            .expect("finish")
            .into_iter()
            .filter(|u| u.is_final),
    );
    finals
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
#[ignore] // Requires HUSH_TEST_MODEL; see module doc.
fn dictation_and_streaming_concurrently_on_shared_context() {
    let Some(model) = model_path() else {
        return;
    };
    let audio = Arc::new(jfk());
    let dictation = Arc::new(WhisperTranscription::new(&model).expect("load whisper model"));
    // The production split: one load, a second transcriber over the same
    // weights with its own inference gate.
    let meeting = Arc::new(dictation.share_context());

    // Solo baselines so the overlap below can be read against them.
    let t = Instant::now();
    let solo_dictation = dictation.transcribe(&audio).expect("solo dictation");
    let solo_dictation_ms = t.elapsed().as_millis();
    eprintln!("solo dictation: {solo_dictation_ms} ms → {solo_dictation:?}");

    let start = Arc::new(Barrier::new(2));
    let t0 = Instant::now();
    let streaming = {
        let (meeting, audio, start) =
            (Arc::clone(&meeting), Arc::clone(&audio), Arc::clone(&start));
        std::thread::spawn(move || {
            start.wait();
            let begin = t0.elapsed().as_millis();
            let text = stream_clip(&meeting, &audio);
            (begin, t0.elapsed().as_millis(), text)
        })
    };
    let dictating = {
        let (dictation, audio, start) = (
            Arc::clone(&dictation),
            Arc::clone(&audio),
            Arc::clone(&start),
        );
        std::thread::spawn(move || {
            start.wait();
            // Dictate repeatedly for as long as one solo dictation takes x3,
            // so several presses land inside the streaming session's run.
            let mut runs = Vec::new();
            for _ in 0..3 {
                let begin = t0.elapsed().as_millis();
                let text = dictation.transcribe(&audio).expect("concurrent dictation");
                runs.push((begin, t0.elapsed().as_millis(), text));
            }
            runs
        })
    };
    let (s_begin, s_end, streamed) = streaming.join().expect("streaming thread");
    let dictations = dictating.join().expect("dictation thread");

    eprintln!("streaming session: {s_begin}–{s_end} ms → {streamed:?}");
    let mut overlapping = 0;
    for (b, e, text) in &dictations {
        let overlaps = *b < s_end && *e > s_begin;
        if overlaps {
            overlapping += 1;
        }
        eprintln!(
            "dictation: {b}–{e} ms ({} ms, solo {solo_dictation_ms} ms, overlaps streaming: {overlaps}) → {text:?}",
            e - b
        );
    }

    for text in std::iter::once(&streamed).chain(dictations.iter().map(|(_, _, t)| t)) {
        let lower = text.to_lowercase();
        assert!(
            lower.contains("country"),
            "expected the JFK clip's words, got {text:?}"
        );
    }
    assert!(
        overlapping > 0,
        "no dictation overlapped the streaming session — the test did not exercise concurrency"
    );
}

/// `(physical footprint MB, RSS MB)` for this process.
fn mem_mb() -> (f64, f64) {
    let pid = std::process::id().to_string();
    let vmmap = std::process::Command::new("vmmap")
        .args(["-summary", &pid])
        .output()
        .expect("run vmmap");
    let text = String::from_utf8_lossy(&vmmap.stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("Physical footprint:"))
        .expect("vmmap prints a Physical footprint line");
    let value = line.split_whitespace().last().expect("footprint value");
    let (num, unit) = value.split_at(value.len() - 1);
    let num: f64 = num.parse().expect("footprint number");
    let footprint = match unit {
        "K" => num / 1024.0,
        "M" => num,
        "G" => num * 1024.0,
        other => panic!("unexpected vmmap unit {other:?}"),
    };
    let ps = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .expect("run ps");
    let rss_kb: f64 = String::from_utf8_lossy(&ps.stdout)
        .trim()
        .parse()
        .expect("ps rss");
    (footprint, rss_kb / 1024.0)
}

fn report(label: &str, base: (f64, f64)) -> (f64, f64) {
    hush_lib::alloc_tuning::force_collect();
    let now = mem_mb();
    eprintln!(
        "{label:<52} footprint {:>7.0} MB ({:>+7.0})   RSS {:>7.0} MB ({:>+7.0})",
        now.0,
        now.0 - base.0,
        now.1,
        now.1 - base.1
    );
    now
}

#[cfg(target_os = "macos")]
#[test]
#[ignore] // Requires HUSH_TEST_MODEL; run with --test-threads=1, see module doc.
fn memory_shared_context_vs_double_load() {
    let Some(model) = model_path() else {
        return;
    };
    // Same allocator setup as the app (purge_delay=0) so freed pages are
    // returned and the numbers reflect what is live, not allocator slack.
    hush_lib::alloc_tuning::init();
    let audio = jfk();
    eprintln!("model: {}", model.display());
    let base = report("baseline (nothing loaded)", (0.0, 0.0));

    // New way: one load, two transcribers over it.
    let dictation = WhisperTranscription::new(&model).expect("load whisper model");
    let meeting = dictation.share_context();
    let shared = report("NEW: 1 load + share_context (both slots)", base);

    // Old way: a second independent load for the meeting slot.
    let second = WhisperTranscription::new(&model).expect("load whisper model");
    let double = report("OLD: 2 independent loads (both slots)", base);
    eprintln!(
        "=> second load costs footprint {:+.0} MB, RSS {:+.0} MB",
        double.0 - shared.0,
        double.1 - shared.1
    );
    drop(second);
    let _ = report("after dropping the second load", base);

    // #636 question: does a context that has served many inferences hold
    // more than a fresh one once its states are gone? Use it hard on both
    // paths, drop every state (sessions end, one-shot states are per call),
    // then compare against a freshly-loaded context.
    for _ in 0..5 {
        dictation.transcribe(&audio).expect("dictation");
    }
    for _ in 0..3 {
        let _ = stream_clip(&meeting, &audio);
    }
    let used = report("after 5 dictations + 3 streaming sessions", base);
    drop(meeting);
    drop(dictation);
    let _ = report("after dropping the used context", base);
    let fresh_dictation = WhisperTranscription::new(&model).expect("load whisper model");
    let _fresh_meeting = fresh_dictation.share_context();
    let fresh = report("fresh context (what a #636 rebuild installs)", base);
    eprintln!(
        "=> used vs fresh context: footprint {:+.0} MB, RSS {:+.0} MB",
        used.0 - fresh.0,
        used.1 - fresh.1
    );
}
