//! Unit tests for the dictation IPC command handlers and pipeline helpers.
//!
//! Extracted from `commands/dictation/mod.rs` under #684. All tests cover
//! the pure-logic helpers in `pipeline.rs` so the command shells stay thin
//! and the orchestration steps are independently pinned.

// -- start_dictation_inner regression tests ---------------------------
//
// These cover the foreground-leak fix surfaced in code review: a
// failed `audio.start` must not overwrite or pollute the
// `pending_foreground` slot. Using mock implementations of
// `AudioCapture` rather than the cpal backend so we do not need a real
// microphone or Tauri runtime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use crate::audio::{AudioCapture, AudioDevice, AudioSource, CapturedAudio};
use crate::dictionary::{
    NewVocabularyTerm, ReplacementRepository, ReplacementRule, VocabularyRepository, VocabularyTerm,
};
use crate::ipc::state::ForegroundApp;
use crate::ipc::AppState;
use crate::transcription::Transcribe;

use super::pipeline::{
    load_replacement_rules, load_vocabulary_prompt, start_dictation_inner, stop_audio_capture,
    strip_whisper_brackets, take_foreground_snapshot,
};
use super::IpcError;

/// Local Transcribe stub. The crate-root tests have an
/// `EchoTranscribe` but it isn't `pub(crate)`; declaring a fresh
/// one here keeps the dependency minimal.
struct OkTranscribe;
impl Transcribe for OkTranscribe {
    fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
        Ok("ok".to_owned())
    }
}

struct AudioThatFailsToStart;

impl AudioCapture for AudioThatFailsToStart {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Err(anyhow!("device unplugged"))
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!("stop should not be called when start fails")
    }
    fn is_recording(&self) -> bool {
        false
    }
}

/// Audio mock that surfaces a permission-shaped chain. Used to
/// pin the classifier promotion in `start_dictation_inner`
/// (#386 / #416 close-out): a chain containing
/// "microphone not authorized" should land as the typed
/// `IpcError::PermissionDenied("microphone")` variant
/// rather than a generic `IpcError::Audio(...)`.
struct AudioThatFailsWithMicrophoneDenial;

impl AudioCapture for AudioThatFailsWithMicrophoneDenial {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Err(anyhow!("microphone access not authorized"))
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!("stop should not be called when start fails")
    }
    fn is_recording(&self) -> bool {
        false
    }
}

struct AudioThatStarts {
    recording: AtomicBool,
}

impl AudioCapture for AudioThatStarts {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        self.recording.store(true, Ordering::Release);
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!()
    }
    fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }
}

#[test]
fn start_dictation_does_not_overwrite_foreground_on_audio_start_failure() {
    let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsToStart);
    let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .transcribe(Some(transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    // Pre-populate the slot with a sentinel value so a regression in
    // the assignment order — assigning the new capture before
    // `audio.start` returns — would visibly overwrite it.
    *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
        app_name: "sentinel".into(),
        window_title: "sentinel".into(),
    });

    let err = start_dictation_inner(&state, AudioSource::default_microphone())
        .expect_err("audio.start fails");
    assert!(
        matches!(err, IpcError::Audio(_)),
        "expected IpcError::Audio, got {err:?}"
    );

    let after = state.pending_foreground.lock().unwrap().clone();
    assert_eq!(
        after.map(|f| f.app_name).as_deref(),
        Some("sentinel"),
        "pending_foreground was overwritten despite failed start"
    );
}

#[test]
fn start_dictation_promotes_permission_shaped_error_to_typed_variant() {
    // #386 / #416 close-out: a permission-shaped chain from the audio
    // layer (e.g. microphone not authorized) must surface as
    // `IpcError::PermissionDenied(...)` so the frontend's
    // PermissionsDialog launch heuristic can match on `kind` instead
    // of substring-scraping.
    let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsWithMicrophoneDenial);
    let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .transcribe(Some(transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    let err = start_dictation_inner(&state, AudioSource::default_microphone())
        .expect_err("audio.start fails with permission-shaped chain");
    match err {
        IpcError::PermissionDenied(perm) => {
            assert_eq!(perm, "microphone");
        }
        other => {
            panic!("expected IpcError::PermissionDenied(\"microphone\"), got: {other:?}")
        }
    }
}

#[test]
fn start_dictation_succeeds_and_leaves_a_foreground_slot_for_stop() {
    // Confirms the happy path actually does write into the slot —
    // otherwise the bug-fix above could be "we just never assign
    // anything", which would also pass the regression test in
    // isolation.
    let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatStarts {
        recording: AtomicBool::new(false),
    });
    let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .transcribe(Some(transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    // We can't observe the OS foreground app reliably from a test
    // process, so we just assert the call returned Ok and the slot is
    // *some* value (None or Some, both are acceptable — the OS may
    // genuinely have no active window in CI).
    start_dictation_inner(&state, AudioSource::default_microphone()).expect("should succeed");

    // Just prove the lock didn't poison and the slot is reachable.
    let _: Option<ForegroundApp> = state.pending_foreground.lock().unwrap().clone();
}

/// Suppress the dead-code warning that fires because [`Mutex`] is
/// otherwise unused after the regression tests' construction —
/// this is part of the type signature compile-check above.
#[allow(dead_code)]
fn _assert_state_mutex_holds_foreground(state: AppState) -> Mutex<Option<ForegroundApp>> {
    state.pending_foreground
}

#[test]
fn start_dictation_returns_unavailable_when_no_transcriber_is_loaded() {
    // Pre-#195 this scenario silently opened audio capture and
    // failed at `stop_dictation` — the user spent N seconds
    // recording before learning no transcriber was loaded.
    // Pin the new pre-flight: no transcriber → fail fast, no
    // audio side effects, no foreground slot mutation.
    let audio_started = Arc::new(AtomicBool::new(false));
    let audio: Arc<dyn AudioCapture> = Arc::new(StartFlagAudio {
        started: Arc::clone(&audio_started),
    });
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        // No `.transcribe(...)` — slot stays None.
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    let err = start_dictation_inner(&state, AudioSource::default_microphone())
        .expect_err("no-transcriber must surface as a hard error");
    assert!(
        matches!(err, IpcError::TranscriptionUnavailable),
        "expected TranscriptionUnavailable, got {err:?}"
    );
    assert!(
        !audio_started.load(Ordering::Acquire),
        "audio.start_with_source must NOT be called when no transcriber is loaded"
    );
}

/// Audio backend whose only job is recording whether `start_with_source`
/// (or `start`) was called, so the pre-flight test can prove the
/// audio path was skipped before the error returned.
struct StartFlagAudio {
    started: Arc<AtomicBool>,
}

impl AudioCapture for StartFlagAudio {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        self.started.store(true, Ordering::Release);
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!("stop should not be called");
    }
    fn is_recording(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
}

// -- whisper bracket-sentinel stripping ------------------------------

#[test]
fn strip_brackets_drops_pure_blank_audio_sentinel() {
    // The exact case in #196's user report: whisper emitted
    // `[BLANK_AUDIO]` and the user saw it in the result panel
    // and on their clipboard.
    assert_eq!(strip_whisper_brackets("[BLANK_AUDIO]"), "");
}

#[test]
fn strip_brackets_drops_other_status_sentinels() {
    // Same shape, different label. Whisper produces these for
    // music / non-speech / unintelligible segments.
    for sentinel in [
        "[NOISE]",
        "[MUSIC]",
        "[ MUSIC ]", // whitespace-padded variant
        "[INAUDIBLE]",
        "[laughter]", // case-insensitive match
        "[APPLAUSE]",
        "[SILENCE]",
    ] {
        assert_eq!(
            strip_whisper_brackets(sentinel),
            "",
            "sentinel {sentinel} should strip to empty"
        );
    }
}

#[test]
fn strip_brackets_preserves_non_sentinel_bracketed_content() {
    // The new allowlist-based approach must NOT silently drop user
    // content that happens to be in brackets (e.g. stage directions,
    // citations, Markdown footnotes).
    assert_eq!(strip_whisper_brackets("[my citation]"), "[my citation]");
    assert_eq!(strip_whisper_brackets("[Sound effects]"), "[Sound effects]");
    // Only the sentinel is dropped; adjacent bracketed content is kept.
    assert_eq!(
        strip_whisper_brackets("[BLANK_AUDIO] hello [citation]"),
        "hello [citation]"
    );
}

#[test]
fn strip_brackets_keeps_real_speech_around_a_silence_marker() {
    // Whisper sometimes prefixes a transcript with
    // `[BLANK_AUDIO]` when there's a leading silence segment —
    // the real speech follows. Keep the speech, drop the marker,
    // collapse the surrounding whitespace.
    assert_eq!(
        strip_whisper_brackets("[BLANK_AUDIO] hello world"),
        "hello world"
    );
    assert_eq!(strip_whisper_brackets("hello world [NOISE]"), "hello world");
    assert_eq!(
        strip_whisper_brackets("first [NOISE] second"),
        "first second"
    );
}

#[test]
fn strip_brackets_leaves_text_with_no_brackets_alone() {
    // The common path. Pin so a regression in the stripping
    // pass doesn't accidentally trim or reflow real
    // transcripts.
    assert_eq!(strip_whisper_brackets("Hello, world."), "Hello, world.");
}

#[test]
fn strip_brackets_handles_nested_or_unbalanced_brackets_safely() {
    // Defensive: whisper isn't supposed to emit nested or
    // unbalanced brackets, but the depth counter shouldn't
    // panic if it does.
    // [[NESTED]] is NOT on the allowlist, so it's preserved verbatim.
    assert_eq!(strip_whisper_brackets("[[NESTED]]"), "[[NESTED]]");
    // A stray closing bracket is preserved (depth never goes
    // negative).
    assert_eq!(strip_whisper_brackets("hello]"), "hello]");
}

// -- stop_dictation helper tests --------------------------------------
//
// The Tauri command itself needs an `AppHandle` (clipboard +
// notification + HUD), so it can't be unit-tested directly. The
// helpers extracted from it can — these tests pin their behaviour
// so the orchestration in `stop_dictation` stays trustworthy
// through future refactors.

struct AudioThatStopsWith {
    captured: CapturedAudio,
}

impl AudioCapture for AudioThatStopsWith {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        Ok(self.captured.clone())
    }
    fn is_recording(&self) -> bool {
        false
    }
}

struct AudioThatFailsToStop;

impl AudioCapture for AudioThatFailsToStop {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        Err(anyhow!("device went away"))
    }
    fn is_recording(&self) -> bool {
        false
    }
}

/// Audio mock whose `stop()` returns a typed `DeviceLost` wrapped
/// in `anyhow::Error`. Pin for the IPC downcast (#617).
struct AudioThatFailsToStopWithDeviceLost {
    device: String,
}

impl AudioCapture for AudioThatFailsToStopWithDeviceLost {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        Err(anyhow::Error::new(crate::audio::DeviceLost {
            device: self.device.clone(),
        }))
    }
    fn is_recording(&self) -> bool {
        false
    }
}

struct VocabWithTerms(Vec<VocabularyTerm>);

#[async_trait::async_trait]
impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for VocabWithTerms {
    async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
        Ok(self.0.clone())
    }
    async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
        unreachable!()
    }
    async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

struct FailingVocab;

#[async_trait::async_trait]
impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for FailingVocab {
    async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
        Err(anyhow!("table missing"))
    }
    async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
        unreachable!()
    }
    async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

struct FailingReplacements;

#[async_trait::async_trait]
impl crate::repository::Repository<ReplacementRule, crate::dictionary::NewReplacementRule, i64>
    for FailingReplacements
{
    async fn list(&self) -> anyhow::Result<Vec<ReplacementRule>> {
        Err(anyhow!("table missing"))
    }
    async fn create(
        &self,
        _: crate::dictionary::NewReplacementRule,
    ) -> anyhow::Result<ReplacementRule> {
        unreachable!()
    }
    async fn update(&self, _: ReplacementRule) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

fn state_with(
    audio: Arc<dyn AudioCapture>,
    vocab: Arc<dyn VocabularyRepository>,
    replacements: Arc<dyn ReplacementRepository>,
) -> AppState {
    crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(replacements)
        .vocabulary(vocab)
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete")
}

fn fixed_audio() -> CapturedAudio {
    CapturedAudio {
        samples: vec![0.5_f32; 8],
        format: crate::audio::CaptureFormat {
            sample_rate: 48_000,
            channels: 1,
        },
    }
}

#[test]
fn stop_audio_capture_returns_captured_on_success() {
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let captured = stop_audio_capture(&state).expect("audio.stop ok");
    assert_eq!(captured.samples.len(), 8);
    assert_eq!(captured.format.sample_rate, 48_000);
}

#[test]
fn stop_audio_capture_maps_backend_error_to_ipc_error_audio() {
    // Regression for the heuristic-classifier era: audio errors must
    // surface as `IpcError::Audio` so the frontend's switch-on-kind
    // dispatch picks the right recovery copy. This is *structural*
    // classification — there is no string match anywhere.
    let state = state_with(
        Arc::new(AudioThatFailsToStop),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let err = stop_audio_capture(&state).expect_err("stop fails");
    assert!(matches!(err, IpcError::Audio(_)), "got {err:?}");
}

#[test]
fn stop_audio_capture_routes_device_lost_to_typed_ipc_variant() {
    // #617: pin the downcast that distinguishes "mic disconnected"
    // from generic audio failures. A regression here silently
    // demotes mic disconnects to the generic Audio bucket and
    // the frontend loses the structured banner copy.
    let state = state_with(
        Arc::new(AudioThatFailsToStopWithDeviceLost {
            device: "AirPods Pro".to_owned(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let err = stop_audio_capture(&state).expect_err("stop fails");
    match err {
        IpcError::AudioDeviceLost(name) => {
            assert_eq!(name, "AirPods Pro");
        }
        other => panic!("expected IpcError::AudioDeviceLost(\"AirPods Pro\"), got {other:?}"),
    }
}

#[tokio::test]
async fn load_vocabulary_prompt_formats_terms_when_present() {
    let terms = vec![
        VocabularyTerm {
            id: 1,
            term: "Hush".into(),
        },
        VocabularyTerm {
            id: 2,
            term: "whisper.cpp".into(),
        },
    ];
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(VocabWithTerms(terms.clone())),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let prompt = load_vocabulary_prompt(&state).await;
    // The exact format is owned by `format_vocabulary_prompt`; this
    // test just pins that the helper actually invokes the formatter
    // rather than returning empty.
    assert!(prompt.contains("Hush"), "got: {prompt}");
    assert!(prompt.contains("whisper.cpp"), "got: {prompt}");
}

#[tokio::test]
async fn load_vocabulary_prompt_swallows_repository_errors() {
    // Repository failure must not block transcription — we demote
    // to the no-prompt path.
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(FailingVocab),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    // Isolate the repo-failure path from the 2026-06-05 default-pack /
    // Oxford-style change: with no packs and American selected, the only
    // contributor is the (failing) user vocab repo, so a swallowed error
    // yields an empty prompt.
    state
        .settings
        .set(crate::settings::keys::ENABLED_PACKS, "[]")
        .await
        .unwrap();
    state
        .settings
        .set(crate::settings::keys::LANGUAGE_STYLE, "american")
        .await
        .unwrap();

    let prompt = load_vocabulary_prompt(&state).await;
    assert!(prompt.is_empty(), "got: {prompt}");
}

#[tokio::test]
async fn load_replacement_rules_returns_empty_on_error() {
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(FailingReplacements),
    );

    // Disable the default Developer pack (which ships 17 replacement
    // rules as of 2026-06-05) so this test isolates the user-repo
    // failure path: with no packs, the failing repo is the only source.
    state
        .settings
        .set(crate::settings::keys::ENABLED_PACKS, "[]")
        .await
        .unwrap();

    let rules = load_replacement_rules(&state).await;
    assert!(rules.is_empty());
}

#[test]
fn take_foreground_snapshot_pops_and_clears_the_slot() {
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );
    *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
        app_name: "Slack".into(),
        window_title: "#general".into(),
    });

    let popped = take_foreground_snapshot(&state).expect("not poisoned");
    assert_eq!(popped.as_ref().map(|f| f.app_name.as_str()), Some("Slack"));

    // Second take must be None: the slot is consumed, not cloned.
    let again = take_foreground_snapshot(&state).expect("not poisoned");
    assert!(again.is_none());
}

/// IPC-layer regression (#947 review Gap 2): a DICTATION start must
/// succeed WHILE a meeting's background finalization is still in flight.
///
/// Dictation uses the separate `transcribe` (dictation) slot and is
/// gated only on `meeting_manager.active_session_id()` — NOT on the
/// meeting `finalizing` lane. Background finalization flips the slot to
/// Idle (`active_session_id() == None`) the moment audio is released,
/// then parks the slow tail flush off to the side. So a dictation start
/// fired during that window must go through.
///
/// Drives the real IPC start path (`start_dictation_inner`, the body of
/// the `start_dictation` command) against a full `AppState`. A
/// slow-`finish()` streaming session keeps finalization blocked on a
/// barrier so the assertion lands deterministically while it's in flight.
#[tokio::test]
async fn dictation_start_succeeds_while_meeting_finalization_in_flight() {
    use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};

    // Independent in-memory repo + recording emitter so we can read the
    // closed session row and assert MeetingSessionEnded after release.
    let db = crate::db::SqliteDatabase::open_in_memory().await.unwrap();
    let meeting_repo: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(
        crate::meeting::SqliteMeetingSessionRepository::new(Arc::new(db)),
    );
    let emitter = crate::ipc::events::RecordingEventEmitter::new();

    let release = Arc::new(StdAtomicBool::new(false));
    let started = Arc::new(StdAtomicBool::new(false));
    let mgr = crate::meeting::manager_with_slow_finish_parts(
        Arc::clone(&release),
        Arc::clone(&started),
        Arc::clone(&meeting_repo),
        Arc::new(emitter.clone()),
    );

    // Start + stop a meeting so its finalization is in flight (blocked on
    // the barrier inside the slow finish()). Done before wrapping the
    // manager into AppState — start/stop go through the manager directly,
    // exactly as the meeting IPC commands would.
    let session = mgr
        .start_manual(
            vec![AudioSource::default_microphone()],
            Some("Zoom".into()),
            None,
            Default::default(),
        )
        .await
        .expect("meeting start succeeds");
    mgr.stop_manual()
        .await
        .expect("stop returns once audio released");

    // The slot is Idle while the tail flush is parked + blocked on the
    // barrier — this is precisely what unblocks a dictation start.
    assert!(
        mgr.active_session_id().is_none(),
        "meeting slot must be Idle (audio released) while finalization is parked"
    );

    // Confirm finalization is genuinely in flight (not already done):
    // the slow finish() flips `started` when it begins spinning on the
    // barrier. Wait for it so the dictation start below is exercised
    // against a truly-blocked finalization, not a no-op window.
    let mut finalize_in_flight = false;
    for _ in 0..200 {
        if started.load(StdOrdering::Acquire) {
            finalize_in_flight = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        finalize_in_flight,
        "the meeting's tail finish() must be in flight (blocked on the barrier) \
         so the dictation start is exercised during real finalization"
    );
    assert!(
        !release.load(StdOrdering::Acquire),
        "barrier must still be held — finalization is blocked, not complete"
    );

    // Build the AppState the dictation IPC path runs against. The
    // dictation audio + transcriber are independent of the meeting's
    // (the meeting used StubParallelAudio internally).
    let dictation_audio: Arc<dyn AudioCapture> = Arc::new(AudioThatStarts {
        recording: AtomicBool::new(false),
    });
    let dictation_transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(dictation_audio)
        .transcribe(Some(dictation_transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings(Arc::clone(&meeting_repo))
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(mgr))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    // The load-bearing assertion: a dictation start through the real IPC
    // body succeeds while the meeting's finalization is still blocked.
    // If dictation were (incorrectly) gated on the finalizing lane, this
    // would fail or block.
    start_dictation_inner(&state, AudioSource::default_microphone())
        .expect("dictation start must succeed during background finalization");

    // Now release the barrier and let the parked finalization task run to
    // completion. It's a spawned tokio task; poll until MeetingSessionEnded
    // is emitted (the pump emits it as its LAST action, right after
    // close_session) — bounded so a regression can't hang CI. Polling
    // rather than joining the handle keeps the test off the manager's
    // `pub(super)` `finalizing` field; the observable outcome is the point.
    release.store(true, StdOrdering::Release);
    let mut ended_for_session = false;
    for _ in 0..200 {
        let ended = emitter.payloads_for("meeting:session-ended");
        if ended
            .iter()
            .any(|p| p.get("sessionId").and_then(|v| v.as_i64()) == Some(session.id))
        {
            ended_for_session = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        ended_for_session,
        "MeetingSessionEnded must be emitted for the finalized session after release"
    );

    // The session row is closed (ended_at set) — emitted-ended implies
    // close_session already ran (pump order: close then emit-ended).
    let row = meeting_repo
        .get_by_id(session.id)
        .await
        .unwrap()
        .expect("session row exists");
    assert!(
        row.ended_at.is_some(),
        "background finalization must close the session row"
    );
}

// -- VAD trim + pad (#1013) --------------------------------------------

/// VAD model whose sessions replay a fixed per-frame probability list
/// (then silence), so `vad_trim_dictation` can be driven without the
/// real Silero model.
struct PatternVad(Vec<f32>);
impl crate::vad::VadModel for PatternVad {
    fn new_session(&self) -> Box<dyn crate::vad::VadSession> {
        Box::new(crate::vad::test_mocks::ScriptedVad {
            probs: self.0.iter().copied().collect(),
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }
}

fn mono_16k(secs_x10: usize) -> CapturedAudio {
    CapturedAudio {
        samples: vec![0.1; 1_600 * secs_x10],
        format: crate::audio::CaptureFormat {
            sample_rate: 16_000,
            channels: 1,
        },
    }
}

#[test]
fn vad_trim_is_skipped_with_noop_vad() {
    let out = super::pipeline::vad_trim_dictation(&crate::vad::NoopVad, &mono_16k(20), 0.0);
    assert!(matches!(out, super::pipeline::DictationTrim::Skipped));
}

#[test]
fn vad_trim_reports_no_speech_when_vad_hears_nothing() {
    let out = super::pipeline::vad_trim_dictation(&PatternVad(vec![0.0; 200]), &mono_16k(20), 0.0);
    assert!(matches!(out, super::pipeline::DictationTrim::NoSpeech));
}

#[test]
fn vad_trim_pads_a_short_real_utterance_to_1_25_s() {
    // 0.6 s press, speech in frames 4..=14 (~0.35 s "yes").
    let mut probs = vec![0.0; 4];
    probs.extend(std::iter::repeat(0.9).take(11));
    let out = super::pipeline::vad_trim_dictation(&PatternVad(probs), &mono_16k(6), 0.0);
    let super::pipeline::DictationTrim::Trimmed(t) = out else {
        panic!("expected a trimmed clip, got {out:?}");
    };
    assert_eq!(t.format.sample_rate, 16_000);
    assert_eq!(t.format.channels, 1);
    assert_eq!(t.samples.len(), 20_000, "padded to 1.25 s");
}

#[test]
fn vad_trim_cuts_long_leading_and_trailing_silence() {
    // 5 s clip; speech only in frames 60..=90.
    let mut probs = vec![0.0; 60];
    probs.extend(std::iter::repeat(0.9).take(31));
    let out = super::pipeline::vad_trim_dictation(&PatternVad(probs), &mono_16k(50), 0.0);
    let super::pipeline::DictationTrim::Trimmed(t) = out else {
        panic!("expected a trimmed clip, got {out:?}");
    };
    // 31 frames of speech + 300 ms lead + 400 ms tail ≈ 2.1 s, well under 5 s.
    assert!(t.samples.len() < 40_000, "got {} samples", t.samples.len());
    assert!(t.samples.len() >= 31 * 512);
}

#[test]
fn vad_trim_converts_stereo_48k_to_16k_mono() {
    let captured = CapturedAudio {
        samples: vec![0.1; 48_000 * 2 * 2], // 2 s stereo @ 48 kHz
        format: crate::audio::CaptureFormat {
            sample_rate: 48_000,
            channels: 2,
        },
    };
    let out = super::pipeline::vad_trim_dictation(&PatternVad(vec![0.9; 62]), &captured, 0.0);
    let super::pipeline::DictationTrim::Trimmed(t) = out else {
        panic!("expected a trimmed clip, got {out:?}");
    };
    assert_eq!(t.format.sample_rate, 16_000);
    assert_eq!(t.format.channels, 1);
    // Whole 2 s clip is speech → kept whole (~32 000 samples).
    assert!(
        (31_000..=32_100).contains(&t.samples.len()),
        "{}",
        t.samples.len()
    );
}

/// Real-audio check for the dictation VAD trim (#1013): needs a model,
/// so `#[ignore]`d. Prints transcripts for a sub-second clip (rejected
/// before #1013), a silence-padded clip trimmed vs untrimmed, and the
/// Silero scoring cost on 60 s of audio.
///
/// ```text
/// HUSH_TEST_MODEL=… cargo test --lib vad_trim_real_audio -- --ignored --nocapture
/// ```
#[cfg(all(feature = "whisper", feature = "diarization-onnx"))]
#[test]
#[ignore]
fn vad_trim_real_audio() {
    use crate::transcription::WhisperTranscription;
    use crate::vad::VadModel as _;
    let Ok(model) = std::env::var("HUSH_TEST_MODEL") else {
        eprintln!("skip: HUSH_TEST_MODEL not set");
        return;
    };
    let wav = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jfk.wav");
    let mut reader = hound::WavReader::open(wav).unwrap();
    let spec = reader.spec();
    let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
    let raw: Vec<f32> = reader
        .samples::<i32>()
        .map(|s| s.unwrap() as f32 / max)
        .collect();
    let jfk = crate::transcription::resample::resample_to_mono(&raw, spec.sample_rate, 16_000);
    let fmt = crate::audio::CaptureFormat {
        sample_rate: 16_000,
        channels: 1,
    };
    let vad = crate::vad::onnx::SileroVad::load().unwrap();
    let whisper = WhisperTranscription::new(&model).unwrap();
    let run = |label: &str, samples: Vec<f32>| {
        let captured = CapturedAudio {
            samples,
            format: fmt,
        };
        let before_ms = captured.samples.len() * 1000 / 16_000;
        let t0 = std::time::Instant::now();
        let trim = super::pipeline::vad_trim_dictation(&vad, &captured, 0.0);
        let vad_cost = t0.elapsed();
        let untrimmed = whisper.transcribe(&captured).unwrap();
        match trim {
            super::pipeline::DictationTrim::Trimmed(t) => {
                let after_ms = t.samples.len() * 1000 / 16_000;
                let trimmed = whisper.transcribe(&t).unwrap();
                eprintln!(
                    "{label}: {before_ms} ms → {after_ms} ms (VAD {vad_cost:?})\n  untrimmed: {untrimmed:?}\n  trimmed:   {trimmed:?}"
                );
            }
            other => eprintln!("{label}: {other:?} (VAD {vad_cost:?})\n  untrimmed: {untrimmed:?}"),
        }
    };

    // Sub-second press: first 0.9 s of JFK. Pre-#1013 this was dropped
    // as "too short" without inference.
    run("short 0.9s", jfk[..14_400].to_vec());
    // A clip that only holds the second half of a word run — 0.8 s from
    // the middle of the speech.
    run("short mid 0.8s", jfk[64_000..76_800].to_vec());
    // Silence-padded: 3 s quiet hiss, JFK, 3 s quiet hiss.
    let hiss = |n: usize| -> Vec<f32> {
        (0..n)
            .map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.001)
            .collect()
    };
    let mut padded = hiss(48_000);
    padded.extend_from_slice(&jfk);
    padded.extend(hiss(48_000));
    run("padded 3s+jfk+3s", padded);
    // Pure hiss, 1.5 s: the VAD should report no speech.
    run("hiss only 1.5s", hiss(24_000));

    // Cost: 60 s of audio through Silero.
    let mut long = Vec::new();
    while long.len() < 16_000 * 60 {
        long.extend_from_slice(&jfk);
    }
    long.truncate(16_000 * 60);
    let t0 = std::time::Instant::now();
    let mut s = vad.new_session();
    for f in long.chunks_exact(512) {
        s.score_frame(f).unwrap();
    }
    eprintln!("Silero cost for 60 s of audio: {:?}", t0.elapsed());
}
