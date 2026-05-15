<!--
  Per-row permission UI (#232). Three rows — Microphone, Screen
  Recording, Input Monitoring — with the traffic-light health dot
  (#378), live OS status pill, why-line, and a per-row deep-link
  to System Settings. Used by:

  - Settings → Permissions tab (embedded directly).
  - PermissionsDialog (the modal wrapper for first-run + ad-hoc
    launches from permission-shaped failures).

  The component is purely presentational: callers pass the
  current `diagnostic` + `health` snapshot and the
  `onOpenPrivacyPane` callback. Refresh cadence is the caller's
  responsibility — Settings refreshes on window-focus + a manual
  Refresh button; the dialog refreshes on `show` flip.
-->
<script lang="ts">
  import type {
    MacosPermissionDiagnostic,
    PermissionsHealth,
  } from "./types";

  type Props = {
    diagnostic: MacosPermissionDiagnostic;
    health: PermissionsHealth | null;
    onOpenPrivacyPane: (
      target: "microphone" | "input-monitoring",
    ) => void | Promise<void>;
  };

  let {
    diagnostic,
    health,
    onOpenPrivacyPane,
  }: Props = $props();

  const ROWS = [
    {
      key: "microphone" as const,
      paneTarget: "microphone" as const,
      label: "Microphone",
      why: "Required for dictation.",
    },
    {
      key: "inputMonitoring" as const,
      paneTarget: "input-monitoring" as const,
      label: "Input Monitoring",
      why: "Required for push-to-talk (on by default). Disable PTT in General → Hotkeys if you'd rather skip the prompt.",
    },
  ];
</script>

<ul class="perm-status-list" aria-label="Permission status summary">
  {#each ROWS as row (row.key)}
    {@const status = diagnostic.statuses[row.key]}
    {@const rowHealth = health?.[row.key] ?? null}
    <li
      class="perm-row"
      data-perm={row.key}
      data-status={status}
      data-health={rowHealth ?? "unknown"}
    >
      <!--
        Vertical stack: title-line (dot + name + pill) → why
        subtitle → action button (right-aligned) → stale notice
        (full-width). Three colours on the dot map to the
        three-state health model: green (confirmed), yellow (was
        granted, now stale — the cert / bundle-id rotation case),
        red (no prior grant). Falls back to a neutral grey dot
        when the health snapshot hasn't loaded yet.
      -->
      <div class="perm-title-line">
        <span
          class="perm-health-dot"
          data-health={rowHealth ?? "unknown"}
          aria-hidden="true"
        ></span>
        <span class="perm-name">{row.label}</span>
        <span class="perm-status-pill">
          {#if rowHealth === "stale"}Was granted — now revoked
          {:else if status === "granted"}Granted
          {:else if status === "denied"}Denied
          {:else if status === "not-determined"}Not yet granted
          {:else}Not applicable
          {/if}
        </span>
      </div>
      <span class="perm-why">{row.why}</span>
      {#if status !== "not-applicable"}
        <div class="perm-row-action-row">
          <button
            type="button"
            class="perm-row-action"
            data-testid="perm-action-{row.key}"
            onclick={() => onOpenPrivacyPane(row.paneTarget)}
          >
            {#if status === "granted"}
              Open in Settings
            {:else}
              Grant in Settings…
            {/if}
          </button>
        </div>
      {/if}
      {#if rowHealth === "stale"}
        <span class="perm-stale-hint">
          macOS no longer recognises a previous grant for Hush
          (common after app updates). Re-enable {row.label} in
          System Settings to restore access.
        </span>
      {/if}
    </li>
  {/each}
</ul>

<style>
  .perm-status-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    max-width: 44rem;
  }
  .perm-row {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    padding: 0.7rem 0.9rem;
    background-color: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .perm-title-line {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .perm-name {
    font-weight: 600;
    color: var(--text-primary);
  }
  .perm-status-pill {
    font-size: 0.72rem;
    font-weight: 600;
    padding: 0.1rem 0.45rem;
    border-radius: 999px;
    background: var(--bg-elevated);
    color: var(--text-secondary);
    line-height: 1.4;
    white-space: nowrap;
  }
  .perm-row[data-status="granted"] .perm-status-pill {
    background: var(--success-bg);
    color: var(--success-text);
  }
  .perm-row[data-status="not-determined"] .perm-status-pill {
    background: var(--warning-bg);
    color: var(--warning-text);
  }
  .perm-row[data-status="denied"] .perm-status-pill {
    background: var(--danger-bg);
    color: var(--danger);
  }
  .perm-row[data-health="stale"] .perm-status-pill {
    background: var(--warning-bg);
    color: var(--warning-text);
  }
  .perm-health-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
    background-color: var(--text-muted);
  }
  .perm-health-dot[data-health="confirmed"] {
    background-color: var(--success-text);
  }
  .perm-health-dot[data-health="stale"] {
    background-color: var(--warning-border);
  }
  .perm-health-dot[data-health="not-granted"] {
    background-color: var(--danger);
  }
  .perm-health-dot[data-health="not-applicable"] {
    background-color: var(--text-muted);
  }
  .perm-stale-hint {
    display: block;
    margin-top: 0.3rem;
    font-size: 0.78rem;
    color: var(--warning-text);
    background-color: var(--warning-bg);
    border-left: 3px solid var(--warning-border);
    padding: 0.4rem 0.6rem;
    border-radius: 4px;
  }
  .perm-why {
    font-size: 0.82rem;
    color: var(--text-secondary);
    line-height: 1.4;
  }
  .perm-row-action-row {
    display: flex;
    justify-content: flex-end;
    margin-top: 0.2rem;
  }
  .perm-row-action {
    padding: 0.35rem 0.7rem;
    font-size: 0.82rem;
    font-weight: 500;
    border: 1px solid var(--border-input);
    background-color: var(--bg-surface);
    border-radius: 6px;
    cursor: pointer;
    color: var(--info-text);
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s;
  }
  .perm-row-action:hover {
    background-color: var(--info-bg);
    border-color: var(--info-border);
  }
  .perm-row-action:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .perm-row[data-status="not-determined"] .perm-row-action,
  .perm-row[data-status="denied"] .perm-row-action {
    background-color: var(--info-bg);
    border-color: var(--info-border);
    color: var(--info-text);
    font-weight: 600;
  }
  .perm-row[data-status="not-determined"] .perm-row-action:hover,
  .perm-row[data-status="denied"] .perm-row-action:hover {
    background-color: var(--accent-subtle);
    border-color: var(--accent);
  }
</style>
