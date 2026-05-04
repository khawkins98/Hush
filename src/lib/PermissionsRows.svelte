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
      target: "microphone" | "input-monitoring" | "screen-recording",
    ) => void | Promise<void>;
  };

  let { diagnostic, health, onOpenPrivacyPane }: Props = $props();

  const ROWS = [
    {
      key: "microphone" as const,
      paneTarget: "microphone" as const,
      label: "Microphone",
      why: "Required for dictation.",
    },
    {
      key: "screenRecording" as const,
      paneTarget: "screen-recording" as const,
      label: "Screen Recording",
      why: "Required for system-audio capture in meetings.",
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
        Two-column layout: text block on the left (title-line +
        why subtitle), action button on the right. Three colours
        on the dot map to the three-state health model: green
        (confirmed), yellow (was granted, now stale — the cert /
        bundle-id rotation case), red (no prior grant). Falls
        back to a neutral grey dot when the health snapshot
        hasn't loaded yet.
      -->
      <div class="perm-text">
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
        {#if rowHealth === "stale"}
          <span class="perm-stale-hint">
            macOS no longer recognises a previous grant for Hush
            (common after app updates). Re-enable {row.label} in
            System Settings to restore access.
          </span>
        {/if}
      </div>
      {#if status !== "not-applicable"}
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
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0.6rem 1rem;
    align-items: center;
    padding: 0.7rem 0.9rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .perm-text {
    /* min-width:0 lets the text column shrink under flex/grid
       constraints so a long "why" wraps instead of pushing the
       button off the row. */
    min-width: 0;
  }
  .perm-title-line {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .perm-name {
    font-weight: 600;
    color: #222;
  }
  .perm-status-pill {
    font-size: 0.72rem;
    font-weight: 600;
    padding: 0.1rem 0.45rem;
    border-radius: 999px;
    background: #ececf0;
    color: #555;
    line-height: 1.4;
    white-space: nowrap;
  }
  .perm-row[data-status="granted"] .perm-status-pill {
    background: #e3f5e8;
    color: #1f6b35;
  }
  .perm-row[data-status="not-determined"] .perm-status-pill {
    background: #fdf1d8;
    color: #7a4e00;
  }
  .perm-row[data-status="denied"] .perm-status-pill {
    background: #fbe3e3;
    color: #8a1f1f;
  }
  .perm-row[data-health="stale"] .perm-status-pill {
    background: #fdf1d8;
    color: #7a4e00;
  }
  .perm-health-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
    background-color: #c0c0c5;
  }
  .perm-health-dot[data-health="confirmed"] {
    background-color: #1f9d3a;
  }
  .perm-health-dot[data-health="stale"] {
    background-color: #e0a020;
  }
  .perm-health-dot[data-health="not-granted"] {
    background-color: var(--danger);
  }
  .perm-health-dot[data-health="not-applicable"] {
    background-color: #c0c0c5;
  }
  .perm-stale-hint {
    display: block;
    margin-top: 0.3rem;
    font-size: 0.78rem;
    color: #7a4e00;
    background-color: #fdf6e3;
    border-left: 3px solid #e0a020;
    padding: 0.4rem 0.6rem;
    border-radius: 4px;
  }
  .perm-why {
    display: block;
    margin-top: 0.15rem;
    font-size: 0.82rem;
    color: #666;
  }
  .perm-row-action {
    align-self: center;
    padding: 0.35rem 0.7rem;
    font-size: 0.82rem;
    font-weight: 500;
    border: 1px solid #d1d1d8;
    background-color: white;
    border-radius: 6px;
    cursor: pointer;
    color: #2c3e8f;
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s;
  }
  .perm-row-action:hover {
    background-color: #f0f4ff;
    border-color: #4a6cd0;
  }
  .perm-row-action:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .perm-row[data-status="not-determined"] .perm-row-action,
  .perm-row[data-status="denied"] .perm-row-action {
    background-color: #eef2ff;
    border-color: #c7d2fe;
    color: #1e1b4b;
    font-weight: 600;
  }
  .perm-row[data-status="not-determined"] .perm-row-action:hover,
  .perm-row[data-status="denied"] .perm-row-action:hover {
    background-color: #e0e7ff;
    border-color: var(--accent);
  }
  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .perm-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    :root:not([data-theme="light"]) .perm-name { color: #e8e8e8; }
    :root:not([data-theme="light"]) .perm-why { color: #a8a8a8; }
    :root:not([data-theme="light"]) .perm-status-pill {
      background: #3a3a3f;
      color: #c8c8cc;
    }
    :root:not([data-theme="light"]) .perm-row[data-status="granted"] .perm-status-pill {
      background: #1d3a26;
      color: #8fd9a3;
    }
    :root:not([data-theme="light"]) .perm-row[data-status="not-determined"] .perm-status-pill {
      background: #3d2f12;
      color: #f0c878;
    }
    :root:not([data-theme="light"]) .perm-row[data-status="denied"] .perm-status-pill {
      background: #3d1d1d;
      color: #f0a0a0;
    }
    :root:not([data-theme="light"]) .perm-row-action {
      background-color: #1f1f22;
      border-color: #38383b;
      color: #c0d0ff;
    }
    :root:not([data-theme="light"]) .perm-row-action:hover {
      background-color: #28283a;
      border-color: var(--accent);
    }
    :root:not([data-theme="light"]) .perm-row[data-status="not-determined"] .perm-row-action,
    :root:not([data-theme="light"]) .perm-row[data-status="denied"] .perm-row-action {
      background-color: #1e1b4b;
      border-color: #4338ca;
      color: #e0e7ff;
    }
  }
  :root[data-theme="dark"] .perm-row {
    background-color: #2a2a2d;
    border-color: #38383b;
  }
  :root[data-theme="dark"] .perm-name { color: #e8e8e8; }
  :root[data-theme="dark"] .perm-why { color: #a8a8a8; }
  :root[data-theme="dark"] .perm-status-pill {
    background: #3a3a3f;
    color: #c8c8cc;
  }
  :root[data-theme="dark"] .perm-row[data-status="granted"] .perm-status-pill {
    background: #1d3a26;
    color: #8fd9a3;
  }
  :root[data-theme="dark"] .perm-row[data-status="not-determined"] .perm-status-pill {
    background: #3d2f12;
    color: #f0c878;
  }
  :root[data-theme="dark"] .perm-row[data-status="denied"] .perm-status-pill {
    background: #3d1d1d;
    color: #f0a0a0;
  }
  :root[data-theme="dark"] .perm-row-action {
    background-color: #1f1f22;
    border-color: #38383b;
    color: #c0d0ff;
  }
  :root[data-theme="dark"] .perm-row-action:hover {
    background-color: #28283a;
    border-color: var(--accent);
  }
  :root[data-theme="dark"] .perm-row[data-status="not-determined"] .perm-row-action,
  :root[data-theme="dark"] .perm-row[data-status="denied"] .perm-row-action {
    background-color: #1e1b4b;
    border-color: #4338ca;
    color: #e0e7ff;
  }
</style>
