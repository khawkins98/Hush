//! Parakeet engine tests.
//!
//! The interesting one needs the real model set and is `#[ignore]`d:
//!
//! ```text
//! HUSH_TEST_PARAKEET_DIR=/path/to/tdt \
//!   cargo test --lib --features parakeet parakeet:: -- --ignored --nocapture
//! ```

/// Per-test scratch directory, unique per process.
///
/// A fixed path under `temp_dir()` collides between concurrent
/// `cargo test` runs and inherits leftovers from a panicked one, which
/// makes these tests flake in ways that look like real failures.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("hush-parakeet-{tag}-{}", std::process::id()))
}

// Note on coverage: `transcribe()` itself — the mutex handling, the
// `to_vec()` copy, and the empty-input short circuit — is exercised
// only by the `#[ignore]`d tests below, because a `ParakeetModel`
// cannot be constructed without real ONNX files on disk. CI therefore
// never runs it. Making that path unit-testable would mean injecting a
// seam behind `ParakeetTDT`, which is worth doing when this engine is
// wired into the pump and not before.

#[test]
fn files_present_rejects_an_incomplete_directory() {
    let dir = unique_temp_dir("files-present");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        !super::ParakeetModel::files_present(&dir),
        "empty directory must not report as present"
    );

    // A partial download is the realistic failure and must not read as
    // ready — encoder but no decoder.
    std::fs::write(dir.join("encoder-model.onnx"), b"x").unwrap();
    std::fs::write(dir.join("vocab.txt"), b"x").unwrap();
    assert!(
        !super::ParakeetModel::files_present(&dir),
        "missing decoder must not report as present"
    );

    std::fs::write(dir.join("decoder_joint-model.onnx"), b"x").unwrap();
    // Still not ready: the fp32 encoder needs its weights sidecar, and
    // `files_present` must agree with `load` about that. Reporting
    // "ready" here and then failing to load is worse than reporting
    // "not ready" — a model picker would offer an entry that can't work.
    assert!(
        !super::ParakeetModel::files_present(&dir),
        "fp32 encoder without its .onnx.data sidecar must not report as present"
    );

    std::fs::write(dir.join("encoder-model.onnx.data"), b"x").unwrap();
    assert!(super::ParakeetModel::files_present(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_present_and_load_agree_about_the_sidecar() {
    // Pins the invariant directly: for any directory, `files_present`
    // must not claim readiness that `load` will then reject. The fp32
    // sidecar is the case where these two drifted apart.
    let dir = unique_temp_dir("agree");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in [
        "encoder-model.onnx",
        "decoder_joint-model.onnx",
        "vocab.txt",
    ] {
        std::fs::write(dir.join(f), b"x").unwrap();
    }
    let present = super::ParakeetModel::files_present(&dir);
    let loads = super::ParakeetModel::load(&dir).is_ok();
    assert!(
        !present && !loads,
        "files_present ({present}) must not disagree with load ({loads})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_present_accepts_the_int8_variant() {
    // int8 is a different file set entirely — ~670 MB vs ~2.5 GB — and
    // the picker must not reject it for lacking the fp32 names.
    let dir = unique_temp_dir("int8");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in [
        "encoder-model.int8.onnx",
        "decoder_joint-model.int8.onnx",
        "vocab.txt",
    ] {
        std::fs::write(dir.join(f), b"x").unwrap();
    }
    assert!(super::ParakeetModel::files_present(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_rejects_fp32_encoder_without_its_weights_sidecar() {
    // fp32 exceeds protobuf's 2 GB limit so its weights live beside it.
    // Copying only the .onnx is an easy mistake with an opaque ORT error.
    let dir = unique_temp_dir("sidecar");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in [
        "encoder-model.onnx",
        "decoder_joint-model.onnx",
        "vocab.txt",
    ] {
        std::fs::write(dir.join(f), b"x").unwrap();
    }
    let err = match super::ParakeetModel::load(&dir) {
        Ok(_) => panic!("load should fail without the weights sidecar"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("encoder-model.onnx.data"),
        "error should name the missing sidecar, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_names_the_missing_file() {
    let dir = unique_temp_dir("load-error");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // `map_err(|e| e)` rather than `unwrap_err()`: ParakeetModel wraps a
    // foreign type that isn't Debug, so the Ok side can't be unwrapped.
    let err = match super::ParakeetModel::load(&dir) {
        Ok(_) => panic!("load should fail on an empty directory"),
        Err(e) => e.to_string(),
    };
    // Match the whole phrase, not just "encoder-model.onnx" — that is a
    // substring of "encoder-model.int8.onnx" too, so a looser assertion
    // would pass on almost any wrong-but-nonempty candidate list.
    assert!(
        err.contains("no encoder model found"),
        "error should say which kind of model is missing, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real transcription against `tests/fixtures/jfk.wav`.
///
/// Prints the transcript and timings rather than asserting on exact
/// text — the point is to see what the model actually produces and how
/// fast, which is a judgement call, not a regression bound. The only
/// hard assertions are that it produced *something* and that a couple
/// of unmistakable words are in it.
#[test]
#[ignore = "needs HUSH_TEST_PARAKEET_DIR and a ~2.5 GB model download"]
fn transcribes_the_jfk_fixture() {
    let dir = std::env::var("HUSH_TEST_PARAKEET_DIR")
        .expect("set HUSH_TEST_PARAKEET_DIR to the Parakeet TDT export directory");

    let load_start = std::time::Instant::now();
    let model = super::ParakeetModel::load(&dir).expect("load parakeet model");
    let load_ms = load_start.elapsed().as_millis();
    println!("load: {load_ms} ms");

    // The fixture is 16 kHz mono PCM already — the format the model
    // wants — so this reads it directly rather than going through the
    // resampler.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jfk.wav");
    let mut reader = hound::WavReader::open(path).expect("open jfk.wav fixture");
    let spec = reader.spec();
    println!(
        "fixture: {} Hz, {} ch, {:?}",
        spec.sample_rate, spec.channels, spec.sample_format
    );
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.expect("read sample") as f32 / i16::MAX as f32)
            .collect(),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("read sample"))
            .collect(),
    };
    let audio_secs = samples.len() as f64 / spec.sample_rate as f64;

    let infer_start = std::time::Instant::now();
    let text = model.transcribe(&samples).expect("transcribe");
    let infer_ms = infer_start.elapsed().as_millis();

    println!("audio:      {audio_secs:.2} s");
    println!("inference:  {infer_ms} ms");
    println!(
        "realtime x: {:.1}",
        (audio_secs * 1000.0) / infer_ms.max(1) as f64
    );
    println!("transcript: {text:?}");

    assert!(!text.trim().is_empty(), "produced no transcript");
    let lower = text.to_lowercase();
    assert!(
        lower.contains("country") && lower.contains("americans"),
        "transcript does not look like the JFK fixture: {text:?}"
    );
}

/// Memory soak — the #641 gate.
///
/// #641 removed ORT from the diarizer because its Apple Silicon
/// prebuilts dispatch through Metal Performance Shaders even on the CPU
/// execution provider, growing IOAccelerator ~1.25 GB/min. This engine
/// reintroduces ORT (tract cannot load the Parakeet graphs at all), so
/// that finding has to be re-measured here rather than assumed stale.
///
/// Runs a long inference loop and prints its own pid so an external
/// sampler can watch it:
///
/// ```text
/// vmmap -summary <pid> | grep -i ioaccelerator
/// footprint <pid>
/// ```
///
/// Deliberately prints rather than asserts. A footprint threshold baked
/// into a test would be a guess; the real acceptance criterion is a
/// hands-on meeting under `npm run memwatch`, per `docs/memory-debugging.md`.
/// This exists to make the cheap version of that check possible before
/// anyone wires the engine into the pump.
#[test]
#[ignore = "memory soak — needs HUSH_TEST_PARAKEET_DIR, runs for minutes"]
fn memory_soak_over_many_inferences() {
    let dir = std::env::var("HUSH_TEST_PARAKEET_DIR")
        .expect("set HUSH_TEST_PARAKEET_DIR to the Parakeet TDT export directory");
    let iterations: usize = std::env::var("HUSH_TEST_PARAKEET_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);

    println!("pid: {}", std::process::id());
    println!("baseline (before load):        {}", sample_memory());
    let model = super::ParakeetModel::load(&dir).expect("load parakeet model");
    println!("after load (weights resident): {}", sample_memory());

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jfk.wav");
    let mut reader = hound::WavReader::open(path).expect("open jfk.wav fixture");
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.expect("read sample") as f32 / i16::MAX as f32)
        .collect();

    let start = std::time::Instant::now();
    for i in 1..=iterations {
        let text = model.transcribe(&samples).expect("transcribe");
        assert!(!text.trim().is_empty(), "iteration {i} produced no text");
        if i % 10 == 0 {
            println!(
                "iter {i:>4}/{iterations}  elapsed {:>6.1}s  {}",
                start.elapsed().as_secs_f64(),
                sample_memory()
            );
        }
    }
    println!("done in {:.1}s", start.elapsed().as_secs_f64());
    println!("final: {}", sample_memory());
}

/// Physical footprint + IOAccelerator total for this process.
///
/// Shells out to `footprint` and `vmmap` rather than using mach APIs
/// directly — same numbers `npm run memwatch` reports, so results here
/// are directly comparable to the figures in `learnings.md`.
///
/// **Footprint, not RSS.** `docs/memory-debugging.md` exists largely
/// because leaks in this codebase have repeatedly hidden in compressed
/// dirty pages that RSS does not count. Both are printed; footprint is
/// the one to read.
#[cfg(target_os = "macos")]
fn sample_memory() -> String {
    let pid = std::process::id().to_string();

    let rss_kb = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();

    let footprint = std::process::Command::new("footprint")
        .arg(&pid)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.lines()
                .find(|l| l.contains("Footprint:"))
                .and_then(|l| l.split("Footprint:").nth(1))
                .map(|v| v.trim().to_owned())
        })
        .unwrap_or_else(|| "n/a".into());

    // The #641 signal. Absent from the summary entirely == zero Metal
    // dispatch, which is the outcome we want.
    let ioaccel = std::process::Command::new("vmmap")
        .args(["-summary", &pid])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.to_lowercase().contains("ioaccelerator"))
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .unwrap_or_else(|| "IOAccelerator: none".into());

    format!("rss={rss_kb}KB footprint={footprint} | {ioaccel}")
}

#[cfg(not(target_os = "macos"))]
fn sample_memory() -> String {
    "memory sampling is macOS-only".to_owned()
}
