<!--
  Meeting auto-copy notice (#432 main-page decomp slice 2/3).

  Owns the transient "Copied to clipboard / auto-copy failed"
  banner that surfaces above the History list after a meeting
  session stops. Pre-#432 this lived inline on `+page.svelte` as
  ~80 lines of state + helpers + render — bundled into a leaf so
  the orchestrator just sets `notice = {...}` and the timer +
  render + dismiss all live here.

  ## Why this is the bulk of the meeting "section"

  The original #432 description envisioned a fuller meeting
  section that owned `meeting_start_manual` + `lastMeetingId` + the
  source-failed listener. In practice the unified Record flow
  (#369) merged those into the click-driven dictation path —
  `meeting_start_manual` is now called from inside `startRecord()`
  based on Screen Recording health, and `lastMeetingId` is
  threaded through stop. Pulling that out of the dictation flow
  would entangle the seam more, not less. So this slice owns the
  one piece that genuinely is meeting-scoped UX: the auto-copy
  notice. The dictation slice (next) owns the click-time meeting
  upgrade.

  ## Auto-clear contract

  When `notice` flips to a non-null value the section starts a
  dwell timer (4 s for success, 10 s for failure — failure carries
  a recovery action the user has to discover). On expiry the
  section sets `notice = null`; the parent's bindable prop
  reflects that. Re-setting `notice` to a fresh value resets the
  timer (last-write-wins).

  Manual dismiss (✕ button) clears immediately. Pre-existing test-
  ids and CSS classes are preserved so e2e specs and styles need
  no updates.
-->
<script lang="ts" module>
  /// Wire shape for the auto-copy outcome notice. Exported as a
  /// named type so the parent can declare its bindable cell with
  /// the same shape that the section consumes.
  export type MeetingCopyNotice = {
    kind: "success" | "failure";
    message: string;
  };
</script>

<script lang="ts">
  import { onDestroy } from "svelte";
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";
  import { motionDuration } from "./motion";

  type Props = {
    /// Bindable. Parent sets to a new notice to surface; the
    /// section flips it back to `null` on dwell expiry or manual
    /// dismiss.
    notice: MeetingCopyNotice | null;
  };

  let { notice = $bindable() }: Props = $props();

  let timer: ReturnType<typeof setTimeout> | null = null;

  // Whenever a new notice appears, arm an auto-clear timer with
  // dwell calibrated to the kind. Failure dwells longer because
  // the message carries an action the user has to discover, not
  // just an acknowledgement. Re-setting to a fresh notice resets
  // the dwell — last write wins.
  $effect(() => {
    if (notice === null) return;
    if (timer !== null) clearTimeout(timer);
    const dwellMs = notice.kind === "success" ? 4000 : 10000;
    timer = setTimeout(() => {
      notice = null;
      timer = null;
    }, dwellMs);
  });

  function dismiss() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    notice = null;
  }

  onDestroy(() => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  });
</script>

<style>
  /* Auto-copy outcome notice (#408). Lives just above the History
     panel, gated on `notice` being set. Two visual variants drive
     off data-kind: success (green-tinted) auto-clears after 4 s,
     failure (amber-tinted) after 10 s. Dismiss button is a manual
     escape hatch in case the dwell feels long. Pulled into the
     section component during #432 — same rules, same selectors,
     just colocated with the markup that uses them. */
  .meeting-copy-notice {
    display: flex;
    align-items: flex-start;
    gap: 0.55rem;
    padding: 0.6rem 0.85rem;
    margin: 0 0 1rem;
    border-radius: 8px;
    font-size: 0.88rem;
    line-height: 1.4;
    border: 1px solid;
  }
  .meeting-copy-notice[data-kind="success"] {
    background-color: #e7f8ec;
    border-color: #b6e5c5;
    color: #2a6b3c;
  }
  .meeting-copy-notice[data-kind="failure"] {
    background-color: #fff7e6;
    border-color: #ffd591;
    color: #8a5a00;
  }
  .meeting-copy-notice-icon {
    font-weight: 700;
    flex-shrink: 0;
    line-height: 1.4;
  }
  .meeting-copy-notice-message {
    flex: 1;
    min-width: 0;
  }
  .meeting-copy-notice-dismiss {
    flex-shrink: 0;
    background: none;
    border: 0;
    padding: 0 0.25rem;
    font-size: 1.05rem;
    line-height: 1;
    cursor: pointer;
    color: inherit;
    /* Bumped from 0.6 → 0.75 (UX review on #415). The original
       was visually quiet enough that the dismiss path read as
       decoration; 0.75 reads as actionable without dominating the
       row. */
    opacity: 0.75;
  }
  .meeting-copy-notice-dismiss:hover {
    opacity: 1;
  }
  @media (prefers-color-scheme: dark) {
    .meeting-copy-notice[data-kind="success"] {
      background-color: rgba(46, 170, 83, 0.15);
      border-color: #2a6b3c;
      color: #b6e5c5;
    }
    .meeting-copy-notice[data-kind="failure"] {
      background-color: rgba(255, 193, 7, 0.12);
      border-color: #6b5300;
      color: #ffd591;
    }
  }
</style>

{#if notice}
  <!--
    Auto-copy outcome notice (#408). Sits above the History list
    so the failure variant's "open History below" copy points at
    exactly what the user sees next. role="status" for SR
    announcement; the dismiss button is a manual escape hatch in
    case the auto-clear timer feels too long mid-session.

    The warning glyph carries an explicit `︎` variation
    selector so macOS doesn't render it as a colour emoji
    (yellow triangle), which would fight the amber-tinted
    container. UX review on the original #415 caught that
    bare U+26A0 picks up emoji presentation by default on
    Apple platforms; the VS-15 selector forces text style.
  -->
  <div
    class="meeting-copy-notice"
    data-kind={notice.kind}
    role="status"
    data-testid="meeting-copy-notice"
    in:fly={{ y: -6, duration: motionDuration(200), easing: backOut }}
    out:fade={{ duration: motionDuration(150), easing: cubicIn }}
  >
    <span class="meeting-copy-notice-icon" aria-hidden="true">
      {notice.kind === "success" ? "✓" : "⚠︎"}
    </span>
    <span class="meeting-copy-notice-message">
      {notice.message}
    </span>
    <button
      type="button"
      class="meeting-copy-notice-dismiss"
      onclick={dismiss}
      aria-label="Dismiss notice"
    >
      ×
    </button>
  </div>
{/if}
