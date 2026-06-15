//! Call-end detection for meeting sessions.
//!
//! Detects when a meeting call has ended while the meeting app stays open
//! (the common case: Zoom call ends, Zoom app remains running). Emits a
//! `meeting:call-may-have-ended` event so the HUD can prompt the user to
//! stop recording.
//!
//! # Architecture
//!
//! A `CallEndSignal` trait abstracts each observable signal. Three concrete
//! implementations cover the available macOS signals:
//! - `MicHalSignal` — CoreAudio `kAudioDevicePropertyDeviceIsRunningSomewhere`
//! - `SystemAudioRmsSignal` — RMS of the system-audio tap (rolling window)
//! - `WhisperSilenceSignal` — consecutive empty Whisper inference ticks
//!
//! `evaluate_call_end_state` is a pure function over explicit enum inputs/outputs,
//! following the `evaluate_mic_state` pattern from `mic_camera_monitor.rs`.
//! It is deterministic and fully unit-tested with injected `Instant` values.
//!
//! # Platform notes
//!
//! macOS: `MicHalSignal` uses CoreAudio HAL (no entitlements needed for same-UID
//!   processes). `SystemAudioRmsSignal` reads the CoreAudioTap level atomic.
//!
//! Linux (future): Replace `MicHalSignal` with an ALSA/PulseAudio open-device
//!   watcher behind `#[cfg(target_os = "linux")]`. `SystemAudioRmsSignal` and
//!   `WhisperSilenceSignal` are platform-agnostic; a PulseAudio adapter would
//!   feed the same `Arc<AtomicU32>` as the macOS tap.
//!
//! Windows (future): `IMMDeviceEnumerator + IMMNotificationClient` for mic;
//!   `IAudioMeterInformation::GetPeakValue` for system audio. Same trait, new structs.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ── Signal abstraction ────────────────────────────────────────────────────────

/// Observation from a single call-end signal source.
pub struct SignalReading {
    /// 0.0 = no evidence of call-end; 1.0 = strong evidence.
    /// Only 0.0 and 1.0 are used currently; the f32 type is kept for
    /// future weighted extensions without a trait change.
    pub strength: f32,
    /// True when this signal actively contradicts call-end — e.g. mic just
    /// became active again, or Whisper transcribed real speech.
    /// A reversal immediately resets the state machine to `Monitoring`.
    pub is_reversal: bool,
}

/// Pluggable call-end signal source.
///
/// Implementors must be `Send + Sync`; the detector task holds them behind
/// `Arc<dyn CallEndSignal>`. `poll()` must not block — it reads atomics or
/// in-memory state only.
pub trait CallEndSignal: Send + Sync {
    fn name(&self) -> &'static str;
    fn poll(&self) -> SignalReading;
}

// ── State machine types ───────────────────────────────────────────────────────

/// Which signals are currently active. Plain struct of bools (not bitflags)
/// to keep the code readable without an extra dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SignalSet {
    pub mic_inactive: bool,
    pub system_audio_quiet: bool,
    pub whisper_silent: bool,
}

impl SignalSet {
    pub fn active_count(self) -> u8 {
        self.mic_inactive as u8 + self.system_audio_quiet as u8 + self.whisper_silent as u8
    }
}

/// State machine node.
#[derive(Debug, Clone, Default)]
pub enum CallEndState {
    /// No suspicion. Default while session signals are normal.
    #[default]
    Monitoring,
    /// One or more signals suggest the call may have ended.
    /// `since`: when suspicion began (used for timing thresholds).
    /// `signals`: which signals were active at transition time.
    Suspected { since: Instant, signals: SignalSet },
    /// Confidence threshold reached. Caller should emit the IPC event.
    /// Immediately transitions back to `Monitoring` so the detector
    /// re-arms for the next call.
    Confident { since: Instant },
}

/// What the state machine should do after one evaluation tick.
#[derive(Debug, PartialEq)]
pub enum CallEndTransition {
    Stay,
    EnterSuspected { signals: SignalSet },
    EnterConfident,
    Reset,
}

/// Inputs to one evaluation tick. All plain data — no IO, no async.
pub struct CallEndInputs {
    pub mic_inactive: bool,
    pub system_audio_quiet: bool,
    pub whisper_silent: bool,
    /// Guard: only activate for manually-started sessions.
    /// Auto-started sessions already have `AutoStop` via `evaluate_mic_state`.
    pub session_is_manual: bool,
    /// True when any signal is a reversal (mic went active, loud audio spike,
    /// Whisper produced real speech). Computed by the caller from `poll()` results.
    pub any_reversal: bool,
}

/// Timing thresholds (all tunable via env vars for dev/testing).
pub struct CallEndThresholds {
    /// How long `Suspected` must hold with ≥ 2 active signals before → Confident.
    pub confirm_two_signal_secs: u64,
    /// How long `Suspected` must hold with 1 active signal before → Confident.
    pub confirm_one_signal_secs: u64,
}

impl Default for CallEndThresholds {
    fn default() -> Self {
        Self {
            confirm_two_signal_secs: std::env::var("HUSH_CE_CONFIRM_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            confirm_one_signal_secs: std::env::var("HUSH_CE_SOLO_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
        }
    }
}

/// Pure state-machine evaluation. No IO, no time-reading — `now` is injected.
///
/// Follows the `evaluate_mic_state()` pattern: pure function over explicit
/// enum inputs → enum output. Testable without any async runtime or wall clock.
pub fn evaluate_call_end_state(
    current: &CallEndState,
    inputs: &CallEndInputs,
    now: Instant,
    thresholds: &CallEndThresholds,
) -> CallEndTransition {
    if !inputs.session_is_manual {
        return CallEndTransition::Stay;
    }

    // A reversal signal (mic active, loud audio, real speech) resets
    // immediately from any state — a resuming call cancels the prompt.
    if inputs.any_reversal {
        return CallEndTransition::Reset;
    }

    let signal_set = SignalSet {
        mic_inactive: inputs.mic_inactive,
        system_audio_quiet: inputs.system_audio_quiet,
        whisper_silent: inputs.whisper_silent,
    };

    match current {
        CallEndState::Monitoring => {
            if signal_set.active_count() >= 1 {
                CallEndTransition::EnterSuspected {
                    signals: signal_set,
                }
            } else {
                CallEndTransition::Stay
            }
        }
        CallEndState::Suspected { since, .. } => {
            let elapsed = now.duration_since(*since).as_secs();
            let count = signal_set.active_count();
            let threshold = if count >= 2 {
                thresholds.confirm_two_signal_secs
            } else {
                thresholds.confirm_one_signal_secs
            };
            if count >= 1 && elapsed >= threshold {
                CallEndTransition::EnterConfident
            } else {
                CallEndTransition::Stay
            }
        }
        CallEndState::Confident { .. } => {
            // Re-arm immediately after emitting the event.
            CallEndTransition::Reset
        }
    }
}

// ── Concrete signal implementations ──────────────────────────────────────────

/// Tracks how long the mic HAL device has been inactive.
///
/// macOS: wraps `MicCameraMonitor::is_any_device_active()`.
/// Linux future: ALSA device open count via /proc, or PulseAudio state change.
/// Windows future: `IMMDeviceEnumerator + IMMNotificationClient`.
///
/// Reliability: HIGH for standard apps. LOW when virtual audio pipelines
/// (Krisp, SoundSource, BlackHole aggregates) hold the HAL device open
/// past call end — those sources prevent `DeviceIsRunningSomewhere` from
/// clearing even when no call is in progress.
#[cfg(target_os = "macos")]
pub struct MicHalSignal {
    monitor: Arc<crate::meeting::mic_camera_monitor::MicCameraMonitor>,
    inactive_since: Mutex<Option<Instant>>,
    threshold_secs: u64,
}

#[cfg(target_os = "macos")]
impl MicHalSignal {
    pub fn new(monitor: Arc<crate::meeting::mic_camera_monitor::MicCameraMonitor>) -> Self {
        let threshold_secs = std::env::var("HUSH_CE_MIC_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        Self {
            monitor,
            inactive_since: Mutex::new(None),
            threshold_secs,
        }
    }
}

#[cfg(target_os = "macos")]
impl CallEndSignal for MicHalSignal {
    fn name(&self) -> &'static str {
        "mic-hal"
    }

    fn poll(&self) -> SignalReading {
        let is_active = self.monitor.is_any_device_active();
        let mut guard = self.inactive_since.lock().unwrap();
        if is_active {
            *guard = None;
            return SignalReading {
                strength: 0.0,
                is_reversal: true,
            };
        }
        let since = guard.get_or_insert_with(Instant::now);
        let elapsed = since.elapsed().as_secs();
        SignalReading {
            strength: if elapsed >= self.threshold_secs {
                1.0
            } else {
                0.0
            },
            is_reversal: false,
        }
    }
}

/// Tracks sustained silence on the system-audio tap using a rolling RMS window.
///
/// Platform-agnostic struct; the `system_audio_level` Arc is written by
/// the meeting pump after each drain of the system-audio source.
///
/// macOS source: `CoreAudioTapSession::current_level()`.
/// Linux future: PulseAudio peak detect or pipewire volume event.
/// Windows future: `IAudioMeterInformation::GetPeakValue`.
///
/// Reliability: MEDIUM standalone. Browser tabs, OS notifications, and
/// background music all spike the level. The 60-second window is deliberately
/// long. Most valuable as a confirming signal when `MicHalSignal` is blocked
/// by a virtual audio pipeline.
pub struct SystemAudioRmsSignal {
    /// Written by meeting pump; read here. f32-as-bits (same encoding as
    /// other AtomicU32 level fields in this codebase).
    system_audio_level: Arc<AtomicU32>,
    /// Rolling window of recent RMS readings (one entry per poll interval).
    window: Mutex<VecDeque<f32>>,
    quiet_threshold: f32,
    /// How many poll ticks to keep in the window (window_secs / poll_interval_secs).
    window_capacity: usize,
    /// Level must be below quiet_threshold for this many consecutive window
    /// entries before strength goes to 1.0.
    required_quiet_ticks: usize,
    quiet_since: Mutex<Option<Instant>>,
    required_quiet_secs: u64,
}

impl SystemAudioRmsSignal {
    pub fn new(system_audio_level: Arc<AtomicU32>, poll_interval_secs: u64) -> Self {
        let window_secs: u64 = std::env::var("HUSH_CE_AUDIO_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let quiet_threshold: f32 = std::env::var("HUSH_CE_AUDIO_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.005); // ≈ -46 dBFS
        let capacity = (window_secs / poll_interval_secs).max(1) as usize;
        Self {
            system_audio_level,
            window: Mutex::new(VecDeque::with_capacity(capacity)),
            quiet_threshold,
            window_capacity: capacity,
            required_quiet_ticks: capacity,
            quiet_since: Mutex::new(None),
            required_quiet_secs: window_secs,
        }
    }
}

impl CallEndSignal for SystemAudioRmsSignal {
    fn name(&self) -> &'static str {
        "system-audio-rms"
    }

    fn poll(&self) -> SignalReading {
        let rms = f32::from_bits(self.system_audio_level.load(Ordering::Relaxed));
        let mut window = self.window.lock().unwrap();
        if window.len() >= self.window_capacity {
            window.pop_front();
        }
        window.push_back(rms);
        let mean = if window.is_empty() {
            0.0
        } else {
            window.iter().copied().sum::<f32>() / window.len() as f32
        };

        // A loud spike (3× threshold) is a reversal — call is definitely active.
        if mean > self.quiet_threshold * 3.0 {
            *self.quiet_since.lock().unwrap() = None;
            return SignalReading {
                strength: 0.0,
                is_reversal: true,
            };
        }

        let all_quiet = window.len() >= self.required_quiet_ticks
            && window.iter().all(|&s| s < self.quiet_threshold);

        if !all_quiet {
            *self.quiet_since.lock().unwrap() = None;
            return SignalReading {
                strength: 0.0,
                is_reversal: false,
            };
        }

        let mut qs = self.quiet_since.lock().unwrap();
        let since = qs.get_or_insert_with(Instant::now);
        SignalReading {
            strength: if since.elapsed().as_secs() >= self.required_quiet_secs {
                1.0
            } else {
                0.0
            },
            is_reversal: false,
        }
    }
}

/// Tracks Whisper inference ticks that produced no real speech.
///
/// Platform-agnostic — reads an `AtomicU32` maintained by the Rust pump layer.
/// Reset to 0 by the pump whenever a real utterance is produced.
///
/// Reliability: HIGH semantic signal. BLANK_AUDIO / empty finals mean VAD
/// gated the inference window — actual audio energy was below Whisper's
/// `no_speech_thold`. False-positive risk: long thinking pauses mid-call can
/// produce 20–30 quiet ticks. Mitigated by requiring `Suspected` state to hold
/// for `confirm_*_secs` before promoting to `Confident`.
pub struct WhisperSilenceSignal {
    consecutive_empty_ticks: Arc<AtomicU32>,
    threshold_ticks: u32,
}

impl WhisperSilenceSignal {
    pub fn new(consecutive_empty_ticks: Arc<AtomicU32>) -> Self {
        let threshold_ticks = std::env::var("HUSH_CE_WHISPER_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20); // ~10 s at 500 ms pump tick
        Self {
            consecutive_empty_ticks,
            threshold_ticks,
        }
    }
}

impl CallEndSignal for WhisperSilenceSignal {
    fn name(&self) -> &'static str {
        "whisper-silence"
    }

    fn poll(&self) -> SignalReading {
        let ticks = self.consecutive_empty_ticks.load(Ordering::Relaxed);
        if ticks == 0 {
            // Real speech just arrived — strongest reversal.
            return SignalReading {
                strength: 0.0,
                is_reversal: true,
            };
        }
        SignalReading {
            strength: if ticks >= self.threshold_ticks {
                1.0
            } else {
                0.0
            },
            is_reversal: false,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> CallEndThresholds {
        CallEndThresholds {
            confirm_two_signal_secs: 30,
            confirm_one_signal_secs: 120,
        }
    }

    fn inputs_manual(
        mic_inactive: bool,
        sys_quiet: bool,
        whisper: bool,
        reversal: bool,
    ) -> CallEndInputs {
        CallEndInputs {
            mic_inactive,
            system_audio_quiet: sys_quiet,
            whisper_silent: whisper,
            session_is_manual: true,
            any_reversal: reversal,
        }
    }

    #[test]
    fn stays_when_session_not_manual() {
        let state = CallEndState::Monitoring;
        let inputs = CallEndInputs {
            mic_inactive: true,
            system_audio_quiet: true,
            whisper_silent: true,
            session_is_manual: false,
            any_reversal: false,
        };
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Stay
        );
    }

    #[test]
    fn monitoring_enters_suspected_on_one_signal() {
        let state = CallEndState::Monitoring;
        let inputs = inputs_manual(true, false, false, false);
        let t = thresholds();
        assert!(matches!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::EnterSuspected { .. }
        ));
    }

    #[test]
    fn reversal_resets_from_monitoring() {
        let state = CallEndState::Monitoring;
        let inputs = inputs_manual(false, false, false, true);
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Reset
        );
    }

    #[test]
    fn suspected_stays_before_threshold() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet {
                mic_inactive: true,
                ..Default::default()
            },
        };
        let inputs = inputs_manual(true, true, false, false);
        let t = thresholds();
        // 29 seconds elapsed — just under two-signal threshold (30s)
        let now = since + std::time::Duration::from_secs(29);
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, now, &t),
            CallEndTransition::Stay
        );
    }

    #[test]
    fn suspected_promotes_to_confident_two_signals_at_30s() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet {
                mic_inactive: true,
                system_audio_quiet: true,
                ..Default::default()
            },
        };
        let inputs = inputs_manual(true, true, false, false);
        let t = thresholds();
        let now = since + std::time::Duration::from_secs(30);
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, now, &t),
            CallEndTransition::EnterConfident
        );
    }

    #[test]
    fn suspected_promotes_one_signal_at_120s() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet {
                system_audio_quiet: true,
                ..Default::default()
            },
        };
        let inputs = inputs_manual(false, true, false, false);
        let t = thresholds();
        let now = since + std::time::Duration::from_secs(120);
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, now, &t),
            CallEndTransition::EnterConfident
        );
    }

    #[test]
    fn reversal_resets_from_suspected() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet {
                mic_inactive: true,
                ..Default::default()
            },
        };
        let inputs = inputs_manual(false, false, false, true);
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Reset
        );
    }

    #[test]
    fn confident_resets_immediately() {
        let state = CallEndState::Confident {
            since: Instant::now(),
        };
        let inputs = inputs_manual(true, true, true, false);
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Reset
        );
    }
}
