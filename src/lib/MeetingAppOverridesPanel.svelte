<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type { MeetingAppKind, MeetingAppOverride } from "./types";

  type Props = {
    overrides: MeetingAppOverride[];
    overridesLoaded: boolean;
    overridesError: ErrorDisplayShape | null;
    // Form fields are bindable so the parent owns the state and can
    // clear them after a successful add.
    newAppName: string;
    newKind: MeetingAppKind;
    inputEl?: HTMLInputElement | null;
    onSubmit: (e: Event) => void | Promise<void>;
    onChangeKind: (
      override: MeetingAppOverride,
      kind: MeetingAppKind,
    ) => void | Promise<void>;
    onDelete: (override: MeetingAppOverride) => void | Promise<void>;
  };

  let {
    overrides,
    overridesLoaded,
    overridesError,
    newAppName = $bindable(),
    newKind = $bindable(),
    inputEl = $bindable(),
    onSubmit,
    onChangeKind,
    onDelete,
  }: Props = $props();

  // Per-row click-to-confirm. First click arms the row's Remove
  // button (label flips to "Click to confirm"); second click within
  // 5 s fires `onDelete`; the timer clears the armed state so a
  // stale arm can't catch the user later. Same shape as
  // VocabularyPanel / ReplacementsPanel / HistoryPanel.
  let confirmingAppName = $state<string | null>(null);
  let confirmTimer: number | undefined;

  function handleDelete(override: MeetingAppOverride) {
    if (confirmingAppName === override.appName) {
      window.clearTimeout(confirmTimer);
      confirmingAppName = null;
      void onDelete(override);
      return;
    }
    window.clearTimeout(confirmTimer);
    confirmingAppName = override.appName;
    confirmTimer = window.setTimeout(() => {
      confirmingAppName = null;
    }, 5000);
  }

</script>

<section
  class="overrides panel-overrides"
  aria-labelledby="overrides-heading"
>
  <header class="history-header">
    <h2 id="overrides-heading">
      <span class="panel-tag panel-tag-overrides" aria-hidden="true">A</span>
      App classification
      <span class="panel-subtitle">teaches Meeting Mode about your apps</span>
    </h2>
  </header>
  <p class="hint-prose">
    Hush ships a built-in list of well-known meeting apps (Zoom, Teams,
    Slack, …) and media apps (Spotify, YouTube, …). Add an override
    here to teach the classifier about an app it doesn't know — or to
    re-classify one it gets wrong. Use <em>Ignore</em> to suppress an
    app the defaults would catch. Edits take effect on the next
    Meeting session start.
  </p>

  {#if overridesError}
    <ErrorDisplay error={overridesError} scope="App classification" />
  {/if}

  <form class="override-form" onsubmit={onSubmit}>
    <input
      type="text"
      bind:this={inputEl}
      bind:value={newAppName}
      placeholder="App name (e.g. com.example.huddle)"
      aria-label="App identifier"
    />
    <select bind:value={newKind} aria-label="Classification">
      <option value="meeting">Meeting</option>
      <option value="media">Media</option>
      <option value="other">Ignore</option>
    </select>
    <button type="submit" disabled={newAppName.trim().length === 0}>
      Add
    </button>
  </form>

  {#if !overridesLoaded}
    <p class="loading-skeleton">Loading overrides…</p>
  {:else if overrides.length === 0}
    <p class="empty-history">
      No overrides yet — Meeting Mode is using the built-in defaults.
      Add one above when you find an app it gets wrong.
    </p>
  {:else}
    <ul class="override-list">
      {#each overrides as override (override.appName)}
        <li class="override-row">
          <code class="override-name">{override.appName}</code>
          <select
            class="override-kind"
            value={override.kind}
            onchange={(e) =>
              onChangeKind(
                override,
                (e.currentTarget as HTMLSelectElement).value as MeetingAppKind,
              )}
            aria-label="Classification for {override.appName}"
          >
            <option value="meeting">Meeting</option>
            <option value="media">Media</option>
            <option value="other">Ignore</option>
          </select>
          <button
            class="ghost danger"
            class:confirming={confirmingAppName === override.appName}
            onclick={() => handleDelete(override)}
            aria-label={confirmingAppName === override.appName
              ? `Click again to confirm removing override for ${override.appName}`
              : `Remove override for ${override.appName}`}
          >
            {confirmingAppName === override.appName ? "Click to confirm" : "Remove"}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
.overrides {
  margin-top: 2.5rem;
  text-align: left;
  border-left: 3px solid #e1e1e1;
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-overrides {
  border-left-color: #c08af0;
}

.history-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}

.history-header h2 {
  margin: 0;
  font-size: 1.1rem;
  font-weight: 600;
  color: #333;
  display: flex;
  align-items: center;
  gap: 0.4rem;
}

.panel-subtitle {
  font-size: 0.78rem;
  color: #888;
  font-weight: 400;
  margin-left: 0.4rem;
}

.panel-tag {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.4em;
  height: 1.4em;
  border-radius: 5px;
  font-size: 0.75em;
  font-weight: 700;
  background-color: #e8e8e8;
  color: #444;
}

.panel-tag-overrides {
  background-color: #f0e1f7;
  color: #743ea0;
}

.hint-prose {
  margin: 0 0 1rem;
  color: #555;
  font-size: 0.9rem;
  line-height: 1.5;
  max-width: 36rem;
}

.override-form {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 1rem;
  flex-wrap: wrap;
}

.override-form input[type="text"] {
  flex: 1 1 18rem;
  min-width: 12rem;
  padding: 0.5em 0.85em;
  font-size: 0.9rem;
}

.override-form select {
  padding: 0.5em 0.6em;
  font-size: 0.9rem;
}

.override-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.override-row {
  display: grid;
  /* Three columns: app name, kind dropdown, remove button. The
     redundant static-label column shipped earlier was dropped in
     the walkthrough polish round — the dropdown's selected value
     was already visible. */
  grid-template-columns: 1fr auto auto;
  align-items: center;
  gap: 0.6rem;
  padding: 0.6rem 0.85rem;
  background-color: white;
  border: 1px solid #e1e1e1;
  border-radius: 8px;
  font-size: 0.9rem;
}

.override-name {
  font-family:
    ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.85rem;
  word-break: break-all;
}

.override-kind {
  padding: 0.25em 0.5em;
  font-size: 0.85rem;
}

.empty-history {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border: 1px dashed #d1d1d1;
  border-radius: 8px;
  color: #666;
  font-size: 0.9rem;
  text-align: center;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border-radius: 6px;
  color: #999;
  font-size: 0.9rem;
  text-align: center;
  font-style: italic;
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.5em 1em;
  font-size: 0.9em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
  font-weight: 600;
}
button:hover:not(:disabled) {
  border-color: #c08af0;
}
button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}
button.ghost {
  padding: 0.3em 0.75em;
  font-size: 0.8rem;
  font-weight: 500;
  background-color: transparent;
  border: 1px solid #d1d1d1;
}
button.ghost:hover:not(:disabled) {
  background-color: #f0f0f0;
}
button.ghost.danger {
  color: #b03030;
  border-color: #e1b8b8;
}
button.ghost.danger:hover:not(:disabled) {
  background-color: #fbeaea;
  border-color: #d83a3a;
}
/* Confirming state — armed first click, awaiting the second one. */
button.ghost.danger.confirming {
  background-color: #fbeaea;
  border-color: #d83a3a;
  color: #8a0000;
  font-weight: 600;
}

/* Error rendering migrated to the shared ErrorDisplay component
   (#199 + follow-up). */

@media (prefers-color-scheme: dark) {
  .history-header h2 {
    color: #d8d8d8;
  }
  .hint-prose {
    color: #b8b8b8;
  }
  .panel-tag {
    background-color: #303030;
    color: #c8c8c8;
  }
  .panel-tag-overrides {
    background-color: #3a2a4a;
    color: #d4a8e8;
  }
  .override-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .empty-history {
    background-color: #1f1f1f;
    border-color: #3a3a3a;
    color: #999;
  }
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button.ghost {
    color: #f0f0f0;
    background-color: transparent;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  button.ghost.danger {
    color: #ff9090;
    border-color: #5a2020;
  }
  button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
  }
  button.ghost.danger.confirming {
    background-color: #3a1818;
    border-color: #d83a3a;
    color: #ffb0b0;
  }
}
</style>
