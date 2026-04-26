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
        against. If you don't see Hush listed under Microphone or
        Input Monitoring, the binary may not be registering under
        this bundle id (common on unsigned dev builds).
      </p>
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
          {macosResetting ? "Resetting…" : "Reset permissions and re-prompt"}
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
