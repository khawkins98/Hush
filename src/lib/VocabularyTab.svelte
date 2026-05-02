<!--
  Settings → Vocabulary tab (#332 phase 1, slice 2 — first slice
  was Permissions, see PermissionsTab.svelte). Owns its own
  state, IPC, and lifecycle wiring; the actual list+form
  presentation still lives in VocabularyPanel.svelte (which
  predates the tab decomposition).

  Vocabulary entries are short proper-noun-shaped strings the
  user wants Whisper to recognise verbatim. They're applied as a
  prompt prefix to inference, not as post-hoc replacements —
  that's the Replacements tab's job.

  Lifecycle: loads on mount. Pre-extraction the page eagerly
  loaded vocabulary on every Settings open regardless of active
  tab; now the IPC fires only when the user actually visits the
  tab. Same data, smaller cold-boot when opening Settings to a
  non-Vocabulary tab.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";

  import VocabularyPanel from "./VocabularyPanel.svelte";
  import { formatErrorDisplay, type ErrorDisplay } from "./errors";
  import type { VocabularyTerm } from "./types";

  let vocabulary = $state<VocabularyTerm[]>([]);
  let loaded = $state(false);
  let loadError = $state<ErrorDisplay | null>(null);
  let newVocab = $state("");
  let inputEl = $state<HTMLInputElement | null>(null);

  async function load(): Promise<void> {
    try {
      vocabulary = await invoke<VocabularyTerm[]>("vocabulary_list");
      loadError = null;
    } catch (e) {
      loadError = formatErrorDisplay(e);
    } finally {
      loaded = true;
    }
  }

  async function add(e: Event) {
    e.preventDefault();
    const term = newVocab.trim();
    if (!term) return;
    try {
      const created = await invoke<VocabularyTerm>("vocabulary_create", { term });
      vocabulary = [...vocabulary, created];
      newVocab = "";
      loadError = null;
      // Refocus the input so a power user can paste-and-add a
      // long list without reaching for the mouse — tested on
      // hands-on usage by the original author.
      await tick();
      inputEl?.focus();
    } catch (err) {
      loadError = formatErrorDisplay(err);
    }
  }

  async function remove(term: VocabularyTerm) {
    try {
      await invoke("vocabulary_delete", { id: term.id });
      vocabulary = vocabulary.filter((v) => v.id !== term.id);
      loadError = null;
    } catch (e) {
      loadError = formatErrorDisplay(e);
    }
  }

  onMount(() => {
    void load();
  });
</script>

<VocabularyPanel
  {vocabulary}
  vocabularyLoaded={loaded}
  vocabularyError={loadError}
  bind:newVocab
  bind:inputEl
  onSubmit={add}
  onDelete={remove}
/>
