//! Optional audio cues for the dictation hot path (#292 / #446).
//!
//! Two transition moments matter most for the PTT workflow where
//! the user's eyes are on the *target app*, not on Hush:
//!
//! - **Recording starts** — mic is hot, safe to speak.
//! - **Transcription complete** — clipboard is ready, safe to paste.
//!
//! Pre-#446 the cues used `NSSound soundNamed:"Tink"/"Glass"` —
//! macOS-only and dependent on Apple-bundled system sounds. #446
//! replaces that with `rodio` playback of two short WAV files
//! synthesised at compile time by [`build.rs::synth_cue_files`].
//! The synthesised cues:
//!
//! - **Cross-platform**: rodio routes through CPAL, so Linux and
//!   Windows users now get the same audio feedback macOS users
//!   already had.
//! - **License-clean**: no Apple-proprietary system sounds, no
//!   third-party CC0 attribution chain to maintain. The cues are
//!   produced by code in this repo, so they're under the project's
//!   own LICENSE.
//! - **Reproducible**: the same input parameters always produce
//!   the same bytes; nothing opaque committed to the repo.
//!
//! Default off; users opt in via Settings → General → Audio cues
//! (mirrored on `AppState::sound_cues_enabled`, an `AtomicBool`).
//! The per-event sub-toggles (#463) gate each cue independently.
//! Output respects system volume + Do Not Disturb because rodio's
//! default sink uses CPAL's default output device, which on macOS
//! goes through CoreAudio (and on Linux/Windows through PulseAudio
//! / WASAPI) — all of which honour OS-level volume + focus modes.

/// Compile-time-embedded WAV bytes. The synthesis happens in
/// `build.rs`; the resulting files land in `OUT_DIR` and we
/// `include_bytes!` from there. This avoids:
///
/// 1. Committing a binary blob to the repo (the reproducible
///    synthesis script is the source of truth).
/// 2. Needing `tauri.conf.json::bundle.resources` since the bytes
///    are baked into the static binary.
pub const CUE_RECORDING_START: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cue-start.wav"));
pub const CUE_TRANSCRIPTION_READY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/cue-done.wav"));

/// Play a cue. No-op when `enabled` is false; the caller passes
/// `state.runtime_flags.sound_cues_enabled.load(Ordering::Relaxed)`
/// (combined with the per-event sub-toggle) so the hot path
/// doesn't have to branch.
///
/// Best-effort: any failure (no default audio device, decoder
/// rejection, sink construction error) is logged at debug and
/// swallowed — a missing cue is a UX papercut, not worth aborting
/// the dictation hot path. Non-blocking: playback runs on a
/// detached thread so the IPC command returns immediately.
pub fn play_if_enabled(enabled: bool, bytes: &'static [u8]) {
    if !enabled {
        return;
    }
    play_bytes(bytes);
}

/// Concurrency gate (#498). When the dictation hotkey is mashed
/// quickly (post-#477's click-record + meeting-pump unification
/// can fire cues on toggle transitions), every press would
/// otherwise spawn a fresh `OutputStream` + `Sink` + thread —
/// each holding the default audio device for ~250–320 ms. CPAL
/// handles the contention on macOS but the Rust thread cost is
/// real, audio output stutters when many sinks share the device,
/// and some Linux ALSA setups click on each open/close cycle.
///
/// The atomic flips to true on play_bytes entry and back to false
/// in the spawned thread's exit path. Subsequent calls observe
/// `true` and short-circuit without spawning. Net effect: one cue
/// at a time, drops are silent (a dropped cue is a UX papercut,
/// not a correctness issue).
static CUE_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Spawn the actual playback. Detached thread because rodio's
/// `OutputStream` must outlive the playback (when it drops, audio
/// cuts off), and we don't want to block the caller. The
/// `Sink::sleep_until_end` call inside the thread keeps the stream
/// alive until the cue finishes; the thread then exits and the
/// stream drops cleanly.
///
/// Debounced (#498) — see [`CUE_IN_FLIGHT`]. A new cue request
/// while one is still playing is dropped silently.
fn play_bytes(bytes: &'static [u8]) {
    use std::sync::atomic::Ordering;
    // `compare_exchange` so the "claim the slot or bail" race is
    // a single atomic op; otherwise two threads racing on
    // load+store could both see false and both spawn.
    if CUE_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        // A cue is already playing. Drop this request silently.
        return;
    }
    std::thread::spawn(move || {
        // Reset the gate on every exit path — early returns
        // (no audio device, sink failure, decoder failure) plus
        // the success path. A panic here would leak the gate
        // forever; that's an acceptable risk because the
        // synthesised WAVs are constant input and the rodio
        // pipeline is well-tested.
        struct GateGuard;
        impl Drop for GateGuard {
            fn drop(&mut self) {
                CUE_IN_FLIGHT.store(false, Ordering::Release);
            }
        }
        let _guard = GateGuard;

        let (_stream, handle) = match rodio::OutputStream::try_default() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!(error = ?e, "audio_cues: no default output stream; skipping");
                return;
            }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = ?e, "audio_cues: sink construction failed; skipping");
                return;
            }
        };
        let cursor = std::io::Cursor::new(bytes);
        let source = match rodio::Decoder::new(cursor) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = ?e, "audio_cues: decoder rejected cue bytes; skipping");
                return;
            }
        };
        sink.append(source);
        sink.sleep_until_end();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin that both compile-time-embedded cue WAVs are valid input
    /// for the rodio decoder we play them through (#501). Catches
    /// regressions in `build.rs::write_wav_mono_16bit` (header byte
    /// order, chunk size math) before someone notices "the cue
    /// stopped playing" through hands-on smoke testing.
    #[test]
    fn embedded_cues_decode_via_rodio() {
        for (name, bytes) in [
            ("start", CUE_RECORDING_START),
            ("done", CUE_TRANSCRIPTION_READY),
        ] {
            let cursor = std::io::Cursor::new(bytes);
            assert!(
                rodio::Decoder::new(cursor).is_ok(),
                "cue {name} failed to decode via rodio"
            );
        }
    }
}
