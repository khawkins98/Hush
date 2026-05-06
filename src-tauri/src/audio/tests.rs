//! Cross-platform tests for the audio module's traits and shared helpers.
//!
//! Pinned here (peer of `mod.rs`) rather than inline so `mod.rs` can stay
//! within the architectural budget of "trait + shared types only" while
//! still exercising the trait defaults and the shared ring helpers.
//!
//! cpal-specific tests (i16/u16 conversion, rms math, push_samples
//! overflow) live in `cpal.rs` next to the code they cover.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use rtrb::RingBuffer;

use super::*;

/// Compile-time check that the trait is object-safe. If this ever fails
/// to compile, a higher layer cannot store an `Arc<dyn AudioCapture>`,
/// which is how the IPC layer plugs in either the cpal backend or a
/// test mock.
#[test]
fn audio_capture_trait_is_object_safe() {
    fn _assert_object_safe(_: &dyn AudioCapture) {}
}

#[test]
fn audio_session_trait_is_object_safe() {
    // Pump (PR2) holds these via `Vec<Box<dyn AudioSession>>`,
    // so object-safety is load-bearing.
    fn _assert_object_safe(_: &dyn AudioSession) {}
}

/// Stub session for tests that don't need a real audio backend.
/// Used by both the default-impl test and the override-behaviour
/// test below. The override holds a shared buffer the test can
/// preload so we can assert `drain_into` correctly appends to
/// the caller's sink without touching cpal.
struct StubSession {
    source: AudioSource,
    prefilled: std::sync::Mutex<Vec<f32>>,
    format: CaptureFormat,
    overrides_drain: bool,
}
impl AudioSession for StubSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }
    fn drain_into(&self, sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        if !self.overrides_drain {
            // Force the default-impl path even though we're
            // overriding the method (the test below distinguishes
            // override-vs-default by this flag).
            return Err(anyhow!("override path disabled for this stub"));
        }
        let mut guard = self.prefilled.lock().unwrap();
        sink.extend(guard.drain(..));
        Ok(self.format)
    }
    fn stop(self: Box<Self>) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: self.prefilled.lock().unwrap().clone(),
            format: self.format,
        })
    }
}

#[test]
fn drain_into_default_impl_errors_with_actionable_message() {
    // The default impl (which the legacy mocks inherit) errors so
    // the streaming pump (#108 PR3) gets a clear "this backend
    // doesn't do streaming" diagnostic at call time rather than
    // silently dropping samples. Pin the message so a caller's
    // "if drain_into errors, fall back to chunk-and-restart"
    // branch can rely on the wording.
    struct LegacySession {
        source: AudioSource,
    }
    impl AudioSession for LegacySession {
        fn source(&self) -> &AudioSource {
            &self.source
        }
        fn stop(self: Box<Self>) -> Result<CapturedAudio> {
            Ok(CapturedAudio {
                samples: vec![],
                format: CaptureFormat {
                    sample_rate: 16_000,
                    channels: 1,
                },
            })
        }
    }
    let s: Box<dyn AudioSession> = Box::new(LegacySession {
        source: AudioSource::default_microphone(),
    });
    let mut sink = Vec::new();
    let err = s.drain_into(&mut sink).expect_err("default impl errors");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not implemented"),
        "default drain_into should call out the missing impl; got: {msg}"
    );
    assert!(
        msg.contains("override"),
        "default drain_into should hint at how to opt in; got: {msg}"
    );
    assert!(sink.is_empty(), "sink must remain untouched on error");
}

#[test]
fn drain_into_override_appends_to_sink_and_returns_format() {
    // Pin the contract the cpal mic + system-audio overrides have to
    // honour: samples land in the caller's sink (appended, not
    // replaced); format matches what the session was capturing in.
    let session = StubSession {
        source: AudioSource::default_microphone(),
        prefilled: std::sync::Mutex::new(vec![0.1, 0.2, 0.3]),
        format: CaptureFormat {
            sample_rate: 48_000,
            channels: 2,
        },
        overrides_drain: true,
    };
    let mut sink = vec![0.9_f32]; // Pre-existing content in the sink
    let format = session.drain_into(&mut sink).unwrap();
    assert_eq!(format.sample_rate, 48_000);
    assert_eq!(format.channels, 2);
    assert_eq!(
        sink,
        vec![0.9, 0.1, 0.2, 0.3],
        "drain_into appends — does not replace the sink"
    );
}

#[test]
fn drain_into_repeated_calls_drain_only_new_samples() {
    // The pump calls drain_into on a tick; each call should only
    // see samples accumulated since the previous drain. Stub
    // behaviour: the prefilled buffer is .drain(..)'d, so a
    // second call returns nothing new. Pins the cumulative
    // behaviour the cpal worker's mem::take + the system-audio path's
    // mem::take both implement.
    let session = StubSession {
        source: AudioSource::default_microphone(),
        prefilled: std::sync::Mutex::new(vec![0.1, 0.2]),
        format: CaptureFormat {
            sample_rate: 16_000,
            channels: 1,
        },
        overrides_drain: true,
    };
    let mut sink_a = Vec::new();
    session.drain_into(&mut sink_a).unwrap();
    assert_eq!(sink_a, vec![0.1, 0.2]);

    let mut sink_b = Vec::new();
    session.drain_into(&mut sink_b).unwrap();
    assert!(
        sink_b.is_empty(),
        "second drain returns no new samples until callback writes more; got: {sink_b:?}"
    );

    // Simulate the audio callback writing more samples between drains.
    session
        .prefilled
        .lock()
        .unwrap()
        .extend_from_slice(&[0.7, 0.8]);
    let mut sink_c = Vec::new();
    session.drain_into(&mut sink_c).unwrap();
    assert_eq!(sink_c, vec![0.7, 0.8]);
}

#[test]
fn default_start_session_errors_for_backends_that_do_not_override() {
    // Mocks that don't override start_session inherit the
    // default-impl error. Pinning the message so callers (the
    // pump's "this backend can't do parallel capture" branch)
    // can rely on the wording for a useful diagnostic.
    struct LegacyOnly;
    impl AudioCapture for LegacyOnly {
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
    // `expect_err` would require Debug on `Box<dyn AudioSession>`,
    // which is not derivable, so destructure manually.
    let err = match LegacyOnly.start_session(AudioSource::default_microphone()) {
        Ok(_) => panic!("default start_session must error, got Ok"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not implemented"),
        "default start_session should call out the missing impl; got: {msg}"
    );
    assert!(
        msg.contains("override"),
        "error should hint at how to opt in; got: {msg}"
    );
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

// -- AudioSource + start_with_source default impl -------------------
//
// The default `start_with_source` impl dispatches `Microphone` to
// `start` and errors on `SystemAudio`. These tests pin both arms so
// a future trait change that "tightens" the default doesn't silently
// break a backend that's relying on it.

/// Mock that records the device id passed to `start` so we can
/// assert the default `start_with_source` actually forwards it.
struct RecordingMic {
    last_device_id: std::sync::Mutex<Option<Option<String>>>,
}
impl RecordingMic {
    fn new() -> Self {
        Self {
            last_device_id: std::sync::Mutex::new(None),
        }
    }
}
impl AudioCapture for RecordingMic {
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, device_id: Option<&str>) -> Result<()> {
        *self.last_device_id.lock().unwrap() = Some(device_id.map(str::to_owned));
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

#[test]
fn start_with_source_microphone_default_forwards_to_start_with_none() {
    let mic = RecordingMic::new();
    mic.start_with_source(AudioSource::default_microphone())
        .unwrap();
    assert_eq!(*mic.last_device_id.lock().unwrap(), Some(None));
}

#[test]
fn start_with_source_microphone_with_id_forwards_the_id() {
    // Pins the unwrap path: the wrapped `Option<String>` is unpacked
    // back to `Option<&str>` for the legacy `start` signature.
    // A future change that drops the inner unwrap would silently
    // pass `Some("None")` or similar.
    let mic = RecordingMic::new();
    mic.start_with_source(AudioSource::Microphone(Some("usb-mic".to_owned())))
        .unwrap();
    assert_eq!(
        *mic.last_device_id.lock().unwrap(),
        Some(Some("usb-mic".to_owned()))
    );
}

#[test]
fn start_with_source_system_audio_default_returns_error_naming_the_gap() {
    // The default impl must surface a clear error rather than
    // silently falling back to mic — that would let a frontend
    // pick "System audio" and unknowingly record the wrong source.
    let mic = RecordingMic::new();
    let err = mic
        .start_with_source(AudioSource::SystemAudio)
        .expect_err("default impl errors for SystemAudio");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("system audio"),
        "error should name what's missing; got: {msg}"
    );
    // And critically: the legacy `start` was NOT called.
    assert_eq!(*mic.last_device_id.lock().unwrap(), None);
}

#[test]
fn supports_source_default_is_microphone_only() {
    // Default impl says yes to every Microphone source, no to
    // SystemAudio. Pinned so a future trait change that flips a
    // default to "everything supported" can't accidentally make
    // the frontend's source picker offer SystemAudio on a backend
    // that hasn't actually shipped it.
    let mic = RecordingMic::new();
    assert!(mic.supports_source(&AudioSource::default_microphone()));
    assert!(mic.supports_source(&AudioSource::Microphone(Some("any".to_owned()))));
    assert!(!mic.supports_source(&AudioSource::SystemAudio));
    assert!(!mic.supports_system_audio());
}

#[test]
fn list_audio_sources_includes_each_input_device_plus_system_audio_entry() {
    struct ThreeMics;
    impl AudioCapture for ThreeMics {
        fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
            Ok(vec![
                AudioDevice {
                    id: "Built-in".into(),
                    name: "Built-in".into(),
                    is_default: true,
                },
                AudioDevice {
                    id: "USB-C".into(),
                    name: "USB-C".into(),
                    is_default: false,
                },
                AudioDevice {
                    id: "Bluetooth".into(),
                    name: "Bluetooth".into(),
                    is_default: false,
                },
            ])
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

    let listings = ThreeMics.list_audio_sources().unwrap();
    // Three mics + one system-audio entry = four listings.
    assert_eq!(listings.len(), 4);

    let mics: Vec<_> = listings
        .iter()
        .filter(|l| l.kind == AudioSourceKind::Microphone)
        .collect();
    assert_eq!(mics.len(), 3);
    assert!(mics.iter().all(|l| l.is_supported));
    // is_default copies through from AudioDevice.
    assert_eq!(
        mics.iter().filter(|l| l.is_default).count(),
        1,
        "exactly one mic should be the default"
    );

    let system: Vec<_> = listings
        .iter()
        .filter(|l| l.kind == AudioSourceKind::SystemAudio)
        .collect();
    assert_eq!(system.len(), 1, "exactly one system-audio entry");
    // Default `supports_system_audio` returns false; the listing
    // mirrors that so the frontend renders it disabled.
    assert!(!system[0].is_supported);
    assert_eq!(system[0].id, "system");
    // System-audio listing is never marked is_default — there's
    // exactly one, "default" doesn't apply, and the frontend
    // shouldn't auto-pick it on first run.
    assert!(!system[0].is_default);
}

#[test]
fn list_audio_sources_marks_system_audio_supported_when_backend_overrides() {
    // Pin the override path: a backend that ships system-audio
    // returns true from supports_system_audio() and therefore
    // surfaces it as is_supported=true to the frontend, which
    // would render it as a selectable option rather than disabled.
    struct WithSystemAudio;
    impl AudioCapture for WithSystemAudio {
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
        fn supports_source(&self, source: &AudioSource) -> bool {
            matches!(
                source,
                AudioSource::Microphone(_) | AudioSource::SystemAudio
            )
        }
    }
    let listings = WithSystemAudio.list_audio_sources().unwrap();
    let sys = listings
        .iter()
        .find(|l| l.kind == AudioSourceKind::SystemAudio)
        .unwrap();
    assert!(sys.is_supported);
}

#[test]
fn audio_source_listing_serde_uses_camel_case_for_frontend_consumption() {
    // The frontend's TypeScript definition uses isDefault,
    // isSupported, deviceId-style camelCase. Pin the wire shape so
    // a future Rust-side rename fails loud rather than silently
    // breaking the picker.
    let listing = AudioSourceListing {
        kind: AudioSourceKind::Microphone,
        id: "Built-in".into(),
        name: "Built-in".into(),
        is_default: true,
        is_supported: true,
    };
    let json = serde_json::to_string(&listing).unwrap();
    assert!(json.contains(r#""isDefault":true"#), "got: {json}");
    assert!(json.contains(r#""isSupported":true"#), "got: {json}");
    assert!(json.contains(r#""kind":"microphone""#), "got: {json}");

    let sys_listing = AudioSourceListing {
        kind: AudioSourceKind::SystemAudio,
        id: "system".into(),
        name: "System audio".into(),
        is_default: false,
        is_supported: false,
    };
    let sys_json = serde_json::to_string(&sys_listing).unwrap();
    assert!(
        sys_json.contains(r#""kind":"system-audio""#),
        "got: {sys_json}"
    );
}

#[test]
fn audio_source_serde_round_trips() {
    // The IPC boundary serialises this enum; round-tripping pins
    // the wire shape (`{ kind: "microphone" | "system-audio",
    // deviceId: ... }`) so the frontend's TypeScript discriminated
    // union stays in lock-step.
    let mic = AudioSource::Microphone(Some("usb-mic".to_owned()));
    let mic_default = AudioSource::default_microphone();
    let sys = AudioSource::SystemAudio;

    let mic_json = serde_json::to_string(&mic).unwrap();
    let mic_default_json = serde_json::to_string(&mic_default).unwrap();
    let sys_json = serde_json::to_string(&sys).unwrap();

    assert!(
        mic_json.contains(r#""kind":"microphone""#),
        "got: {mic_json}"
    );
    assert!(
        mic_json.contains(r#""deviceId":"usb-mic""#),
        "got: {mic_json}"
    );
    assert!(
        mic_default_json.contains(r#""kind":"microphone""#),
        "got: {mic_default_json}"
    );
    assert!(
        sys_json.contains(r#""kind":"system-audio""#),
        "got: {sys_json}"
    );

    assert_eq!(serde_json::from_str::<AudioSource>(&mic_json).unwrap(), mic);
    assert_eq!(
        serde_json::from_str::<AudioSource>(&sys_json).unwrap(),
        AudioSource::SystemAudio
    );
}

// -- drain_consumer + log_overflow_if_set helpers --------------------
//
// PR #77 fixed a real bug surfaced in hands-on testing: stop_session
// used Arc::try_unwrap to take the buffer Vec, requiring sole Arc
// ownership. On macOS 26 (and apparently other platforms), cpal's
// stream cleanup is asynchronous — the callback closure's Arc clone
// can outlive drop(session.stream) by a beat — so try_unwrap
// sporadically failed on perfectly-good recordings with "audio buffer
// still shared after stream drop." The fix swapped to lock + mem::take.
//
// These tests pin the new behaviour: drain_buffer must succeed
// regardless of how many Arc clones are still alive at call time.
// The unit-test coverage matters because the cpal stream itself is
// impossible to construct without a real audio device, so the
// race-prone bit lives entirely in the buffer-take path now. A
// future regression that puts try_unwrap (or any
// strong-count-sensitive operation) back fails these tests.

#[test]
fn drain_consumer_takes_contents() {
    // Push three samples into a tiny ring, drain, observe the
    // values come out in FIFO order. Replaces the pre-#55
    // `drain_buffer_takes_contents_when_arc_is_unique` test —
    // the rtrb shape doesn't have an Arc to hold (single-owner
    // halves), so the Arc-clone variants from the old suite are
    // gone too.
    let (mut p, mut c) = RingBuffer::<f32>::new(8);
    for v in [1.0_f32, 2.0, 3.0] {
        p.push(v).expect("ring has room for 3 samples");
    }
    let samples = drain_consumer(&mut c);
    assert_eq!(samples, vec![1.0_f32, 2.0, 3.0]);
}

#[test]
fn drain_consumer_returns_empty_for_empty_ring() {
    // The "user pressed Stop almost immediately" path. Drain
    // returns an empty Vec rather than erroring; the
    // transcription stack will surface a more useful error
    // downstream if the silence matters.
    let (_p, mut c) = RingBuffer::<f32>::new(8);
    let samples = drain_consumer(&mut c);
    assert!(samples.is_empty());
}

#[test]
fn log_overflow_if_set_resets_the_flag() {
    // Defensive: a chronic overflow should log once per drain,
    // not once per callback. The flag is reset on observation
    // so the next drain only logs again if the next batch of
    // callbacks overflowed.
    let flag = AtomicBool::new(true);
    log_overflow_if_set(&flag);
    assert!(!flag.load(Ordering::Relaxed));
    log_overflow_if_set(&flag); // no-op when unset
    assert!(!flag.load(Ordering::Relaxed));
}
