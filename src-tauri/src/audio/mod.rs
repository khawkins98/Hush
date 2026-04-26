//! Cross-platform audio capture for offline dictation.
//!
//! Concept inspired by VoiceInk's AVFoundation-based audio capture.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Enumerate available input devices.
//! - Begin and end a capture session driven by the hotkey layer.
//! - Convert whatever native sample format the device hands us into f32 PCM.
//! - Surface the captured samples plus the format they were captured in.
//!
//! ## Design notes
//!
//! **Capture-at-native-format.** Whisper.cpp expects 16 kHz mono f32 PCM, but
//! consumer microphones almost universally prefer 44.1 or 48 kHz at 1–2
//! channels, and the platform backends (CoreAudio, WASAPI, ALSA/PulseAudio)
//! often refuse to honour an arbitrary sample-rate request. Rather than fight
//! the OS at capture time, this module captures at the device's preferred
//! format and surfaces both the samples and the format. Downmix and
//! resampling happen downstream where we can recover from a poor format
//! match without losing the buffer. See `learnings.md` (2026-04-25) for the
//! full reasoning.
//!
//! **Threading.** `cpal::Stream` is `!Send` on most platforms — its backing
//! audio thread must be polled from the thread that constructed it. We
//! therefore own the stream on a dedicated worker thread and drive it via a
//! command channel. The public API is `Send + Sync` and synchronous from the
//! caller's perspective.
//!
//! **Test seam (PRD §13.5).** Consumers of audio capture depend on the
//! [`AudioCapture`] trait, never on [`CpalAudioCapture`] directly, so unit
//! tests of higher layers can substitute a deterministic mock without
//! pulling in `cpal` or a real device.

mod format;

pub use format::downmix_to_mono;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamError, SupportedStreamConfig};
use serde::{Deserialize, Serialize};

/// Format of a captured audio buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureFormat {
    /// Samples per second, per channel.
    pub sample_rate: u32,
    /// Channel count. Samples in [`CapturedAudio::samples`] are interleaved
    /// in channel order when this is greater than 1.
    pub channels: u16,
}

/// Identifying information about an input device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// Stable identifier used to re-select the device across runs.
    ///
    /// `cpal` does not expose a backend-stable opaque ID, so we identify
    /// devices by name. This is good enough for the common case where users
    /// pick a device once in settings and we re-resolve it on next launch; if
    /// two devices share a name (e.g. two identical USB mics) the user will
    /// have to disambiguate by unplugging one. A future backend-specific
    /// identifier can replace this without changing the public type.
    pub id: String,
    /// Human-readable name shown in the settings UI.
    pub name: String,
    /// True if this device is the host's default input.
    pub is_default: bool,
}

/// Captured audio plus the format it was recorded in.
#[derive(Debug, Clone)]
pub struct CapturedAudio {
    /// Channel-interleaved f32 PCM samples, normalised to `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    /// Format of `samples`. The transcription layer uses this to drive
    /// downmix and (eventually) resampling before handing data to whisper.
    pub format: CaptureFormat,
}

/// Trait at the OS boundary. Higher layers (IPC, transcription pipeline)
/// depend only on this trait so OS-touching code can be mocked at the seam.
pub trait AudioCapture: Send + Sync {
    /// Enumerate input devices known to the host.
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>>;

    /// Begin capturing from `device_id`, or the system default if `None`.
    ///
    /// Returns immediately; samples accumulate on a background thread until
    /// [`AudioCapture::stop`] is called. Returns an error if a recording is
    /// already in progress, the named device cannot be found, or the host
    /// refuses to open an input stream on it.
    fn start(&self, device_id: Option<&str>) -> Result<()>;

    /// Stop capturing and return the accumulated samples.
    ///
    /// Returns an error if no recording is in progress.
    fn stop(&self) -> Result<CapturedAudio>;

    /// True if a recording is currently in progress.
    fn is_recording(&self) -> bool;

    /// Latest RMS level computed by the most recent capture callback,
    /// roughly in `[0.0, 1.0]`. Drives the HUD level meter (#21).
    /// Default returns `0.0` — non-cpal backends and test mocks
    /// inherit a no-op level so the HUD's bar simply stays at idle
    /// for them. Implementations that *do* compute a level should
    /// return `0.0` while not recording so the meter idles cleanly
    /// across start/stop cycles.
    fn current_level(&self) -> f32 {
        0.0
    }
}

// -- cpal backend ----------------------------------------------------------

/// Production audio backend, wrapping `cpal`.
///
/// Owns a worker thread that holds the `cpal::Stream` (which is `!Send` on
/// most platforms). Public methods send commands to the worker over an
/// `mpsc` channel and block on a one-shot reply. The control plane (start,
/// stop, list-devices) is human-paced — the lock and channel hops cost
/// microseconds and never run on the audio callback thread.
pub struct CpalAudioCapture {
    /// Wrapped in a [`Mutex`] because [`mpsc::Sender`] is `Send` but `!Sync`,
    /// and we need `&self` access from multiple threads through the trait.
    cmd_tx: Mutex<mpsc::Sender<Cmd>>,
    /// Cheap, lock-free read of "is something recording right now?".
    /// Updated by the worker, read by the public API.
    is_recording: Arc<AtomicBool>,
    /// Latest RMS level, encoded as `f32::to_bits()`. Written by the cpal
    /// callback at audio-callback rate (~100 Hz at 48 kHz / 480-frame
    /// callbacks), read by the HUD level pump at ~30 Hz. `Relaxed`
    /// ordering is sound: each store is independent, the read does not
    /// synchronise with anything else, and a stale read just means the
    /// HUD shows the previous level for one extra frame. Cleared back
    /// to `0.0` on stop so the meter idles cleanly.
    level: Arc<AtomicU32>,
    /// Joined on drop. Wrapped in [`Option`] so [`Drop`] can take ownership.
    worker: Option<JoinHandle<()>>,
}

/// Commands sent from the public API into the audio worker thread.
enum Cmd {
    ListDevices(mpsc::Sender<Result<Vec<AudioDevice>>>),
    Start {
        device_id: Option<String>,
        reply: mpsc::Sender<Result<()>>,
    },
    Stop(mpsc::Sender<Result<CapturedAudio>>),
    Shutdown,
}

impl CpalAudioCapture {
    /// Spawn the audio worker thread and return a handle.
    ///
    /// Allocating the thread up-front (rather than on first `start`) keeps
    /// the latency between hotkey-press and first sample bounded, since the
    /// thread is already alive and blocked on `recv`.
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let is_recording = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0_f32.to_bits()));
        let worker_flag = Arc::clone(&is_recording);
        let worker_level = Arc::clone(&level);

        let worker = thread::Builder::new()
            .name("hush-audio".into())
            .spawn(move || worker_loop(cmd_rx, worker_flag, worker_level))
            .expect("failed to spawn audio worker thread");

        Self {
            cmd_tx: Mutex::new(cmd_tx),
            is_recording,
            level,
            worker: Some(worker),
        }
    }

    /// Send a command and block on its reply. Centralised so every public
    /// method gets the same lock / channel-error handling.
    fn dispatch<T>(&self, make_cmd: impl FnOnce(mpsc::Sender<Result<T>>) -> Cmd) -> Result<T> {
        let (tx, rx) = mpsc::channel::<Result<T>>();
        let cmd = make_cmd(tx);
        self.cmd_tx
            .lock()
            .map_err(|_| anyhow!("audio command channel lock poisoned"))?
            .send(cmd)
            .map_err(|_| anyhow!("audio worker thread has exited"))?;
        rx.recv()
            .map_err(|_| anyhow!("audio worker dropped reply channel"))?
    }
}

impl Default for CpalAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CpalAudioCapture {
    fn drop(&mut self) {
        // Best-effort shutdown. If the channel is already closed the worker
        // has exited on its own — nothing to do. We deliberately do not
        // unwrap the join: a panic in the worker should not poison the
        // application shutdown path.
        if let Ok(tx) = self.cmd_tx.lock() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl AudioCapture for CpalAudioCapture {
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        self.dispatch(Cmd::ListDevices)
    }

    fn start(&self, device_id: Option<&str>) -> Result<()> {
        let device_id = device_id.map(str::to_owned);
        self.dispatch(|reply| Cmd::Start { device_id, reply })
    }

    fn stop(&self) -> Result<CapturedAudio> {
        self.dispatch(Cmd::Stop)
    }

    fn is_recording(&self) -> bool {
        // Acquire ordering so a `true` reading happens-after the worker's
        // store, ensuring the corresponding stream is actually live.
        self.is_recording.load(Ordering::Acquire)
    }

    fn current_level(&self) -> f32 {
        // Gate on `is_recording`: the level field is only cleared on stop,
        // but `is_recording` flips at the same point. Reading the flag
        // first lets a future change to the meter (e.g. fade-out instead
        // of hard-zero) live entirely in the consumer without changing
        // the storage discipline here.
        if self.is_recording() {
            f32::from_bits(self.level.load(Ordering::Relaxed))
        } else {
            0.0
        }
    }
}

/// State held by the worker thread for the duration of a single recording.
struct Session {
    /// Kept alive for the duration of capture. Dropping it stops the stream.
    /// We do not need to read from it after construction; the underlying
    /// callback writes directly into [`Self::buffer`].
    stream: Stream,
    format: CaptureFormat,
    /// Shared with the cpal callback. The callback locks briefly, the worker
    /// drains it on stop. See the comment in [`append_samples`] for why a
    /// short-held mutex is acceptable here.
    buffer: Arc<Mutex<Vec<f32>>>,
}

fn worker_loop(cmd_rx: mpsc::Receiver<Cmd>, is_recording: Arc<AtomicBool>, level: Arc<AtomicU32>) {
    // The cpal host is created on the worker thread to avoid any chance of
    // cross-thread state: some backends keep thread-locals pointing back at
    // the host that constructed them.
    let host = cpal::default_host();
    let mut session: Option<Session> = None;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::ListDevices(reply) => {
                let _ = reply.send(list_devices(&host));
            }
            Cmd::Start { device_id, reply } => {
                if session.is_some() {
                    let _ = reply.send(Err(anyhow!("recording already in progress")));
                    continue;
                }
                match start_session(&host, device_id.as_deref(), Arc::clone(&level)) {
                    Ok(s) => {
                        // Release ordering pairs with Acquire in `is_recording()`.
                        is_recording.store(true, Ordering::Release);
                        session = Some(s);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Cmd::Stop(reply) => {
                let result = match session.take() {
                    Some(s) => stop_session(s),
                    None => Err(anyhow!("no recording in progress")),
                };
                // Always clear the flag, even on error: a failed stop should
                // not leave us stuck pretending we're still recording.
                is_recording.store(false, Ordering::Release);
                // Reset the level so the HUD's meter idles cleanly between
                // sessions instead of holding the last RMS reading until
                // the next start.
                level.store(0_f32.to_bits(), Ordering::Relaxed);
                let _ = reply.send(result);
            }
            Cmd::Shutdown => break,
        }
    }
}

fn list_devices(host: &cpal::Host) -> Result<Vec<AudioDevice>> {
    // Capture the default device's name once so we can flag it in the list.
    // We use the name as the comparison key because that is also our public
    // identifier; see the doc comment on [`AudioDevice::id`].
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let devices = host
        .input_devices()
        .context("failed to enumerate input devices")?;
    let mut out = Vec::new();
    for device in devices {
        // A device that fails to report its name is unusable from the UI; we
        // skip it rather than surface a synthetic ID we cannot round-trip.
        let Ok(name) = device.name() else { continue };
        let is_default = default_name.as_deref() == Some(name.as_str());
        out.push(AudioDevice {
            id: name.clone(),
            name,
            is_default,
        });
    }
    Ok(out)
}

fn start_session(
    host: &cpal::Host,
    device_id: Option<&str>,
    level: Arc<AtomicU32>,
) -> Result<Session> {
    let device = match device_id {
        Some(id) => host
            .input_devices()
            .context("enumerate input devices")?
            .find(|d| d.name().map(|n| n == id).unwrap_or(false))
            .ok_or_else(|| anyhow!("input device '{id}' not found"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device available"))?,
    };

    // `default_input_config` returns the format the OS thinks the device is
    // happiest at. Picking it (rather than negotiating a 16 kHz mono config
    // ourselves) maximises the chance the stream actually opens. See the
    // module-level "Capture-at-native-format" note.
    let supported = device
        .default_input_config()
        .context("query default input config")?;

    let format = CaptureFormat {
        sample_rate: supported.sample_rate().0,
        channels: supported.channels(),
    };

    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let stream = build_input_stream(&device, &supported, Arc::clone(&buffer), level)?;
    stream.play().context("start input stream")?;

    Ok(Session {
        stream,
        format,
        buffer,
    })
}

fn stop_session(session: Session) -> Result<CapturedAudio> {
    // Pause first so no further callbacks can land while we move the buffer
    // out of the Arc. Dropping the stream alone is technically sufficient on
    // every backend we currently target, but `pause()` makes the intent
    // obvious and is cheap on the human-paced control plane.
    let _ = session.stream.pause();
    drop(session.stream);

    // Take the buffer contents via a brief lock rather than requiring sole
    // Arc ownership. Earlier versions called `Arc::try_unwrap`, but on some
    // platforms cpal's stream cleanup is asynchronous — the closure clone
    // of the Arc may live a beat longer than the `drop(session.stream)`
    // call above, and `try_unwrap` then errors with "audio buffer still
    // shared after stream drop" even though the recording was successful
    // and the buffer is in a valid state. Locking is correct regardless of
    // how many Arc clones are still alive: if a final callback is mid-write
    // we wait the milliseconds it takes to finish; otherwise the lock is
    // uncontended. The leftover Arc clone(s) drop on their own as cpal
    // finishes cleanup. `mem::take` swaps the Vec out of the mutex so we
    // own the samples and the mutex's interior goes back to an empty Vec.
    let samples = {
        let mut guard = session
            .buffer
            .lock()
            .map_err(|_| anyhow!("audio buffer mutex poisoned"))?;
        std::mem::take(&mut *guard)
    };

    Ok(CapturedAudio {
        samples,
        format: session.format,
    })
}

fn build_input_stream(
    device: &cpal::Device,
    supported: &SupportedStreamConfig,
    buffer: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
) -> Result<Stream> {
    let config: cpal::StreamConfig = supported.config();

    // cpal hands samples back in the device's native format. We convert to
    // f32 inside the callback so the rest of the pipeline only ever deals
    // with one type. The integer paths are exhaustive over what cpal 0.15
    // exposes today; an unknown variant is treated as a hard error rather
    // than a silent fallback so we notice when cpal adds a new format.
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            let buf = Arc::clone(&buffer);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[f32], _| append_samples(&buf, data, |s| *s, &lvl),
                log_stream_error,
                None,
            )
        }
        SampleFormat::I16 => {
            let buf = Arc::clone(&buffer);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[i16], _| append_samples(&buf, data, i16_to_f32, &lvl),
                log_stream_error,
                None,
            )
        }
        SampleFormat::U16 => {
            let buf = Arc::clone(&buffer);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[u16], _| append_samples(&buf, data, u16_to_f32, &lvl),
                log_stream_error,
                None,
            )
        }
        other => return Err(anyhow!("unsupported cpal sample format: {other:?}")),
    }
    .context("build cpal input stream")?;

    Ok(stream)
}

/// Append a callback's worth of samples to the shared buffer and publish
/// the per-callback RMS to the level meter.
///
/// The audio callback runs on a real-time-ish thread; it must not block for
/// long. Locking the mutex is acceptable because the only other lock holder
/// is the worker thread, and only on stop, by which point callbacks have
/// already been paused. RMS is computed in the same single pass that
/// converts and pushes samples — no extra allocation, no second iteration.
/// If profiling later shows contention we can swap in an SPSC ring buffer
/// (e.g. `rtrb`) without changing the public API.
fn append_samples<T: Copy>(
    buffer: &Mutex<Vec<f32>>,
    data: &[T],
    convert: impl Fn(&T) -> f32,
    level: &AtomicU32,
) {
    // A poisoned mutex here means another thread panicked while holding it.
    // Recovering the inner buffer is preferable to panicking the audio
    // thread, which on some backends would tear down the whole process.
    let mut buf = match buffer.lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    };
    buf.reserve(data.len());
    let mut sum_sq = 0.0_f32;
    for sample in data {
        let f = convert(sample);
        sum_sq += f * f;
        buf.push(f);
    }
    if !data.is_empty() {
        let rms = rms_from_sum_sq(sum_sq, data.len());
        // `Relaxed`: each callback writes the latest reading; the HUD
        // pump reads independently and can tolerate a stale value for one
        // 33 ms tick. There is no other field that needs to be observed
        // alongside the level.
        level.store(rms.to_bits(), Ordering::Relaxed);
    }
}

/// RMS from a pre-computed sum-of-squares plus the sample count.
/// Pulled out as a free function so the level-meter math can be
/// unit-tested without spinning up a real cpal stream — the callback
/// itself stays a one-line call into this helper.
fn rms_from_sum_sq(sum_sq: f32, n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        (sum_sq / n as f32).sqrt()
    }
}

fn log_stream_error(err: StreamError) {
    tracing::error!(error = ?err, "audio input stream error");
}

fn i16_to_f32(s: &i16) -> f32 {
    // Symmetric scaling: divide by `i16::MAX` so a full-scale negative sample
    // (`i16::MIN` = -32768) maps to slightly past -1.0, which we leave as-is
    // rather than clamping. Whisper handles values just outside [-1, 1] fine
    // and clamping would introduce a one-sample DC bias.
    *s as f32 / i16::MAX as f32
}

fn u16_to_f32(s: &u16) -> f32 {
    // cpal models U16 as unsigned-PCM with 0x8000 = silence. Shift to
    // signed-centered, then scale into [-1.0, 1.0].
    (*s as f32 - 32768.0) / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_conversion_endpoints() {
        // Spot-check the integer-to-f32 conversions at their extremes; this
        // is the kind of off-by-one that is silent until a recording sounds
        // wrong, so it is worth pinning down.
        assert!((i16_to_f32(&i16::MAX) - 1.0).abs() < 1e-6);
        assert!(i16_to_f32(&0).abs() < 1e-6);
        assert!(i16_to_f32(&i16::MIN) < -0.999);
    }

    #[test]
    fn u16_conversion_endpoints() {
        assert!((u16_to_f32(&u16::MAX) - 0.99997).abs() < 1e-3);
        assert!(u16_to_f32(&32768).abs() < 1e-6);
        assert!((u16_to_f32(&0) + 1.0).abs() < 1e-6);
    }

    /// Compile-time check that the trait is object-safe. If this ever fails
    /// to compile, a higher layer cannot store an `Arc<dyn AudioCapture>`,
    /// which is how the IPC layer (TODO(#9)) plugs in either the cpal
    /// backend or a test mock.
    #[test]
    fn audio_capture_trait_is_object_safe() {
        fn _assert_object_safe(_: &dyn AudioCapture) {}
    }

    #[test]
    fn rms_of_silence_is_zero() {
        // All-zero buffer must produce a zero level so the HUD's
        // meter idles cleanly while the user is between words.
        let n = 480; // typical 10 ms callback at 48 kHz mono
        let sum_sq = 0.0;
        assert!(rms_from_sum_sq(sum_sq, n).abs() < 1e-7);
    }

    #[test]
    fn rms_of_full_scale_signal_is_one() {
        // A buffer of all-±1 samples has sum-of-squares == n, so RMS
        // is exactly 1.0. Pinned because the HUD's bar boost (×4) is
        // calibrated against this scale — if the math drifts the
        // meter would saturate at the wrong amplitude.
        let n = 480;
        let sum_sq = n as f32; // each sample squared is 1.0
        assert!((rms_from_sum_sq(sum_sq, n) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rms_handles_empty_buffer_without_panicking() {
        // An empty data slice on a callback (rare, but cpal does not
        // forbid it) must not divide by zero.
        assert_eq!(rms_from_sum_sq(0.0, 0), 0.0);
    }

    #[test]
    fn default_current_level_is_zero_for_mocks() {
        // Default trait method backs every non-cpal implementation
        // (test mocks, future Parakeet adapter); the HUD treats 0.0
        // as idle, so this is the value mocks are expected to surface.
        struct Stub;
        impl AudioCapture for Stub {
            fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
                Ok(vec![])
            }
            fn start(&self, _: Option<&str>) -> Result<()> {
                Ok(())
            }
            fn stop(&self) -> Result<CapturedAudio> {
                Ok(CapturedAudio {
                    samples: vec![],
                    format: CaptureFormat {
                        sample_rate: 16_000,
                        channels: 1,
                    },
                })
            }
            fn is_recording(&self) -> bool {
                false
            }
        }
        assert_eq!(Stub.current_level(), 0.0);
    }
}
