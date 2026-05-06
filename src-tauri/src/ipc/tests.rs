//! Tests for the IPC layer's state, builder, and pipeline orchestration.
//!
//! Pinned here as a peer of `mod.rs` (rather than inline) so the front door
//! stays minimal. Tests reach into the split modules via `super::*`, which
//! mod.rs re-exports the necessary names through.

use std::sync::Arc;

use anyhow::anyhow;

use super::pipeline::{
    is_huggingface_host, redirect_decision, RedirectDecision, MAX_DOWNLOAD_REDIRECTS,
};
use super::state::{
    decode_autostart_mode, encode_autostart_mode, parse_diarization_enabled_setting,
    parse_hud_enabled_setting,
};
use super::*;
use crate::audio::{AudioCapture, AudioDevice, CaptureFormat, CapturedAudio};
use crate::history::HistoryRepository;
use crate::settings::SettingsRepository;
use crate::transcription::Transcribe;

/// Mock that returns a fixed [`CapturedAudio`] from `stop`. `start` and
/// `is_recording` keep just enough state for tests to assert on.
struct MockAudio {
    captured: CapturedAudio,
    recording: std::sync::atomic::AtomicBool,
}

impl MockAudio {
    fn new(captured: CapturedAudio) -> Self {
        Self {
            captured,
            recording: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl AudioCapture for MockAudio {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![AudioDevice {
            id: "mock".into(),
            name: "Mock Mic".into(),
            is_default: true,
        }])
    }
    fn start(&self, _device_id: Option<&str>) -> anyhow::Result<()> {
        self.recording
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        self.recording
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(self.captured.clone())
    }
    fn is_recording(&self) -> bool {
        self.recording.load(std::sync::atomic::Ordering::Acquire)
    }
}

struct EchoTranscribe {
    text: String,
}

impl Transcribe for EchoTranscribe {
    fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
        Ok(self.text.clone())
    }
}

struct FailingTranscribe;

impl Transcribe for FailingTranscribe {
    fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
        Err(anyhow!("model exploded"))
    }
}

fn fake_audio() -> CapturedAudio {
    CapturedAudio {
        samples: vec![0.0_f32; 4],
        format: CaptureFormat {
            sample_rate: 48_000,
            channels: 1,
        },
    }
}

#[test]
fn run_pipeline_trims_whitespace_from_model_output() {
    let audio = MockAudio::new(fake_audio());
    let transcribe = EchoTranscribe {
        text: "  hello world\n".into(),
    };
    let text = run_pipeline(&audio, &transcribe).unwrap();
    assert_eq!(text, "hello world");
}

#[test]
fn run_pipeline_propagates_audio_stop_failure() {
    struct BrokenAudio;
    impl AudioCapture for BrokenAudio {
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

    let err = run_pipeline(&BrokenAudio, &EchoTranscribe { text: "x".into() })
        .unwrap_err()
        .to_string();
    assert!(err.contains("device went away"), "got: {err}");
}

#[test]
fn run_pipeline_propagates_transcription_failure() {
    let audio = MockAudio::new(fake_audio());
    let err = run_pipeline(&audio, &FailingTranscribe)
        .unwrap_err()
        .to_string();
    assert!(err.contains("model exploded"), "got: {err}");
}

/// Tiny mock for unit tests that need an `Arc<dyn HistoryRepository>`
/// in `AppState` but don't exercise its methods. Pinning the count to
/// `0` keeps surface minimal — tests that need real behaviour against
/// the SQLite-backed impl call the repository directly.
pub(crate) struct NoopHistory;

#[async_trait::async_trait]
impl HistoryRepository for NoopHistory {
    async fn create(&self, _: crate::history::NewHistoryEntry) -> anyhow::Result<i64> {
        Ok(0)
    }
    async fn list(&self, _: i64, _: i64) -> anyhow::Result<Vec<crate::history::HistoryEntry>> {
        Ok(vec![])
    }
    async fn search(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> anyhow::Result<Vec<crate::history::HistoryEntry>> {
        Ok(vec![])
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
    async fn clear(&self) -> anyhow::Result<i64> {
        Ok(0)
    }
    async fn count(&self) -> anyhow::Result<i64> {
        Ok(0)
    }
    async fn get_stats(&self) -> anyhow::Result<crate::history::DictationStats> {
        Ok(crate::history::DictationStats::default())
    }
}

/// Tiny mock that returns an empty rules list so the dictation
/// pipeline behaves as if no replacements are configured. Tests that
/// need actual replacement behaviour use the SQLite-backed repo
/// directly rather than mocking the trait.
pub(crate) struct NoopReplacements;

#[async_trait::async_trait]
impl
    crate::repository::Repository<
        crate::dictionary::ReplacementRule,
        crate::dictionary::NewReplacementRule,
        i64,
    > for NoopReplacements
{
    async fn list(&self) -> anyhow::Result<Vec<crate::dictionary::ReplacementRule>> {
        Ok(vec![])
    }
    async fn create(
        &self,
        _: crate::dictionary::NewReplacementRule,
    ) -> anyhow::Result<crate::dictionary::ReplacementRule> {
        unreachable!("mock does not exercise create")
    }
    async fn update(&self, _: crate::dictionary::ReplacementRule) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Same shape as [`NoopReplacements`] / [`NoopHistory`] — empty
/// list so the dictation pipeline behaves as if no vocab terms are
/// configured. Tests that need real behaviour against the SQLite-
/// backed repo call it directly.
pub(crate) struct NoopVocabulary;

#[async_trait::async_trait]
impl
    crate::repository::Repository<
        crate::dictionary::VocabularyTerm,
        crate::dictionary::NewVocabularyTerm,
        i64,
    > for NoopVocabulary
{
    async fn list(&self) -> anyhow::Result<Vec<crate::dictionary::VocabularyTerm>> {
        Ok(vec![])
    }
    async fn create(
        &self,
        _: crate::dictionary::NewVocabularyTerm,
    ) -> anyhow::Result<crate::dictionary::VocabularyTerm> {
        unreachable!("mock does not exercise create")
    }
    async fn update(&self, _: crate::dictionary::VocabularyTerm) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Noop meeting-session repository — returns empty lists, eats
/// inserts. Tests that exercise the IPC layer don't need real
/// persistence here today; the streaming pump that actually
/// writes to it lands in #110.
pub(crate) struct NoopMeetings;

#[async_trait::async_trait]
impl
    crate::repository::Repository<
        crate::meeting::MeetingSession,
        crate::meeting::NewMeetingSession,
        i64,
    > for NoopMeetings
{
    async fn list(&self) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
        Ok(vec![])
    }
    async fn create(
        &self,
        _: crate::meeting::NewMeetingSession,
    ) -> anyhow::Result<crate::meeting::MeetingSession> {
        unreachable!("mock does not exercise create")
    }
    async fn update(&self, _: crate::meeting::MeetingSession) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::meeting::MeetingSessionRepository for NoopMeetings {
    async fn close_session(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
    async fn append_utterance(
        &self,
        _: crate::meeting::NewPersistedUtterance,
    ) -> anyhow::Result<crate::meeting::PersistedUtterance> {
        unreachable!("mock does not exercise append_utterance")
    }
    async fn list_utterances(
        &self,
        _: i64,
    ) -> anyhow::Result<Vec<crate::meeting::PersistedUtterance>> {
        Ok(vec![])
    }
    async fn set_notes(&self, _: i64, _: Option<String>) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_by_id(&self, _: i64) -> anyhow::Result<Option<crate::meeting::MeetingSession>> {
        Ok(None)
    }
    async fn list_open_sessions(&self) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
        Ok(vec![])
    }
    async fn search_sessions(
        &self,
        _: &str,
    ) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
        Ok(vec![])
    }
}

/// Test mock for the meeting-app overrides repo (#112). Returns
/// an empty list so the classifier falls through to the static
/// defaults — same behaviour the pre-#112 IPC layer exhibited.
pub(crate) struct NoopMeetingAppOverrides;

#[async_trait::async_trait]
impl crate::meeting::MeetingAppOverrideRepository for NoopMeetingAppOverrides {
    async fn list(&self) -> anyhow::Result<Vec<crate::meeting::MeetingAppOverride>> {
        Ok(vec![])
    }
    async fn upsert(
        &self,
        _: crate::meeting::NewMeetingAppOverride,
    ) -> anyhow::Result<crate::meeting::MeetingAppOverride> {
        unreachable!("mock does not exercise upsert")
    }
    async fn set_profile(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> anyhow::Result<crate::meeting::MeetingAppOverride> {
        unreachable!("mock does not exercise set_profile")
    }
    async fn delete(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// In-memory settings store backed by a HashMap. Lighter than
/// spinning up a SQLite for tests that just need to round-trip a
/// few keys.
pub(crate) struct MemSettings {
    pub map: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[async_trait::async_trait]
impl SettingsRepository for MemSettings {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.map.lock().unwrap().get(key).cloned())
    }
    async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }
    async fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.map.lock().unwrap().remove(key);
        Ok(())
    }
}

pub(crate) fn mock_state() -> AppState {
    AppStateBuilder::new()
        .audio(Arc::new(MockAudio::new(fake_audio())))
        .history(Arc::new(NoopHistory))
        .replacements(Arc::new(NoopReplacements))
        .vocabulary(Arc::new(NoopVocabulary))
        .settings(Arc::new(MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("mock_state: builder fields complete")
}

#[test]
fn appstate_can_be_constructed_with_no_transcriber() {
    // Mirrors the runtime path where `--features whisper` is off or
    // `HUSH_MODEL_PATH` is unset: the app boots, device enumeration
    // works, and the IPC layer surfaces `TranscriptionUnavailable` on
    // stop. We just check construction here; the unavailable behaviour
    // is exercised by the `commands` module's runtime path.
    let state = mock_state();
    assert!(state.transcribe.lock().unwrap().is_none());
}

#[test]
fn swap_transcriber_replaces_the_inner_arc_and_returns_previous() {
    // Round-7 architecture reviewer flagged that `swap_transcriber`
    // (called from `model_select` when the user picks a new model
    // with a downloaded file) had no test coverage. Pin the
    // observable contract: the new value lands inside the Mutex,
    // and the previous value is returned so the caller can drop
    // it explicitly if it cares to. A future change that
    // accidentally swaps in an async lock or a different replacement
    // strategy would fail this.

    struct StubTranscriber {
        label: &'static str,
    }
    impl crate::transcription::Transcribe for StubTranscriber {
        fn transcribe(&self, _: &crate::audio::CapturedAudio) -> anyhow::Result<String> {
            Ok(String::new())
        }
        fn model_label(&self) -> String {
            self.label.to_owned()
        }
    }

    let state = mock_state();
    // mock_state() leaves both slots = None (no model loaded).
    assert!(state.transcribe.lock().unwrap().is_none());
    assert!(state.transcribe_meeting.lock().unwrap().is_none());

    let first_d: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "first" });
    let first_m: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "first" });
    let prev = state
        .swap_transcriber(Some(first_d), Some(first_m))
        .expect("first swap succeeds");
    assert!(prev.is_none(), "previous was None (mock_state baseline)");

    // Now confirm the swap actually landed in both slots.
    {
        let guard = state.transcribe.lock().unwrap();
        assert_eq!(
            guard.as_ref().map(|t| t.model_label()),
            Some("first".to_owned()),
            "new transcriber readable from the dictation slot"
        );
    }
    {
        let guard = state.transcribe_meeting.lock().unwrap();
        assert_eq!(
            guard.as_ref().map(|t| t.model_label()),
            Some("first".to_owned()),
            "new transcriber readable from the meeting slot"
        );
    }

    // Second swap returns the first one as the "previous" value
    // (from the dictation slot — the meeting-slot prev is dropped
    // on the floor as documented).
    let second_d: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "second" });
    let second_m: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "second" });
    let prev = state
        .swap_transcriber(Some(second_d), Some(second_m))
        .expect("second swap succeeds");
    assert_eq!(
        prev.map(|t| t.model_label()),
        Some("first".to_owned()),
        "previous dictation value returned to caller"
    );
    assert_eq!(
        state
            .transcribe
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.model_label()),
        Some("second".to_owned()),
        "second dictation transcriber landed"
    );
    assert_eq!(
        state
            .transcribe_meeting
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.model_label()),
        Some("second".to_owned()),
        "second meeting transcriber landed"
    );

    // Swap to None to confirm the unload path works for both slots.
    let prev = state
        .swap_transcriber(None, None)
        .expect("clear swap succeeds");
    assert_eq!(prev.map(|t| t.model_label()), Some("second".to_owned()));
    assert!(state.transcribe.lock().unwrap().is_none());
    assert!(state.transcribe_meeting.lock().unwrap().is_none());
}

#[test]
fn appstate_builder_errors_descriptively_on_missing_required_field() {
    // Round-7 architecture reviewer flagged that the builder's
    // self-documenting error messages had no test coverage. A future
    // refactor that "fixed" the error message wording (or stopped
    // ok_or_else'ing entirely) would silently regress the developer
    // experience of "I forgot a field — the error tells me which one."
    // Spot-check one required field. The message format ("audio not
    // set") is part of the developer-facing contract — pin it.
    let result = AppStateBuilder::new().build();
    // AppState doesn't implement Debug, so we can't use
    // `expect_err`; match on the Result instead.
    let err = match result {
        Ok(_) => panic!("empty builder must error, got Ok"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("audio not set"),
        "error must name the first missing required field; got: {msg}"
    );
}

#[test]
fn huggingface_host_predicate_accepts_apex_and_subdomains() {
    // Pin the load-bearing security check: the download redirect
    // policy treats these as in-zone. Both `huggingface.co` and
    // `hf.co` are HF-owned; the Xet CDN that HF migrated large-
    // file serving to in 2025 lives on the `hf.co` zone (e.g.
    // `cas-bridge.xethub.hf.co`), not `huggingface.co`.
    assert!(is_huggingface_host(Some("huggingface.co")));
    assert!(is_huggingface_host(Some("cdn-lfs.huggingface.co")));
    assert!(is_huggingface_host(Some("cdn-lfs-us-1.huggingface.co")));
    assert!(is_huggingface_host(Some("hf.co")));
    assert!(is_huggingface_host(Some("xethub.hf.co")));
    assert!(is_huggingface_host(Some("cas-bridge.xethub.hf.co")));
}

#[test]
fn huggingface_host_predicate_rejects_typosquats_and_lookalikes() {
    // Regression for "ends_with" naivety: `evilhuggingface.co`
    // (no leading dot) is not in zone but a sloppy `ends_with`
    // without the dot would accept it. The predicate must also
    // reject obvious off-domain hosts.
    assert!(!is_huggingface_host(Some("evilhuggingface.co")));
    assert!(!is_huggingface_host(Some("huggingface.co.attacker.com")));
    assert!(!is_huggingface_host(Some("attacker.com")));
    // hf.co-zone equivalents of the same trap.
    assert!(!is_huggingface_host(Some("myhf.co")));
    assert!(!is_huggingface_host(Some("hf.co.attacker.com")));
    assert!(!is_huggingface_host(Some("")));
    assert!(!is_huggingface_host(None));
}

/// Helper: build a `reqwest::Url` for the redirect tests below.
fn url(s: &str) -> reqwest::Url {
    reqwest::Url::parse(s).expect("test URL parses")
}

#[test]
fn redirect_decision_allows_hop_within_hf_zone() {
    // Common case: huggingface.co → cas-bridge.xethub.hf.co.
    let prev = vec![url("https://huggingface.co/foo")];
    let dest = url("https://cas-bridge.xethub.hf.co/bar");
    assert_eq!(redirect_decision(&prev, &dest), RedirectDecision::Follow);
}

#[test]
fn redirect_decision_allows_hf_to_signed_cdn() {
    // The whole reason this PR exists (#258): HF redirects to
    // a signed AWS / Cloudflare URL outside the HF zone.
    let prev = vec![
        url("https://huggingface.co/foo"),
        url("https://cas-bridge.xethub.hf.co/bar"),
    ];
    let dest = url("https://hf-cdn.s3.amazonaws.com/weights.gguf?X-Amz-Signature=abc123");
    assert_eq!(redirect_decision(&prev, &dest), RedirectDecision::Follow);
}

#[test]
fn redirect_decision_allows_first_hop_hf_to_signed_cdn() {
    // Single-hop variant: HF immediately redirects to the
    // signed URL with no in-zone intermediary.
    let prev = vec![url("https://huggingface.co/resolve/main/foo.gguf")];
    let dest = url("https://r2-signed.cloudflarestorage.com/x?sig=abc");
    assert_eq!(redirect_decision(&prev, &dest), RedirectDecision::Follow);
}

#[test]
fn redirect_decision_blocks_chain_extension_from_signed_url() {
    // After we've hopped to a signed CDN URL, that URL's host
    // is no longer trusted to redirect us further. If the CDN
    // tries to send us to attacker.com, deny.
    let prev = vec![
        url("https://huggingface.co/foo"),
        url("https://hf-cdn.s3.amazonaws.com/x?sig=abc"),
    ];
    let dest = url("https://attacker.com/evil.gguf");
    match redirect_decision(&prev, &dest) {
        RedirectDecision::Stop(reason) => {
            assert!(
                reason.contains("non-HF host"),
                "non-HF → non-HF should be blocked, got: {reason}"
            );
        }
        d => panic!("expected Stop, got {d:?}"),
    }
}

#[test]
fn redirect_decision_blocks_http_downgrade() {
    // Defence-in-depth: an HF host telling us to downgrade
    // to plain http:// is rejected, not followed. We don't
    // trust HF (or anyone) to send us cleartext.
    let prev = vec![url("https://huggingface.co/foo")];
    let dest = url("http://huggingface.co/foo"); // http not https
    match redirect_decision(&prev, &dest) {
        RedirectDecision::Stop(reason) => {
            assert!(reason.contains("non-HTTPS"), "got: {reason}");
        }
        d => panic!("expected Stop for http://, got {d:?}"),
    }
}

#[test]
fn redirect_decision_caps_at_max_redirects() {
    // The hop-count cap fires before host checks so a chain
    // that's legitimate at every hop still terminates.
    let prev: Vec<reqwest::Url> = (0..MAX_DOWNLOAD_REDIRECTS)
        .map(|i| url(&format!("https://huggingface.co/hop-{i}")))
        .collect();
    let dest = url("https://huggingface.co/final");
    match redirect_decision(&prev, &dest) {
        RedirectDecision::Stop(reason) => {
            assert!(reason.contains("too many"), "got: {reason}");
        }
        d => panic!("expected Stop for over-cap, got {d:?}"),
    }
}

#[test]
fn redirect_decision_blocks_non_hf_origin() {
    // Unlikely path (the request started at HF and reqwest
    // wouldn't manufacture a fresh non-HF origin), but pin it
    // anyway: zero-length previous + non-HF destination is a
    // straight reject.
    let prev: Vec<reqwest::Url> = vec![];
    let dest = url("https://attacker.com/evil.gguf");
    match redirect_decision(&prev, &dest) {
        RedirectDecision::Stop(_) => {}
        d => panic!("expected Stop for empty-prev + non-HF, got {d:?}"),
    }
}

#[test]
fn parse_hud_enabled_setting_handles_all_branches() {
    // Absent row → on. First-time users must see the HUD even
    // before they have ever touched the toggle.
    assert!(parse_hud_enabled_setting(None));
    // Literal "false" → off. The only string that turns it off.
    assert!(!parse_hud_enabled_setting(Some("false".into())));
    // Literal "true" → on.
    assert!(parse_hud_enabled_setting(Some("true".into())));
    // Anything else falls through to on. Defends against a
    // settings-table corruption that scribbled garbage into
    // the row — silently turning the HUD off for that user
    // would be worse than silently re-enabling.
    assert!(parse_hud_enabled_setting(Some("garbage".into())));
    assert!(parse_hud_enabled_setting(Some("".into())));
    assert!(parse_hud_enabled_setting(Some("True".into())));
    assert!(parse_hud_enabled_setting(Some("FALSE".into())));
}

#[test]
fn parse_diarization_enabled_setting_handles_all_branches() {
    // Absent row → on (#478 default flip). The wespeaker model
    // is bundled into the first-run download flow, so by the
    // time this is read on a fresh install the model is on
    // disk; existing users with an explicit `"false"` row
    // keep their preference (the round-trip below pins that).
    assert!(parse_diarization_enabled_setting(None));
    // Literal "true" → on.
    assert!(parse_diarization_enabled_setting(Some("true".into())));
    // Literal "false" → off. Critical: an existing user who
    // explicitly toggled diarization OFF before #478 has
    // exactly this row, and the upgrade must respect it.
    assert!(!parse_diarization_enabled_setting(Some("false".into())));
    // Anything else falls through to the absent-row default
    // (now `true`) — same fallthrough policy other settings
    // (`hud_enabled`) use for corrupted rows.
    assert!(parse_diarization_enabled_setting(Some("garbage".into())));
    assert!(parse_diarization_enabled_setting(Some("".into())));
    assert!(parse_diarization_enabled_setting(Some("True".into())));
    assert!(parse_diarization_enabled_setting(Some("1".into())));
}

#[test]
fn autostart_mode_round_trips_through_atomic_encoding() {
    use crate::meeting::MeetingAutostartMode;
    // Every defined variant must encode + decode back to itself.
    for mode in [MeetingAutostartMode::Off, MeetingAutostartMode::Always] {
        let byte = encode_autostart_mode(mode);
        assert_eq!(decode_autostart_mode(byte), mode, "round-trip for {mode:?}");
    }
}

#[test]
fn autostart_mode_decode_falls_back_to_off_for_unknown_bytes() {
    use crate::meeting::MeetingAutostartMode;
    // A future variant added to the enum but not yet known to a
    // stale build (or a corrupted atomic from some unforeseen
    // path) must read as `Off` — the safer default. Nobody wants
    // their mic to spontaneously turn on because of a byte the
    // decoder didn't recognise.
    for byte in [2u8, 3, 7, 42, 99, 255] {
        assert_eq!(
            decode_autostart_mode(byte),
            MeetingAutostartMode::Off,
            "unknown byte {byte} should decode to Off"
        );
    }
}
