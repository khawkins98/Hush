<!--
  macOS permission status pill on the Dictation surface.

  Three render branches:
  - All three permissions granted → compact green "macOS
    permissions OK" pill with a "View" link to the diagnostic.
  - At least one denied → yellow recovery hint with a link to
    open the diagnostic (where the user can re-grant).
  - Any other state (mostly `not-determined` on a fresh install
    before the user has tried recording) → render nothing. The
    page would otherwise nag the user about a problem that
    doesn't exist yet.

  Renders nothing on non-macOS (`capable={false}`); the parent
  page is responsible for setting `capable` based on
  `diagnose_macos_permissions`'s `canReset` flag.

  Extracted from `+page.svelte` (#156 follow-up) — pulls ~40 LOC
  of branch-rendering markup + ~80 LOC of styles into a leaf
  component so the page focuses on orchestration.
-->
<script lang="ts">
  type Props = {
    /// True only on macOS hosts where the diagnostic actually
    /// applies. Non-macOS gets `false` and the component renders
    /// nothing.
    capable: boolean;
    /// Computed by the parent: true iff microphone +
    /// screen-recording report `granted` AND input-monitoring is
    /// not `denied` (input-monitoring is `not-determined` until
    /// the user enables PTT, which is fine).
    allGranted: boolean;
    /// True iff any of the three perms are explicitly `denied`.
    /// `not-determined` does *not* count — see the "no nagging on
    /// fresh install" reason in the file header.
    anyDenied: boolean;
    /// Click → open the Settings → Permissions tab.
    onOpenPermissions: () => void | Promise<void>;
  };

  let { capable, allGranted, anyDenied, onOpenPermissions }: Props = $props();
</script>

{#if capable}
  {#if allGranted}
    <!--
      Green pill: AVFoundation / CoreGraphics / IOKit all report
      `granted`. Compact so it doesn't crowd the Dictation
      surface; the View link still leads into the Settings window
      for users who want to verify or adjust.
    -->
    <p class="permissions-pill permissions-pill-ok" data-testid="perms-pill-ok">
      <span class="dot" aria-hidden="true"></span>
      macOS permissions OK.
      <button
        type="button"
        class="link-button"
        onclick={() => void onOpenPermissions()}
      >View</button>
    </p>
  {:else if anyDenied}
    <!--
      Yellow hint: at least one permission is *denied* (a real,
      actionable problem). On a fresh install where mic /
      screen-recording are still `not-determined` because the
      user hasn't tried recording yet, the hint stays hidden —
      pre-emptively asking "Trouble?" reads as "something is
      broken" when nothing actually is.
    -->
    <!--
      Banner shape mirrors the "Set up your first model" banner on
      Dictation: title + body on the left, primary action button
      on the right. Previously this was an inline link inside body
      copy, which the UX walkthrough flagged as inconsistent with
      the no-model banner's filled-button affordance.
    -->
    <aside class="permissions-banner" data-testid="perms-hint-yellow" role="status">
      <div class="permissions-banner-text">
        <strong>Permission needed</strong>
        <span>
          On macOS, dictation needs Microphone access. Input
          Monitoring enables push-to-talk while you're in another app.
        </span>
      </div>
      <button
        type="button"
        class="primary"
        onclick={() => void onOpenPermissions()}
      >Open Permissions diagnostic</button>
    </aside>
  {/if}
{/if}

<style>
/* Action-led banner — same shape as the no-model setup banner
   in ControlsSection.svelte, just in a yellow palette to match
   the warning state. The previous implementation used an inline
   link inside body copy; the UX walkthrough flagged the
   inconsistency with the setup banner's filled-button affordance. */
.permissions-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.85rem 1rem;
  margin: 1.25rem 0 0;
  background-color: var(--warning-bg);
  border: 1px solid #ffd591;
  border-radius: 8px;
}
.permissions-banner-text {
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
  flex: 1;
  min-width: 0;
}
.permissions-banner-text strong {
  font-size: 0.95rem;
  color: var(--warning-text);
}
.permissions-banner-text span {
  font-size: 0.85rem;
  color: var(--warning-text);
  line-height: 1.4;
}
.permissions-banner button {
  flex-shrink: 0;
  white-space: nowrap;
  border-radius: 8px;
  border: 1px solid var(--accent);
  padding: 0.5em 1em;
  font-size: 0.9em;
  font-family: inherit;
  font-weight: 600;
  background-color: var(--accent);
  color: white;
  cursor: pointer;
}
.permissions-banner button:hover:not(:disabled) {
  background-color: var(--accent-hover);
  border-color: var(--accent-hover);
}
.permissions-pill {
  margin: 1.25rem auto 0;
  padding: 0.5rem 0.85rem;
  background-color: var(--success-bg);
  border: 1px solid var(--success-border);
  border-radius: 999px;
  color: var(--success-text);
  font-size: 0.8rem;
  line-height: 1.4;
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  /* Compact pill, centred horizontally rather than expanding to
     the full width of the .app-main centring fallback. */
  max-width: max-content;
  margin-left: auto;
  margin-right: auto;
}
.permissions-pill .dot {
  width: 0.55rem;
  height: 0.55rem;
  border-radius: 50%;
  background-color: var(--success-text);
  box-shadow: 0 0 0 2px rgba(46, 170, 83, 0.18);
  display: inline-block;
}
.link-button {
  background: none;
  border: none;
  padding: 0;
  color: inherit;
  font: inherit;
  text-decoration: underline;
  cursor: pointer;
}
.link-button:hover {
  text-decoration: none;
}
</style>
