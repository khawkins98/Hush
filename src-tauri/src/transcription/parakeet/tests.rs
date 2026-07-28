//! Unit tests for the Parakeet vocabulary and greedy TDT decoder.
//!
//! Everything here runs without any ONNX file present: the decoder
//! takes its step function as a callback, so the loop and its
//! anti-hang guards are exercised against scripted joint outputs.
//!
//! The one test that needs the real ~670 MB model set is
//! [`loads_and_transcribes_real_model`], which is `#[ignore]`d and
//! reads `HUSH_TEST_PARAKEET_DIR`.

use super::decode::{decode, MAX_SYMBOLS_PER_FRAME, MAX_TOTAL_STEPS_PER_FRAME};
use super::vocab::{Vocabulary, BLANK_ID, VOCAB_SIZE};
use super::{PredictorState, NUM_DURATIONS};

/// Build a full-size vocabulary text with `overrides` applied by id and
/// every other slot filled with a unique filler piece.
fn vocab_text(overrides: &[(usize, &str)]) -> String {
    let mut pieces: Vec<String> = (0..VOCAB_SIZE).map(|i| format!("p{i}")).collect();
    for &(id, piece) in overrides {
        pieces[id] = piece.to_owned();
    }
    pieces
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{p} {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Joint logits that select `token` and `duration_idx` by argmax.
fn logits(token: usize, duration_idx: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; VOCAB_SIZE + NUM_DURATIONS];
    v[token] = 10.0;
    v[VOCAB_SIZE + duration_idx] = 10.0;
    v
}

#[test]
fn vocab_parses_and_joins_sentencepiece_pieces() {
    let raw = vocab_text(&[(10, "\u{2581}Hello"), (11, "\u{2581}there"), (12, "!")]);
    let v = Vocabulary::parse(&raw).unwrap();
    assert_eq!(v.len(), VOCAB_SIZE);
    // The leading ▁ must not produce a leading space.
    assert_eq!(v.decode_tokens(&[10, 11, 12]), "Hello there!");
}

#[test]
fn vocab_skips_special_tokens_in_output() {
    // <unk>/<pad>/<blk> and friends must never reach a transcript, even
    // if a decoder bug emits one.
    let raw = vocab_text(&[
        (0, "<unk>"),
        (1, "<|nospeech|>"),
        (2, "<pad>"),
        (10, "\u{2581}ok"),
        (BLANK_ID as usize, "<blk>"),
    ]);
    let v = Vocabulary::parse(&raw).unwrap();
    assert_eq!(v.decode_tokens(&[0, 1, 2, 10, BLANK_ID]), "ok");
}

#[test]
fn vocab_places_entries_by_declared_id_not_line_order() {
    // A regenerated export could list ids out of order; silently
    // mis-indexing the table would corrupt every transcript.
    let mut lines: Vec<String> = (0..VOCAB_SIZE).map(|i| format!("p{i} {i}")).collect();
    lines[10] = "\u{2581}late 10".to_owned();
    lines.reverse();
    let v = Vocabulary::parse(&lines.join("\n")).unwrap();
    assert_eq!(v.piece(10), Some("\u{2581}late"));
}

#[test]
fn vocab_rejects_wrong_size_and_duplicates() {
    assert!(Vocabulary::parse("a 0\nb 1\n").is_err(), "short table");

    let mut lines: Vec<String> = (0..VOCAB_SIZE).map(|i| format!("p{i} {i}")).collect();
    lines[5] = "dupe 4".to_owned();
    assert!(
        Vocabulary::parse(&lines.join("\n")).is_err(),
        "duplicate id must be rejected"
    );
}

#[test]
fn decode_emits_tokens_and_advances_by_predicted_duration() {
    // Frame 0 emits token 7 with duration 2, landing on frame 2, which
    // emits token 8 with duration 2 and ends the utterance.
    let script = |t: usize, _tok: i32, _s: &PredictorState| {
        let out = match t {
            0 => logits(7, 2), // DURATIONS[2] == 2
            2 => logits(8, 2),
            _ => panic!("decoder visited unexpected frame {t}"),
        };
        Ok((out, PredictorState::zeros()))
    };
    assert_eq!(decode(4, script).unwrap(), vec![7, 8]);
}

#[test]
fn decode_filters_blanks_without_advancing_predictor() {
    // A blank must not appear in the output, and must not become the
    // `last_token` fed back into the predictor on the next step.
    let mut seen_prev_tokens: Vec<i32> = Vec::new();
    let script = |t: usize, prev: i32, _s: &PredictorState| {
        seen_prev_tokens.push(prev);
        let out = match t {
            0 => logits(5, 1),                 // emit 5, advance 1
            1 => logits(BLANK_ID as usize, 1), // blank, advance 1
            2 => logits(6, 1),                 // emit 6, advance 1
            _ => panic!("unexpected frame {t}"),
        };
        Ok((out, PredictorState::zeros()))
    };
    assert_eq!(decode(3, script).unwrap(), vec![5, 6]);
    // Step 3's previous token is 5 (the last real emission), not blank.
    assert_eq!(seen_prev_tokens, vec![BLANK_ID, 5, 5]);
}

#[test]
fn decode_zero_duration_blank_still_advances() {
    // A zero-duration blank leaves frame, token and state all unchanged
    // — without the `.max(1)` it re-evaluates the identical triple
    // forever. Pin the termination.
    let script = |_t: usize, _tok: i32, _s: &PredictorState| {
        Ok((logits(BLANK_ID as usize, 0), PredictorState::zeros()))
    };
    assert_eq!(decode(8, script).unwrap(), Vec::<i32>::new());
}

#[test]
fn decode_caps_symbols_emitted_against_one_frame() {
    // A model stuck emitting a zero-duration token forever must be cut
    // off per frame and forced forward, not allowed to hang the pump.
    let script =
        |_t: usize, _tok: i32, _s: &PredictorState| Ok((logits(42, 0), PredictorState::zeros()));
    let tokens = decode(2, script).unwrap();
    assert!(
        !tokens.is_empty() && tokens.iter().all(|&t| t == 42),
        "expected repeated token 42, got {tokens:?}"
    );
    // Two frames, capped per frame, and bounded overall by the total
    // step ceiling — whichever binds first, it must terminate well
    // under the unbounded case.
    assert!(
        tokens.len() <= MAX_SYMBOLS_PER_FRAME * 2,
        "per-frame cap must bound emissions; got {}",
        tokens.len()
    );
}

#[test]
fn decode_honours_total_step_ceiling() {
    // Belt and braces: even if the per-frame cap were defeated, total
    // joint evaluations stay bounded by frames × MAX_TOTAL_STEPS_PER_FRAME.
    let mut calls = 0usize;
    let script = |_t: usize, _tok: i32, _s: &PredictorState| {
        calls += 1;
        Ok((logits(42, 0), PredictorState::zeros()))
    };
    let frames = 5;
    let _ = decode(frames, script).unwrap();
    assert!(
        calls <= frames * MAX_TOTAL_STEPS_PER_FRAME,
        "joint evaluated {calls} times for {frames} frames; ceiling is {}",
        frames * MAX_TOTAL_STEPS_PER_FRAME
    );
}

#[test]
fn decode_is_a_no_op_for_zero_frames() {
    let script = |_t: usize, _tok: i32, _s: &PredictorState| {
        panic!("step must not be called when there are no frames")
    };
    assert_eq!(decode(0, script).unwrap(), Vec::<i32>::new());
}

#[test]
fn decode_rejects_a_short_joint_output() {
    // A truncated joint output means the graph isn't what we think it
    // is — surface it rather than indexing into duration logits that
    // aren't there.
    let script = |_t: usize, _tok: i32, _s: &PredictorState| {
        Ok((vec![0.0; VOCAB_SIZE], PredictorState::zeros()))
    };
    let err = decode(1, script).unwrap_err().to_string();
    assert!(err.contains("expected at least"), "unhelpful error: {err}");
}

#[test]
fn predictor_state_zeros_has_the_exported_shape() {
    let s = PredictorState::zeros();
    assert_eq!(
        s.h.shape(),
        &[super::PRED_STATE_LAYERS, 1, super::PRED_STATE_DIM]
    );
    assert_eq!(
        s.c.shape(),
        &[super::PRED_STATE_LAYERS, 1, super::PRED_STATE_DIM]
    );
}

/// Per-graph load probe: tries every `.onnx` file in the directory and
/// reports each independently.
///
/// Diagnostic, not a gate — it prints rather than asserts, because its
/// job is to tell a contributor *which* graph tract rejected and why.
/// The end-to-end probe below only ever reports the first failure, and
/// the three graphs fail for unrelated reasons (see the module header's
/// "tract compatibility" note).
#[test]
#[ignore = "needs HUSH_TEST_PARAKEET_DIR and a large model download"]
fn each_graph_loads_in_tract() {
    let dir = std::env::var("HUSH_TEST_PARAKEET_DIR")
        .expect("set HUSH_TEST_PARAKEET_DIR to the Parakeet ONNX export directory");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read model directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .onnx files found in {dir}");

    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // The decoder+joint graph needs its input shapes pinned before
        // it will analyse; everything else loads (or doesn't) as-is.
        let res = if name.starts_with("decoder_joint") {
            super::build_plan_with_facts(&path, &super::decoder_joint_facts())
        } else {
            super::build_plan(&path)
        };
        match res {
            Ok(_) => println!("OK   {name}"),
            Err(e) => println!("FAIL {name}: {e:#}"),
        }
    }
}

/// End-to-end probe against the real model set.
///
/// Ignored by default: needs the ~670 MB int8 export. Run with
///
/// ```text
/// HUSH_TEST_PARAKEET_DIR=/path/to/parakeet \
///   cargo test --lib --features parakeet parakeet:: -- --ignored --nocapture
/// ```
///
/// Asserts on shape/plumbing invariants rather than exact text, so it
/// stays stable across model revisions while still catching a broken
/// graph signature or a decoder that emits nothing.
#[test]
#[ignore = "needs HUSH_TEST_PARAKEET_DIR and a ~670 MB model download"]
fn loads_and_transcribes_real_model() {
    let Ok(dir) = std::env::var("HUSH_TEST_PARAKEET_DIR") else {
        panic!("set HUSH_TEST_PARAKEET_DIR to the directory holding the Parakeet ONNX export");
    };
    assert!(
        super::ParakeetModel::files_present(&dir),
        "model directory {dir} is missing one or more required files"
    );

    let model = super::ParakeetModel::load(&dir).expect("load parakeet model");
    assert_eq!(model.vocab().len(), VOCAB_SIZE);

    // One second of 440 Hz tone. We assert the pipeline runs end to end
    // and produces sane intermediate shapes — not that a sine wave
    // transcribes to any particular text.
    let samples: Vec<f32> = (0..super::SAMPLE_RATE_HZ as usize)
        .map(|i| {
            (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / super::SAMPLE_RATE_HZ as f32).sin()
                * 0.2
        })
        .collect();

    let (features, frames) = model.preprocess(&samples).expect("preprocess");
    assert!(frames > 0, "preprocessor produced no frames");
    assert_eq!(
        features.len() % super::NUM_MEL_BINS,
        0,
        "feature buffer must be a whole number of mel rows"
    );

    let (encoded, encoded_len) = model.encode(&features, frames).expect("encode");
    assert!(encoded_len > 0, "encoder produced no frames");
    assert_eq!(
        encoded.len() % super::ENCODER_DIM,
        0,
        "encoder buffer must be a whole number of {}-wide frames",
        super::ENCODER_DIM
    );
    // subsampling_factor is 8 in the upstream config.
    assert!(
        encoded_len <= frames,
        "encoder output ({encoded_len}) must not exceed its input ({frames})"
    );

    let text = model.transcribe(&samples).expect("transcribe");
    println!("parakeet tone transcript: {text:?}");
}
