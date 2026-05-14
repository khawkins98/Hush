<!--
  Settings → Vocabulary tab (#332 phase 1, slice 2). Thin
  mediator: owns form-input state (newVocab, inputEl) and the
  onMount lifecycle trigger; all domain IPC and reactive
  vocabulary/packs/languageStyle state live in
  state/vocabulary.svelte.ts (#694).

  Vocabulary entries are short proper-noun-shaped strings the
  user wants Whisper to recognise verbatim. They're applied as a
  prompt prefix to inference, not as post-hoc replacements —
  that's the Replacements tab's job.
-->
<script lang="ts">
  import { onMount, tick } from "svelte";

  import VocabularyPanel from "./VocabularyPanel.svelte";
  import { vocab } from "./state/vocabulary.svelte";

  // Form input state stays in the component because it's tied to
  // the DOM (tick + inputEl.focus() on successful add).
  let newVocab = $state("");
  let inputEl = $state<HTMLInputElement | null>(null);

  async function add(e: Event) {
    e.preventDefault();
    const term = newVocab.trim();
    if (!term) return;
    const created = await vocab.createTerm(term);
    if (created) {
      newVocab = "";
      // Refocus so a power user can paste-and-add a long list
      // without reaching for the mouse.
      await tick();
      inputEl?.focus();
    }
  }

  onMount(() => {
    void vocab.load();
  });
</script>

<VocabularyPanel
  vocabulary={vocab.terms}
  vocabularyLoaded={vocab.loaded}
  vocabularyError={vocab.error}
  packs={vocab.packs}
  languageStyle={vocab.languageStyle}
  bind:newVocab
  bind:inputEl
  onSubmit={add}
  onDelete={(term) => vocab.deleteTerm(term.id)}
  onTogglePack={vocab.togglePack}
  onSetLanguageStyle={vocab.setLanguageStyle}
/>
