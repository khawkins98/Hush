<script lang="ts">
  import { openExternal } from "./openExternal";
  import type { MacosPermissionDiagnostic } from "./types";

  type Props = {
    macosDiagnostic: MacosPermissionDiagnostic;
    macosDiagnosticOpen: boolean;
    macosResetMessage: string | null;
    macosResetting: boolean;
    onReset: () => void | Promise<void>;
  };

  let {
    macosDiagnostic,
    macosDiagnosticOpen = $bindable(),
    macosResetMessage,
    macosResetting,
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
            Terminal) rather than under <code>com.khawkins.hush</code>.
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
        >docs/macos-permissions.md</a>.
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
  background-color: rgba(124, 111, 247, 0.1);
  border-left: 3px solid var(--accent);
  border-radius: 4px;
  font-size: 0.9rem;
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
  color: #444;
  user-select: none;
}
.macos-diag-why[open] summary {
  margin-bottom: 0.5rem;
}

.macos-diag-doc-pointer {
  font-size: 0.85rem;
  color: #555;
}

@media (prefers-color-scheme: dark) {
  .macos-diagnostic details {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.03);
  }
  .macos-diag-bundle code,
  .macos-diag-bundle-list code {
    background-color: rgba(255, 255, 255, 0.08);
  }
  .macos-diag-doc-pointer {
    color: #aaa;
  }
  .macos-diag-why {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.03);
  }
  .macos-diag-why summary {
    color: #ccc;
  }
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
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
  color: #b03030;
  border-color: #e1b8b8;
  font-weight: 500;
}

button.danger:hover:not(:disabled) {
  background-color: #fbeaea;
  border-color: #d83a3a;
}

@media (prefers-color-scheme: dark) {
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: var(--accent);
  }
  button.danger {
    background-color: transparent;
    color: #ff9090;
    border-color: #5a2020;
  }
  button.danger:hover:not(:disabled) {
    background-color: #3a1818;
    border-color: #d83a3a;
  }
}
</style>
