<!--
  Meeting auto-copy notice (#408): "Copied to clipboard /
  auto-copy failed" banner surfaced above the History list after
  a meeting session stops.

  Auto-clear contract: parent assigns `notice = {...}` to surface;
  section arms a dwell timer (4 s success, 10 s failure — failure
  needs longer because the message carries a recovery action) and
  flips the cell back to `null` on expiry. Re-assigning resets
  the timer (last-write-wins). Manual ✕ clears immediately.
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
    background-color: var(--success-bg);
    border-color: var(--success-border);
    color: var(--success-text);
  }
  .meeting-copy-notice[data-kind="failure"] {
    background-color: var(--warning-bg);
    border-color: var(--warning-border);
    color: var(--warning-text);
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
</style>

{#if notice}
  <!--
    The warning glyph carries an explicit `︎` variation
    selector so macOS doesn't render it as a colour emoji (which
    would fight the amber-tinted failure container). UX review
    on #415 caught that bare U+26A0 picks up emoji presentation
    by default on Apple platforms.
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
