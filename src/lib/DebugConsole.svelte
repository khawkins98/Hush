<!--
  Live backend log console (#532).

  Subscribes to the `log:event` Tauri event and maintains a local
  ring buffer (capped at MAX_ENTRIES). On mount it calls
  `get_log_entries` for the startup-to-mount catchup window; any
  entries already received via the live listener are deduplicated
  by their `seq` field.

  Auto-scrolls to the bottom on each new entry unless the user has
  scrolled up (hoverPaused state). A "Clear" button empties the
  local list without touching the Rust ring buffer.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount, tick } from "svelte";
  import { Events } from "./events";

  type LogEntry = {
    seq: number;
    timestampMs: number;
    level: string;
    target: string;
    message: string;
  };

  const MAX_ENTRIES = 200;

  let entries = $state<LogEntry[]>([]);
  let scrollEl = $state<HTMLElement | null>(null);
  let isPaused = $state(false);
  let unlisten: UnlistenFn | null = null;

  // Highest seq we have seen — used to deduplicate the snapshot.
  let maxSeenSeq = -1;

  function append(entry: LogEntry) {
    if (entry.seq <= maxSeenSeq) return;
    maxSeenSeq = entry.seq;
    entries.push(entry);
    if (entries.length > MAX_ENTRIES) {
      entries.splice(0, entries.length - MAX_ENTRIES);
    }
    if (!isPaused) {
      tick().then(() => {
        scrollEl?.scrollTo({ top: scrollEl.scrollHeight, behavior: "instant" });
      });
    }
  }

  function levelClass(level: string): string {
    switch (level.toUpperCase()) {
      case "ERROR":
        return "level-error";
      case "WARN":
        return "level-warn";
      case "DEBUG":
        return "level-debug";
      case "TRACE":
        return "level-trace";
      default:
        return "level-info";
    }
  }

  function formatTime(ms: number): string {
    const d = new Date(ms);
    return d.toTimeString().slice(0, 8);
  }

  function onClear() {
    entries = [];
    maxSeenSeq = -1;
  }

  let allCopied = $state(false);
  async function onCopyAll() {
    const text = entries
      .map(
        (e) =>
          `[${new Date(e.timestampMs).toISOString()}] ${e.level.padEnd(5)} ${e.target} ${e.message}`,
      )
      .join("\n");
    try {
      await navigator.clipboard.writeText(text);
      allCopied = true;
      setTimeout(() => (allCopied = false), 2000);
    } catch (err) {
      console.warn("[hush] clipboard write failed", err);
    }
  }

  onMount(async () => {
    // Subscribe first to guarantee no events are missed between the
    // snapshot call and the listener registration.
    unlisten = await listen<LogEntry>(Events.LogEvent, (e) => {
      append(e.payload);
    });

    try {
      const snapshot = await invoke<LogEntry[]>("get_log_entries");
      for (const entry of snapshot) {
        append(entry);
      }
    } catch (e) {
      console.warn("[hush] get_log_entries failed", e);
    }
  });

  onDestroy(() => {
    unlisten?.();
  });
</script>

<div class="debug-console-toolbar">
  <span class="debug-console-count">
    {entries.length} entries
  </span>
  <div class="debug-console-actions">
    <button type="button" class="ghost small" disabled={entries.length === 0} onclick={onCopyAll}>
      {allCopied ? "Copied!" : "Copy All"}
    </button>
    <button type="button" class="ghost small" onclick={onClear}> Clear </button>
  </div>
</div>

<div
  class="debug-console-output"
  bind:this={scrollEl}
  onmouseenter={() => (isPaused = true)}
  onmouseleave={() => (isPaused = false)}
  role="log"
  aria-label="Backend log entries"
  aria-live="polite"
>
  {#if entries.length === 0}
    <p class="debug-console-empty">No log entries yet.</p>
  {:else}
    {#each entries as entry (entry.seq)}
      <div class="log-row">
        <span class="log-time">{formatTime(entry.timestampMs)}</span>
        <span class={`log-level ${levelClass(entry.level)}`}
          >{entry.level.slice(0, 4)}</span
        >
        <span class="log-target">{entry.target}</span>
        <span class="log-message">{entry.message}</span>
      </div>
    {/each}
  {/if}
</div>

<style>
  .debug-console-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.4rem 0;
    margin-bottom: 0.5rem;
  }

  .debug-console-count {
    font-size: 0.78rem;
    color: var(--text-secondary);
  }

  .debug-console-actions {
    display: flex;
    gap: 0.4rem;
  }

  /* The log output is intentionally always dark — it's a terminal
     surface, not a theme-aware panel. Using explicit colours rather
     than --text-primary / --bg-surface so the text stays legible in
     both light and dark app themes (light mode sets --text-primary
     to a dark value which would disappear on the dark background). */
  .debug-console-output {
    height: 320px;
    overflow-y: auto;
    background: #141414;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem;
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 0.72rem;
    line-height: 1.5;
    color: #e6edf3;
  }

  .debug-console-empty {
    margin: 0;
    color: #8b949e;
    font-style: italic;
    text-align: center;
    padding-top: 2rem;
  }

  .log-row {
    display: flex;
    gap: 0.4rem;
    min-width: 0;
    padding: 0.05rem 0;
  }

  .log-time {
    flex-shrink: 0;
    color: var(--text-secondary);
  }

  .log-level {
    flex-shrink: 0;
    width: 3.2rem;
    font-weight: 600;
    text-align: right;
  }

  .level-error {
    color: #f85149;
  }
  .level-warn {
    color: #e3b341;
  }
  .level-info {
    color: #58a6ff;
  }
  .level-debug {
    color: #7ee787;
  }
  .level-trace {
    color: #8b949e;
  }

  .log-target {
    flex-shrink: 0;
    color: var(--text-secondary);
    max-width: 18rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .log-message {
    flex: 1;
    min-width: 0;
    overflow-wrap: break-word;
    word-break: break-all;
    white-space: pre-wrap;
  }
</style>
