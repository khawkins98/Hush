<!--
  Settings → Replacements tab (#332 phase 1, slice 3 — see also
  PermissionsTab #387 + VocabularyTab #389). Thin shell around
  ReplacementsPanel.svelte; all IPC and list state is in
  src/lib/state/replacements.svelte.ts (#721).

  Replacement rules are post-Whisper text substitutions
  ("anth" → "Anthropic"). The find/replace pair is stored with
  a stable sort order so the user controls precedence; the
  backend applies them in order on every transcript before it
  hits the clipboard.

  Lifecycle: loads on mount. Pre-extraction the page eagerly
  loaded replacements on every Settings open regardless of
  active tab; now the IPC fires only when the tab actually
  mounts.
-->
<script lang="ts">
  import { tick } from "svelte";
  import { onMount } from "svelte";

  import ReplacementsPanel from "./ReplacementsPanel.svelte";
  import { formatErrorDisplay } from "./errors";
  import { replacements } from "./state/replacements.svelte";

  let newFind = $state("");
  let newReplace = $state("");
  let inputEl = $state<HTMLInputElement | null>(null);

  async function add(e: Event) {
    e.preventDefault();
    const find = newFind.trim();
    const replace = newReplace;
    if (!find) return;
    try {
      await replacements.add(find, replace);
      newFind = "";
      newReplace = "";
      // Refocus the find field for paste-and-add workflow.
      await tick();
      inputEl?.focus();
    } catch (err) {
      replacements.error = formatErrorDisplay(err);
    }
  }

  async function remove(rule: import("./types").ReplacementRule) {
    try {
      await replacements.remove(rule);
    } catch (e) {
      replacements.error = formatErrorDisplay(e);
    }
  }

  onMount(() => {
    void replacements.load();
  });
</script>

<ReplacementsPanel
  replacements={replacements.rules}
  replacementsLoaded={replacements.loaded}
  replacementsError={replacements.error}
  bind:newFind
  bind:newReplace
  bind:inputEl
  onSubmit={add}
  onDelete={remove}
/>
