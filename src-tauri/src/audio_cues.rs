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

/// Spawn the actual playback. Detached thread because rodio's
/// `OutputStream` must outlive the playback (when it drops, audio
/// cuts off), and we don't want to block the caller. The
/// `Sink::sleep_until_end` call inside the thread keeps the stream
/// alive until the cue finishes; the thread then exits and the
/// stream drops cleanly.
fn play_bytes(bytes: &'static [u8]) {
    std::thread::spawn(move || {
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
