<!--
  Reusable Permissions modal (#232). Wraps `<PermissionsRows>` in
  a dialog with focus-trap, Escape dismiss, intro copy, and a
  primary "Done" button. Used by:

  - First-run flow: opened by `+page.svelte` after the welcome
    modal's Got It dismiss, so the user gets an actionable step
    after the privacy-posture explainer.
  - Ad-hoc launches: opened by `+page.svelte` when a meeting-
    start failure is permission-shaped (Screen Recording or
    Microphone denied), so the next click is in the right place
    instead of buried under an error chip.

  The Settings → Permissions tab does NOT use this dialog; it
  embeds `<PermissionsRows>` directly so the surface keeps the
  in-context "I'm in Settings" framing the issue's design
  question called out.

  Refresh shape: the dialog runs `diagnose_macos_permissions` +
  `get_permission_health` whenever `show` flips to true. It does
  not refresh on window-focus while open — the dialog is
  short-lived and the Refresh button covers the corner case.

  A11y plumbing mirrors `FirstRunModal.svelte`: backdrop with
  role=dialog + aria-modal=true, Escape dismiss, Tab cycles
  inside the modal, focus auto-lands on the first action button,
  focus restores on dismiss.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import PermissionsRows from "./PermissionsRows.svelte";
  import type {
    MacosPermissionDiagnostic,
    PermissionHealthResponse,
    PermissionsHealth,
  } from "./types";

  type Props = {
    show: boolean;
    onDismiss: () => void | Promise<void>;
    onOpenPrivacyPane: (
      target: "microphone" | "input-monitoring" | "screen-recording",
    ) => void | Promise<void>;
    /**
     * Optional intro copy override. Defaults to the generic
     * first-run / ad-hoc framing; ad-hoc launches from a specific
     * failure (e.g. "Screen Recording denied") may pass a tighter
     * intro that names the offending permission.
     */
    intro?: string;
  };

  let {
    show,
    onDismiss,
    onOpenPrivacyPane,
    intro = "Hush uses two macOS permissions. Grant or revoke each below; you can also wait for Hush to prompt on first use.",
  }: Props = $props();

  let diagnostic: MacosPermissionDiagnostic | null = $state(null);
  let health: PermissionsHealth | null = $state(null);
  let loadError: string | null = $state(null);
  let refreshing = $state(false);
  // Whether IM was granted when the dialog first loaded in this session.
  // Used to detect a within-session grant that requires a restart.
  let imGrantedAtLoad = $state<boolean | null>(null);
  // Optimistic signal: user clicked "Grant in Settings…" for IM this session.
  let imGrantAttempted = $state(false);

  function handleOpenPrivacyPane(
    target: "microphone" | "input-monitoring",
  ): void | Promise<void> {
    if (target === "input-monitoring") imGrantAttempted = true;
    return onOpenPrivacyPane(target);
  }

  let cardEl: HTMLElement | undefined = $state();
  let previousFocus: HTMLElement | null = null;

  const FOCUSABLE_SELECTOR =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  async function refresh() {
    refreshing = true;
    loadError = null;
    try {
      // Two independent IPCs — diagnostic carries raw OS statuses
      // + bundle id; health adds the staleness verdict layered on
      // top. Run in parallel; if either fails, surface a single
      // error string but try to render whatever did load.
      const [diagRes, healthRes] = await Promise.allSettled([
        invoke<MacosPermissionDiagnostic>("diagnose_macos_permissions"),
        invoke<PermissionHealthResponse>("get_permission_health"),
      ]);
      if (diagRes.status === "fulfilled") {
        diagnostic = diagRes.value;
        if (imGrantedAtLoad === null) {
          imGrantedAtLoad =
            diagnostic.statuses.inputMonitoring === "granted";
        }
      } else {
        loadError = String(diagRes.reason);
      }
      if (healthRes.status === "fulfilled") {
        health = healthRes.value.health;
      }
      // Health failure alone is non-fatal — the rows still render
      // with raw status pills against the "unknown" health dot.
    } finally {
      refreshing = false;
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (!show) return;
    if (event.key === "Escape") {
      event.preventDefault();
      void dismiss();
      return;
    }
    if (event.key !== "Tab" || !cardEl) return;
    const focusable = cardEl.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  async function dismiss() {
    previousFocus?.focus();
    previousFocus = null;
    await onDismiss();
  }

  // Refresh + focus management on the open transition.
  $effect(() => {
    if (show && cardEl) {
      previousFocus =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
      void refresh();
      // Defer the focus call one microtask so the just-rendered
      // dialog has its first focusable element in place.
      queueMicrotask(() => {
        const first = cardEl?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
        first?.focus();
      });

      // Re-poll when the user returns from System Settings so the
      // restart notice appears automatically — the dialog's initial
      // refresh() only fires on open, not on window refocus.
      function onFocus() {
        if (!refreshing) void refresh();
      }
      window.addEventListener("focus", onFocus);
      return () => window.removeEventListener("focus", onFocus);
    }
  });
</script>

<svelte:window onkeydown={handleKeydown} />

{#if show}
  <div
    class="perm-dialog-backdrop"
    role="dialog"
    aria-modal="true"
    aria-labelledby="perm-dialog-heading"
  >
    <article class="perm-dialog-card" bind:this={cardEl} tabindex="-1">
      <header class="perm-dialog-header">
        <h2 id="perm-dialog-heading">Permissions</h2>
        <button
          type="button"
          class="ghost"
          onclick={() => void refresh()}
          disabled={refreshing}
          aria-label="Re-check macOS permission status"
          data-testid="perm-dialog-refresh"
        >
          {refreshing ? "Checking…" : "Refresh"}
        </button>
      </header>
      <p class="perm-dialog-intro">{intro}</p>

      {#if diagnostic}
        <PermissionsRows
          {diagnostic}
          {health}
          onOpenPrivacyPane={handleOpenPrivacyPane}
          imGrantedAtLoad={imGrantedAtLoad ?? undefined}
          {imGrantAttempted}
        />
      {:else if loadError}
        <p class="perm-dialog-error" role="alert">
          Couldn't load permission status: {loadError}
        </p>
      {:else}
        <p class="perm-dialog-loading">Checking permissions…</p>
      {/if}

      <footer class="perm-dialog-footer">
        <button class="primary" onclick={() => void dismiss()}>Done</button>
      </footer>
    </article>
  </div>
{/if}

<style>
  .perm-dialog-backdrop {
    position: fixed;
    inset: 0;
    background-color: rgba(15, 15, 15, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 1.5rem;
  }
  .perm-dialog-card {
    background-color: #ffffff;
    border-radius: 12px;
    padding: 1.5rem 1.75rem;
    max-width: 36rem;
    width: 100%;
    max-height: calc(100vh - 3rem);
    overflow-y: auto;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.18);
    text-align: left;
  }
  .perm-dialog-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.75rem;
    margin-bottom: 0.5rem;
  }
  .perm-dialog-header h2 {
    margin: 0;
    font-size: 1.35rem;
    letter-spacing: -0.01em;
  }
  .perm-dialog-intro {
    margin: 0 0 1rem;
    font-size: 0.88rem;
    color: #555;
    line-height: 1.5;
  }
  .perm-dialog-error {
    margin: 0.5rem 0;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    background: #fbe3e3;
    color: #8a1f1f;
    font-size: 0.85rem;
  }
  .perm-dialog-loading {
    margin: 0.5rem 0;
    color: #666;
    font-size: 0.9rem;
  }
  .perm-dialog-footer {
    margin-top: 1.25rem;
    display: flex;
    justify-content: flex-end;
  }
  /* Local copies of the parent-page button variants. Svelte's
     scoped styles don't inherit page-level rules into a
     component, so we duplicate the visible attributes — same
     pattern FirstRunModal already uses. */
  button {
    border-radius: 8px;
    border: 1px solid #d1d1d1;
    padding: 0.55em 1.1em;
    font-size: 0.95em;
    font-family: inherit;
    color: #0f0f0f;
    background-color: #ffffff;
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
    background-color: #f0f0f0;
  }
  button.primary {
    background-color: var(--accent);
    color: white;
    border-color: var(--accent);
  }
  button.primary:hover:not(:disabled) {
    background-color: #4a6cd0;
    border-color: #4a6cd0;
  }
  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .perm-dialog-backdrop {
      background-color: rgba(0, 0, 0, 0.65);
    }
    :root:not([data-theme="light"]) .perm-dialog-card {
      background-color: #1f1f1f;
      color: #f0f0f0;
      box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    }
    :root:not([data-theme="light"]) .perm-dialog-intro {
      color: #b8b8b8;
    }
    :root:not([data-theme="light"]) .perm-dialog-loading {
      color: #a8a8a8;
    }
    :root:not([data-theme="light"]) .perm-dialog-error {
      background: #3d1d1d;
      color: #f0a0a0;
    }
    :root:not([data-theme="light"]) button {
      color: #f0f0f0;
      background-color: #2a2a2a;
      border-color: #3a3a3a;
    }
    :root:not([data-theme="light"]) button.ghost {
      background-color: transparent;
    }
    :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
      background-color: #353535;
    }
  }
  :root[data-theme="dark"] .perm-dialog-backdrop {
    background-color: rgba(0, 0, 0, 0.65);
  }
  :root[data-theme="dark"] .perm-dialog-card {
    background-color: #1f1f1f;
    color: #f0f0f0;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  }
  :root[data-theme="dark"] .perm-dialog-intro {
    color: #b8b8b8;
  }
  :root[data-theme="dark"] .perm-dialog-loading {
    color: #a8a8a8;
  }
  :root[data-theme="dark"] .perm-dialog-error {
    background: #3d1d1d;
    color: #f0a0a0;
  }
  :root[data-theme="dark"] button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root[data-theme="dark"] button.ghost {
    background-color: transparent;
  }
  :root[data-theme="dark"] button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
</style>
