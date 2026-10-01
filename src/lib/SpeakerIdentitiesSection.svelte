<!--
  Settings → Meeting → Speakers: the saved cross-session speaker identities
  (#667), with rename, merge and delete.

  Merge exists mainly for re-enrolment after the #1013 feature fix: older
  voiceprints ("Older voice profile") still label past meetings but never
  match new ones, so a returning person shows up as a new speaker. Merging
  that new speaker *into* the older named profile keeps the name and adopts
  the new voiceprint (the backend's merge keeps the newer embedding when
  versions differ). The `keep` side is always the merge target the user
  picks, so the name they chose survives.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import { formatErrorMessage } from "./errors";
  import { timerScope } from "./timers";
  import type { SpeakerIdentity } from "./types";
  import "./settings-tab.css";

  type Props = { enabled: boolean };
  let { enabled }: Props = $props();

  const timers = timerScope();
  onDestroy(() => timers.clearAll());

  let speakers = $state<SpeakerIdentity[]>([]);
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let busy = $state(false);

  let renamingId = $state<number | null>(null);
  let renameDraft = $state("");
  // Merge: the row being merged away, and the target it merges into.
  let mergeFromId = $state<number | null>(null);
  let mergeIntoId = $state<number | null>(null);
  let confirmDeleteId = $state<number | null>(null);

  function nameOf(s: SpeakerIdentity): string {
    return s.displayName?.trim() || `Unnamed speaker ${s.id}`;
  }

  async function load() {
    try {
      speakers = await invoke<SpeakerIdentity[]>("speaker_list");
      error = null;
    } catch (e) {
      error = formatErrorMessage(e);
    } finally {
      loaded = true;
    }
  }

  async function run(action: () => Promise<unknown>) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await action();
      await load();
    } catch (e) {
      error = formatErrorMessage(e);
    } finally {
      busy = false;
    }
  }

  function startRename(s: SpeakerIdentity) {
    renamingId = s.id;
    renameDraft = s.displayName ?? "";
    mergeFromId = null;
  }

  function saveRename(id: number) {
    const displayName = renameDraft.trim() || null;
    renamingId = null;
    void run(() => invoke("speaker_rename", { id, displayName }));
  }

  function startMerge(s: SpeakerIdentity) {
    mergeFromId = s.id;
    mergeIntoId = null;
    renamingId = null;
  }

  function confirmMerge() {
    const absorbId = mergeFromId;
    const keepId = mergeIntoId;
    if (absorbId === null || keepId === null) return;
    mergeFromId = null;
    mergeIntoId = null;
    void run(() => invoke("speaker_merge", { keepId, absorbId }));
  }

  function onDelete(id: number) {
    if (confirmDeleteId !== id) {
      confirmDeleteId = id;
      timers.set(() => {
        if (confirmDeleteId === id) confirmDeleteId = null;
      }, 5000);
      return;
    }
    confirmDeleteId = null;
    void run(() => invoke("speaker_delete", { id }));
  }

  let hasLegacy = $derived(speakers.some((s) => s.legacyVoiceprint));
  let mergeFrom = $derived(speakers.find((s) => s.id === mergeFromId) ?? null);
  let mergeInto = $derived(speakers.find((s) => s.id === mergeIntoId) ?? null);

  onMount(load);
</script>

{#if enabled || speakers.length > 0}
  <section class="speaker-identities" aria-labelledby="saved-speakers-heading" data-testid="saved-speakers">
    <h3 id="saved-speakers-heading" class="saved-speakers-heading">Saved speakers</h3>

    {#if hasLegacy}
      <p class="settings-row-desc" data-testid="saved-speakers-legacy-hint">
        Profiles marked <strong>Older voice profile</strong> were saved before a
        recognition upgrade and won't be matched in new meetings. To keep a
        name, use <strong>Merge into…</strong> on the new speaker and pick the
        older profile — it keeps its name and learns the new voice.
      </p>
    {/if}

    {#if loaded && speakers.length === 0}
      <p class="settings-row-desc">
        No saved speakers yet. They appear here after a meeting where the same
        voice talked enough to be remembered.
      </p>
    {/if}

    <ul class="speaker-list">
      {#each speakers as s (s.id)}
        <li class="settings-row speaker-row" data-testid="saved-speaker-{s.id}">
          <div class="speaker-main">
            {#if renamingId === s.id}
              <form
                class="speaker-rename"
                onsubmit={(e) => {
                  e.preventDefault();
                  saveRename(s.id);
                }}
              >
                <input
                  type="text"
                  bind:value={renameDraft}
                  aria-label="Name for {nameOf(s)}"
                  placeholder="Name"
                  maxlength="80"
                />
                <button type="submit" class="ghost" disabled={busy}>Save</button>
                <button type="button" class="ghost" onclick={() => (renamingId = null)}>Cancel</button>
              </form>
            {:else}
              <span class="speaker-name">{nameOf(s)}</span>
              <span class="speaker-meta">
                {s.utteranceCount} {s.utteranceCount === 1 ? "utterance" : "utterances"}
                {#if s.legacyVoiceprint}
                  <span class="speaker-badge" data-testid="legacy-badge">Older voice profile</span>
                {/if}
              </span>
            {/if}
          </div>

          {#if renamingId !== s.id && mergeFromId !== s.id}
            <div class="speaker-actions">
              <button type="button" class="ghost" disabled={busy} onclick={() => startRename(s)}>
                Rename
              </button>
              {#if speakers.length > 1}
                <button type="button" class="ghost" disabled={busy} onclick={() => startMerge(s)}>
                  Merge into…
                </button>
              {/if}
              <button
                type="button"
                class="ghost speaker-delete"
                class:confirming={confirmDeleteId === s.id}
                disabled={busy}
                onclick={() => onDelete(s.id)}
                aria-label={confirmDeleteId === s.id
                  ? `Click again to delete ${nameOf(s)}`
                  : `Delete ${nameOf(s)}`}
              >
                {confirmDeleteId === s.id ? "Click to confirm" : "Delete"}
              </button>
            </div>
          {/if}

          {#if mergeFromId === s.id}
            <div class="speaker-merge" data-testid="speaker-merge-panel">
              <label class="speaker-merge-label">
                Merge <strong>{nameOf(s)}</strong> into
                <select
                  bind:value={mergeIntoId}
                  aria-label="Speaker to merge {nameOf(s)} into"
                >
                  <option value={null} disabled>Choose a speaker…</option>
                  {#each speakers.filter((o) => o.id !== s.id) as o (o.id)}
                    <option value={o.id}>
                      {nameOf(o)}{o.legacyVoiceprint ? " (older profile)" : ""}
                    </option>
                  {/each}
                </select>
              </label>
              {#if mergeFrom && mergeInto}
                <p class="speaker-merge-note">
                  {nameOf(mergeInto)} keeps its name; {nameOf(mergeFrom)}'s
                  meetings are relabelled to it.
                  {#if mergeInto.legacyVoiceprint && !mergeFrom.legacyVoiceprint}
                    It will be recognised again in new meetings.
                  {/if}
                </p>
              {/if}
              <div class="speaker-actions">
                <button
                  type="button"
                  class="ghost"
                  disabled={busy || mergeIntoId === null}
                  onclick={confirmMerge}
                >Merge</button>
                <button type="button" class="ghost" onclick={() => (mergeFromId = null)}>Cancel</button>
              </div>
            </div>
          {/if}
        </li>
      {/each}
    </ul>

    {#if error}
      <p class="settings-error" role="alert">{error}</p>
    {/if}
  </section>
{/if}

<style>
  .speaker-identities {
    margin-top: 0.85rem;
  }
  .saved-speakers-heading {
    margin: 0 0 0.4rem;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--text-primary);
  }
  .speaker-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .speaker-row {
    flex-wrap: wrap;
    align-items: center;
  }
  .speaker-main {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
    flex: 1;
  }
  .speaker-name {
    font-weight: 600;
    color: var(--text-primary);
    overflow-wrap: anywhere;
  }
  .speaker-meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .speaker-badge {
    padding: 0.05rem 0.45rem;
    border-radius: 999px;
    font-size: 0.72rem;
    background-color: var(--warning-bg);
    border: 1px solid var(--warning-border);
    color: var(--warning-text);
  }
  .speaker-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .speaker-delete {
    color: var(--danger);
  }
  .speaker-delete.confirming {
    background-color: var(--danger-bg);
    border-color: var(--danger);
    font-weight: 600;
  }
  .speaker-rename {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    align-items: center;
  }
  .speaker-rename input {
    flex: 1;
    min-width: 8rem;
  }
  .speaker-merge {
    flex-basis: 100%;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    padding-top: 0.45rem;
    border-top: 1px solid var(--border);
  }
  .speaker-merge-label {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
    color: var(--text-secondary);
  }
  .speaker-merge-note {
    margin: 0;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
</style>
