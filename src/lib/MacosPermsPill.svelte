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
    <p class="permissions-hint" data-testid="perms-hint-yellow">
      On macOS, dictation needs Microphone access (and Screen
      Recording for system-audio capture in meetings). Trouble?
      <button
        type="button"
        class="link-button"
        onclick={() => void onOpenPermissions()}
      >Open the Permissions diagnostic</button>.
    </p>
  {/if}
{/if}

<style>
.permissions-hint {
  margin: 1.25rem auto 0;
  padding: 0.75rem 1rem;
  background-color: #fff7e6;
  border: 1px solid #ffd591;
  border-radius: 8px;
  color: #8a5a00;
  font-size: 0.85rem;
  line-height: 1.5;
}
.permissions-pill {
  margin: 1.25rem auto 0;
  padding: 0.5rem 0.85rem;
  background-color: #e7f8ec;
  border: 1px solid #b6e5c5;
  border-radius: 999px;
  color: #2a6b3c;
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
  background-color: #2eaa53;
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
@media (prefers-color-scheme: dark) {
  .permissions-hint {
    background-color: #3a2c00;
    border-color: #6b5300;
    color: #ffd591;
  }
  .permissions-pill {
    background-color: #1a3a23;
    border-color: #2a6b3c;
    color: #b6e5c5;
  }
  .permissions-pill .dot {
    background-color: #4ad07a;
    box-shadow: 0 0 0 2px rgba(74, 208, 122, 0.2);
  }
}
</style>
