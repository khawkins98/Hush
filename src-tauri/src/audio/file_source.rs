//! File-backed audio capture for deterministic testing.
//!
//! [`WavFileAudioCapture`] is an [`AudioCapture`] implementation that
//! reads samples from a pre-loaded buffer rather than a real audio
//! device. It is intended for use in integration tests where you want
//! to exercise the full meeting pump or dictation pipeline without
//! requiring a microphone or model — only the WAV file and a Whisper
//! model need be present.
//!
//! ## Usage in integration tests
//!
//! ```no_run
//! # use hush_lib::audio::file_source::WavFileAudioCapture;
//! # use hush_lib::audio::CaptureFormat;
//! let samples = vec![0.0f32; 16_000]; // 1 s of silence at 16 kHz mono
//! let format = CaptureFormat { sample_rate: 16_000, channels: 1 };
//! let cap = WavFileAudioCapture::new(samples, format, /*chunk_ms=*/ 500);
//! ```
//!
//! ## Design
//!
//! Samples are stored in an `Arc<Vec<f32>>` so each [`WavFileAudioSession`]
//! created by [`WavFileAudioCapture::start_session`] shares the same
//! allocation. Sessions track their read position with an `AtomicUsize`
//! so they are `Send + Sync` without a mutex.
//!
//! Each `drain_into` call returns the next `chunk_samples` samples, where
//! `chunk_samples = (sample_rate * chunk_ms / 1000) * channels`. When the
//! session exhausts the buffer it returns an empty slice on every subsequent
//! drain — the pump will see silence, which is the correct "recording has
//! finished" signal for a fixture test.
//!
//! `stop` drains any remaining samples as a [`CapturedAudio`] so the
//! dictation path works too.
//!
//! ## Feature gate
//!
//! This module is compiled only when the `test-utils` Cargo feature is
//! enabled. Add `--features test-utils` to your `cargo test` invocation.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};

use super::{AudioCapture, AudioDevice, AudioSession, AudioSource, CaptureFormat, CapturedAudio};

/// File-backed audio capture backend.
///
/// All sources opened via [`start_session`](Self::start_session) share the same
/// sample buffer so a single WAV file is enough for single-mic fixture tests.
///
/// The dictation path (`start` / `stop`) is also supported: `start` sets an
/// `is_recording` flag; `stop` returns the whole buffer wrapped in
/// [`CapturedAudio`].
pub struct WavFileAudioCapture {
    samples: Arc<Vec<f32>>,
    format: CaptureFormat,
    /// Number of samples returned per `drain_into` call.
    chunk_samples: usize,
    recording: AtomicBool,
}

impl WavFileAudioCapture {
    /// Load a WAV file from disk and construct a capture backend.
    ///
    /// Handles every PCM variant `hound` supports (16-bit / 24-bit / 32-bit
    /// int and 32-bit float). The format is taken directly from the WAV
    /// header so the transcription pipeline's resampler sees the correct
    /// sample rate rather than assuming 16 kHz.
    pub fn from_wav(path: &std::path::Path, chunk_ms: u64) -> Result<Self> {
        let mut reader = hound::WavReader::open(path)
            .map_err(|e| anyhow!("open WAV fixture {:?}: {e}", path))?;
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| Ok(s? as f32 / max))
                    .collect::<std::result::Result<Vec<_>, hound::Error>>()
                    .map_err(|e| anyhow!("read WAV int samples: {e}"))?
            }
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<std::result::Result<Vec<_>, hound::Error>>()
                .map_err(|e| anyhow!("read WAV float samples: {e}"))?,
        };
        let format = CaptureFormat {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        };
        Ok(Self::new(samples, format, chunk_ms))
    }

    /// Create a new fixture backend from pre-loaded samples.
    ///
    /// * `samples` — interleaved f32 PCM in `[-1.0, 1.0]`.
    /// * `format` — sample rate and channel count that matches `samples`.
    /// * `chunk_ms` — how many milliseconds of audio to return per
    ///   `drain_into` call. Should match the pump tick (500 ms) for
    ///   realistic timing in meeting fixture tests.
    pub fn new(samples: Vec<f32>, format: CaptureFormat, chunk_ms: u64) -> Self {
        let chunk_samples =
            ((format.sample_rate as u64 * chunk_ms / 1000) * format.channels as u64) as usize;
        Self {
            samples: Arc::new(samples),
            format,
            chunk_samples,
            recording: AtomicBool::new(false),
        }
    }
}

impl AudioCapture for WavFileAudioCapture {
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![AudioDevice {
            id: "fixture".into(),
            name: "WAV fixture device".into(),
            is_default: true,
        }])
    }

    fn start(&self, _device_id: Option<&str>) -> Result<()> {
        self.recording.store(true, Ordering::Release);
        Ok(())
    }

    fn stop(&self) -> Result<CapturedAudio> {
        if !self.recording.swap(false, Ordering::AcqRel) {
            return Err(anyhow!(
                "WavFileAudioCapture: stop called while not recording"
            ));
        }
        Ok(CapturedAudio {
            samples: (*self.samples).clone(),
            format: self.format,
        })
    }

    fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    fn supports_source(&self, _source: &AudioSource) -> bool {
        true
    }

    fn start_session(&self, source: AudioSource) -> Result<Box<dyn AudioSession>> {
        Ok(Box::new(WavFileAudioSession {
            source,
            samples: Arc::clone(&self.samples),
            format: self.format,
            position: AtomicUsize::new(0),
            chunk_samples: self.chunk_samples,
        }))
    }
}

/// Per-session handle returned by [`WavFileAudioCapture::start_session`].
pub struct WavFileAudioSession {
    source: AudioSource,
    samples: Arc<Vec<f32>>,
    format: CaptureFormat,
    position: AtomicUsize,
    chunk_samples: usize,
}

impl AudioSession for WavFileAudioSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }

    fn drain_into(&self, sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        let pos = self.position.load(Ordering::Acquire);
        let end = (pos + self.chunk_samples).min(self.samples.len());
        sink.extend_from_slice(&self.samples[pos..end]);
        self.position.store(end, Ordering::Release);
        Ok(self.format)
    }

    fn stop(self: Box<Self>) -> Result<CapturedAudio> {
        let pos = self.position.load(Ordering::Acquire);
        let remaining = self.samples[pos..].to_vec();
        Ok(CapturedAudio {
            samples: remaining,
            format: self.format,
        })
    }
}
