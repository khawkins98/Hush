/**
 * Shared formatting helpers for surfaces that show backend metadata.
 *
 * Created in #589 to consolidate `formatBuildTimestamp` (originally
 * duplicated across `DebugTab.svelte`, `routes/debug/+page.svelte`,
 * and `AboutTab.svelte`).
 */

/** Wire shape of `crate::ipc::commands::system::get_build_info`. */
export type BuildInfo = {
  version: string;
  /** Unix epoch seconds — set by `build.rs` at last compile time. */
  buildTimestamp: number;
};

const MONTHS = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

/**
 * Render a Unix-epoch-seconds value as `D Month, YYYY HH:MM UTC`.
 * Always UTC — build timestamps are epoch values with no local context.
 * Returns the literal string `"unknown"` for `0`, which is the sentinel
 * `get_build_info` returns when `SystemTime::now()` failed in build.rs.
 */
export function formatBuildTimestamp(unixSecs: number): string {
  if (unixSecs === 0) return "unknown";
  const d = new Date(unixSecs * 1000);
  const day = d.getUTCDate();
  const month = MONTHS[d.getUTCMonth()];
  const yyyy = d.getUTCFullYear();
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const min = String(d.getUTCMinutes()).padStart(2, "0");
  return `${day} ${month}, ${yyyy} ${hh}:${min} UTC`;
}
