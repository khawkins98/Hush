<script lang="ts">
  import { openExternal } from "./openExternal";
  import type { MacosPermissionDiagnostic } from "./types";

  type Props = {
    macosDiagnostic: MacosPermissionDiagnostic;
    macosDiagnosticOpen: boolean;
    macosResetMessage: string | null;
    macosResetting: boolean;
    /// When true, show the step-by-step stale-row removal guide
    /// below the reset result. Set by PermissionsTab after a
    /// successful reset_macos_permissions call.
    showResetGuide?: boolean;
    /// Deep-link only (no SCK priming) — used by the recovery
    /// guide's convenience buttons for Microphone and Input
    /// Monitoring panes. Screen Recording was opened automatically
    /// on reset; these let the user reach the other two without
    /// hunting in System Settings.
    onDeepLinkPrivacyPane?: (
      target: "microphone" | "input-monitoring",
    ) => void | Promise<void>;
    onReset: () => void | Promise<void>;
  };

  let {
    macosDiagnostic,
    macosDiagnosticOpen = $bindable(),
    macosResetMessage,
    macosResetting,
    showResetGuide = false,
    onDeepLinkPrivacyPane,
    onReset,
  }: Props = $props();
</script>

<!--
  macOS permission diagnostic — only rendered when the backend
  reports `canReset: true` (effectively `cfg!(target_os = "macos")`
  on the Rust side). Linux/Windows users see this section hidden
  entirely; there's no permission story to diagnose for them.
  The disclosure starts collapsed because most users don't need
  it; it's the recovery path for the stuck-permission state.
-->
<section class="macos-diagnostic" aria-labelledby="macos-diag-heading">
  <details bind:open={macosDiagnosticOpen}>
    <summary id="macos-diag-heading">
      macOS permissions — diagnostic and reset
    </summary>
    <div class="macos-diagnostic-body">
      <!--
        Per-row "Grant in Settings…" / "Open in Settings" buttons in
        the parent permission cards now own the deep-link surface;
        the diagnostic disclosure is just for the actually-stuck-
        path: reset, plus the bundle-id forensics. The per-permission
        hint paragraphs that used to live here moved up onto the
        rows themselves.
      -->
      <p class="macos-diag-reset-intro">
        If a permission won't stick after a fresh prompt — or a
        stale Hush.app row appears under a previous build's signing
        identity — reset Hush's grants for Microphone, Screen
        Recording, and Input Monitoring in one click. The reset
        takes effect on next launch.
      </p>
      <div class="macos-diag-actions">
        <!--
          Reset is a nuclear, last-resort action and lives behind
          this disclosure precisely because most users don't need
          it. Style it as a danger-ghost button rather than a
          filled primary so the eye is drawn to the per-row
          "Grant in Settings…" CTAs above (which are the actual
          forward-progress action), not down here.
        -->
        <button
          type="button"
          class="danger"
          onclick={onReset}
          disabled={macosResetting}
        >
          {macosResetting ? "Resetting…" : "Reset permissions"}
        </button>
      </div>
      {#if macosResetMessage}
        <p class="macos-diag-reset-result" role="status">
          {macosResetMessage}
        </p>
      {/if}

      {#if showResetGuide}
        <!--
          Guided stale-row removal walkthrough. Shown after a
          successful tccutil reset. Screen Recording was opened
          automatically; Microphone and Input Monitoring buttons
          let the user reach the other two panes without hunting.

          Critical copy: the reset only takes effect on next
          launch — say "Quit and reopen Hush" not "click Refresh"
          to avoid the user getting confused by a state that won't
          change in the running session.
        -->
        <div class="macos-reset-guide" role="status">
          <p class="macos-reset-guide-intro">
            System Settings has been opened to Screen Recording.
            Remove any stale Hush rows for each permission:
          </p>
          <ol class="macos-reset-guide-steps">
            <li>Find any <strong>Hush</strong> row in the list</li>
            <li>Select it and click the <strong>−</strong> button to remove it</li>
            <li>Repeat for Microphone and Input Monitoring (buttons below)</li>
            <li><strong>Quit and reopen Hush</strong> — the reset takes effect on next launch</li>
          </ol>
          {#if onDeepLinkPrivacyPane}
            <div class="macos-reset-guide-actions">
              <button
                type="button"
                class="guide-pane-btn"
                onclick={() => onDeepLinkPrivacyPane?.("microphone")}
              >Open Microphone Settings</button>
              <button
                type="button"
                class="guide-pane-btn"
                onclick={() => onDeepLinkPrivacyPane?.("input-monitoring")}
              >Open Input Monitoring Settings</button>
            </div>
          {/if}
        </div>
      {/if}

      <details class="macos-diag-why">
        <summary>Why isn't Hush in the list?</summary>
        <p class="macos-diag-bundle">
          <strong>Bundle id:</strong>
          <code>{macosDiagnostic.bundleId}</code>
          — this is what System Settings → Privacy &amp; Security
          keys against. Two reasons Hush might not appear in the
          permission lists:
        </p>
        <ul class="macos-diag-bundle-list">
          <li>
            <strong>Hush hasn't asked for that permission yet.</strong>
            macOS only adds an app to a permission list once the app
            actively requests it. Hush requests Microphone the first
            time you click Start recording — until then it won't show
            under Microphone. Hush requests Input Monitoring on first
            launch (PTT is on by default since #194); if you've
            disabled PTT in Settings → General → Hotkeys, the listener
            never spawns and Hush won't show under Input Monitoring.
          </li>
          <li>
            <strong>Bundle-id mismatch on dev builds.</strong> When
            running via <code>npm run tauri dev</code>, the binary at
            <code>target/debug/hush</code> is unsigned, so macOS may key
            the permission entry against the launching shell (iTerm /
            Terminal) rather than under <code>io.github.khawkins98.hush</code>.
            Production-signed builds register under the bundle id
            cleanly.
          </li>
        </ul>
      </details>

      <p class="macos-diag-doc-pointer">
        Full troubleshooting recipe is in
        <a
          href="https://github.com/khawkins98/Hush/blob/main/docs/macos-permissions.md"
          onclick={(e) => {
            e.preventDefault();
            openExternal(
              "https://github.com/khawkins98/Hush/blob/main/docs/macos-permissions.md",
            );
          }}
          rel="noopener noreferrer"
        >Permissions troubleshooting guide</a>.
      </p>
    </div>
  </details>
</section>

<style>
.macos-diagnostic {
  margin: 1.5rem 0 0;
  padding: 0;
}

.macos-diagnostic details {
  border: 1px solid #d1d1d1;
  border-radius: 8px;
  padding: 0.5rem 1rem;
  background-color: rgba(0, 0, 0, 0.02);
}

.macos-diagnostic summary {
  cursor: pointer;
  font-weight: 600;
  padding: 0.25rem 0;
  user-select: none;
}

.macos-diagnostic-body {
  padding: 0.75rem 0 0.25rem;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.macos-diagnostic-body p {
  margin: 0;
  line-height: 1.5;
  font-size: 0.95rem;
}

.macos-diag-bundle code {
  background-color: rgba(0, 0, 0, 0.06);
  padding: 0.1em 0.4em;
  border-radius: 4px;
  font-size: 0.9em;
}

.macos-diag-bundle-list {
  margin: 0.25rem 0 0 0;
  padding-left: 1.2rem;
  font-size: 0.9rem;
  line-height: 1.5;
}

.macos-diag-bundle-list li {
  margin-bottom: 0.4rem;
}

.macos-diag-bundle-list code {
  background-color: rgba(0, 0, 0, 0.06);
  padding: 0.05em 0.3em;
  border-radius: 3px;
  font-size: 0.9em;
}

.macos-diag-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin-top: 0.25rem;
}

.macos-diag-reset-result {
  padding: 0.5rem 0.75rem;
  background-color: rgba(220, 121, 50, 0.1);
  border-left: 3px solid var(--accent);
  border-radius: 4px;
  font-size: 0.9rem;
}

.macos-reset-guide {
  padding: 0.75rem 0.9rem;
  background-color: #fdf6e3;
  border: 1px solid #e0a020;
  border-radius: 6px;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.macos-reset-guide-intro {
  margin: 0;
  font-size: 0.9rem;
  font-weight: 600;
  color: #5a3e00;
}

.macos-reset-guide-steps {
  margin: 0;
  padding-left: 1.3rem;
  font-size: 0.88rem;
  line-height: 1.6;
  color: #5a3e00;
}

.macos-reset-guide-steps li {
  margin-bottom: 0.15rem;
}

.macos-reset-guide-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem;
}

.guide-pane-btn {
  padding: 0.3em 0.75em;
  font-size: 0.82rem;
  font-weight: 500;
  border: 1px solid #c08000;
  background-color: #fff8e6;
  border-radius: 5px;
  cursor: pointer;
  color: #5a3e00;
  transition: background-color 0.1s, border-color 0.1s;
  font-family: inherit;
}

.guide-pane-btn:hover {
  background-color: #ffedc0;
  border-color: #a06000;
}

.macos-diag-why {
  margin-top: 0.25rem;
  padding: 0.4rem 0.65rem;
  border: 1px solid #e1e1e1;
  border-radius: 6px;
  background-color: rgba(0, 0, 0, 0.015);
}
.macos-diag-why summary {
  cursor: pointer;
  font-weight: 500;
  font-size: 0.9rem;
  color: var(--text-secondary);
  user-select: none;
}
.macos-diag-why[open] summary {
  margin-bottom: 0.5rem;
}

.macos-diag-doc-pointer {
  font-size: 0.85rem;
  color: var(--text-secondary);
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: var(--text-primary);
  background-color: var(--bg-surface);
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s;
}

button:hover:not(:disabled) {
  border-color: var(--accent-hover);
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

button.danger {
  /* Quiet destructive — outlined red, not filled — so it reads
     as "this is the last resort" rather than "click me." */
  background-color: transparent;
  color: var(--danger);
  border-color: var(--danger-border);
  font-weight: 500;
}

button.danger:hover:not(:disabled) {
  background-color: var(--danger-bg);
  border-color: var(--danger);
}

</style>
