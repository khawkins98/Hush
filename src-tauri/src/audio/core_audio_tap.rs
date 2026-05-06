//! macOS system-audio capture via CoreAudio process tap (#600).
//!
//! Replaces the ScreenCaptureKit path with `AudioHardwareCreateProcessTap`
//! (available since macOS 14.2, confirmed no Screen Recording TCC on macOS 26 —
//! see `learnings.md` 2026-05-xx).  The tap is implemented in a small Swift
//! helper binary (`resources/hush-audio-tap-capture`) that writes a 12-byte
//! header followed by continuous f32 LE interleaved PCM to stdout.  This
//! module spawns that binary, reads the header, then streams samples from the
//! child's stdout into an `rtrb` ring that the meeting-pump drains per tick.
//!
//! ## Wire protocol
//!
//! ```text
//! bytes  0–3   "HUSH" magic (0x48 55 53 48)
//! bytes  4–7   sample_rate  as u32 little-endian
//! bytes  8–11  channel_count as u32 little-endian
//! bytes 12..   interleaved f32 LE PCM — continuous until SIGTERM / exit
//! ```
//!
//! ## Stopping
//!
//! [`CoreAudioTapSession::stop`] sends SIGTERM to the child and waits up to
//! 1 s for a clean exit before falling back to `kill()`.  The reader thread
//! detects `UnexpectedEof` when the child exits and shuts itself down.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, Context, Result};
use rtrb::{Consumer, RingBuffer};

use super::{
    drain_consumer, log_overflow_if_set, AudioSession, AudioSource, CaptureFormat, CapturedAudio,
    MAX_BUFFER_FRAMES,
};

// ── Public struct ────────────────────────────────────────────────────────────

/// Live system-audio capture session backed by the CoreAudio tap binary.
///
/// Created by [`CoreAudioTapSession::start`]; dropped or explicitly stopped
/// via [`AudioSession::stop`].  `inner` is `Option` so the explicit stop path
/// can `take()` it without a double-stop on `Drop`.
pub struct CoreAudioTapSession {
    pub(super) source: AudioSource,
    pub(super) inner: Option<TapInner>,
    pub(super) active_sessions: Arc<AtomicU32>,
    pub(super) level: Arc<AtomicU32>,
}

/// Heap-allocated state owned by a live tap session.
pub(super) struct TapInner {
    pub(super) format: CaptureFormat,
    child: Mutex<Option<Child>>,
    consumer: Mutex<Consumer<f32>>,
    overflow_flag: Arc<AtomicBool>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
}

// ── Construction ─────────────────────────────────────────────────────────────

impl CoreAudioTapSession {
    /// Spawn the CoreAudio tap binary and return a ready session.
    ///
    /// `resource_dir` is `AppHandle::path().resource_dir()` — the binary is
    /// expected at `<resource_dir>/resources/hush-audio-tap-capture`.
    pub fn start(
        resource_dir: &Path,
        active_sessions: Arc<AtomicU32>,
        level: Arc<AtomicU32>,
    ) -> Result<Self> {
        let binary = resource_dir
            .join("resources")
            .join("hush-audio-tap-capture");

        if !binary.exists() {
            return Err(anyhow!(
                "CoreAudio tap binary not found at {}",
                binary.display()
            ));
        }

        let mut child = Command::new(&binary)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // tap diagnostic messages → app stderr/log
            .spawn()
            .with_context(|| format!("spawn {}", binary.display()))?;

        // Read the 12-byte protocol header before handing off to the
        // background reader thread.  The Swift binary writes the header before
        // starting `engine.start()`, so this read will unblock promptly once
        // the tap is established.  A timeout is not needed — if the binary
        // crashes before writing the header the pipe EOF will propagate here
        // as an `UnexpectedEof` error.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child stdout not captured"))?;
        let mut reader = BufReader::new(stdout);

        let mut hdr = [0u8; 12];
        reader
            .read_exact(&mut hdr)
            .context("read protocol header from audio tap binary")?;

        if &hdr[0..4] != b"HUSH" {
            return Err(anyhow!(
                "audio tap binary sent unexpected magic: {:?}",
                &hdr[0..4]
            ));
        }
        let sample_rate = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
        let channels = u32::from_le_bytes(hdr[8..12].try_into().unwrap());

        if sample_rate == 0 || channels == 0 {
            return Err(anyhow!(
                "audio tap binary reported degenerate format: sr={sample_rate} ch={channels}"
            ));
        }

        let format = CaptureFormat {
            sample_rate,
            channels: channels as u16,
        };

        // SPSC ring large enough for ~2 min of 48 kHz stereo (same ceiling as
        // the cpal path — see MAX_BUFFER_FRAMES in mod.rs).
        let capacity = MAX_BUFFER_FRAMES * channels as usize;
        let (mut producer, consumer) = RingBuffer::new(capacity);
        let overflow_flag = Arc::new(AtomicBool::new(false));
        let overflow_writer = Arc::clone(&overflow_flag);
        let level_writer = Arc::clone(&level);

        let reader_thread = thread::Builder::new()
            .name("hush-cat-reader".into())
            .spawn(move || {
                // Read 4 bytes (one f32) at a time.  `read_exact` blocks until
                // the bytes are available or the pipe closes.  On EOF we exit
                // cleanly; on other errors we log and exit.
                let mut buf = [0u8; 4];
                loop {
                    match reader.read_exact(&mut buf) {
                        Ok(()) => {
                            let sample = f32::from_le_bytes(buf);
                            // Update the level meter with the absolute value of
                            // each sample (cheap per-sample metric; the pump
                            // drains quickly enough that this stays fresh).
                            level_writer.store(sample.abs().to_bits(), Ordering::Relaxed);
                            if producer.push(sample).is_err() {
                                overflow_writer.store(true, Ordering::Relaxed);
                                // Drop the sample — ring is full.
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                            // Child exited — normal shutdown.
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "CoreAudio tap reader: stdout read error; stopping"
                            );
                            break;
                        }
                    }
                }
            })
            .expect("failed to spawn hush-cat-reader thread");

        active_sessions.fetch_add(1, Ordering::Release);

        Ok(Self {
            source: AudioSource::SystemAudio,
            inner: Some(TapInner {
                format,
                child: Mutex::new(Some(child)),
                consumer: Mutex::new(consumer),
                overflow_flag,
                reader_thread: Mutex::new(Some(reader_thread)),
            }),
            active_sessions,
            level,
        })
    }
}

// ── AudioSession impl ─────────────────────────────────────────────────────────

impl AudioSession for CoreAudioTapSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }

    fn current_level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn drain_into(&self, sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        let inner = self.inner.as_ref().ok_or_else(|| {
            anyhow!("CoreAudio tap session already stopped; drain_into unavailable")
        })?;
        let mut consumer = inner
            .consumer
            .lock()
            .map_err(|_| anyhow!("CoreAudio tap consumer lock poisoned"))?;
        let samples = drain_consumer(&mut consumer);
        log_overflow_if_set(&inner.overflow_flag);
        sink.extend_from_slice(&samples);
        Ok(inner.format)
    }

    fn stop(mut self: Box<Self>) -> Result<CapturedAudio> {
        let inner = self
            .inner
            .take()
            .ok_or_else(|| anyhow!("CoreAudio tap session already stopped"))?;

        let format = inner.format;
        let result = stop_inner(inner);

        self.active_sessions.fetch_sub(1, Ordering::Release);
        if self.active_sessions.load(Ordering::Acquire) == 0 {
            self.level.store(0_f32.to_bits(), Ordering::Relaxed);
        }

        Ok(CapturedAudio {
            samples: result?,
            format,
        })
    }
}

impl Drop for CoreAudioTapSession {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if let Err(e) = stop_inner(inner) {
                tracing::warn!(error = ?e, "CoreAudio tap session stop failed during Drop");
            }
            self.active_sessions.fetch_sub(1, Ordering::Release);
            if self.active_sessions.load(Ordering::Acquire) == 0 {
                self.level.store(0_f32.to_bits(), Ordering::Relaxed);
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Gracefully stop a `TapInner`: SIGTERM the child, join the reader thread,
/// drain any remaining samples from the ring.
fn stop_inner(inner: TapInner) -> Result<Vec<f32>> {
    // 1. Terminate child: SIGTERM, then 1 s poll, then SIGKILL fallback.
    if let Ok(mut guard) = inner.child.lock() {
        if let Some(ref mut child) = *guard {
            let pid = child.id() as i32;
            // SAFETY: `pid` is a live child PID obtained from `Child::id()`.
            unsafe { libc::kill(pid, libc::SIGTERM) };

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        thread::sleep(std::time::Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }

    // 2. Join reader thread — it exits once it sees EOF on the child's stdout.
    if let Ok(mut guard) = inner.reader_thread.lock() {
        if let Some(handle) = guard.take() {
            let _ = handle.join();
        }
    }

    // 3. Drain remaining samples from the ring.
    let samples = if let Ok(mut consumer) = inner.consumer.lock() {
        drain_consumer(&mut consumer)
    } else {
        Vec::new()
    };
    log_overflow_if_set(&inner.overflow_flag);

    Ok(samples)
}

// ── Path helper ───────────────────────────────────────────────────────────────

/// Resolve the CoreAudio tap binary path from the Tauri resource directory.
pub fn capture_binary_path(resource_dir: &Path) -> PathBuf {
    resource_dir
        .join("resources")
        .join("hush-audio-tap-capture")
}
