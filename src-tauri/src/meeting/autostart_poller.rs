//! Autostart poller wiring (#112) — extracted from `lib.rs` so the
//! tick-by-tick orchestration is unit-testable without spinning up
//! a Tauri runtime.
//!
//! The poller does three things on every tick:
//! 1. Snapshot the foreground app via [`ForegroundAppProbe`].
//! 2. Run the result through the [`AppClassifier`].
//! 3. Hand the verdict to [`AutostartDecision::decide`] alongside
//!    the previous tick's kind, the current mode, and whether a
//!    session is already active.
//!
//! The pure decision logic lives in [`super::autostart`]. This
//! module owns the *wiring*: probe-error handling, off-mode memory
//! reset, and the conversion from a `decide` verdict into a
//! [`TickOutcome`] the caller can either log + ignore or act on
//! by calling `start_manual`.
//!
//! `lib.rs::run_meeting_autostart_poller` is the production caller.
//! Tests stub the probe via the trait and assert the outcomes.
//!
//! ## Why a trait, not a closure
//!
//! `ForegroundAppProbe` could be `Fn() -> Option<String>`, but the
//! production impl wraps `active_win_pos_rs::get_active_window`
//! which is a synchronous OS call; a trait keeps the type explicit
//! and lets the caller swap in a stub `Vec<String>`-returning probe
//! in tests without juggling closure types.

use super::autostart::{AutostartDecision, MeetingAutostartMode};
use super::classifier::AppClassifier;
use super::MeetingAppKind;

/// Snapshot of the currently-focused app.
///
/// Implementations return `None` when no usable foreground app is
/// available (lock screen, full-screen game, OS-level transient
/// where `active-win-pos-rs` errors out). Returning `None` keeps
/// the poller's `last_kind` memory unchanged so a brief gap doesn't
/// re-trigger an autostart on the next "real" focus.
pub trait ForegroundAppProbe: Send + Sync {
    fn current_app_name(&self) -> Option<String>;
}

/// Outcome of a single poller tick. The caller is responsible for
/// any IO — this enum is the boundary between the pure-Rust
/// classification work and the side-effecting `start_manual` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    /// Nothing to do; keep the same `last_kind`. Covers probe
    /// failure, no-transition steady states, and session-active
    /// guard.
    NoChange,
    /// Mode is `Off`. Caller should clear its `last_kind` memory
    /// so flipping the mode back on later doesn't re-trigger on a
    /// long-stale verdict.
    ResetMemory,
    /// Auto-start a session for `app_name`. The new
    /// `last_kind` (always `Meeting` here) is returned alongside
    /// so the caller can update its memory in one place.
    Start {
        app_name: String,
        last_kind: MeetingAppKind,
    },
    /// Same as `NoChange` but updates the caller's `last_kind` to
    /// the kind we observed this tick. Used when we classified the
    /// foreground app but didn't decide to start (e.g. transition
    /// into Media, or steady-state Meeting).
    UpdateMemory { last_kind: MeetingAppKind },
}

/// Evaluate one poller tick. Returns the [`TickOutcome`] the loop
/// in `lib.rs::run_meeting_autostart_poller` should act on.
///
/// Pure logic — no IO beyond the `probe.current_app_name()` call.
/// All inputs are explicit so tests can construct any state.
pub fn evaluate_autostart_tick(
    probe: &dyn ForegroundAppProbe,
    classifier: &AppClassifier,
    last_kind: Option<MeetingAppKind>,
    mode: MeetingAutostartMode,
    session_active: bool,
) -> TickOutcome {
    if mode == MeetingAutostartMode::Off {
        return TickOutcome::ResetMemory;
    }

    let Some(app_name) = probe.current_app_name() else {
        return TickOutcome::NoChange;
    };

    let kind = classifier.classify(&app_name);
    let decision = AutostartDecision::decide(last_kind, kind, &app_name, mode, session_active);

    match decision {
        AutostartDecision::Start { app_name } => TickOutcome::Start {
            app_name,
            last_kind: kind,
        },
        AutostartDecision::Ignore => TickOutcome::UpdateMemory { last_kind: kind },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test probe that returns canned values, optionally cycling
    /// through a queue across calls so a test can simulate "user
    /// switches focus from Slack to Zoom on tick 2".
    struct StubProbe {
        responses: Mutex<Vec<Option<String>>>,
    }

    impl StubProbe {
        fn always(name: &str) -> Self {
            Self {
                responses: Mutex::new(vec![Some(name.to_owned())]),
            }
        }
        fn never() -> Self {
            Self {
                responses: Mutex::new(vec![None]),
            }
        }
    }

    impl ForegroundAppProbe for StubProbe {
        fn current_app_name(&self) -> Option<String> {
            let mut q = self.responses.lock().unwrap();
            // Return the first response; if there's only one, keep
            // returning it (steady-state).
            if q.len() > 1 {
                q.remove(0)
            } else {
                q.first().cloned().unwrap_or(None)
            }
        }
    }

    fn classifier_with(entries: &[(&str, MeetingAppKind)]) -> AppClassifier {
        AppClassifier::with_overrides(
            entries
                .iter()
                .map(|(name, kind)| ((*name).to_owned(), *kind))
                .collect(),
        )
    }

    #[test]
    fn off_mode_returns_reset_memory_regardless_of_probe() {
        let probe = StubProbe::always("Zoom");
        let cls = classifier_with(&[]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Other),
            MeetingAutostartMode::Off,
            false,
        );
        assert_eq!(outcome, TickOutcome::ResetMemory);
    }

    #[test]
    fn probe_returning_none_yields_no_change() {
        // Probe returning None (lock screen, etc.) must not churn
        // last_kind; otherwise a transient gap re-triggers on the
        // next "real" focus.
        let probe = StubProbe::never();
        let cls = classifier_with(&[]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Meeting),
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(outcome, TickOutcome::NoChange);
    }

    #[test]
    fn transition_into_meeting_yields_start() {
        // Slack (Other) → Zoom (Meeting) is the canonical
        // autostart trigger.
        let probe = StubProbe::always("Zoom");
        let cls = classifier_with(&[("Zoom", MeetingAppKind::Meeting)]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Other),
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(
            outcome,
            TickOutcome::Start {
                app_name: "Zoom".to_owned(),
                last_kind: MeetingAppKind::Meeting,
            }
        );
    }

    #[test]
    fn first_tick_meeting_yields_start() {
        // Fresh poller (last_kind=None) with a meeting app already
        // focused: still Start. Skipping would mean "user opens app,
        // launches Hush, nothing happens" which is the wrong UX.
        let probe = StubProbe::always("Microsoft Teams");
        let cls = classifier_with(&[("Microsoft Teams", MeetingAppKind::Meeting)]);
        let outcome =
            evaluate_autostart_tick(&probe, &cls, None, MeetingAutostartMode::Always, false);
        match outcome {
            TickOutcome::Start {
                app_name,
                last_kind,
            } => {
                assert_eq!(app_name, "Microsoft Teams");
                assert_eq!(last_kind, MeetingAppKind::Meeting);
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn steady_state_meeting_yields_update_memory_not_start() {
        // last_kind=Meeting, probe still says Meeting: do NOT
        // re-start. UpdateMemory keeps the caller's last_kind in
        // sync (which is already Meeting in this case, but the
        // path is the same as a freshly-classified Meeting tick).
        let probe = StubProbe::always("Zoom");
        let cls = classifier_with(&[("Zoom", MeetingAppKind::Meeting)]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Meeting),
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(
            outcome,
            TickOutcome::UpdateMemory {
                last_kind: MeetingAppKind::Meeting
            }
        );
    }

    #[test]
    fn session_active_blocks_start() {
        // User is mid-session; another autostart would double-open
        // the manager. session_active=true → Ignore branch in
        // AutostartDecision → UpdateMemory here (we still observed
        // the kind; we just don't act).
        let probe = StubProbe::always("Zoom");
        let cls = classifier_with(&[("Zoom", MeetingAppKind::Meeting)]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Other),
            MeetingAutostartMode::Always,
            true,
        );
        assert_eq!(
            outcome,
            TickOutcome::UpdateMemory {
                last_kind: MeetingAppKind::Meeting
            }
        );
    }

    #[test]
    fn unknown_app_classifies_to_other_and_yields_update_memory() {
        // Random app the classifier doesn't recognise — falls into
        // Other, no autostart, last_kind tracks the observation.
        let probe = StubProbe::always("SomeRandomApp");
        let cls = classifier_with(&[]);
        let outcome =
            evaluate_autostart_tick(&probe, &cls, None, MeetingAutostartMode::Always, false);
        assert_eq!(
            outcome,
            TickOutcome::UpdateMemory {
                last_kind: MeetingAppKind::Other
            }
        );
    }

    #[test]
    fn classifier_override_respected() {
        // User added a custom override classifying "MyMeetingApp"
        // as Meeting. The poller must honour it.
        let probe = StubProbe::always("MyMeetingApp");
        let cls = classifier_with(&[("MyMeetingApp", MeetingAppKind::Meeting)]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Other),
            MeetingAutostartMode::Always,
            false,
        );
        assert!(
            matches!(outcome, TickOutcome::Start { ref app_name, .. } if app_name == "MyMeetingApp")
        );
    }

    #[test]
    fn media_app_does_not_trigger_start() {
        // Spotify is Media (per default table). Transition into
        // Media is explicitly not an autostart trigger.
        let probe = StubProbe::always("Spotify");
        let cls = classifier_with(&[("Spotify", MeetingAppKind::Media)]);
        let outcome = evaluate_autostart_tick(
            &probe,
            &cls,
            Some(MeetingAppKind::Other),
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(
            outcome,
            TickOutcome::UpdateMemory {
                last_kind: MeetingAppKind::Media
            }
        );
    }
}
