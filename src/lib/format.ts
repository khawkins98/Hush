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
 *  for history rows, meeting sessions, etc. */
export function formatTimestamp(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString();
}
