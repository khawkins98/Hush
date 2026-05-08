// Small formatting helpers shared between the main window and the
// standalone Settings window. Each window owns its own Svelte tree
// and `import` from this module to avoid duplicating one-line
// helpers in two places.

/** Render a byte count as megabytes with one decimal — used by the
 *  model picker for download sizes and progress lines. */
export function formatMb(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** Format an ISO-8601 timestamp using the host locale. The backend
 *  stores `YYYY-MM-DDTHH:MM:SSZ`; this is the user-facing rendering
 *  for history rows, meeting sessions, etc.
 *  Intentionally omits seconds — they add noise without informational value. */
export function formatTimestamp(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

/** Render a millisecond duration as a compact human string. Used by
 *  the dictation result block to surface "Recorded for X.Xs" — and
 *  matches the same shape `HistoryPanel.formatDuration` produces, so
 *  the two surfaces don't drift. Sub-second clips get one decimal so
 *  a 0.4s mis-press is visibly different from a 4s real recording.
 *  ≥1 minute uses m:ss.
 *
 *  Returns `null` for null / negative input so callers can `{#if}`
 *  the surrounding affordance away cleanly. */
export function formatDuration(ms: number | null): string | null {
  if (ms === null || ms < 0) return null;
  if (ms < 1000) return `${(ms / 1000).toFixed(1)}s`;
  const totalSeconds = Math.round(ms / 1000);
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}
