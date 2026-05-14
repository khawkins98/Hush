/// Vocabulary + preset-pack + language-style state module (#694).
/// Extracted from VocabularyTab.svelte so all vocabulary-domain IPC
/// has a single owner and the tab component becomes a thin mediator.
///
/// What lives here:
///   - terms, packs, languageStyle reactive state
///   - load() (parallel-fetches all three on first tab visit)
///   - createTerm(), deleteTerm(), togglePack(), setLanguageStyle()
///
/// What stays in VocabularyTab:
///   - newVocab / inputEl (form input state, tied to component DOM)
///   - the tick() + inputEl.focus() reset after a successful add
///   - onMount wiring (the component owns the mount lifecycle)
import { invoke } from "@tauri-apps/api/core";

import { formatErrorDisplay, type ErrorDisplay } from "$lib/errors";
import type { LanguageStyle, PackStatus, VocabularyTerm } from "$lib/types";

let terms = $state<VocabularyTerm[]>([]);
let loaded = $state(false);
let error = $state<ErrorDisplay | null>(null);
let packs = $state<PackStatus[]>([]);
let languageStyle = $state<LanguageStyle>("american");

export const vocab = {
  get terms() {
    return terms;
  },
  get loaded() {
    return loaded;
  },
  get error() {
    return error;
  },
  set error(val: ErrorDisplay | null) {
    error = val;
  },
  get packs() {
    return packs;
  },
  get languageStyle() {
    return languageStyle;
  },

  /// Load vocabulary terms, preset packs, and language style in
  /// parallel. Called once on VocabularyTab mount. Re-fetches on
  /// every mount rather than caching across mounts — pack
  /// enablement state can change across Settings opens.
  async load(): Promise<void> {
    try {
      const [fetchedTerms, fetchedPacks, style] = await Promise.all([
        invoke<VocabularyTerm[]>("vocabulary_list"),
        invoke<PackStatus[]>("list_packs"),
        invoke<string>("get_language_style"),
      ]);
      terms = fetchedTerms;
      packs = fetchedPacks;
      languageStyle = (style as LanguageStyle) ?? "american";
      error = null;
    } catch (e) {
      error = formatErrorDisplay(e);
    } finally {
      loaded = true;
    }
  },

  /// Add a new vocabulary term. On success, the new term is appended
  /// to `terms` and the error is cleared; on failure, `error` is set.
  /// Returns the created term so the caller can reset its form inputs,
  /// or `null` if the IPC failed.
  async createTerm(term: string): Promise<VocabularyTerm | null> {
    try {
      const created = await invoke<VocabularyTerm>("vocabulary_create", {
        term,
      });
      terms = [...terms, created];
      error = null;
      return created;
    } catch (e) {
      error = formatErrorDisplay(e);
      return null;
    }
  },

  async deleteTerm(id: number): Promise<void> {
    try {
      await invoke("vocabulary_delete", { id });
      terms = terms.filter((v) => v.id !== id);
      error = null;
    } catch (e) {
      error = formatErrorDisplay(e);
    }
  },

  async togglePack(slug: string, enable: boolean): Promise<void> {
    try {
      await invoke(enable ? "enable_pack" : "disable_pack", { slug });
      packs = packs.map((p) => (p.slug === slug ? { ...p, enabled: enable } : p));
    } catch (e) {
      error = formatErrorDisplay(e);
    }
  },

  async setLanguageStyle(style: LanguageStyle): Promise<void> {
    try {
      await invoke("set_language_style", { style });
      languageStyle = style;
    } catch (e) {
      error = formatErrorDisplay(e);
    }
  },
};
