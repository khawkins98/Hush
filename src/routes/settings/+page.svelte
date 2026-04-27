<!--
  Standalone Settings window — Phase 2 scaffold of the IA redesign.
  Loaded into the secondary `settings` Tauri window (label `settings`,
  configured in `tauri.conf.json`). Opened via:

    - macOS: ⌘, accelerator on the native menu (Hush → Settings…)
    - Any platform: backend `settings_window::open()` IPC, called
      from the main window or in response to deep-link intents.

  This PR ships the empty shell so Phase 3 can lift the
  Model / Vocabulary / Replacements / Permissions panels into it
  without simultaneously inventing a window. Each tab body is a
  placeholder; the underlying components stay on the main window's
  "Configuration" tab until that move lands.

  Toolbar tabs (the macOS-idiomatic settings pattern) rather than
  a sidebar: Apple HIG says settings windows use toolbar tabs, and
  the settings surface is small enough that the horizontal real
  estate is fine.
-->
<script lang="ts">
  type SettingsTab =
    | "general"
    | "audio"
    | "model"
    | "vocabulary"
    | "replacements"
    | "permissions"
    | "about";

  let active = $state<SettingsTab>("general");

  const tabs: Array<{ key: SettingsTab; label: string }> = [
    { key: "general", label: "General" },
    { key: "audio", label: "Audio" },
    { key: "model", label: "Model" },
    { key: "vocabulary", label: "Vocabulary" },
    { key: "replacements", label: "Replacements" },
    { key: "permissions", label: "Permissions" },
    { key: "about", label: "About" },
  ];
</script>

<main class="settings-window">
  <header class="settings-toolbar" aria-label="Settings categories">
    {#each tabs as tab (tab.key)}
      <button
        type="button"
        class="tab-button"
        class:active={active === tab.key}
        aria-current={active === tab.key ? "page" : undefined}
        data-testid="settings-tab-{tab.key}"
        onclick={() => (active = tab.key)}
      >
        {tab.label}
      </button>
    {/each}
  </header>

  <section class="tab-body" aria-live="polite">
    <h1 class="tab-title">{tabs.find((t) => t.key === active)?.label ?? ""}</h1>
    <p class="placeholder">
      This pane is a Phase 2 scaffold. The
      {#if active === "model"}model picker
      {:else if active === "vocabulary"}vocabulary terms
      {:else if active === "replacements"}post-transcription find/replace rules
      {:else if active === "permissions"}macOS permissions diagnostic
      {:else if active === "audio"}audio source defaults
      {:else if active === "general"}first-run, autostart, and hotkey settings
      {:else}build info and links
      {/if}
      will move here in the next PR.
    </p>
  </section>
</main>

<style>
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: #f3f3f5;
    color: #0f0f0f;
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  .settings-window {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }

  .settings-toolbar {
    display: flex;
    gap: 0.25rem;
    padding: 0.6rem 0.75rem;
    background-color: #ececef;
    border-bottom: 1px solid #d8d8dc;
    overflow-x: auto;
  }

  .tab-button {
    padding: 0.4rem 0.85rem;
    border: 1px solid transparent;
    background-color: transparent;
    color: #333;
    font-family: inherit;
    font-size: 0.85rem;
    font-weight: 500;
    border-radius: 6px;
    cursor: pointer;
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s, color 0.12s;
  }
  .tab-button:hover { background-color: rgba(0, 0, 0, 0.06); }
  .tab-button.active {
    background-color: white;
    border-color: #d1d1d8;
    color: #2c3e8f;
    font-weight: 600;
  }
  .tab-button:focus-visible {
    outline: 2px solid #6a8cf0;
    outline-offset: 1px;
  }

  .tab-body {
    flex: 1;
    padding: 2rem 2.5rem;
    max-width: 48rem;
    width: 100%;
    box-sizing: border-box;
  }

  .tab-title {
    margin: 0 0 0.75rem;
    font-size: 1.4rem;
    letter-spacing: -0.01em;
  }

  .placeholder {
    margin: 0;
    color: #666;
    font-size: 0.95rem;
    line-height: 1.5;
  }

  @media (prefers-color-scheme: dark) {
    :global(html), :global(body) {
      background-color: #1d1d1f;
      color: #e8e8e8;
    }
    .settings-toolbar {
      background-color: #2a2a2d;
      border-bottom-color: #38383b;
    }
    .tab-button { color: #d8d8d8; }
    .tab-button:hover { background-color: rgba(255, 255, 255, 0.06); }
    .tab-button.active {
      background-color: #1d1d1f;
      border-color: #38383b;
      color: #b8c8ff;
    }
    .placeholder { color: #a8a8a8; }
  }
</style>
