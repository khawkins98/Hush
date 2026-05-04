<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type {
    AudioSourceListing,
    BuiltinAppEntry,
    MeetingAppKind,
    MeetingAppOverride,
    ModelCard,
  } from "./types";

  type Props = {
    overrides: MeetingAppOverride[];
    overridesLoaded: boolean;
    overridesError: ErrorDisplayShape | null;
    /// Built-in classification table (#320). Read-only; rendered
    /// inside a `<details>` disclosure so users can see what's
    /// already covered before adding a redundant override.
    /// Empty array is fine — the disclosure just stays empty.
    defaults: BuiltinAppEntry[];
    /// Audio sources for the per-app profile dropdown (#427 Item 5).
    /// Loaded by the parent from `audio_list_sources` and threaded
    /// in. Empty array hides the dropdowns; the per-row UI keeps
    /// rendering the kind + remove controls as before.
    audioSources?: AudioSourceListing[];
    /// Whisper models for the per-app profile dropdown (#427 Item 5).
    /// Loaded by the parent from `model_list`. Same empty-array
    /// fallback as `audioSources`.
    models?: ModelCard[];
    // Form fields are bindable so the parent owns the state and can
    // clear them after a successful add.
    newAppName: string;
    newKind: MeetingAppKind;
    inputEl?: HTMLInputElement | null;
    onSubmit: (e: Event) => void | Promise<void>;
    /// Batch-add for the variant-suggestion box (#320 part 2).
    /// Called when the user picks N defaults from the suggestion
    /// list and clicks "Add N selected variants".
    onSubmitVariants: (
      appNames: string[],
      kind: MeetingAppKind,
    ) => void | Promise<void>;
    onChangeKind: (
      override: MeetingAppOverride,
      kind: MeetingAppKind,
    ) => void | Promise<void>;
    onDelete: (override: MeetingAppOverride) => void | Promise<void>;
    /// Set or clear the per-app audio profile (#427 Item 5). Both
    /// args together represent the full intended state — `null`
    /// resets to "use the global default", a string pins the
    /// value. Optional so the panel still renders cleanly if a
    /// future caller doesn't wire this yet.
    onSetProfile?: (
      override: MeetingAppOverride,
      preferredAudioSource: string | null,
      preferredModelId: string | null,
    ) => void | Promise<void>;
  };

  let {
    overrides,
    overridesLoaded,
    overridesError,
    defaults,
    audioSources = [],
    models = [],
    newAppName = $bindable(),
    newKind = $bindable(),
    inputEl = $bindable(),
    onSubmit,
    onSubmitVariants,
    onChangeKind,
    onDelete,
    onSetProfile,
  }: Props = $props();

  // Redundant-override warning (#320). When the user types an
  // app_name that's already in the built-in defaults table, surface
  // a non-blocking notice so they don't add a redundant row. Live
  // — recomputes as they type. Trim to match the backend's
  // upsert trim.
  let redundantDefault = $derived.by(() => {
    const trimmed = newAppName.trim();
    if (trimmed.length === 0) return null;
    return defaults.find((d) => d.appName === trimmed) ?? null;
  });

  // Group defaults by classification kind so the disclosure renders
  // Meeting and Media as separate visual sections. Source order
  // within each kind is preserved (mirrors `default_table()`'s
  // curated order).
  let defaultsByKind = $derived.by(() => {
    const meeting: BuiltinAppEntry[] = [];
    const media: BuiltinAppEntry[] = [];
    const other: BuiltinAppEntry[] = [];
    for (const entry of defaults) {
      if (entry.kind === "meeting") meeting.push(entry);
      else if (entry.kind === "media") media.push(entry);
      else other.push(entry);
    }
    return { meeting, media, other };
  });

  // Variant suggestions (#320 part 2). When the user types in the
  // Add input, find every default entry whose `appName` contains
  // the typed substring (case-insensitive). The classifier needs
  // a separate row per platform variant — surfacing the matches
  // lets the user pick "give me overrides for all 7 Zoom
  // variants" in one click rather than typing each manually.
  //
  // Excludes the typed text itself (the redundancy warning
  // already handles the exact-match case) and only fires when
  // there are at least 2 matches — a single match is just the
  // redundancy warning's territory.
  let variantSuggestions = $derived.by(() => {
    const trimmed = newAppName.trim();
    if (trimmed.length < 2) return [];
    const lower = trimmed.toLowerCase();
    const matches = defaults.filter((d) =>
      d.appName.toLowerCase().includes(lower),
    );
    // If there's exactly one match AND it equals the typed text,
    // the redundancy warning carries the message — no suggestion
    // needed.
    if (matches.length <= 1) return [];
    return matches;
  });

  // Which suggested variants the user has checked. Defaults to
  // all-checked when the suggestion list first appears (the
  // "yes give me everything for this app" path is the common
  // case). Re-resets when the suggestion list changes.
  let selectedVariants = $state<Set<string>>(new Set());

  // Re-init `selectedVariants` whenever the suggestion list shape
  // changes. Tracking the list's identity directly via $effect so
  // the user's mid-edit selections aren't preserved across a
  // different input — that would be confusing.
  let lastSuggestionKey = $state("");
  $effect(() => {
    const key = variantSuggestions.map((s) => s.appName).join("|");
    if (key !== lastSuggestionKey) {
      lastSuggestionKey = key;
      selectedVariants = new Set(variantSuggestions.map((s) => s.appName));
    }
  });

  function toggleVariant(appName: string) {
    const next = new Set(selectedVariants);
    if (next.has(appName)) {
      next.delete(appName);
    } else {
      next.add(appName);
    }
    selectedVariants = next;
  }

  async function onAddVariants() {
    const picks = variantSuggestions
      .map((s) => s.appName)
      .filter((n) => selectedVariants.has(n));
    if (picks.length === 0) return;
    await onSubmitVariants(picks, newKind);
  }

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

  {#if redundantDefault}
    <p class="override-redundant-note" data-testid="override-redundant-note">
      <strong>{redundantDefault.appName}</strong> is already classified as
      <em
        >{redundantDefault.kind === "meeting"
          ? "Meeting"
          : redundantDefault.kind === "media"
            ? "Media"
            : "Ignore"}</em
      >
      by default — you only need an override to change that.
    </p>
  {/if}

  {#if variantSuggestions.length > 0}
    <!--
      Variant suggestion box (#320 part 2). Active-win returns
      different strings per OS (bundle ID on macOS, exe basename
      on Windows, process name on Linux). Surfacing the matching
      defaults as a checkbox list lets the user pick the
      cross-platform variants they want overrides for in one
      submit, instead of typing each manually.
    -->
    <div
      class="override-variants"
      data-testid="override-variant-suggestions"
    >
      <p class="override-variants-prose">
        Did you mean one of these {variantSuggestions.length} variants? Each row is what
        <code>active-win</code> reports on a different platform — pick whichever you want overrides for and click <em>Add</em>.
      </p>
      <ul class="override-variants-list">
        {#each variantSuggestions as suggestion (suggestion.appName)}
          <li>
            <label>
              <input
                type="checkbox"
                checked={selectedVariants.has(suggestion.appName)}
                onchange={() => toggleVariant(suggestion.appName)}
              />
              <code>{suggestion.appName}</code>
              <span class="override-variants-kind"
                >({suggestion.kind === "meeting"
                  ? "Meeting"
                  : suggestion.kind === "media"
                    ? "Media"
                    : "Ignore"} default)</span
              >
            </label>
          </li>
        {/each}
      </ul>
      <button
        type="button"
        class="ghost"
        data-testid="override-variant-submit"
        disabled={selectedVariants.size === 0}
        onclick={onAddVariants}
      >
        Add {selectedVariants.size} as
        {newKind === "meeting"
          ? "Meeting"
          : newKind === "media"
            ? "Media"
            : "Ignore"}
      </button>
    </div>
  {/if}

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

          {#if onSetProfile && (audioSources.length > 0 || models.length > 0)}
            <!--
              Per-app audio profile (#427 Item 5). Two optional
              dropdowns: pick a preferred audio source / Whisper
              model for this app. "— use global —" maps to the DB
              NULL sentinel that the foreground-watcher will
              interpret as "fall through to the global default".
              Each select fires onSetProfile with the FULL state
              (both fields) so the panel always sends the user's
              full intent — no merge.
            -->
            <div class="override-profile" data-testid="override-profile-row">
              {#if audioSources.length > 0}
                <label class="override-profile-field">
                  <span class="override-profile-label">Audio</span>
                  <select
                    class="override-profile-select"
                    data-testid={`override-audio-${override.appName}`}
                    aria-label="Preferred audio source for {override.appName}"
                    value={override.preferredAudioSource ?? ""}
                    onchange={(e) => {
                      const next =
                        (e.currentTarget as HTMLSelectElement).value || null;
                      void onSetProfile?.(
                        override,
                        next,
                        override.preferredModelId ?? null,
                      );
                    }}
                  >
                    <option value="">— use global —</option>
                    {#each audioSources as src (src.id)}
                      <option value={src.id}>
                        {src.name}{src.isDefault ? " (default)" : ""}
                      </option>
                    {/each}
                  </select>
                </label>
              {/if}

              {#if models.length > 0}
                <label class="override-profile-field">
                  <span class="override-profile-label">Model</span>
                  <select
                    class="override-profile-select"
                    data-testid={`override-model-${override.appName}`}
                    aria-label="Preferred model for {override.appName}"
                    value={override.preferredModelId ?? ""}
                    onchange={(e) => {
                      const next =
                        (e.currentTarget as HTMLSelectElement).value || null;
                      void onSetProfile?.(
                        override,
                        override.preferredAudioSource ?? null,
                        next,
                      );
                    }}
                  >
                    <option value="">— use global —</option>
                    {#each models as model (model.id)}
                      <option value={model.id}>{model.displayName}</option>
                    {/each}
                  </select>
                </label>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if defaults.length > 0}
    <details class="override-defaults" data-testid="override-defaults">
      <summary>
        Built-in defaults ({defaults.length} entries)
      </summary>
      <p class="hint-prose">
        Meeting Mode classifies these without any override. Each
        platform variant is a separate row — macOS returns reverse-DNS
        bundle ids, Windows returns the executable basename
        (<code>.exe</code>), Linux returns the process name. Adding
        an override for any of these is redundant unless you're
        re-classifying it.
      </p>

      {#if defaultsByKind.meeting.length > 0}
        <h3 class="override-defaults-heading">Meeting</h3>
        <ul class="override-defaults-list">
          {#each defaultsByKind.meeting as entry (entry.appName)}
            <li><code>{entry.appName}</code></li>
          {/each}
        </ul>
      {/if}

      {#if defaultsByKind.media.length > 0}
        <h3 class="override-defaults-heading">Media</h3>
        <ul class="override-defaults-list">
          {#each defaultsByKind.media as entry (entry.appName)}
            <li><code>{entry.appName}</code></li>
          {/each}
        </ul>
      {/if}

      {#if defaultsByKind.other.length > 0}
        <h3 class="override-defaults-heading">Ignored</h3>
        <ul class="override-defaults-list">
          {#each defaultsByKind.other as entry (entry.appName)}
            <li><code>{entry.appName}</code></li>
          {/each}
        </ul>
      {/if}
    </details>
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
  color: var(--text-primary);
  display: flex;
  align-items: center;
  gap: 0.4rem;
}

.panel-subtitle {
  font-size: 0.78rem;
  color: var(--text-muted);
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
  background-color: var(--bg-sidebar);
  color: var(--text-secondary);
}

.panel-tag-overrides {
  background-color: #f0e1f7;
  color: #743ea0;
}

.hint-prose {
  margin: 0 0 1rem;
  color: var(--text-secondary);
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
  /* First row: app name, kind dropdown, remove button. Second
     row spans all three columns and holds the optional profile
     dropdowns (#427 Item 5). The redundant static-label column
     shipped earlier was dropped in the walkthrough polish round
     — the dropdown's selected value was already visible. */
  grid-template-columns: 1fr auto auto;
  align-items: center;
  gap: 0.6rem;
  padding: 0.6rem 0.85rem;
  background-color: var(--bg-surface);
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

/* Per-app audio profile (#427 Item 5). Spans all three columns
   below the app name + kind + remove row so the dropdowns aren't
   crowded into the narrow auto columns. */
.override-profile {
  grid-column: 1 / -1;
  display: flex;
  flex-wrap: wrap;
  gap: 0.6rem;
  padding-top: 0.4rem;
  border-top: 1px dashed #e7e7e7;
}
.override-profile-field {
  display: flex;
  align-items: center;
  gap: 0.35rem;
  font-size: 0.8rem;
}
.override-profile-label {
  color: var(--text-muted);
  font-weight: 500;
}
.override-profile-select {
  padding: 0.18em 0.4em;
  font-size: 0.8rem;
  font-family: inherit;
  max-width: 12rem;
}

.empty-history {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: var(--bg-surface);
  border: 1px dashed #d1d1d1;
  border-radius: 8px;
  color: var(--text-muted);
  font-size: 0.9rem;
  text-align: center;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: var(--bg-surface);
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
  color: var(--text-primary);
  background-color: var(--bg-surface);
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
  background-color: var(--bg-app);
}
button.ghost.danger {
  color: var(--danger);
  border-color: var(--danger-border);
}
button.ghost.danger:hover:not(:disabled) {
  background-color: var(--danger-bg);
  border-color: var(--danger);
}
/* Confirming state — armed first click, awaiting the second one. */
button.ghost.danger.confirming {
  background-color: var(--danger-bg);
  border-color: var(--danger);
  color: #8a0000;
  font-weight: 600;
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .panel-tag-overrides {
    background-color: #3a2a4a;
    color: #d4a8e8;
  }
  :root:not([data-theme="light"]) .override-row {
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) .empty-history {
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button {
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  :root:not([data-theme="light"]) button.ghost.danger {
    color: #ff9090;
  }
  :root:not([data-theme="light"]) button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
  }
  :root:not([data-theme="light"]) button.ghost.danger.confirming {
    background-color: #3a1818;
    border-color: var(--danger);
    color: #ffb0b0;
  }
}
:root[data-theme="dark"] .panel-tag-overrides {
  background-color: #3a2a4a;
  color: #d4a8e8;
}
:root[data-theme="dark"] .override-row {
  border-color: #3a3a3a;
}
:root[data-theme="dark"] .empty-history {
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button {
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button.ghost:hover:not(:disabled) {
  background-color: #353535;
}
:root[data-theme="dark"] button.ghost.danger {
  color: #ff9090;
}
:root[data-theme="dark"] button.ghost.danger:hover:not(:disabled) {
  background-color: #3a1818;
}
:root[data-theme="dark"] button.ghost.danger.confirming {
  background-color: #3a1818;
  border-color: var(--danger);
  color: #ffb0b0;
}
</style>
