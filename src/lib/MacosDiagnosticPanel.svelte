<script lang="ts">
  import type { MacosPermissionDiagnostic } from "./types";

  type Props = {
    macosDiagnostic: MacosPermissionDiagnostic;
    macosDiagnosticOpen: boolean;
    macosResetMessage: string | null;
    macosResetting: boolean;
    onOpenPrivacyPane: (target: "microphone" | "input-monitoring") => void | Promise<void>;
    onReset: () => void | Promise<void>;
  };

  let {
    macosDiagnostic,
    macosDiagnosticOpen = $bindable(),
    macosResetMessage,
    macosResetting,
    onOpenPrivacyPane,
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
      <p class="macos-diag-bundle">
        <strong>Bundle id:</strong>
        <code>{macosDiagnostic.bundleId}</code>
        — this is what System Settings → Privacy &amp; Security keys
        against. <strong>Two reasons Hush might not appear in the
        permission lists:</strong>
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
      <p>
        <strong>Microphone:</strong> {macosDiagnostic.microphoneHint}
      </p>
      <p>
        <strong>Input Monitoring:</strong> {macosDiagnostic.inputMonitoringHint}
      </p>
      <div class="macos-diag-actions">
        <button
          type="button"
          class="ghost"
          onclick={() => onOpenPrivacyPane("microphone")}
        >
          Open Microphone settings
        </button>
        <button
          type="button"
          class="ghost"
          onclick={() => onOpenPrivacyPane("input-monitoring")}
        >
          Open Input Monitoring settings
        </button>
        <button
          type="button"
          class="primary"
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
      <p class="macos-diag-doc-pointer">
        Full troubleshooting recipe (including the
        <code>tccutil</code> commands this button wraps) is in
        <a
          href="https://github.com/khawkins98/Hush/blob/main/docs/macos-permissions.md"
          target="_blank"
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
  background-color: rgba(106, 140, 240, 0.1);
  border-left: 3px solid #6a8cf0;
  border-radius: 4px;
  font-size: 0.9rem;
}

.macos-diag-doc-pointer {
  font-size: 0.85rem;
  color: #555;
}

.macos-diag-doc-pointer code {
  background-color: rgba(0, 0, 0, 0.06);
  padding: 0.05em 0.3em;
  border-radius: 3px;
}

@media (prefers-color-scheme: dark) {
  .macos-diagnostic details {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.03);
  }
  .macos-diag-bundle code,
  .macos-diag-bundle-list code,
  .macos-diag-doc-pointer code {
    background-color: rgba(255, 255, 255, 0.08);
  }
  .macos-diag-doc-pointer {
    color: #aaa;
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
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

button.ghost {
  padding: 0.3em 0.75em;
  font-size: 0.8rem;
  font-weight: 500;
  background-color: transparent;
  border: 1px solid #d1d1d1;
}

button.ghost:hover:not(:disabled) {
  background-color: #f0f0f0;
}

button.primary {
  background-color: #6a8cf0;
  color: white;
  border-color: #6a8cf0;
  font-weight: 600;
}

button.primary:hover:not(:disabled) {
  background-color: #4a6cd0;
  border-color: #4a6cd0;
}

@media (prefers-color-scheme: dark) {
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  button.ghost {
    border-color: #3a3a3a;
    color: #f0f0f0;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
}
</style>
