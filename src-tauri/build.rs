use std::f32::consts::PI;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

fn main() {
    tauri_build::build();
    synth_cue_files();
    emit_build_timestamp();
}

/// Stamp the binary with the Unix-second time at which `build.rs` last ran.
/// Consumed by the `get_build_info` IPC command via `env!("HUSH_BUILD_TIMESTAMP")`.
///
/// Accuracy note: Cargo only re-runs `build.rs` when a watched file changes
/// (here: `build.rs` itself, via `cargo:rerun-if-changed=build.rs` below).
/// Release and CI builds always start clean, so the stamp is accurate there.
/// Incremental dev builds reuse the previous stamp until `build.rs` is
/// touched — good enough for a "when was this binary built" display.
fn emit_build_timestamp() {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=HUSH_BUILD_TIMESTAMP={secs}");
}

/// Synthesise the two audio-cue WAV files (#446) into OUT_DIR so
/// `audio_cues.rs` can `include_bytes!` them at compile time.
///
/// The previous shape — `NSSound soundNamed:"Tink"` — was macOS-
/// only and depended on Apple's bundled system sounds (which can
/// vary across macOS versions and are technically Apple's
/// property). Synthesising the cues here gives:
///
/// - Cross-platform parity (Linux + Windows get the same audio
///   feedback once the rodio playback path lands).
/// - License clarity: the cues are produced by code in this repo,
///   so they're under the project's own LICENSE — no third-party
///   provenance to track in `resources/sounds/CREDITS.md` or worry
///   about across macOS releases.
/// - Reproducibility: the same input parameters always produce the
///   same bytes; no opaque WAV blob committed to the repo.
///
/// Generated as 16-bit PCM, 44.1 kHz, mono. Both cues are ≤ 400 ms
/// per the issue's character brief.
fn synth_cue_files() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_dir = Path::new(&out_dir);

    // "Recording starts" cue — soft rising arpeggio (A4 → E5)
    // saying "I'm listening". 250 ms total; quick attack so the
    // user gets immediate feedback the moment the mic goes hot.
    let start = synth_chime(&[
        ChimeNote {
            freq_hz: 440.0,
            start_s: 0.00,
            duration_s: 0.20,
            amp: 0.55,
        },
        ChimeNote {
            freq_hz: 659.25,
            start_s: 0.06,
            duration_s: 0.22,
            amp: 0.55,
        },
    ]);
    write_wav_mono_16bit(&out_dir.join("cue-start.wav"), &start);

    // "Transcription complete" cue — warmer descending arpeggio
    // (E5 → A4) saying "done, safe to paste". 320 ms; slightly
    // longer than the start cue because completion deserves a
    // marginally more deliberate sound.
    let done = synth_chime(&[
        ChimeNote {
            freq_hz: 659.25,
            start_s: 0.00,
            duration_s: 0.22,
            amp: 0.55,
        },
        ChimeNote {
            freq_hz: 440.0,
            start_s: 0.07,
            duration_s: 0.28,
            amp: 0.55,
        },
    ]);
    write_wav_mono_16bit(&out_dir.join("cue-done.wav"), &done);
}

const SAMPLE_RATE: u32 = 44_100;

struct ChimeNote {
    freq_hz: f32,
    start_s: f32,
    duration_s: f32,
    amp: f32,
}

/// Synthesise a chime composed of overlapping bell-like notes.
///
/// Each note is a sum of three harmonics (fundamental + two
/// overtones with decreasing amplitude) shaped by an envelope
/// that ramps up over 5 ms (avoids click), holds, then decays
/// exponentially. The result sums all notes' samples and
/// normalises to 0.95 peak to leave a touch of headroom.
fn synth_chime(notes: &[ChimeNote]) -> Vec<f32> {
    // Total duration is the latest end-time across all notes.
    let total_s = notes
        .iter()
        .map(|n| n.start_s + n.duration_s)
        .fold(0.0f32, f32::max);
    let total_samples = (total_s * SAMPLE_RATE as f32).ceil() as usize;
    let mut out = vec![0.0f32; total_samples];

    for note in notes {
        let start_idx = (note.start_s * SAMPLE_RATE as f32) as usize;
        let end_idx =
            (((note.start_s + note.duration_s) * SAMPLE_RATE as f32) as usize).min(total_samples);
        for (offset, sample) in out[start_idx..end_idx].iter_mut().enumerate() {
            let t = offset as f32 / SAMPLE_RATE as f32;
            let env = bell_envelope(t, note.duration_s);
            let core = (2.0 * PI * note.freq_hz * t).sin();
            // Overtones — each fades faster, giving the sound
            // its bell-like quality without sharpness.
            let h2 = (2.0 * PI * note.freq_hz * 2.0 * t).sin() * 0.35 * (-3.0 * t).exp();
            let h3 = (2.0 * PI * note.freq_hz * 3.0 * t).sin() * 0.18 * (-5.0 * t).exp();
            *sample += note.amp * env * (core + h2 + h3);
        }
    }

    // Normalise to 0.95 peak. Leaves headroom so the WAV doesn't
    // clip when played back through a system that adds its own
    // gain or boost.
    let peak = out.iter().fold(0.0f32, |acc, &x| acc.max(x.abs()));
    if peak > 0.0 {
        let scale = 0.95 / peak;
        for s in &mut out {
            *s *= scale;
        }
    }
    out
}

/// Bell-style envelope: 5 ms attack, immediate exponential decay
/// to silence over the note's `duration_s`. The attack avoids the
/// click an instant-onset sine wave would produce; the
/// exponential tail gives the bell its "ringing" character
/// without a hard cutoff at the end.
fn bell_envelope(t: f32, duration_s: f32) -> f32 {
    let attack_s = 0.005;
    let decay_const = 4.0; // higher = faster decay
    let attack = if t < attack_s { t / attack_s } else { 1.0 };
    let release = if t > duration_s {
        0.0
    } else {
        (-decay_const * t / duration_s).exp()
    };
    attack * release
}

/// Write a mono 16-bit PCM WAV file at 44.1 kHz. Inputs are
/// floats in `[-1.0, 1.0]`; values outside that range are
/// clamped before quantising to int16.
fn write_wav_mono_16bit(path: &Path, samples: &[f32]) {
    let file = File::create(path).expect("create cue wav");
    let mut w = BufWriter::new(file);

    let num_samples = samples.len() as u32;
    let byte_rate = SAMPLE_RATE * 2; // mono * 2 bytes per sample
    let data_size = num_samples * 2;
    let total_size = 36 + data_size;

    // RIFF header
    w.write_all(b"RIFF").unwrap();
    w.write_all(&total_size.to_le_bytes()).unwrap();
    w.write_all(b"WAVE").unwrap();

    // fmt chunk (PCM)
    w.write_all(b"fmt ").unwrap();
    w.write_all(&16u32.to_le_bytes()).unwrap(); // chunk size
    w.write_all(&1u16.to_le_bytes()).unwrap(); // format = PCM
    w.write_all(&1u16.to_le_bytes()).unwrap(); // channels = mono
    w.write_all(&SAMPLE_RATE.to_le_bytes()).unwrap();
    w.write_all(&byte_rate.to_le_bytes()).unwrap();
    w.write_all(&2u16.to_le_bytes()).unwrap(); // block align
    w.write_all(&16u16.to_le_bytes()).unwrap(); // bits per sample

    // data chunk
    w.write_all(b"data").unwrap();
    w.write_all(&data_size.to_le_bytes()).unwrap();
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_sample = (clamped * i16::MAX as f32) as i16;
        w.write_all(&i16_sample.to_le_bytes()).unwrap();
    }
    w.flush().unwrap();
}
