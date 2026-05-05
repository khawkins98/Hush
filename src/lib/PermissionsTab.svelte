<!--
  Settings → Permissions tab (#332 phase 1 first slice).
  Extracted from `src/routes/settings/+page.svelte` so the tab
  owns its own state, IPC handlers, and lifecycle wiring rather
  than living as scattered regions of a 2.4k-LOC monolith.

  What lives here:
  - All permission-shaped state (diagnostic snapshot, three-state
    health, reset modal flags, refresh-in-flight guard).
  - Loaders: `loadDiagnostic` (parallel diagnostic + health probe),
    `runReset` (tccutil-driven reset), `openPrivacyPane` (deep-link
    into System Settings, with SCK-priming pre-step).
  - Lifecycle: load on mount, refresh on window-focus while the
    tab is mounted. Pre-extraction the page mounted the data
    eagerly regardless of which tab was active; now the load
    fires only when the user actually visits the tab. Same data,
    smaller boot cost when the user opens Settings to a
    non-Permissions tab.

  What does NOT live here:
  - The per-row markup — that's `<PermissionsRows>` (#232).
  - The reset disclosure — that's `<MacosDiagnosticPanel>`.
  - Cross-window event routing — `settings:goto-tab` listener
    stays on the page so the menu's "Settings → Permissions"
    entry still wins regardless of which tab is currently active.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";

  import MacosDiagnosticPanel from "./MacosDiagnosticPanel.svelte";
  import PermissionsRows from "./PermissionsRows.svelte";
  import { formatErrorMessage } from "./errors";
  import type {
    MacosPermissionDiagnostic,
    MacosPermissionResetResult,
    PermissionsHealth,
    PermissionHealthResponse,
  } from "./types";

  let diagnostic = $state<MacosPermissionDiagnostic | null>(null);
  // Three-state permission health (#378). Populated alongside the
  // diagnostic — the per-row traffic-light dot reads from this,
  // falling back to the diagnostic's raw status if the health IPC
  // errored (older builds / transient settings-DB hiccup).
  let health = $state<PermissionsHealth | null>(null);
  // Open by default in the dedicated tab — the recovery
  // disclosure is the second-most-actionable thing on the page
  // after the row list, and an extra click to expand was a
  // hands-on-feedback papercut on first contact.
  let diagnosticOpen = $state(true);
  let resetMessage = $state<string | null>(null);
  let resetting = $state(false);
  // Set true after a successful reset to show the guided
  // stale-row removal walkthrough in MacosDiagnosticPanel.
  let showResetGuide = $state(false);
  // Track whether a refresh is in flight so the manual Refresh
  // button can show a "Checking…" affordance and disable while
  // the IPC is round-tripping. AVFoundation / CoreGraphics /
  // IOKit reads complete in single-digit milliseconds, but the
  // disabled-flicker is a deliberate hint that the click did
  // something even on a fast machine.
  let refreshing = $state(false);

  let focusHandler: (() => void) | null = null;

  async function loadDiagnostic(): Promise<void> {
    refreshing = true;
    try {
      // Fetch the diagnostic + the three-state health in parallel.
      // Both are read-only AVFoundation / CoreGraphics / settings-
      // DB reads; running serially would just add latency for no
      // ordering reason.
      const [res, healthRes] = await Promise.all([
        invoke<MacosPermissionDiagnostic>("diagnose_macos_permissions"),
        invoke<PermissionHealthResponse>("get_permission_health").catch(
          () => null,
        ),
      ]);
      diagnostic = res.canReset ? res : null;
      health = healthRes?.health ?? null;
    } catch {
      diagnostic = null;
      health = null;
    } finally {
      refreshing = false;
    }
  }

  async function openPrivacyPane(
    target: "microphone" | "input-monitoring" | "screen-recording",
  ) {
    try {
      // For Screen Recording: macOS only adds Hush to the Screen
      // & System Audio Recording list once Hush has actively
      // queried SCK. A user who hasn't started a Meeting Mode
      // session yet would land on the pane with no Hush row to
      // toggle. Prime the permission first so the row appears
      // (and the standard TCC prompt fires for not-determined
      // state). Fire-and-forget — we don't block deep-linking on
      // it, and the helper internally swallows the typical
      // "denied" return.
      if (target === "screen-recording") {
        try {
          await invoke("prime_screen_recording_permission");
        } catch (primeErr) {
          console.warn("[hush] prime SCK permission failed", primeErr);
        }
      }
      await invoke("open_macos_privacy_pane", { target });
    } catch (e) {
      console.warn("[hush] open privacy pane failed", e);
    }
  }

  async function runReset() {
    resetting = true;
    resetMessage = null;
    showResetGuide = false;
    try {
      const res = await invoke<MacosPermissionResetResult>(
        "reset_macos_permissions",
      );
      resetMessage = res.summary;
      showResetGuide = true;
      // Open Screen Recording pane directly — no SCK priming.
      // Priming is correct for "Grant in Settings…" (row hasn't
      // enrolled yet), but wrong here: we just ran tccutil reset
      // and the user needs to remove stale rows, not trigger a
      // fresh TCC prompt. Call the IPC directly to bypass the
      // priming step in openPrivacyPane().
      void invoke("open_macos_privacy_pane", {
        target: "screen-recording",
      }).catch((e) => console.warn("[hush] open pane after reset:", e));
    } catch (e) {
      resetMessage = formatErrorMessage(e);
    } finally {
      resetting = false;
    }
  }

  onMount(() => {
    void loadDiagnostic();
    // Window-focus refresh: the user toggles a permission in
    // System Settings, switches back to Hush. Cheap (single-
    // digit ms) so re-running on every focus is fine.
    // Only fires on the macOS-capable path (`diagnostic` is the
    // gate) — non-macOS builds skip the IPC entirely.
    focusHandler = () => {
      if (diagnostic !== null && !refreshing) {
        void loadDiagnostic();
      }
    };
    window.addEventListener("focus", focusHandler);
  });

  onDestroy(() => {
    if (focusHandler) {
      window.removeEventListener("focus", focusHandler);
      focusHandler = null;
    }
  });
</script>

{#if diagnostic}
  <div class="permissions-tab-header">
    <h2 class="tab-title">Permissions</h2>
    <!--
      Manual refresh button — belt-and-suspenders for the
      window-focus auto-refresh. Auto-refresh covers the common
      case (user toggles a permission in System Settings, switches
      back to Hush); the button covers Settings + System Settings
      side-by-side, keyboard-only navigation, screen-reader users.
    -->
    <button
      type="button"
      class="ghost"
      onclick={() => void loadDiagnostic()}
      disabled={refreshing}
      aria-label="Re-check macOS permission status"
      data-testid="perms-refresh"
    >
      {refreshing ? "Checking…" : "Refresh"}
    </button>
  </div>
  <PermissionsRows
    {diagnostic}
    {health}
    onOpenPrivacyPane={openPrivacyPane}
  />
  <p class="perm-recovery-intro">
    Stuck? Open the diagnostic below to reset all three
    permission grants (Microphone, Screen Recording, Input
    Monitoring) at once, or learn why a permission row might
    not appear in System Settings.
  </p>
  <MacosDiagnosticPanel
    macosDiagnostic={diagnostic}
    bind:macosDiagnosticOpen={diagnosticOpen}
    macosResetMessage={resetMessage}
    macosResetting={resetting}
    {showResetGuide}
    onDeepLinkPrivacyPane={openPrivacyPane}
    onReset={runReset}
  />
{:else}
  <h2 class="tab-title">Permissions</h2>
  <p class="placeholder">
    Permission diagnostics are macOS-only. There's nothing
    actionable to surface on this platform.
  </p>
{/if}

<style>
  .permissions-tab-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .permissions-tab-header .tab-title {
    margin: 0;
  }
  .tab-title {
    font-size: 1.1rem;
    font-weight: 600;
    margin: 0 0 0.5rem;
    letter-spacing: -0.01em;
  }
  .placeholder {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.95rem;
  }
  .perm-recovery-intro {
    margin: 1rem 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
    max-width: 44rem;
  }
  /* Local copy of the parent page's button + .ghost variant.
     Svelte's scoped styles don't inherit page-level rules into
     a component, so the visible attributes are duplicated. */
  button {
    border-radius: 8px;
    border: 1px solid #d1d1d1;
    padding: 0.55em 1.1em;
    font-size: 0.95em;
    font-family: inherit;
    color: var(--text-primary);
    background-color: var(--bg-surface);
    cursor: pointer;
    font-weight: 600;
    transition: border-color 0.15s, background-color 0.15s;
  }
  button:hover:not(:disabled) {
    border-color: var(--accent-hover);
  }
  button.ghost {
    padding: 0.3em 0.75em;
    font-size: 0.8rem;
    font-weight: 500;
    background-color: transparent;
  }
  button.ghost:hover:not(:disabled) {
    background-color: var(--bg-app);
  }
  button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) button {
      border-color: #3a3a3a;
    }
    :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
      background-color: #353535;
    }
  }
  :root[data-theme="dark"] button {
    border-color: #3a3a3a;
  }
  :root[data-theme="dark"] button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
</style>
