//! Re-test of the #641 finding against ONNX Runtime rc.13.
//!
//! Test-only, `#[ignore]`d, and gated on the `parakeet` feature because
//! that is the only configuration where `ort` is in the tree at all.
//!
//! ## Why this exists
//!
//! #641 removed `ort` from the diarizer after measuring ~1.25 GB/min of
//! IOAccelerator growth on the wespeaker ResNet34-LM model, and the
//! diarizer moved to `tract-onnx`. #521 then reintroduced `ort` for
//! Parakeet, justified by a soak showing flat footprint.
//!
//! Those two results are not directly comparable, and a code review
//! rightly flagged the gap:
//!
//! | | #641 | #521 soak |
//! |---|---|---|
//! | ORT | rc.12 | rc.13 |
//! | Model | wespeaker ResNet34-LM | Parakeet TDT |
//! | Session lifetime | recreated every 25 embeds | one, process-lifetime |
//!
//! Three variables moved at once, so "ORT no longer leaks" was never
//! established — only "this model, this runtime, this usage is bounded".
//! This probe holds the model fixed at wespeaker and re-runs #641's own
//! workload on rc.13, isolating the runtime and session-lifetime axes.
//!
//! ## Result (2026-07-28, rc.13, 200 iterations)
//!
//! **No growth in either mode.** The leak was rc.12-specific.
//!
//! | | single long-lived session | recreated every 25 |
//! |---|---|---|
//! | Footprint start → end | 69 MB → **68 MB** | 102 MB → 106 MB |
//! | IOAccelerator regions | **7, constant** | **7, constant** |
//! | IOAccelerator virtual | **260.5 MB, constant** | **260.5 MB, constant** |
//!
//! Against #641's 96 regions / 9 GB virtual / ~1.25 GB/min on this exact
//! model, that is the absence of the phenomenon, not an improvement in
//! degree. Full write-up in `learnings.md` 2026-07-28 (latest).
//!
//! Note the mild irony in column two: session *recreation* is the only
//! mode showing any drift at all (~4 MB across 8 rebuilds), so #642's
//! periodic-recreation mitigation is now marginally counterproductive
//! rather than protective.
//!
//! ## How to read a future run
//!
//! - **Growth in both modes** → the leak is back (an ORT regression, or
//!   it was never fixed on this machine's OS/driver combination). The
//!   diarizer's place on tract is vindicated and the Parakeet engine
//!   needs re-justifying.
//! - **No growth in either mode** → as of the run above. GPU execution
//!   providers stay on the table for Parakeet.
//! - **Growth only with session recreation** → the bug is in `Session`
//!   teardown rather than inference, and long-lived sessions like
//!   Parakeet's are safe by construction.
//!
//! ## Running it
//!
//! ```text
//! HUSH_TEST_WESPEAKER=~/Library/Application\ Support/io.github.khawkins98.hush/models/voxceleb_resnet34_LM.onnx \
//!   cargo test --lib --features parakeet ort_641 -- --ignored --nocapture
//! ```
//!
//! `HUSH_ORT_PROBE_RECREATE=N` recreates the session every N inferences
//! (0 = never). Defaults to running both modes.

use ort::session::Session;
use ort::value::Value;

use crate::memprobe::sample_memory;

/// Mel frames per synthetic input. The diarizer feeds one utterance at a
/// time; ~3 s at a 10 ms hop is a representative chunk.
const FRAMES: usize = 300;

/// Mel bins the wespeaker model expects (`feats` is `[B, T, 80]`).
const MEL_BINS: usize = 80;

/// Build a synthetic feature batch. Content is irrelevant to the memory
/// question — only that the graph executes — so this avoids needing real
/// audio or the Mel extractor.
fn synthetic_feats(seed: usize) -> Vec<f32> {
    (0..FRAMES * MEL_BINS)
        .map(|i| (((i + seed) % 97) as f32 / 97.0) - 0.5)
        .collect()
}

fn build_session(path: &str) -> Session {
    Session::builder()
        .expect("ort session builder")
        .commit_from_file(path)
        .expect("load wespeaker model")
}

/// Run `iterations` embeddings, recreating the session every
/// `recreate_every` runs (0 = never), sampling memory as it goes.
fn run_probe(path: &str, iterations: usize, recreate_every: usize) {
    let mode = if recreate_every == 0 {
        "single long-lived session (Parakeet-like)".to_owned()
    } else {
        format!("session recreated every {recreate_every} (#641/#642-like)")
    };
    println!("\n=== mode: {mode} ===");
    println!("start:  {}", sample_memory());

    let mut session = build_session(path);
    for i in 1..=iterations {
        if recreate_every > 0 && i % recreate_every == 0 {
            // Drop before rebuilding so the old session's allocations are
            // released first — the shape #642 shipped.
            drop(session);
            session = build_session(path);
        }
        // Owned `Vec` — `from_array` wants `OwnedTensorArrayData`, and
        // the borrowed `(shape, &[T])` impl is for `from_array_view`.
        let feats = synthetic_feats(i);
        let value =
            Value::from_array(([1usize, FRAMES, MEL_BINS], feats)).expect("build feats tensor");
        let outputs = session
            .run(ort::inputs!["feats" => value])
            .expect("wespeaker inference");
        // Touch the output so the graph isn't optimised away.
        let embs = outputs["embs"]
            .try_extract_array::<f32>()
            .expect("extract embedding");
        assert_eq!(embs.shape()[1], 256, "unexpected embedding width");

        if i % 25 == 0 {
            println!("iter {i:>4}/{iterations}  {}", sample_memory());
        }
    }
    println!("end:    {}", sample_memory());
}

#[test]
#[ignore = "#641 diagnostic — needs HUSH_TEST_WESPEAKER and runs for minutes"]
fn ort_641_wespeaker_memory_probe() {
    let path = std::env::var("HUSH_TEST_WESPEAKER")
        .expect("set HUSH_TEST_WESPEAKER to the voxceleb_resnet34_LM.onnx path");
    let iterations: usize = std::env::var("HUSH_ORT_PROBE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    // NB: the ORT version is pinned in Cargo.toml, not readable here —
    // `CARGO_PKG_VERSION` would report Hush's own version, which is a
    // confusing thing to print next to the word "ort".
    println!("ort probe: wespeaker via ONNX Runtime (see Cargo.toml pin)");
    println!("model: {path}");

    match std::env::var("HUSH_ORT_PROBE_RECREATE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        Some(n) => run_probe(&path, iterations, n),
        None => {
            // Both modes, so the session-lifetime axis is isolated within
            // a single run of the probe.
            run_probe(&path, iterations, 0);
            run_probe(&path, iterations, 25);
        }
    }
}
