<!--
  Settings → Debug tab (#532).

  Two sections:
  1. Live backend log console — a scrolling view of the Rust
     `tracing` event stream, rendered by `DebugConsole.svelte`.
  2. Issue report generator — collects version, OS, and the last
     50 log entries into a pre-formatted block for filing a GitHub
     issue. The block is copyable and there's a direct "Open issue"
     link with a pre-filled title.

  Privacy note: log entries may contain real transcription content.
  The copy block includes a reminder to review before sharing.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { version as osVersion } from "@tauri-apps/plugin-os";
  import { onMount } from "svelte";
  import DebugConsole from "./DebugConsole.svelte";
  import "./settings-tab.css";

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

<h2 class="tab-title">Debug</h2>

<section class="settings-group" aria-labelledby="debug-log-heading">
  <h2 id="debug-log-heading" class="group-heading">Backend log</h2>
  <p class="settings-row-note">
    Live view of the Rust backend's <code>tracing</code> log stream.
    Hover to pause auto-scroll.
  </p>
  <DebugConsole />
</section>

<section class="settings-group" aria-labelledby="debug-report-heading">
  <h2 id="debug-report-heading" class="group-heading">Issue report</h2>
  <p class="settings-row-note">
    Review the generated report before sharing — log entries may
    contain transcription content.
  </p>
  <div class="report-actions">
    <button type="button" class="ghost small" onclick={generateReport}>
      Refresh
    </button>
    <button
      type="button"
      class="ghost small"
      disabled={!reportText}
      onclick={onCopyReport}
    >
      {copied ? "Copied!" : "Copy"}
    </button>
    <button
      type="button"
      class="ghost small"
      disabled={!reportText}
      onclick={openGitHubIssue}
    >
      Open GitHub issue ↗
    </button>
  </div>
  <pre class="report-block">{reportText || "Generating…"}</pre>
</section>

<style>
  .settings-row-note {
    font-size: 0.82rem;
    color: var(--text-secondary);
    margin: 0 0 0.6rem;
    line-height: 1.5;
  }

  .report-actions {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }

  .report-block {
    background: var(--bg-code, #1a1a1a);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.75rem;
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 0.72rem;
    line-height: 1.5;
    color: var(--text-primary);
    white-space: pre-wrap;
    word-break: break-all;
    overflow-y: auto;
    max-height: 280px;
    margin: 0;
  }
</style>
