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

/**
 * Render a Unix-epoch-seconds value as `DD/MM/YYYY HH:MM` in the user's
 * local time. Returns the literal string `"unknown"` for `0`, which is
 * the sentinel `get_build_info` returns when the build script's
 * `SystemTime::now()` failed (extremely unlikely outside a broken
 * sandbox; pinned for predictability).
 */
export function formatBuildTimestamp(unixSecs: number): string {
  if (unixSecs === 0) return "unknown";
  const d = new Date(unixSecs * 1000);
  const dd = String(d.getDate()).padStart(2, "0");
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const yyyy = d.getFullYear();
  const hh = String(d.getHours()).padStart(2, "0");
  const min = String(d.getMinutes()).padStart(2, "0");
  return `${dd}/${mm}/${yyyy} ${hh}:${min}`;
}
