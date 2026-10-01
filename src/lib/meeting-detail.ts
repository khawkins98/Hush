// Merge logic for the live meeting transcript's incremental poll
// (`meeting_session_get_since`). Kept pure + framework-free so vitest can
// pin it without a Svelte runtime.
import type { MeetingSessionDetail, PersistedUtterance } from "./types";

/// Highest persisted utterance id held locally — the cursor for the next
/// `meeting_session_get_since` call. `utterances.id` is AUTOINCREMENT on
/// the Rust side, so "rows after this id" is exactly "rows appended since".
/// 0 when nothing is held yet (ids start at 1).
export function lastUtteranceId(utterances: PersistedUtterance[]): number {
  let max = 0;
  for (const u of utterances) if (u.id > max) max = u.id;
  return max;
}

/// Fold an incremental response into the previously held detail.
///
/// * New finals are appended and the list re-sorted by start time (ties
///   by id) to match the full fetch's `ORDER BY started_at_ms`: a final
///   from one source can start earlier than one already held from
///   another source.
/// * Partials and the session header are always replaced — the delta
///   carries them in full.
/// * Rows the cursor already covered are ignored, so a duplicate or
///   out-of-order response can't double a line.
///
/// Returns `prev` itself (same reference) when nothing changed, so the
/// caller can skip a `$state` write and the transcript re-join it would
/// trigger.
export function mergeDetailDelta(
  prev: MeetingSessionDetail,
  delta: MeetingSessionDetail,
): MeetingSessionDetail {
  const known = new Set(prev.utterances.map((u) => u.id));
  const fresh = delta.utterances.filter((u) => !known.has(u.id));
  if (
    fresh.length === 0
    && JSON.stringify(prev.session) === JSON.stringify(delta.session)
    && JSON.stringify(prev.currentPartials) === JSON.stringify(delta.currentPartials)
  ) {
    return prev;
  }
  const utterances = fresh.length === 0
    ? prev.utterances
    : [...prev.utterances, ...fresh].sort(
        (a, b) => a.startedAtMs - b.startedAtMs || a.id - b.id,
      );
  return {
    session: delta.session,
    utterances,
    currentPartials: delta.currentPartials,
  };
}
