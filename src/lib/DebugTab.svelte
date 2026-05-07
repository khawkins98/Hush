<!--
  Settings → Debug tab (#532).

  Two sections:
  1. Debug window launcher — opens the floating debug-console
     palette (the "debug" Tauri window) via `open_debug_window`.
     The live log is displayed there so it can float above the
     main app while the user clicks around.
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
  import "./settings-tab.css";
  import { formatBuildTimestamp, type BuildInfo } from "./utils/format";

  type LogEntry = {
    seq: number;
    timestampMs: number;
    level: string;
    target: string;
    message: string;
  };

  let buildInfo = $state<BuildInfo>({ version: "…", buildTimestamp: 0 });

  /// One per-phase row from `get_startup_timings` (#584 Angle 1).
  /// `elapsedMs` is absolute ms since `build_default` started; the
  /// per-phase duration is the gap between consecutive entries.
  type StartupPhase = { name: string; elapsedMs: number };
  let startupTimings = $state<StartupPhase[]>([]);
  // Threshold above which the total is rendered amber rather than
  // green. Matches the issue spec (#584 Angle 1: ≥ 2 s = warn).
  const STARTUP_AMBER_MS = 2000;
  let os = $state<string>("macOS");
  let reportText = $state<string>("");
  let copied = $state(false);

  let audioTapResult = $state<string>("");
  let audioTapRunning = $state(false);

  /// Resolved on-disk log dir + today's filename (#622-followup).
  /// `null` means file logging is off (HUSH_LOG_FILE=off, non-macOS,
  /// or path resolution failed). The Debug tab hides the whole
  /// section in that case rather than advertising a path the user
  /// disabled.
  type LogDirInfo = { dir: string; todayFile: string };
  let logDirInfo = $state<LogDirInfo | null>(null);
  let logPathCopied = $state(false);

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
      `**Hush version:** ${buildInfo.version}`,
      `**Built:** ${formatBuildTimestamp(buildInfo.buildTimestamp)}`,
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

  /// Reveal the log directory in Finder so the user can grep the
  /// daily file with whatever tool they prefer (`tail`, `less`,
  /// QuickLook). Routes through `tauri-plugin-shell::open` which
  /// macOS treats as `open <path>` — Finder is the registered
  /// handler for directories.
  async function onRevealLogDir() {
    if (!logDirInfo) return;
    try {
      const { open } = await import("@tauri-apps/plugin-shell");
      await open(logDirInfo.dir);
    } catch (e) {
      console.warn("[hush] reveal log dir failed", e);
    }
  }

  /// Copy a ready-to-run grep command for today's file. Cheaper
  /// than copying the path alone — most uses of the path are "I
  /// want to find a specific event" rather than "I want to look
  /// at the directory."
  async function onCopyGrepSnippet() {
    if (!logDirInfo) return;
    const cmd = `grep "" "${logDirInfo.dir}/${logDirInfo.todayFile}"`;
    try {
      await navigator.clipboard.writeText(cmd);
      logPathCopied = true;
      setTimeout(() => (logPathCopied = false), 2000);
    } catch (e) {
      console.warn("[hush] clipboard write failed", e);
    }
  }

  async function onProbeAudioTap() {
    audioTapRunning = true;
    audioTapResult = "";
    try {
      audioTapResult = await invoke<string>("probe_audio_tap_permission");
    } catch (e) {
      audioTapResult = `Error: ${typeof e === "object" ? JSON.stringify(e) : e}`;
    } finally {
      audioTapRunning = false;
    }
  }

  onMount(async () => {
    try {
      buildInfo = await invoke<BuildInfo>("get_build_info");
    } catch {
      buildInfo = { version: "unknown", buildTimestamp: 0 };
    }
    try {
      os = `macOS ${await osVersion()}`;
    } catch {
      os = "macOS";
    }
    // #584 Angle 1 — pull the startup phase trace. Empty list is
    // expected on tests + on `--no-default-features` builds where
    // some phases skip; the section gates on `length > 0`.
    try {
      startupTimings = await invoke<StartupPhase[]>("get_startup_timings");
    } catch {
      startupTimings = [];
    }
    // #622-followup: surface the on-disk log path. None means file
    // logging is off — section stays hidden in that case.
    try {
      logDirInfo = await invoke<LogDirInfo | null>("get_log_dir");
    } catch {
      logDirInfo = null;
    }
    await generateReport();
  });
</script>

<h2 class="tab-title">Debug</h2>

{#if startupTimings.length > 0}
  {@const totalMs = startupTimings[startupTimings.length - 1].elapsedMs}
  {@const isSlow = totalMs >= STARTUP_AMBER_MS}
  <!--
    Per-phase startup trace (#584 Angle 1). Collapsible so it doesn't
    crowd the more frequently-used issue-report section, but expanded
    headline shows the total + an amber "slow" indicator when the
    boot crossed STARTUP_AMBER_MS — visible at-a-glance regression
    signal without opening the panel.
  -->
  <section
    class="settings-group"
    aria-labelledby="debug-startup-heading"
    data-testid="debug-startup-section"
  >
    <details>
      <summary class="startup-summary">
        <h2 id="debug-startup-heading" class="group-heading">
          ⏱ Startup
          <span
            class="startup-total"
            class:startup-total-slow={isSlow}
            data-testid="debug-startup-total"
          >
            {totalMs} ms
          </span>
        </h2>
      </summary>
      <p class="settings-row-note">
        Per-phase wall-clock from <code>AppState::build_default</code>.
        Capture once at boot. {isSlow
          ? "Total ≥ 2 s — investigate which phase regressed."
          : ""}
      </p>
      <table class="startup-table" data-testid="debug-startup-table">
        <thead>
          <tr>
            <th>Phase</th>
            <th>At (ms)</th>
            <th>Duration (ms)</th>
          </tr>
        </thead>
        <tbody>
          {#each startupTimings as phase, i}
            {@const prevMs = i === 0 ? 0 : startupTimings[i - 1].elapsedMs}
            {@const durationMs = phase.elapsedMs - prevMs}
            <tr>
              <td>{phase.name}</td>
              <td class="num">{phase.elapsedMs}</td>
              <td class="num">{durationMs}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </details>
  </section>
{/if}

<section class="settings-group" aria-labelledby="debug-log-heading">
  <h2 id="debug-log-heading" class="group-heading">Backend log</h2>
  <p class="settings-row-note">
    Open the floating debug console to watch the live Rust
    <code>tracing</code> log stream while clicking around the app.
  </p>
  <button
    type="button"
    class="ghost"
    onclick={() => invoke("open_debug_window")}
  >
    Open Debug Console ↗
  </button>
</section>

{#if logDirInfo}
  <section
    class="settings-group"
    aria-labelledby="debug-log-file-heading"
    data-testid="debug-log-file-section"
  >
    <h2 id="debug-log-file-heading" class="group-heading">Log file</h2>
    <p class="settings-row-note">
      Hush writes a daily-rolling plain-text log here so you can
      grep events after the fact (e.g. when filing a bug report).
      Disable with <code>HUSH_LOG_FILE=off</code>.
    </p>
    <pre class="log-path" data-testid="debug-log-path">{logDirInfo.dir}/{logDirInfo.todayFile}</pre>
    <div class="log-actions">
      <button
        type="button"
        class="ghost small"
        onclick={onRevealLogDir}
        data-testid="debug-log-reveal"
      >
        Reveal in Finder ↗
      </button>
      <button
        type="button"
        class="ghost small"
        onclick={onCopyGrepSnippet}
        data-testid="debug-log-copy-grep"
      >
        {logPathCopied ? "Copied!" : "Copy grep command"}
      </button>
    </div>
  </section>
{/if}

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

<section class="settings-group" aria-labelledby="audio-tap-probe-heading">
  <h2 id="audio-tap-probe-heading" class="group-heading">
    CoreAudio tap probe (#585)
  </h2>
  <p class="settings-row-note">
    Runs <code>AudioHardwareCreateProcessTap</code> from inside the Hush bundle.
    Watch for which dialog appears — mic icon (good) or lock icon (Screen Recording).
    Requires a signed bundle build (<code>npm run tauri:bundle</code>).
  </p>
  <button
    type="button"
    class="ghost"
    disabled={audioTapRunning}
    onclick={onProbeAudioTap}
  >
    {audioTapRunning ? "Running…" : "Run Audio Tap Probe"}
  </button>
  {#if audioTapResult}
    <pre class="report-block" style="margin-top: 0.75rem">{audioTapResult}</pre>
  {/if}
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

  /* Log file path display (#622-followup). Same monospace shell
   * as `.report-block` but inline-sized for a single path. */
  .log-path {
    background: #141414;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.65rem;
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 0.72rem;
    line-height: 1.4;
    color: #e6edf3;
    white-space: pre-wrap;
    word-break: break-all;
    margin: 0 0 0.6rem;
    user-select: all;
  }

  .log-actions {
    display: flex;
    gap: 0.5rem;
  }

  .report-block {
    background: #141414;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.75rem;
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 0.72rem;
    line-height: 1.5;
    color: #e6edf3;
    white-space: pre-wrap;
    word-break: break-all;
    overflow-y: auto;
    max-height: 280px;
    margin: 0;
  }

  /* Startup timing trace (#584 Angle 1). */
  .startup-summary {
    cursor: pointer;
    list-style-position: inside;
  }
  .startup-summary > .group-heading {
    display: inline;
  }
  .startup-total {
    font-weight: normal;
    font-size: 0.85rem;
    color: var(--text-secondary);
    margin-left: 0.4rem;
  }
  .startup-total-slow {
    color: #c08000;
    font-weight: 500;
  }
  .startup-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
    margin-top: 0.5rem;
  }
  .startup-table th,
  .startup-table td {
    padding: 0.3rem 0.5rem;
    border-bottom: 1px solid var(--border);
    text-align: left;
  }
  .startup-table th {
    font-weight: 500;
    color: var(--text-secondary);
  }
  .startup-table td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-family: "SF Mono", "Fira Code", monospace;
  }
</style>
