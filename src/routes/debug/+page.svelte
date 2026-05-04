<!--
  Floating debug-console window (#540).

  A self-contained always-on-top palette showing only the live
  backend log stream (via DebugConsole.svelte). The issue-report
  generator lives in Settings → Debug tab instead.

  Opening: invoked from Settings → Debug tab via `open_debug_window`
  IPC command, which shows the "debug" window declared in
  tauri.conf.json. The window is always-on-top so it floats above
  the main app while the user clicks around.

  This page intentionally shares no layout with the main app
  shell — it's a minimal dark surface whose whole job is the log.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import DebugConsole from "$lib/DebugConsole.svelte";

  let appVersion = $state<string>("…");

  onMount(async () => {
    try {
      appVersion = await invoke<string>("get_app_version");
    } catch {
      appVersion = "unknown";
    }
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

  /* DebugConsole fills the remaining space */
  .debug-window-console {
    flex: 1;
    min-height: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    padding: 0 0.75rem 0.75rem;
  }
</style>
