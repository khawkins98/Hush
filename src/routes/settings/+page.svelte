<!--
  Standalone Settings window — kept as a thin wrapper around
  `SettingsPanel.svelte` for #479 slice 2 so the legacy tray
  "Settings…" menu item still has somewhere to land. Slice 3
  deletes this route entirely along with the `settings` Tauri
  window, the `open_settings` IPC, and `capabilities/settings.json`.

  Cross-window invalidation today is **minimal**:
    - `model:download-done` is broadcast, picked up by
      `SettingsPanel` and the main page.
    - Replacements / vocabulary changes are read at the next
      dictation start by the Rust pipeline.
-->
<script lang="ts">
  import SettingsPanel from "$lib/SettingsPanel.svelte";
</script>

<main class="settings-window">
  <SettingsPanel />
</main>

<style>
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: var(--bg-app);
    color: var(--text-primary);
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen,
      Ubuntu, Cantarell, "Helvetica Neue", Arial, sans-serif,
      "Apple Color Emoji", "Segoe UI Emoji";
    -webkit-font-smoothing: antialiased;
    color-scheme: light dark;
    accent-color: auto;
  }

  .settings-window {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }
</style>
