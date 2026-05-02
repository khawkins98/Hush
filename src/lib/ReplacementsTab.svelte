<!--
  Settings → Replacements tab (#332 phase 1, slice 3 — see also
  PermissionsTab #387 + VocabularyTab #389). Owns its own state,
  IPC, and lifecycle wiring; the actual list+form presentation
  still lives in ReplacementsPanel.svelte (which predates the tab
  decomposition).

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
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";

  import ReplacementsPanel from "./ReplacementsPanel.svelte";
  import { formatErrorDisplay, type ErrorDisplay } from "./errors";
  import type { ReplacementRule } from "./types";

  let replacements = $state<ReplacementRule[]>([]);
  let loaded = $state(false);
  let loadError = $state<ErrorDisplay | null>(null);
  let newFind = $state("");
  let newReplace = $state("");
  let inputEl = $state<HTMLInputElement | null>(null);

  async function load(): Promise<void> {
    try {
      replacements = await invoke<ReplacementRule[]>("replacements_list");
      loadError = null;
    } catch (e) {
      loadError = formatErrorDisplay(e);
    } finally {
      loaded = true;
    }
  }

  async function add(e: Event) {
    e.preventDefault();
    const find = newFind.trim();
    const replace = newReplace;
    if (!find) return;
    try {
      // sortOrder = current length: append-at-end semantics.
      // The backend stores it on the row and uses it for the
      // canonical ordering; reordering UI would update the
      // sortOrder without re-creating rows.
      const created = await invoke<ReplacementRule>("replacement_create", {
        findText: find,
        replaceText: replace,
        sortOrder: replacements.length,
      });
      replacements = [...replacements, created];
      newFind = "";
      newReplace = "";
      loadError = null;
      // Refocus the find field for paste-and-add workflow.
      await tick();
      inputEl?.focus();
    } catch (err) {
      loadError = formatErrorDisplay(err);
    }
  }

  async function remove(rule: ReplacementRule) {
    try {
      await invoke("replacement_delete", { id: rule.id });
      replacements = replacements.filter((r) => r.id !== rule.id);
      loadError = null;
    } catch (e) {
      loadError = formatErrorDisplay(e);
    }
  }

  onMount(() => {
    void load();
  });
</script>

<ReplacementsPanel
  {replacements}
  replacementsLoaded={loaded}
  replacementsError={loadError}
  bind:newFind
  bind:newReplace
  bind:inputEl
  onSubmit={add}
  onDelete={remove}
/>
