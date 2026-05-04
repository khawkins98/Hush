<!--
  Floating debug-console window (#540).

  A self-contained always-on-top palette showing:
    1. The live backend log stream (via DebugConsole.svelte).
    2. The issue-report generator (shared logic from DebugTab).

  Opening: invoked from Settings → Debug tab via `open_debug_window`
  IPC command, which shows the "debug" window declared in
  tauri.conf.json. The window is always-on-top so it floats above
  the main app while the user clicks around.

  This page intentionally shares no layout with the main app
  shell — it's a minimal dark surface whose whole job is the log.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { version as osVersion } from "@tauri-apps/plugin-os";
  import { onMount } from "svelte";
  import DebugConsole from "$lib/DebugConsole.svelte";

  type LogEntry = {
    seq: number;
    timestampMs: number;
    level: string;
    target: string;
    message: string;
  };

  let appVersion = $state<string>("…");
  let os = $state<string>("macOS");
  let reportText = $state<string>("");
  let copied = $state(false);

  function formatTime(ms: number): string {
    return new Date(ms).toISOString();
  }

  async function generateReport() {
    let entries: LogEntry[] = [];
    try {
      entries = await invoke<LogEntry[]>("get_log_entries");
    } catch {
      // best-effort
    }
    const last50 = entries.slice(-50);
    const logBlock = last50
      .map(
        (e) =>
          `[${formatTime(e.timestampMs)}] ${e.level.padEnd(5)} ${e.target} ${e.message}`,
      )
      .join("\n");

    reportText = [
      `**Hush version:** ${appVersion}`,
      `**OS:** ${os}`,
      ``,
      `**Last 50 log entries:**`,
      `\`\`\``,
      logBlock || "(no entries)",
      `\`\`\``,
      ``,
      `**Steps to reproduce:**`,
      `1. `,
      ``,
      `**Expected behavior:**`,
      ``,
      `**Actual behavior:**`,
    ].join("\n");
  }

  async function onCopyReport() {
    try {
      await navigator.clipboard.writeText(reportText);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch (e) {
      console.warn("[hush] clipboard write failed", e);
    }
  }

  function openGitHubIssue() {
    const title = encodeURIComponent(`Bug: `);
    const body = encodeURIComponent(reportText);
    const url = `https://github.com/khawkins98/Hush/issues/new?title=${title}&body=${body}`;
    window.open(url, "_blank");
  }

  onMount(async () => {
    try {
      appVersion = await invoke<string>("get_app_version");
    } catch {
      appVersion = "unknown";
    }
    try {
      os = `macOS ${await osVersion()}`;
    } catch {
      os = "macOS";
    }
    await generateReport();
  });
</script>

<div class="debug-window">
  <header class="debug-window-header">
    <span class="debug-window-title">Debug Console</span>
    <span class="debug-window-version">{appVersion}</span>
  </header>

  <div class="debug-window-console">
    <DebugConsole />
  </div>

  <details class="debug-window-report">
    <summary class="debug-report-summary">Issue Report</summary>
    <p class="debug-report-note">
      Review before sharing — log entries may contain transcription content.
    </p>
    <div class="debug-report-actions">
      <button type="button" class="debug-btn" onclick={generateReport}>
        Refresh
      </button>
      <button
        type="button"
        class="debug-btn"
        disabled={!reportText}
        onclick={onCopyReport}
      >
        {copied ? "Copied!" : "Copy Report"}
      </button>
      <button
        type="button"
        class="debug-btn"
        disabled={!reportText}
        onclick={openGitHubIssue}
      >
        Open GitHub Issue ↗
      </button>
    </div>
    <pre class="debug-report-block">{reportText || "Generating…"}</pre>
  </details>
</div>

<style>
  :global(body) {
    margin: 0;
    padding: 0;
    background: #141414;
    color: #e6edf3;
    font-family:
      -apple-system,
      BlinkMacSystemFont,
      "Segoe UI",
      sans-serif;
    font-size: 13px;
    overflow: hidden;
  }

  .debug-window {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: #141414;
    color: #e6edf3;
  }

  .debug-window-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.75rem;
    background: #1e1e1e;
    border-bottom: 1px solid #333;
    /* macOS title-bar drag region */
    -webkit-app-region: drag;
    flex-shrink: 0;
  }

  .debug-window-title {
    font-weight: 600;
    font-size: 0.85rem;
    color: #cdd9e5;
  }

  .debug-window-version {
    font-size: 0.75rem;
    color: #8b949e;
    font-family: "SF Mono", monospace;
  }

  /* DebugConsole fills the available space; the details panel
     collapses to just its summary line by default */
  .debug-window-console {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .debug-window-report {
    flex-shrink: 0;
    border-top: 1px solid #333;
    background: #1a1a1a;
  }

  .debug-report-summary {
    padding: 0.4rem 0.75rem;
    cursor: pointer;
    font-size: 0.78rem;
    color: #8b949e;
    user-select: none;
    list-style: none;
  }

  .debug-report-summary::marker,
  .debug-report-summary::-webkit-details-marker {
    display: none;
  }

  .debug-report-summary::before {
    content: "▶ ";
    font-size: 0.6rem;
    color: #8b949e;
  }

  details[open] .debug-report-summary::before {
    content: "▼ ";
  }

  .debug-report-note {
    font-size: 0.75rem;
    color: #8b949e;
    margin: 0 0.75rem 0.5rem;
    line-height: 1.4;
  }

  .debug-report-actions {
    display: flex;
    gap: 0.4rem;
    padding: 0 0.75rem 0.5rem;
  }

  .debug-btn {
    padding: 0.25rem 0.6rem;
    border-radius: 4px;
    border: 1px solid #444;
    background: #2a2a2a;
    color: #cdd9e5;
    font-size: 0.75rem;
    cursor: pointer;
    transition: background 0.15s;
  }

  .debug-btn:hover:not(:disabled) {
    background: #333;
  }

  .debug-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .debug-report-block {
    background: #0d1117;
    border-top: 1px solid #333;
    padding: 0.5rem 0.75rem;
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 0.7rem;
    line-height: 1.5;
    color: #e6edf3;
    white-space: pre-wrap;
    word-break: break-all;
    overflow-y: auto;
    max-height: 200px;
    margin: 0;
  }
</style>
