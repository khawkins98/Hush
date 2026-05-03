/**
 * Motion utilities for spring/fade transitions (#411 phase F6).
 *
 * Svelte's built-in `fade` / `fly` transitions don't read
 * `prefers-reduced-motion` on their own — they animate regardless.
 * This module wraps the OS preference behind a single helper so
 * every transition site collapses to a 0 ms (effectively
 * synchronous) duration when the user has motion reduced, without
 * each call site re-deriving the matchMedia query.
 *
 * Returns 0 in non-DOM contexts (SSR, tests where matchMedia isn't
 * polyfilled) so callers don't need to guard.
 */
export function prefersReducedMotion(): boolean {
  if (typeof window === "undefined") return false;
  if (typeof window.matchMedia !== "function") return false;
  try {
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}

/**
 * Default fade/fly duration for the F6 transitions. Spec calls for
 * 150–200 ms; 180 lands in the middle and reads as "snappy but not
 * abrupt" against the 60 Hz frame budget. Collapses to 0 under
 * reduced-motion so the transition is effectively synchronous.
 */
export function motionDuration(base = 180): number {
  return prefersReducedMotion() ? 0 : base;
}
