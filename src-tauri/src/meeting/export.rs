//! Meeting session export formatters (#699).
//!
//! Extracted from `ipc::commands::meeting` so the format logic lives
//! in the domain layer rather than the IPC dispatch layer. The IPC
//! command `meeting_session_export` delegates format selection here.
//!
//! ## Why here and not in `ipc/commands/meeting.rs`
//!
//! These functions are pure domain transformations (session + utterances
//! → bytes); they have no dependency on `AppHandle`, `State`, or
//! `IpcError`. Co-locating them with the `meeting/` domain types means
//! a future CLI path, test harness, or alternate frontend can reuse
//! them without going through the IPC layer.

use serde::Deserialize;

/// Output format for a single meeting session export.
///
/// `serde(rename_all = "lowercase")` so the IPC wire accepts the
/// frontend's lowercase format strings (`"text"` / `"csv"` / `"json"`)
/// without an explicit converter.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetingExportFormat {
    /// Human-readable "send notes to a colleague" format.
    /// Header line + relative-time prefixed utterances.
    Text,
    /// One row per utterance, RFC-4180 escaped via the `csv` crate.
    /// Schema: utterance_id, session_id, started_at_ms, ended_at_ms,
    /// speaker_label, text. Speaker label is the rendered string
    /// (`You` / `Remote` / `Speaker N`) — no raw `mic` / `system`
    /// leakage per the #357 phase 3 acceptance.
    Csv,
    /// Full session metadata + utterance array. Pretty-printed for
    /// readability. The shape mirrors `MeetingSessionDetail` but
    /// with utterance speaker labels substituted for their rendered
    /// copy, again per the no-raw-`mic`/`system` rule.
    Json,
}

/// Render the per-row speaker label the same way
/// `HistoryMeetingRow.svelte::speakerCopy` does, so exports never
/// leak the raw source-derived `mic` / `system` tokens (#357 phase 3).
pub(crate) fn rendered_speaker_label(raw: Option<&str>) -> &str {
    match raw {
        Some("mic") => "You",
        Some("system") => "Remote",
        Some(other) => other,
        None => "Unknown",
    }
}

/// Format `started_at_ms` (relative to session start) as `[hh:mm:ss]`.
/// Relative time makes plain-text exports readable as a session-internal
/// timeline rather than a wall-clock log.
fn format_relative_timestamp(ms: i64) -> String {
    let total_secs = ms.max(0) / 1000;
    let hours = total_secs / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let seconds = total_secs % 60;
    format!("[{hours:02}:{minutes:02}:{seconds:02}]")
}

/// Plain-text "send notes to a colleague" format. Header line with
/// session metadata, blank line, utterances one per line with a
/// relative-time prefix and the rendered speaker label. Trailing
/// newline so the file ends cleanly when concatenated.
pub(crate) fn meeting_session_text(
    session: &super::MeetingSession,
    utterances: &[super::PersistedUtterance],
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} · started {} · {} utterance{}",
        session.app_name,
        session.started_at,
        session.utterance_count,
        if session.utterance_count == 1 {
            ""
        } else {
            "s"
        }
    );
    if let Some(sources) = &session.sources {
        if !sources.is_empty() {
            let _ = writeln!(out, "Sources: {}", sources.join(" + "));
        }
    }
    if let Some(notes) = &session.notes {
        if !notes.is_empty() {
            let _ = writeln!(out, "Notes: {}", notes);
        }
    }
    out.push('\n');

    for u in utterances {
        let _ = writeln!(
            out,
            "{} {}: {}",
            format_relative_timestamp(u.started_at_ms),
            rendered_speaker_label(u.speaker_label.as_deref()),
            u.text
        );
    }
    out
}

/// CSV with one row per utterance. `csv` crate does the RFC-4180
/// escape (quotes, commas, newlines in transcript text). Speaker
/// label is the rendered copy, not the raw token.
pub(crate) fn meeting_session_csv(
    session: &super::MeetingSession,
    utterances: &[super::PersistedUtterance],
) -> anyhow::Result<String> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record([
        "utterance_id",
        "session_id",
        "started_at_ms",
        "ended_at_ms",
        "speaker_label",
        "text",
    ])?;
    for u in utterances {
        wtr.write_record(&[
            u.id.to_string(),
            session.id.to_string(),
            u.started_at_ms.to_string(),
            u.ended_at_ms.to_string(),
            rendered_speaker_label(u.speaker_label.as_deref()).to_owned(),
            u.text.clone(),
        ])?;
    }
    let bytes = wtr.into_inner()?;
    Ok(String::from_utf8(bytes)?)
}

/// JSON with the full session metadata + utterance array. Pretty-printed
/// for readability — the file is meant to be human-inspectable. The
/// shape mirrors `MeetingSessionDetail` but with speaker labels
/// substituted for their rendered copy so raw `mic`/`system` tokens
/// never leak into user-facing files.
pub(crate) fn meeting_session_json(
    session: &super::MeetingSession,
    utterances: &[super::PersistedUtterance],
) -> anyhow::Result<String> {
    // Build a `serde_json::Value` rather than serializing the raw types
    // directly, so we can substitute rendered speaker labels without
    // leaking the raw `mic`/`system` token. The export schema stays
    // independent of the wire shape — re-arranging the wire shape
    // won't silently re-shape every meeting JSON file the user has
    // on disk.
    let utterances_json: Vec<serde_json::Value> = utterances
        .iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "started_at_ms": u.started_at_ms,
                "ended_at_ms": u.ended_at_ms,
                "speaker_label": rendered_speaker_label(u.speaker_label.as_deref()),
                "text": u.text,
            })
        })
        .collect();

    let envelope = serde_json::json!({
        "session": {
            "id": session.id,
            "app_name": session.app_name,
            "app_kind": session.app_kind,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
            "speaker_count": session.speaker_count,
            "utterance_count": session.utterance_count,
            "notes": session.notes,
            "sources": session.sources,
            "app_title": session.app_title,
        },
        "utterances": utterances_json,
    });
    Ok(serde_json::to_string_pretty(&envelope)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> crate::meeting::MeetingSession {
        crate::meeting::MeetingSession {
            id: 7,
            app_name: "Microsoft Teams".to_owned(),
            app_kind: crate::meeting::MeetingAppKind::Meeting,
            started_at: "2026-04-30T12:39:00Z".to_owned(),
            ended_at: Some("2026-04-30T13:39:00Z".to_owned()),
            speaker_count: Some(2),
            utterance_count: 3,
            notes: Some("Action: send recap".to_owned()),
            sources: Some(vec!["mic".to_owned(), "system".to_owned()]),
            app_title: Some("Q3 sync".to_owned()),
            name: None,
        }
    }

    fn sample_utterances() -> Vec<crate::meeting::PersistedUtterance> {
        vec![
            crate::meeting::PersistedUtterance {
                id: 100,
                session_id: 7,
                started_at_ms: 3_000,
                ended_at_ms: 4_500,
                speaker_label: Some("mic".to_owned()),
                text: "Hello everyone, thanks for joining.".to_owned(),
                is_final: true,
                speaker_identity_id: None,
            },
            crate::meeting::PersistedUtterance {
                id: 101,
                session_id: 7,
                started_at_ms: 9_200,
                ended_at_ms: 11_000,
                speaker_label: Some("system".to_owned()),
                text: "Hi! Can you share your screen?".to_owned(),
                is_final: true,
                speaker_identity_id: None,
            },
            crate::meeting::PersistedUtterance {
                id: 102,
                session_id: 7,
                started_at_ms: 65 * 1000 + 500,
                ended_at_ms: 67 * 1000,
                speaker_label: None,
                text: "Sure, one second.".to_owned(),
                is_final: true,
                speaker_identity_id: None,
            },
        ]
    }

    #[test]
    fn rendered_speaker_label_maps_source_tokens_and_passes_others_through() {
        assert_eq!(rendered_speaker_label(Some("mic")), "You");
        assert_eq!(rendered_speaker_label(Some("system")), "Remote");
        assert_eq!(rendered_speaker_label(Some("Speaker 1")), "Speaker 1");
        assert_eq!(rendered_speaker_label(None), "Unknown");
    }

    #[test]
    fn format_relative_timestamp_pads_each_field_to_two_digits() {
        assert_eq!(format_relative_timestamp(0), "[00:00:00]");
        assert_eq!(format_relative_timestamp(3_500), "[00:00:03]");
        assert_eq!(format_relative_timestamp(65 * 1000), "[00:01:05]");
        // Hour rollover — the meetings that need this exist; the
        // 7h-44m example is the load-bearing one.
        assert_eq!(
            format_relative_timestamp((7 * 3600 + 44 * 60) * 1000),
            "[07:44:00]"
        );
    }

    #[test]
    fn meeting_session_text_renders_header_and_utterances() {
        let body = meeting_session_text(&sample_session(), &sample_utterances());
        let lines: Vec<&str> = body.lines().collect();
        assert!(
            lines[0].starts_with("Microsoft Teams · started 2026-04-30T12:39:00Z"),
            "first line: {:?}",
            lines[0]
        );
        assert!(lines[0].ends_with("3 utterances"));
        assert!(
            lines.contains(&"Sources: mic + system"),
            "missing sources line: {body}"
        );
        assert!(
            lines.contains(&"Notes: Action: send recap"),
            "missing notes line: {body}"
        );
        // Speaker labels rendered, no raw `mic`/`system`.
        assert!(
            lines.iter().any(|l| l.contains("[00:00:03] You: Hello")),
            "expected `You` for mic-source utterance: {body}"
        );
        assert!(
            lines.iter().any(|l| l.contains("[00:00:09] Remote: Hi!")),
            "expected `Remote` for system-source utterance: {body}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[00:01:05] Unknown: Sure, one second.")),
            "expected `Unknown` for null-speaker utterance: {body}"
        );
    }

    #[test]
    fn meeting_session_csv_renders_one_row_per_utterance_with_rendered_labels() {
        let body = meeting_session_csv(&sample_session(), &sample_utterances()).expect("csv ok");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "header + 3 utterances; got: {body}");
        assert_eq!(
            lines[0],
            "utterance_id,session_id,started_at_ms,ended_at_ms,speaker_label,text"
        );
        assert!(
            lines[1].contains(",You,"),
            "row 1 should have `You`: {:?}",
            lines[1]
        );
        assert!(
            lines[2].contains(",Remote,"),
            "row 2 should have `Remote`: {:?}",
            lines[2]
        );
        assert!(
            lines[3].contains(",Unknown,"),
            "row 3 should have `Unknown`: {:?}",
            lines[3]
        );
    }

    #[test]
    fn meeting_session_json_substitutes_rendered_labels_in_envelope() {
        let body = meeting_session_json(&sample_session(), &sample_utterances()).expect("json ok");
        let value: serde_json::Value = serde_json::from_str(&body).expect("output parses as JSON");
        let session = value.get("session").expect("session field");
        assert_eq!(session["id"], 7);
        let uts = value.get("utterances").expect("utterances field");
        let arr = uts.as_array().expect("utterances is array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["speaker_label"], "You");
        assert_eq!(arr[1]["speaker_label"], "Remote");
        assert_eq!(arr[2]["speaker_label"], "Unknown");
        for (i, u) in arr.iter().enumerate() {
            let label = u["speaker_label"]
                .as_str()
                .expect("speaker_label is string");
            assert_ne!(
                label, "mic",
                "utterance {i} leaked raw `mic` into speaker_label"
            );
            assert_ne!(
                label, "system",
                "utterance {i} leaked raw `system` into speaker_label"
            );
        }
    }
}
